//! Story 2.1: starting the Daemon from a System Config file — exit codes, stderr, and the startup
//! log naming the resolved path and its source.
//!
//! Almost everything drives `run_with` in-process with an already-complete shutdown future, so
//! the success path runs start to finish without a signal. The environment variable and default
//! path go in through `Discovery`, so no test mutates the process environment. The last tests
//! run the real binary: `main`'s hand-off and real SIGTERM/SIGINT handling cannot be checked
//! in-process.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use hexput_daemon::{Discovery, run_with};

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hexput-daemon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        Self { dir }
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, text).expect("a temporary file");
        path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A log sink the test can read after the Daemon's subscriber has written to it.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedLog {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).expect("UTF-8 log")
    }
}

struct Run {
    code: String,
    stdout: String,
    stderr: String,
    log: String,
}

impl Run {
    fn exit(&self, expected: u8) -> &Self {
        assert_eq!(
            self.code,
            format!("{:?}", ExitCode::from(expected)),
            "expected exit {expected}\nstdout: {}\nstderr: {}\nlog: {}",
            self.stdout,
            self.stderr,
            self.log
        );
        self
    }
}

fn daemon(args: &[&str], discovery: &Discovery) -> Run {
    let mut argv: Vec<OsString> = vec!["hexput-daemon".into()];
    argv.extend(args.iter().map(OsString::from));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let log = SharedLog::default();
    let code = run_with(
        argv,
        discovery,
        &mut out,
        &mut err,
        log.clone(),
        std::future::ready(()),
    );
    Run {
        code: format!("{code:?}"),
        stdout: String::from_utf8(out).unwrap(),
        stderr: String::from_utf8(err).unwrap(),
        log: log.text(),
    }
}

impl Sandbox {
    /// Where this sandbox's Daemon puts its socket. Short: a socket path has a platform limit.
    fn socket(&self) -> PathBuf {
        self.dir.join("d.sock")
    }

    /// A minimal valid System Config whose socket lives inside this sandbox.
    fn valid(&self) -> String {
        format!(
            "[transport.uds]\npath = {:?}\n",
            self.socket().to_str().unwrap()
        )
    }
}

fn nothing(sandbox: &Sandbox) -> Discovery {
    Discovery::new(None, sandbox.dir.join("no-default.toml"))
}

// --- success ---

#[test]
fn a_valid_file_via_the_flag_starts_and_logs_the_path_and_source() {
    let sandbox = Sandbox::new();
    let config = sandbox.file("config.toml", &sandbox.valid());
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(0);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "");
    let loaded_line = run
        .log
        .lines()
        .find(|line| line.contains("System Config loaded"))
        .unwrap_or_else(|| panic!("no startup line in log:\n{}", run.log));
    assert!(loaded_line.contains(" INFO "), "{loaded_line}");
    assert!(
        loaded_line.contains(&format!("path={}", config.display())),
        "{loaded_line}"
    );
    assert!(loaded_line.contains("source=flag"), "{loaded_line}");
    // The shutdown future passed in is already complete, so the Daemon never waits.
    assert!(!run.log.contains("waiting"), "{}", run.log);
    assert!(run.log.contains("daemon stopping"), "{}", run.log);
}

#[test]
fn the_startup_log_names_the_env_var_and_default_sources() {
    let sandbox = Sandbox::new();
    let from_env = sandbox.file("env.toml", &sandbox.valid());
    let run = daemon(
        &[],
        &Discovery::new(
            Some(from_env.clone().into_os_string()),
            sandbox.dir.join("absent.toml"),
        ),
    );
    run.exit(0);
    assert!(
        run.log
            .contains(&format!("path={} source=env", from_env.display())),
        "{}",
        run.log
    );

    let default = sandbox.file("default.toml", &sandbox.valid());
    let run = daemon(&[], &Discovery::new(None, &default));
    run.exit(0);
    assert!(
        run.log
            .contains(&format!("path={} source=default", default.display())),
        "{}",
        run.log
    );
}

#[cfg(unix)]
#[test]
fn a_relative_path_is_logged_as_given() {
    // The path is not canonicalised: "absolute-or-as-given", so an operator sees what they typed.
    let sandbox = Sandbox::new();
    sandbox.file("config.toml", &sandbox.valid());
    let relative = pathdiff(&sandbox.dir.join("config.toml"));
    let run = daemon(&["--config", &relative], &nothing(&sandbox));
    run.exit(0);
    assert!(
        run.log.contains(&format!("path={relative} ")),
        "{}",
        run.log
    );
}

#[cfg(unix)]
/// A path to `target` relative to the current directory, built from `..` components.
fn pathdiff(target: &Path) -> String {
    let cwd = std::env::current_dir().unwrap();
    let depth = cwd.components().count() - 1;
    let mut relative = PathBuf::new();
    for _ in 0..depth {
        relative.push("..");
    }
    for component in target.components().skip(1) {
        relative.push(component);
    }
    assert!(relative.is_relative());
    relative.to_str().unwrap().to_owned()
}

#[test]
fn the_log_honours_the_configured_level() {
    let sandbox = Sandbox::new();
    let config = sandbox.file(
        "quiet.toml",
        &format!("log_level = \"warn\"\n{}", sandbox.valid()),
    );
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(0);
    assert_eq!(run.log, "", "info events are filtered out at warn");
}

// --- failures: exit 1, one line on stderr naming the file ---

#[test]
fn a_flag_naming_a_missing_file_fails_without_falling_through() {
    let sandbox = Sandbox::new();
    let env_file = sandbox.file("env.toml", &sandbox.valid());
    let default = sandbox.file("default.toml", &sandbox.valid());
    let run = daemon(
        &["--config", "/nope.toml"],
        &Discovery::new(Some(env_file.into_os_string()), default),
    );
    run.exit(1);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        "error: /nope.toml: System Config file not found (named by the --config flag)\n"
    );
    assert_eq!(
        run.log, "",
        "nothing is logged before a System Config loads"
    );
}

#[test]
fn nothing_supplied_and_no_default_names_the_default_and_the_overrides() {
    let sandbox = Sandbox::new();
    let discovery = nothing(&sandbox);
    let run = daemon(&[], &discovery);
    run.exit(1);
    assert_eq!(
        run.stderr,
        format!(
            "error: {}: no System Config file at the default path; pass --config <path> or set \
             HEXPUT_CONFIG to use another file\n",
            discovery.default_path.display()
        )
    );
}

#[test]
fn every_broken_file_exits_one_naming_the_file() {
    let sandbox = Sandbox::new();
    for (name, text, expected) in [
        ("malformed.toml", "[transport.uds\n", ":1:"),
        (
            "missing.toml",
            "[transport.tcp]\nbind = \"0.0.0.0:1\"\ntls_key = \"k\"\n",
            "missing field `tls_cert`",
        ),
        (
            "level.toml",
            "log_level = \"loud\"\n[transport.uds]\npath = \"x\"\n",
            "invalid value for `log_level`: expected one of error, warn, info, debug, trace",
        ),
        (
            "ttl.toml",
            "session_ttl_secs = 0\n[transport.uds]\npath = \"x\"\n",
            "invalid value for `session_ttl_secs`",
        ),
        (
            "unknown.toml",
            "log_levle = \"info\"\n[transport.uds]\npath = \"x\"\n",
            "unknown field `log_levle`",
        ),
        (
            "empty.toml",
            "log_level = \"info\"\n",
            "no transport configured",
        ),
    ] {
        let path = sandbox.file(name, text);
        let run = daemon(&["--config", path.to_str().unwrap()], &nothing(&sandbox));
        run.exit(1);
        assert!(
            run.stderr
                .starts_with(&format!("error: {}", path.display())),
            "{name}: {}",
            run.stderr
        );
        assert!(run.stderr.contains(expected), "{name}: {}", run.stderr);
        assert_eq!(run.stderr.lines().count(), 1, "{name}: {}", run.stderr);
        assert_eq!(run.log, "");
    }
}

// --- usage ---

#[test]
fn a_bad_invocation_is_usage_exit_two() {
    let sandbox = Sandbox::new();
    let run = daemon(&["--bogus"], &nothing(&sandbox));
    run.exit(2);
    assert!(run.stderr.contains("--bogus"), "{}", run.stderr);

    let run = daemon(&["--config"], &nothing(&sandbox));
    run.exit(2);
    assert_eq!(run.stdout, "");
}

#[test]
fn help_goes_to_stdout_and_succeeds() {
    let sandbox = Sandbox::new();
    let run = daemon(&["--help"], &nothing(&sandbox));
    run.exit(0);
    assert!(run.stdout.contains("--config <PATH>"), "{}", run.stdout);
    assert!(run.stdout.contains("HEXPUT_CONFIG"), "{}", run.stdout);
    assert_eq!(run.stderr, "");
}

#[test]
fn version_goes_to_stdout_and_succeeds() {
    let sandbox = Sandbox::new();
    let run = daemon(&["--version"], &nothing(&sandbox));
    run.exit(0);
    assert!(
        run.stdout.contains(env!("CARGO_PKG_VERSION")),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "");
}

// --- the real binary ---

/// The `hexput-daemon` binary `hexput-bin` produces, beside this test executable. See
/// `cli_core.rs`'s `hexput_binary` for why `CARGO_BIN_EXE_*` is unavailable here.
fn daemon_binary() -> PathBuf {
    let path = std::env::current_exe()
        .expect("this test executable's own path")
        .parent()
        .and_then(Path::parent)
        .expect("a target directory above this test executable")
        .join(format!("hexput-daemon{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "`{}` is not built; run the workspace build first (`cargo build --workspace`)",
        path.display()
    );
    path
}

#[test]
fn the_binary_reports_a_missing_flag_file_and_exits_one() {
    let output = std::process::Command::new(daemon_binary())
        .args(["--config", "/nope.toml"])
        .env("HEXPUT_CONFIG", "/also-nope.toml")
        .output()
        .expect("the daemon binary runs");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: /nope.toml: System Config file not found (named by the --config flag)\n"
    );
}

/// The real signal path: the binary starts from `HEXPUT_CONFIG`, logs the path, and a SIGTERM
/// (then, separately, a SIGINT) stops it with exit 0.
/// A spawned Daemon that is killed and reaped when dropped, so a failing assertion never leaves
/// it running.
#[cfg(unix)]
struct Reaped(std::process::Child);

#[cfg(unix)]
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
#[test]
fn the_binary_logs_its_config_and_stops_cleanly_on_a_signal() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const DEADLINE: Duration = Duration::from_secs(10);
    let sandbox = Sandbox::new();
    let config = sandbox.file("config.toml", &sandbox.valid());
    for signal in ["-TERM", "-INT"] {
        let mut child = Reaped(
            Command::new(daemon_binary())
                .env("HEXPUT_CONFIG", &config)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the daemon binary starts"),
        );
        // Read stderr on a thread so the wait for each line is bounded.
        let stderr = BufReader::new(child.0.stderr.take().unwrap());
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        // The handlers are installed before the wait begins, which is logged after the path.
        let mut seen = Vec::new();
        while !seen
            .last()
            .is_some_and(|l: &String| l.contains("waiting for a shutdown signal"))
        {
            match received.recv_timeout(DEADLINE) {
                Ok(line) => seen.push(line),
                Err(error) => panic!("no \"waiting\" line ({error}): {seen:?}"),
            }
        }
        assert!(
            seen.iter()
                .any(|l| l.contains(&format!("path={} source=env", config.display()))),
            "{seen:?}"
        );
        let status = Command::new("kill")
            .args([signal, &child.0.id().to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success());
        let started = Instant::now();
        let exit = loop {
            if let Some(exit) = child.0.try_wait().expect("the daemon's status") {
                break exit;
            }
            assert!(
                started.elapsed() < DEADLINE,
                "{signal}: the daemon did not exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(exit.code(), Some(0), "{signal}");
        assert!(
            !sandbox.socket().exists(),
            "{signal}: the socket is removed on a signalled shutdown"
        );
    }
}

// --- Story 2.3: serving a Unix Domain Socket ---

/// A Daemon running on its own thread until [`Serving::stop`], with a real socket.
#[cfg(unix)]
struct Serving {
    stop: tokio::sync::oneshot::Sender<()>,
    thread: std::thread::JoinHandle<Run>,
    socket: PathBuf,
}

#[cfg(unix)]
impl Serving {
    fn start(sandbox: &Sandbox, config_text: &str) -> Self {
        let config = sandbox.file("config.toml", config_text);
        let discovery = nothing(sandbox);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let argv: Vec<OsString> =
                vec!["hexput-daemon".into(), "--config".into(), config.into()];
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let log = SharedLog::default();
            let code = run_with(argv, &discovery, &mut out, &mut err, log.clone(), async {
                let _ = stopped.await;
            });
            Run {
                code: format!("{code:?}"),
                stdout: String::from_utf8(out).unwrap(),
                stderr: String::from_utf8(err).unwrap(),
                log: log.text(),
            }
        });
        Self {
            stop,
            thread,
            socket: sandbox.socket(),
        }
    }

    /// A client connection, waiting until the Daemon is listening.
    fn connect(&self) -> std::os::unix::net::UnixStream {
        use std::time::{Duration, Instant};
        let started = Instant::now();
        loop {
            match std::os::unix::net::UnixStream::connect(&self.socket) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(10)))
                        .unwrap();
                    return stream;
                }
                Err(error) => {
                    assert!(
                        started.elapsed() < Duration::from_secs(10) && !self.thread.is_finished(),
                        "the Daemon never listened: {error}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    fn stop(self) -> Run {
        let _ = self.stop.send(());
        self.thread.join().expect("the Daemon thread")
    }
}

#[cfg(unix)]
mod wire {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use hexput_port::{CorrelationId, Envelope, MessageType, Value, decode, encode, encode_frame};

    pub fn request(id: u64) -> Envelope<Value> {
        Envelope::new(CorrelationId(id), MessageType::ExecutionStart, Value::Nil)
    }

    pub fn send(stream: &mut UnixStream, envelope: &Envelope<Value>) {
        send_raw(stream, &encode_frame(&encode(envelope).unwrap()).unwrap());
    }

    pub fn send_raw(stream: &mut UnixStream, bytes: &[u8]) {
        stream.write_all(bytes).unwrap();
    }

    /// The next reply, or `None` at end of stream.
    pub fn reply(stream: &mut UnixStream) -> Option<Envelope<Value>> {
        let mut prefix = [0; 4];
        match stream.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(error) => panic!("no reply: {error}"),
        }
        let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
        stream.read_exact(&mut body).unwrap();
        Some(decode(&body).unwrap())
    }

    /// The `(id, code)` of the next reply, which must be an `Error`.
    pub fn refusal(stream: &mut UnixStream) -> (Option<u64>, String) {
        let reply = reply(stream).expect("a reply");
        assert_eq!(reply.message_type, MessageType::Error);
        let Value::Map(fields) = reply.payload else {
            panic!("an error payload is a map");
        };
        let code = fields
            .iter()
            .find(|(key, _)| key.as_str() == Some("code"))
            .and_then(|(_, value)| value.as_str())
            .expect("a code")
            .to_owned();
        (reply.id.map(CorrelationId::get), code)
    }
}

#[cfg(unix)]
#[test]
fn the_daemon_answers_over_its_socket_and_removes_it_on_shutdown() {
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut client = serving.connect();
    wire::send(&mut client, &wire::request(41));
    assert_eq!(
        wire::refusal(&mut client),
        (Some(41), "protocol.init_not_completed".to_owned())
    );
    let socket = serving.socket.clone();
    let run = serving.stop();
    run.exit(0);
    assert!(
        run.log.contains(&format!(
            "listening on a Unix Domain Socket path={}",
            socket.display()
        )),
        "{}",
        run.log
    );
    assert!(run.log.contains("daemon stopping"), "{}", run.log);
    assert!(!socket.exists(), "the socket is removed on shutdown");
}

/// Story 2.4: an `Init` over the real socket is answered with a Client ID, and each connection
/// gets its own.
#[cfg(unix)]
#[test]
fn init_over_the_socket_answers_a_client_id() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let init = |id| {
        let registrations = Value::Array(vec![Value::Map(vec![(
            Value::from("name"),
            Value::from("getUser"),
        )])]);
        Envelope::new(
            CorrelationId(id),
            MessageType::Init,
            Value::Map(vec![
                (Value::from("config"), Value::Map(vec![])),
                (Value::from("registrations"), registrations),
            ]),
        )
    };
    let mut issued = Vec::new();
    for id in [11, 12] {
        let mut client = serving.connect();
        wire::send(&mut client, &init(id));
        let reply = wire::reply(&mut client).expect("a reply");
        assert_eq!(reply.id, Some(CorrelationId(id)));
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        let fields = reply.payload.as_map().expect("a map payload");
        assert_eq!(fields.len(), 1, "exactly `client_id`: {fields:?}");
        assert_eq!(fields[0].0.as_str(), Some("client_id"));
        let client_id = fields[0].1.as_str().expect("a string Client ID").to_owned();
        assert_eq!(client_id.len(), 32);
        assert!(
            client_id
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{client_id}"
        );
        // Initialized: execution is now past the gate, and its payload is what is judged.
        wire::send(&mut client, &wire::request(id + 10));
        assert_eq!(
            wire::refusal(&mut client),
            (Some(id + 10), "protocol.invalid_payload".to_owned())
        );
        issued.push(client_id);
    }
    assert_ne!(issued[0], issued[1]);
    serving.stop().exit(0);
}

/// Story 2.6: an initialized connection runs a Script over the real socket and gets its result
/// back; a failing Script on one connection disturbs neither that connection nor another.
#[cfg(unix)]
#[test]
fn a_script_runs_over_the_socket_and_a_failure_disturbs_no_one() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let text = |t: &str| Value::from(t);
    let initialized = |serving: &Serving| {
        let mut client = serving.connect();
        let init = Envelope::new(
            CorrelationId(1),
            MessageType::Init,
            Value::Map(vec![
                (text("config"), Value::Map(vec![])),
                (text("registrations"), Value::Array(vec![])),
            ]),
        );
        wire::send(&mut client, &init);
        assert_eq!(
            wire::reply(&mut client).expect("a reply").message_type,
            MessageType::Result
        );
        client
    };
    let execution = |id, source: &str, variables: Vec<(Value, Value)>| {
        Envelope::new(
            CorrelationId(id),
            MessageType::ExecutionStart,
            Value::Map(vec![
                (text("source"), text(source)),
                (text("variables"), Value::Map(variables)),
            ]),
        )
    };

    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut a = initialized(&serving);
    let mut b = initialized(&serving);

    // A's Script fails; A's reply is the diagnostic, with A's id.
    wire::send(&mut a, &execution(20, "return 1 / 0;", vec![]));
    // B's is sent before A's reply is read. (Both finish at once: a slow Script's independence is
    // shown by `a_slow_script_delays_nothing_on_its_connection_or_another` below.)
    wire::send(
        &mut b,
        &execution(30, "return a + 1;", vec![(text("a"), Value::from(2))]),
    );
    assert_eq!(
        wire::refusal(&mut a),
        (Some(20), "arithmetic.division_by_zero".to_owned())
    );
    let reply = wire::reply(&mut b).expect("B's reply");
    assert_eq!(reply.id, Some(CorrelationId(30)));
    assert_eq!(reply.message_type, MessageType::Result);
    assert_eq!(
        reply.payload,
        Value::Map(vec![(text("value"), Value::from(3))])
    );

    // A keeps serving after its failure.
    wire::send(&mut a, &execution(21, "return [1, \"x\", null];", vec![]));
    let reply = wire::reply(&mut a).expect("A's second reply");
    assert_eq!(reply.id, Some(CorrelationId(21)));
    assert_eq!(
        reply.payload,
        Value::Map(vec![(
            text("value"),
            Value::Array(vec![Value::from(1), text("x"), Value::Nil])
        )])
    );
    serving.stop().exit(0);
}

/// Story 2.7: a slow Script delays neither a fast one sent after it on the same connection nor
/// one on another connection, over the real socket.
#[cfg(unix)]
#[test]
fn a_slow_script_delays_nothing_on_its_connection_or_another() {
    use std::io::Read;

    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    // A counted loop, never a sleep: about half a second in a debug build.
    const SLOW_TURNS: i64 = 150_000;
    let text = |t: &str| Value::from(t);
    let initialized = |serving: &Serving| {
        let mut client = serving.connect();
        let init = Envelope::new(
            CorrelationId(1),
            MessageType::Init,
            Value::Map(vec![
                (text("config"), Value::Map(vec![])),
                (text("registrations"), Value::Array(vec![])),
            ]),
        );
        wire::send(&mut client, &init);
        assert_eq!(
            wire::reply(&mut client).expect("a reply").message_type,
            MessageType::Result
        );
        client
    };
    let execution = |id, source: &str| {
        Envelope::new(
            CorrelationId(id),
            MessageType::ExecutionStart,
            Value::Map(vec![
                (text("source"), text(source)),
                (text("variables"), Value::Map(vec![])),
            ]),
        )
    };
    let slow = format!("let i = 0; while (i < {SLOW_TURNS}) {{ i = i + 1; }}; return i;");
    let value = |reply: &Envelope<Value>| {
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        reply.payload.clone()
    };
    let result = |v: i64| Value::Map(vec![(text("value"), Value::from(v))]);

    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());

    // Same connection: the fast Script sent second is answered first.
    let mut a = initialized(&serving);
    wire::send(&mut a, &execution(20, &slow));
    wire::send(&mut a, &execution(21, "return 21;"));
    let first = wire::reply(&mut a).expect("A's first reply");
    assert_eq!(first.id, Some(CorrelationId(21)), "the fast one first");
    assert_eq!(value(&first), result(21));
    let second = wire::reply(&mut a).expect("A's second reply");
    assert_eq!(second.id, Some(CorrelationId(20)));
    assert_eq!(value(&second), result(SLOW_TURNS));

    // Another connection: B inits and runs a fast Script while A's slow one is still running.
    wire::send(&mut a, &execution(30, &slow));
    let mut b = initialized(&serving);
    wire::send(&mut b, &execution(40, "return 40;"));
    let reply = wire::reply(&mut b).expect("B's reply");
    assert_eq!(reply.id, Some(CorrelationId(40)));
    assert_eq!(value(&reply), result(40));
    a.set_nonblocking(true).unwrap();
    let mut byte = [0; 1];
    let pending = a
        .read(&mut byte)
        .expect_err("A's slow Script is still running");
    assert_eq!(pending.kind(), std::io::ErrorKind::WouldBlock);
    a.set_nonblocking(false).unwrap();
    let reply = wire::reply(&mut a).expect("A's own reply, later");
    assert_eq!(reply.id, Some(CorrelationId(30)));
    assert_eq!(value(&reply), result(SLOW_TURNS));
    serving.stop().exit(0);
}

#[cfg(unix)]
#[test]
fn one_client_leaving_abruptly_disturbs_no_other() {
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut first = serving.connect();
    let mut leaving = serving.connect();
    let mut third = serving.connect();

    // Half a frame, then gone.
    let frame =
        hexput_port::encode_frame(&hexput_port::encode(&wire::request(2)).unwrap()).unwrap();
    wire::send_raw(&mut leaving, &frame[..frame.len() / 2]);
    drop(leaving);

    for (client, id) in [(&mut first, 1), (&mut third, 3)] {
        wire::send(client, &wire::request(id));
        assert_eq!(
            wire::refusal(client),
            (Some(id), "protocol.init_not_completed".to_owned())
        );
    }
    let mut later = serving.connect();
    wire::send(&mut later, &wire::request(4));
    assert_eq!(wire::refusal(&mut later).0, Some(4));
    serving.stop().exit(0);
}

#[cfg(unix)]
#[test]
fn a_malformed_frame_is_answered_and_the_connection_keeps_serving() {
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut client = serving.connect();
    wire::send_raw(&mut client, &hexput_port::encode_frame(&[0xc1]).unwrap());
    wire::send(&mut client, &wire::request(6));
    assert_eq!(
        wire::refusal(&mut client),
        (None, "protocol.malformed_frame".to_owned())
    );
    assert_eq!(
        wire::refusal(&mut client),
        (Some(6), "protocol.init_not_completed".to_owned())
    );
    serving.stop().exit(0);
}

#[cfg(unix)]
#[test]
fn an_oversized_frame_is_answered_then_the_connection_is_closed() {
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut client = serving.connect();
    let too_long = u32::try_from(hexput_port::MAX_FRAME_LEN + 1).unwrap();
    wire::send_raw(&mut client, &too_long.to_be_bytes());
    assert_eq!(
        wire::refusal(&mut client),
        (None, "protocol.frame_too_large".to_owned())
    );
    assert!(wire::reply(&mut client).is_none(), "the Daemon closed it");
    // The Daemon itself carries on.
    let mut next = serving.connect();
    wire::send(&mut next, &wire::request(1));
    assert_eq!(wire::refusal(&mut next).0, Some(1));
    serving.stop().exit(0);
}

#[cfg(unix)]
#[test]
fn a_socket_mode_is_in_force_while_serving() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &format!("{}mode = \"0600\"\n", sandbox.valid()));
    let _client = serving.connect();
    let bits = std::fs::metadata(&serving.socket)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(bits, 0o600);
    serving.stop().exit(0);
}

#[cfg(unix)]
#[test]
fn a_stale_socket_is_replaced_and_the_removal_logged() {
    let sandbox = Sandbox::new();
    drop(std::os::unix::net::UnixListener::bind(sandbox.socket()).unwrap());
    let config = sandbox.file("config.toml", &sandbox.valid());
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(0);
    assert!(
        run.log.contains("removed a stale socket file"),
        "{}",
        run.log
    );
    assert!(!sandbox.socket().exists());
}

#[cfg(unix)]
#[test]
fn every_startup_failure_exits_one_naming_the_cause_and_touches_nothing() {
    let sandbox = Sandbox::new();
    let socket = sandbox.socket();
    let socket_path = socket.display().to_string();

    // A live socket: another process is listening.
    let live = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let config = sandbox.file("config.toml", &sandbox.valid());
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(1);
    assert_eq!(
        run.stderr,
        format!(
            "error: {socket_path}: socket already in use: another process is listening on it\n"
        )
    );
    assert!(!run.log.contains("daemon started"), "{}", run.log);
    std::os::unix::net::UnixStream::connect(&socket).expect("the live socket survives");
    drop(live);
    std::fs::remove_file(&socket).unwrap();

    // A regular file where the socket goes.
    std::fs::write(&socket, "precious").unwrap();
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(1);
    assert!(
        run.stderr.starts_with(&format!("error: {socket_path}: ")),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("is not a socket"), "{}", run.stderr);
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "precious");
    std::fs::remove_file(&socket).unwrap();

    // A missing parent directory.
    let orphan = sandbox.dir.join("absent").join("d.sock");
    let config = sandbox.file(
        "orphan.toml",
        &format!("[transport.uds]\npath = {:?}\n", orphan.to_str().unwrap()),
    );
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(1);
    assert!(
        run.stderr
            .starts_with(&format!("error: {}: cannot bind", orphan.display())),
        "{}",
        run.stderr
    );

    // Transports no adapter serves yet, alone or beside UDS.
    for (name, extra, expected) in [
        (
            "tcp.toml",
            "[transport.tcp]\nbind = \"127.0.0.1:7400\"\ntls_cert = \"c\"\ntls_key = \"k\"\n",
            "[transport.tcp] not supported yet",
        ),
        (
            "ws.toml",
            "[transport.websocket]\nbind = \"127.0.0.1:7401\"\n",
            "[transport.websocket] not supported yet",
        ),
    ] {
        for text in [extra.to_owned(), format!("{}{extra}", sandbox.valid())] {
            let config = sandbox.file(name, &text);
            let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
            run.exit(1);
            assert!(
                run.stderr
                    .starts_with(&format!("error: {}: {expected}", config.display())),
                "{}",
                run.stderr
            );
            assert!(!socket.exists(), "nothing is bound: {text}");
        }
    }

    // An invalid mode is a System Config error, before anything is bound.
    let config = sandbox.file("mode.toml", &format!("{}mode = \"rw\"\n", sandbox.valid()));
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(1);
    assert!(
        run.stderr.contains("`transport.uds.mode`"),
        "{}",
        run.stderr
    );
    assert!(!socket.exists());
}
