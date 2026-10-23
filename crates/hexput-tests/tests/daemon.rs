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
#[cfg(unix)]
use std::time::{Duration, Instant};

use hexput_daemon::{Discovery, run_with};

#[path = "daemon/conformance.rs"]
mod conformance;

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
            "format.toml",
            "log_format = \"xml\"\n[transport.uds]\npath = \"x\"\n",
            ":1:14: invalid value for `log_format`: expected one of text, json, got \"xml\"",
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
    /// The Daemon's log, readable while it runs (Story 5.8 waits on a line of it).
    log: SharedLog,
}

#[cfg(unix)]
impl Serving {
    fn start(sandbox: &Sandbox, config_text: &str) -> Self {
        let config = sandbox.file("config.toml", config_text);
        let discovery = nothing(sandbox);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let log = SharedLog::default();
        let thread_log = log.clone();
        let thread = std::thread::spawn(move || {
            let argv: Vec<OsString> =
                vec!["hexput-daemon".into(), "--config".into(), config.into()];
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_with(
                argv,
                &discovery,
                &mut out,
                &mut err,
                thread_log.clone(),
                async {
                    let _ = stopped.await;
                },
            );
            Run {
                code: format!("{code:?}"),
                stdout: String::from_utf8(out).unwrap(),
                stderr: String::from_utf8(err).unwrap(),
                log: thread_log.text(),
            }
        });
        Self {
            stop,
            thread,
            socket: sandbox.socket(),
            log,
        }
    }

    /// The log, once `needle` appears in it; panics if the Daemon stops first or after 10 s.
    fn log_with(&self, needle: &str) -> String {
        let started = Instant::now();
        loop {
            let text = self.log.text();
            if text.contains(needle) {
                return text;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10) && !self.thread.is_finished(),
                "never logged `{needle}`; log:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(10));
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
        let registrations = Value::Array(vec![Value::Map(vec![
            (Value::from("name"), Value::from("getUser")),
            (Value::from("blanket"), Value::from(true)),
        ])]);
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
    let mut secrets = Vec::new();
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
        // Story 5.5: exactly `client_id` then `reconnect_secret`.
        assert_eq!(
            fields.len(),
            2,
            "exactly `client_id` and `reconnect_secret`: {fields:?}"
        );
        assert_eq!(fields[0].0.as_str(), Some("client_id"));
        assert_eq!(fields[1].0.as_str(), Some("reconnect_secret"));
        let client_id = fields[0].1.as_str().expect("a string Client ID").to_owned();
        assert_eq!(client_id.len(), 32);
        assert!(
            client_id
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{client_id}"
        );
        let secret = fields[1].1.as_str().expect("a string reconnect secret");
        assert_eq!(secret.len(), 64, "{secret}");
        assert!(
            secret
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{secret}"
        );
        secrets.push(secret.to_owned());
        // Initialized: execution is now past the gate, and its payload is what is judged.
        wire::send(&mut client, &wire::request(id + 10));
        assert_eq!(
            wire::refusal(&mut client),
            (Some(id + 10), "protocol.invalid_payload".to_owned())
        );
        issued.push(client_id);
    }
    assert_ne!(issued[0], issued[1]);
    assert_ne!(secrets[0], secrets[1]);
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

    // A counted loop, never a sleep: about a fifth of a second in a debug build, and far inside
    // the CPU time budget (Story 3.5).
    const SLOW_TURNS: i64 = 50_000;
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

/// Story 3.5: shutting the Daemon down while a runaway Script runs no longer hangs — the runaway
/// ends at its CPU time budget, which frees the blocking thread the runtime's shutdown waits for.
#[cfg(unix)]
#[test]
fn shutdown_while_a_runaway_runs_ends_once_it_hits_its_cpu_time() {
    use std::time::{Duration, Instant};

    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let text = |t: &str| Value::from(t);
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut client = serving.connect();
    wire::send(
        &mut client,
        &Envelope::new(
            CorrelationId(1),
            MessageType::Init,
            Value::Map(vec![
                (text("config"), Value::Map(vec![])),
                (text("registrations"), Value::Array(vec![])),
            ]),
        ),
    );
    assert_eq!(
        wire::reply(&mut client).expect("a reply").message_type,
        MessageType::Result
    );
    let started = Instant::now();
    wire::send(
        &mut client,
        &Envelope::new(
            CorrelationId(2),
            MessageType::ExecutionStart,
            Value::Map(vec![
                (text("source"), text("while (true) {}")),
                (text("variables"), Value::Map(vec![])),
            ]),
        ),
    );
    // Let the runaway get going, then shut down — under a deadline, so a regression fails the
    // test instead of hanging the suite.
    std::thread::sleep(Duration::from_millis(100));
    let (done, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(serving.stop());
    });
    let run = stopped
        .recv_timeout(Duration::from_secs(10))
        .expect("the Daemon exits within 10 s of shutdown with a runaway running");
    run.exit(0);
    let took = started.elapsed();
    assert!(
        took < hexput_enforce::DEFAULT_CPU_TIME + Duration::from_secs(3),
        "the Daemon took {took:?} to exit"
    );
    // Shutdown aborts the connection, so the runaway's reply is usually never written; if it
    // was, it is the budget error.
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut prefix = [0; 4];
    if std::io::Read::read_exact(&mut client, &mut prefix).is_ok() {
        let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
        std::io::Read::read_exact(&mut client, &mut body).unwrap();
        let reply = hexput_port::decode(&body).unwrap();
        assert_eq!(reply.id, Some(CorrelationId(2)));
        let Value::Map(fields) = &reply.payload else {
            panic!("an error payload is a map: {:?}", reply.payload);
        };
        let code = fields
            .iter()
            .find(|(key, _)| key.as_str() == Some("code"))
            .and_then(|(_, value)| value.as_str());
        assert_eq!(code, Some("budget.cpu_time_exceeded"));
    }
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

    // Story 5.2: WebSocket is served now, so it no longer has a "not supported yet" refusal;
    // its startup failures are tested beside the other TLS ones in `mod tls_serving`.

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

// --- Story 2.8: every request traced back to its Client ID ---

/// An initialized client over the real socket, with the Client ID it was issued.
#[cfg(unix)]
fn traced_client(serving: &Serving, init_id: u64) -> (std::os::unix::net::UnixStream, String) {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let mut client = serving.connect();
    let init = Envelope::new(
        CorrelationId(init_id),
        MessageType::Init,
        Value::Map(vec![
            (Value::from("config"), Value::Map(vec![])),
            (Value::from("registrations"), Value::Array(vec![])),
        ]),
    );
    wire::send(&mut client, &init);
    let reply = wire::reply(&mut client).expect("an init reply");
    assert_eq!(reply.message_type, MessageType::Result);
    let client_id = reply.payload.as_map().expect("a map")[0]
        .1
        .as_str()
        .expect("a Client ID")
        .to_owned();
    (client, client_id)
}

/// A Direct Execution request.
#[cfg(unix)]
fn traced_execution(id: u64, source: &str) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::ExecutionStart,
        Value::Map(vec![
            (Value::from("source"), Value::from(source)),
            (Value::from("variables"), Value::Map(vec![])),
        ]),
    )
}

/// One parsed JSON log line.
#[cfg(unix)]
struct JsonEvent(serde_json::Value);

#[cfg(unix)]
impl JsonEvent {
    fn message(&self) -> &str {
        self.0["fields"]["message"].as_str().unwrap_or_default()
    }

    /// Every enclosing span, outermost first; empty outside any span.
    fn spans(&self) -> &[serde_json::Value] {
        self.0["spans"].as_array().map_or(&[], Vec::as_slice)
    }

    /// The `connection` span's fields, if the event is inside one.
    fn connection(&self) -> Option<&serde_json::Value> {
        self.spans()
            .iter()
            .find(|span| span["name"] == "connection")
    }

    /// The `request` span's `id`, if the event is inside one.
    fn request(&self) -> Option<&serde_json::Value> {
        self.spans()
            .iter()
            .find(|span| span["name"] == "request")
            .map(|span| &span["id"])
    }
}

/// Every log line, each of which must be exactly one JSON object.
#[cfg(unix)]
fn json_events(log: &str) -> Vec<JsonEvent> {
    log.lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("not one JSON object ({error}): {line}"));
            assert!(value.is_object(), "not an object: {line}");
            JsonEvent(value)
        })
        .collect()
}

/// Story 5.5: at `trace` in JSON, the most the Daemon logs, the reconnect secret reaches no log
/// line — while the Client ID the connection's spans carry does, so the log is not empty of it.
#[cfg(unix)]
#[test]
fn the_reconnect_secret_is_never_logged_even_at_trace_in_json() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let config = format!(
        "log_level = \"trace\"\nlog_format = \"json\"\n{}",
        sandbox.valid()
    );
    let serving = Serving::start(&sandbox, &config);
    let mut client = serving.connect();
    let init = Envelope::new(
        CorrelationId(1),
        MessageType::Init,
        Value::Map(vec![
            (Value::from("config"), Value::Map(vec![])),
            (Value::from("registrations"), Value::Array(vec![])),
        ]),
    );
    wire::send(&mut client, &init);
    let reply = wire::reply(&mut client).expect("an init reply");
    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "{:?}",
        reply.payload
    );
    let fields = reply.payload.as_map().expect("a map payload");
    let client_id = fields[0].1.as_str().expect("a Client ID").to_owned();
    let secret = fields[1].1.as_str().expect("a reconnect secret").to_owned();
    assert_eq!(secret.len(), 64);

    wire::send(&mut client, &traced_execution(2, "return 1 + 1;"));
    let answer = wire::reply(&mut client).expect("an execution reply");
    assert_eq!(answer.message_type, MessageType::Result);

    let run = serving.stop();
    run.exit(0);
    assert!(
        json_events(&run.log)
            .iter()
            .any(|e| e.connection().is_some())
    );
    assert!(run.log.contains(&client_id), "the spans name the Client ID");
    assert!(
        !run.log.contains(&secret),
        "the log carries the reconnect secret: {}",
        run.log
    );
}

/// Story 5.6: at `trace` in JSON, neither the issued reconnect secret nor a wrong one presented on
/// a second connection reaches any log line — while the Client ID still does.
#[cfg(unix)]
#[test]
fn neither_a_right_nor_a_wrong_reconnect_secret_is_logged_even_at_trace_in_json() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let config = format!(
        "log_level = \"trace\"\nlog_format = \"json\"\n{}",
        sandbox.valid()
    );
    let serving = Serving::start(&sandbox, &config);
    let mut first = serving.connect();
    wire::send(
        &mut first,
        &Envelope::new(
            CorrelationId(1),
            MessageType::Init,
            Value::Map(vec![
                (Value::from("config"), Value::Map(vec![])),
                (Value::from("registrations"), Value::Array(vec![])),
            ]),
        ),
    );
    let reply = wire::reply(&mut first).expect("an init reply");
    let fields = reply.payload.as_map().expect("a map payload");
    let text_of = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("the init reply has a string `{key}`"))
            .to_owned()
    };
    let client_id = text_of("client_id");
    let secret = text_of("reconnect_secret");

    let mut second = serving.connect();
    let mut wrong = secret.clone();
    let last = wrong.pop().expect("a secret");
    wrong.push(if last == '0' { '1' } else { '0' });
    let reconnect = |id: u64, presented: &str| {
        Envelope::new(
            CorrelationId(id),
            MessageType::Reconnect,
            Value::Map(vec![
                (Value::from("client_id"), Value::from(client_id.as_str())),
                (Value::from("reconnect_secret"), Value::from(presented)),
            ]),
        )
    };
    wire::send(&mut second, &reconnect(2, &wrong));
    let denied = wire::reply(&mut second).expect("a denial");
    assert_eq!(
        denied.message_type,
        MessageType::Error,
        "{:?}",
        denied.payload
    );
    wire::send(&mut second, &reconnect(3, &secret));
    let resumed = wire::reply(&mut second).expect("a resumed reply");
    assert_eq!(
        resumed.message_type,
        MessageType::Result,
        "{:?}",
        resumed.payload
    );
    // A failing request on the reconnected connection, so its events can be traced.
    wire::send(&mut second, &traced_execution(4, "return 1 / 0;"));
    assert_eq!(
        wire::refusal(&mut second),
        (Some(4), "arithmetic.division_by_zero".to_owned())
    );

    let run = serving.stop();
    run.exit(0);
    assert!(
        !run.log.contains(&secret),
        "the log carries the issued reconnect secret: {}",
        run.log
    );
    assert!(
        !run.log.contains(&wrong),
        "the log carries the presented wrong secret: {}",
        run.log
    );
    assert!(run.log.contains(&client_id), "the spans name the Client ID");

    // FR-12 across a reconnect: the denial is still unattributed, the completed reconnect is the
    // first event naming the Client ID on the second connection, and every later event there
    // carries it — exactly as after `Init`.
    let events = json_events(&run.log);
    let find = |message: &str, request: u64| -> &JsonEvent {
        let request = serde_json::Value::from(request.to_string());
        events
            .iter()
            .find(|e| e.message() == message && e.request() == Some(&request))
            .unwrap_or_else(|| panic!("no {message:?} for request {request}:\n{}", run.log))
    };
    let connection_of = |event: &JsonEvent| event.connection().unwrap()["connection"].clone();
    let denied = find("refused a request", 2);
    assert_eq!(denied.connection().unwrap()["client_id"], "none");
    assert_eq!(denied.0["fields"]["code"], "protocol.reconnect_denied");
    let completed = find("reconnect completed; attached to the Session", 3);
    assert_eq!(
        completed.connection().unwrap()["client_id"],
        client_id.as_str()
    );
    assert_eq!(completed.0["level"], "DEBUG");
    let second_connection = connection_of(completed);
    assert_eq!(connection_of(denied), second_connection);
    let failed = find("a Direct Execution failed", 4);
    assert_eq!(connection_of(failed), second_connection);
    assert_eq!(
        failed.connection().unwrap()["client_id"],
        client_id.as_str()
    );
    let on_second: Vec<&JsonEvent> = events
        .iter()
        .filter(|e| e.connection().is_some() && connection_of(e) == second_connection)
        .collect();
    let first_named = on_second
        .iter()
        .position(|e| e.connection().unwrap()["client_id"] != "none")
        .expect("an event naming the Client ID");
    assert_eq!(
        on_second[first_named].message(),
        "reconnect completed; attached to the Session"
    );
    assert!(
        on_second[first_named..]
            .iter()
            .all(|e| e.connection().unwrap()["client_id"] == client_id.as_str()),
        "{}",
        run.log
    );
}

/// Story 2.8, the matrix's pre-init, init, execution, two-connection and JSON rows over the real
/// socket: every event a connection logs carries `connection` and `client_id` as span fields —
/// `"none"` before init, its own Client ID after — and every event for a request its `id`,
/// including those emitted on the blocking thread that runs the Script.
#[cfg(unix)]
#[test]
fn every_connection_event_carries_its_client_id_and_request_id_in_json() {
    let sandbox = Sandbox::new();
    let config = format!(
        "log_level = \"debug\"\nlog_format = \"json\"\n{}",
        sandbox.valid()
    );
    let serving = Serving::start(&sandbox, &config);

    // Pre-init: refused, with nothing run.
    let mut early = serving.connect();
    wire::send(&mut early, &wire::request(41));
    assert_eq!(
        wire::refusal(&mut early),
        (Some(41), "protocol.init_not_completed".to_owned())
    );

    // Malformed frames on the same connection: one whose id cannot be read, one whose id can.
    wire::send_raw(&mut early, &hexput_port::encode_frame(&[0xc1]).unwrap());
    assert_eq!(
        wire::refusal(&mut early),
        (None, "protocol.malformed_frame".to_owned())
    );
    let bogus = rmpv::Value::Map(vec![
        (rmpv::Value::from("id"), rmpv::Value::from(50)),
        (rmpv::Value::from("type"), rmpv::Value::from("Bogus")),
        (rmpv::Value::from("payload"), rmpv::Value::Nil),
    ]);
    let mut body = Vec::new();
    rmpv::encode::write_value(&mut body, &bogus).unwrap();
    wire::send_raw(&mut early, &hexput_port::encode_frame(&body).unwrap());
    assert_eq!(
        wire::refusal(&mut early),
        (Some(50), "protocol.unknown_message_type".to_owned())
    );

    // Two initialized connections; A's Script fails, B's succeeds.
    let (mut a, a_id) = traced_client(&serving, 1);
    let (mut b, b_id) = traced_client(&serving, 2);
    assert_ne!(a_id, b_id);
    wire::send(&mut a, &traced_execution(20, "return 1 / 0;"));
    assert_eq!(
        wire::refusal(&mut a),
        (Some(20), "arithmetic.division_by_zero".to_owned())
    );
    wire::send(&mut b, &traced_execution(30, "return 1;"));
    assert!(wire::reply(&mut b).is_some());

    let run = serving.stop();
    run.exit(0);
    let events = json_events(&run.log);
    assert!(!events.is_empty());

    // Every event inside a connection span carries both fields; nothing names a Client ID in
    // its message text.
    for event in &events {
        if let Some(connection) = event.connection() {
            assert!(connection["connection"].is_u64(), "{}", event.0);
            let client_id = connection["client_id"].as_str().expect("a client_id field");
            assert!(
                client_id == "none" || client_id.len() == 32,
                "{client_id}: {}",
                event.0
            );
        }
        for id in [&a_id, &b_id] {
            assert!(!event.message().contains(id.as_str()), "{}", event.0);
        }
    }
    // Only the Daemon's own startup and shutdown events are outside any connection.
    for event in events.iter().filter(|e| e.connection().is_none()) {
        assert!(
            [
                "System Config loaded",
                "listening on a Unix Domain Socket",
                "daemon started; waiting for a shutdown signal",
                "shutdown requested; daemon stopping",
            ]
            .contains(&event.message()),
            "an unattributed event: {}",
            event.0
        );
    }

    // A request id is always a string, so the field has one type on every line.
    let find = |message: &str, request: u64| -> &JsonEvent {
        let request = serde_json::Value::from(request.to_string());
        events
            .iter()
            .find(|e| e.message() == message && e.request() == Some(&request))
            .unwrap_or_else(|| panic!("no {message:?} for request {request}:\n{}", run.log))
    };

    // Pre-init: the refusal carries the connection and the no-Client-ID marker.
    let refused = find("refused a request", 41);
    assert_eq!(refused.connection().unwrap()["client_id"], "none");
    assert_eq!(refused.0["fields"]["code"], "protocol.init_not_completed");

    // Init: "init completed" already carries the issued Client ID.
    let a_init = find("init completed; Session created", 1);
    let b_init = find("init completed; Session created", 2);
    assert_eq!(a_init.connection().unwrap()["client_id"], a_id.as_str());
    assert_eq!(b_init.connection().unwrap()["client_id"], b_id.as_str());

    // Execution: logged on the blocking thread, with the Client ID and the request id.
    let failed = find("a Direct Execution failed", 20);
    assert_eq!(failed.connection().unwrap()["client_id"], a_id.as_str());
    assert_eq!(failed.0["span"]["name"], "request", "the innermost span");
    assert_eq!(failed.0["span"]["id"], "20");

    // Malformed frames: a readable id gets its request span inside the connection span; an
    // unreadable one is still logged in the connection span.
    let readable = find("rejected a malformed frame", 50);
    assert_eq!(readable.connection().unwrap()["client_id"], "none");
    assert_eq!(readable.0["span"]["name"], "request");
    let unreadable = events
        .iter()
        .find(|e| e.message() == "rejected a malformed frame" && e.request().is_none())
        .expect("the unreadable frame's event");
    let connection = unreadable.connection().expect("in the connection span");
    assert!(connection["connection"].is_u64());
    assert_eq!(connection["client_id"], "none");
    assert_eq!(unreadable.0["span"]["name"], "connection");

    // Two connections: each connection's events carry only its own Client ID, once it has one.
    let connection_of = |event: &JsonEvent| event.connection().unwrap()["connection"].clone();
    for (init, own) in [(a_init, &a_id), (b_init, &b_id)] {
        let this = connection_of(init);
        let named: std::collections::HashSet<&str> = events
            .iter()
            .filter(|e| e.connection().is_some() && connection_of(e) == this)
            .map(|e| e.connection().unwrap()["client_id"].as_str().unwrap())
            .collect();
        assert!(named.contains(own.as_str()));
        assert!(
            named.iter().all(|id| *id == "none" || id == own),
            "{named:?}"
        );
        // In log order, nothing on this connection after its "init completed" is unmarked.
        let after_init: Vec<&str> = events
            .iter()
            .filter(|e| e.connection().is_some() && connection_of(e) == this)
            .skip_while(|e| e.message() != "init completed; Session created")
            .map(|e| e.connection().unwrap()["client_id"].as_str().unwrap())
            .collect();
        assert!(!after_init.is_empty());
        assert!(after_init.iter().all(|id| id == own), "{after_init:?}");
    }
    assert_ne!(connection_of(a_init), connection_of(b_init));
    assert_ne!(connection_of(refused), connection_of(a_init));
}

/// Story 2.8 in plain text, still the default: the Client ID appears in a line's span context,
/// never in its message, and before init the context says `client_id=none`.
#[cfg(unix)]
#[test]
fn plain_text_shows_the_client_id_in_span_context_not_in_messages() {
    let sandbox = Sandbox::new();
    let config = format!("log_level = \"debug\"\n{}", sandbox.valid());
    let serving = Serving::start(&sandbox, &config);
    let mut early = serving.connect();
    wire::send(&mut early, &wire::request(41));
    wire::refusal(&mut early);
    let (mut client, client_id) = traced_client(&serving, 7);
    wire::send(&mut client, &traced_execution(8, "return 1 / 0;"));
    wire::refusal(&mut client);
    let run = serving.stop();
    run.exit(0);

    let line = |needle: &str| -> &str {
        run.log
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line containing {needle:?}:\n{}", run.log))
    };
    let refused = line("refused a request");
    assert!(refused.contains("client_id=none"), "{refused}");
    assert!(refused.contains("request{id=41}"), "{refused}");

    for (message, request) in [
        ("init completed; Session created", 7),
        ("a Direct Execution failed", 8),
    ] {
        let found = line(message);
        let context = format!("client_id={client_id}}}:request{{id={request}}}");
        assert!(found.contains(&context), "{found}");
        // The span context comes before the target and the message; the message is clean.
        let (_, text) = found
            .split_once("hexput_connection: ")
            .expect("the target precedes the message");
        assert!(!text.contains(&client_id), "{found}");
    }
    // Plain text is not JSON.
    assert!(!run.log.trim_start().starts_with('{'), "{}", run.log);
}

/// Story 4.1: a registration over the real socket is answered with a handle. The handles of one
/// Daemon run come from one counter, so two connections never share one.
#[cfg(unix)]
#[test]
fn a_registration_over_the_socket_is_answered_with_a_handle_from_one_counter_per_run() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut handles = Vec::new();
    for _ in 0..2 {
        let mut client = serving.connect();
        let init = Envelope::new(
            CorrelationId(1),
            MessageType::Init,
            Value::Map(vec![
                (Value::from("config"), Value::Map(vec![])),
                (Value::from("registrations"), Value::Array(vec![])),
            ]),
        );
        wire::send(&mut client, &init);
        assert_eq!(
            wire::reply(&mut client).expect("a reply").message_type,
            MessageType::Result
        );
        let register = Envelope::new(
            CorrelationId(2),
            MessageType::CodeRegister,
            Value::Map(vec![(Value::from("source"), Value::from("return 1 + x;"))]),
        );
        wire::send(&mut client, &register);
        let reply = wire::reply(&mut client).expect("a reply");
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        let fields = reply.payload.as_map().expect("a map payload");
        assert_eq!(fields.len(), 1, "exactly `handle`: {fields:?}");
        assert_eq!(fields[0].0.as_str(), Some("handle"));
        handles.push(fields[0].1.as_str().expect("a string handle").to_owned());
    }
    assert_eq!(handles, ["0000000000000001", "0000000000000002"]);
    serving.stop().exit(0);
}

// --- Story 4.2: a registered Script re-run over the socket with fresh variables ---

#[cfg(unix)]
#[test]
fn a_cached_execution_over_the_socket_runs_the_registered_script_with_fresh_variables() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut client = serving.connect();
    let init = Envelope::new(
        CorrelationId(1),
        MessageType::Init,
        Value::Map(vec![
            (Value::from("config"), Value::Map(vec![])),
            (Value::from("registrations"), Value::Array(vec![])),
        ]),
    );
    wire::send(&mut client, &init);
    assert_eq!(
        wire::reply(&mut client).expect("a reply").message_type,
        MessageType::Result
    );
    let register = Envelope::new(
        CorrelationId(2),
        MessageType::CodeRegister,
        Value::Map(vec![(
            Value::from("source"),
            Value::from("return grade >= 50;"),
        )]),
    );
    wire::send(&mut client, &register);
    let reply = wire::reply(&mut client).expect("a reply");
    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "{:?}",
        reply.payload
    );
    let handle = reply
        .payload
        .as_map()
        .expect("a map payload")
        .iter()
        .find(|(key, _)| key.as_str() == Some("handle"))
        .expect("a `handle` key")
        .1
        .as_str()
        .expect("a string handle")
        .to_owned();
    for (id, grade, expected) in [(3u64, 72i64, true), (4, 12, false)] {
        let run = Envelope::new(
            CorrelationId(id),
            MessageType::CachedExecutionStart,
            Value::Map(vec![
                (Value::from("handle"), Value::from(handle.as_str())),
                (
                    Value::from("variables"),
                    Value::Map(vec![(Value::from("grade"), Value::from(grade))]),
                ),
            ]),
        );
        wire::send(&mut client, &run);
        let reply = wire::reply(&mut client).expect("a reply");
        assert_eq!(reply.id, Some(CorrelationId(id)));
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        assert_eq!(
            reply.payload,
            Value::Map(vec![(Value::from("value"), Value::Boolean(expected))])
        );
    }
    serving.stop().exit(0);
}

// --- Story 4.3: the cache's capacity, and CodeUnregister, over the real socket ---

/// A `CachedExecutionStart` for `handle` with no variables, as an envelope.
#[cfg(unix)]
fn cached_run_request(id: u64, handle: &str) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::CachedExecutionStart,
        Value::Map(vec![
            (Value::from("handle"), Value::from(handle)),
            (Value::from("variables"), Value::Map(vec![])),
        ]),
    )
}

/// A `CodeUnregister` for `handle`, as an envelope.
#[cfg(unix)]
fn unregister_request(id: u64, handle: &str) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::CodeUnregister,
        Value::Map(vec![(Value::from("handle"), Value::from(handle))]),
    )
}

/// The `code` of an `Error` reply's payload.
#[cfg(unix)]
fn error_code_of(reply: &hexput_port::Envelope<hexput_port::Value>) -> String {
    use hexput_port::{MessageType, Value};

    assert_eq!(
        reply.message_type,
        MessageType::Error,
        "{:?}",
        reply.payload
    );
    let Value::Map(fields) = &reply.payload else {
        panic!("an error payload is a map");
    };
    fields
        .iter()
        .find(|(key, _)| key.as_str() == Some("code"))
        .and_then(|(_, value)| value.as_str())
        .expect("a code")
        .to_owned()
}

/// Story 4.3: with `ast_cache_capacity = 1`, two registrations over the real socket leave one
/// Script cached. Exactly one of the two handles still runs, the other is
/// `protocol.unknown_script_handle`, and `CodeUnregister` removes the survivor.
#[cfg(unix)]
#[test]
fn a_capacity_of_one_over_the_socket_keeps_one_script_and_code_unregister_removes_it() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let serving = Serving::start(
        &sandbox,
        &format!("ast_cache_capacity = 1\n{}", sandbox.valid()),
    );
    let mut client = serving.connect();
    let init = Envelope::new(
        CorrelationId(1),
        MessageType::Init,
        Value::Map(vec![
            (Value::from("config"), Value::Map(vec![])),
            (Value::from("registrations"), Value::Array(vec![])),
        ]),
    );
    wire::send(&mut client, &init);
    assert_eq!(
        wire::reply(&mut client).expect("a reply").message_type,
        MessageType::Result
    );

    let mut handles = Vec::new();
    for (id, source) in [(2u64, "return 1;"), (3, "return 2;")] {
        let register = Envelope::new(
            CorrelationId(id),
            MessageType::CodeRegister,
            Value::Map(vec![(Value::from("source"), Value::from(source))]),
        );
        wire::send(&mut client, &register);
        let reply = wire::reply(&mut client).expect("a reply");
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        let handle = reply.payload.as_map().expect("a map payload")[0]
            .1
            .as_str()
            .expect("a string handle")
            .to_owned();
        handles.push(handle);
    }

    let mut replies = Vec::new();
    for (id, handle) in [(4u64, &handles[0]), (5, &handles[1])] {
        wire::send(&mut client, &cached_run_request(id, handle));
        let reply = wire::reply(&mut client).expect("a reply");
        assert_eq!(reply.id, Some(CorrelationId(id)));
        replies.push((handle.clone(), reply));
    }
    let (kept, evicted): (Vec<_>, Vec<_>) = replies
        .iter()
        .partition(|(_, reply)| reply.message_type == MessageType::Result);
    assert_eq!(kept.len(), 1, "exactly one Script is still cached");
    assert_eq!(evicted.len(), 1);
    assert_eq!(
        error_code_of(&evicted[0].1),
        "protocol.unknown_script_handle"
    );

    let survivor = kept[0].0.clone();
    wire::send(&mut client, &unregister_request(6, &survivor));
    let removed = wire::reply(&mut client).expect("a reply");
    assert_eq!(removed.id, Some(CorrelationId(6)));
    assert_eq!(
        removed.message_type,
        MessageType::Result,
        "{:?}",
        removed.payload
    );
    assert_eq!(removed.payload, Value::Map(vec![]));
    wire::send(&mut client, &cached_run_request(7, &survivor));
    assert_eq!(
        wire::refusal(&mut client),
        (Some(7), "protocol.unknown_script_handle".to_owned())
    );
    serving.stop().exit(0);
}

// --- Story 4.4: the cache benchmark's workload, verified over the real socket ---

/// Story 4.4: the measured workload (spec `cache` bench, constant `RULE`) and its Backend. Each
/// run makes 10 host calls, every reply is checked against the verdict Rust computes, and the
/// connections are initialized with blanket grants so no `Authorize` question is ever sent.
#[cfg(unix)]
mod cache_win {
    use super::*;
    use std::os::unix::net::UnixStream;

    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    const RULE: &str = "let total = 0;\nlet weights = 0;\nfor (grade in grades) {\n  let w = weight(grade.course);\n  total = total + grade.score * w;\n  weights = weights + w;\n};\nlet passed = total / weights >= passMark(student.program);\nrecord(student.id, passed);\nreturn passed;\n";
    const COURSES: [(&str, i64); 8] = [
        ("algebra", 3),
        ("biology", 4),
        ("chemistry", 2),
        ("compsci", 5),
        ("history", 1),
        ("literature", 2),
        ("music", 1),
        ("physics", 4),
    ];
    const STUDENTS: i64 = 16;
    const HOST_CALLS_PER_RUN: usize = 10;
    const CONCURRENCY: usize = 8;
    const RUNS_PER_CONNECTION: usize = 20;

    fn course_weight(course: &str) -> i64 {
        COURSES
            .iter()
            .find(|(name, _)| *name == course)
            .map(|(_, weight)| *weight)
            .expect("a known course")
    }

    fn pass_mark(program: &str) -> i64 {
        match program {
            "science" => 50,
            "engineering" => 60,
            other => panic!("an unknown program `{other}`"),
        }
    }

    struct Student {
        id: String,
        program: &'static str,
        grades: Vec<(&'static str, i64)>,
    }

    /// Sixteen deterministic students: score `j` of student `i` is `(i*37 + j*11) % 101`, and the
    /// programs alternate.
    fn students() -> Vec<Student> {
        (0..STUDENTS)
            .map(|i| Student {
                id: format!("student-{i:02}"),
                program: if i % 2 == 0 { "science" } else { "engineering" },
                grades: (0..8i64)
                    .map(|j| (COURSES[((i + j) % 8) as usize].0, (i * 37 + j * 11) % 101))
                    .collect(),
            })
            .collect()
    }

    /// The verdict, with the same f64 operations in the same order as the Script.
    fn expected_verdict(student: &Student) -> bool {
        let mut total = 0.0_f64;
        let mut weights = 0.0_f64;
        for (course, score) in &student.grades {
            let w = course_weight(course) as f64;
            total += *score as f64 * w;
            weights += w;
        }
        total / weights >= pass_mark(student.program) as f64
    }

    fn expected_payload(student: &Student) -> Value {
        Value::Map(vec![(
            Value::from("value"),
            Value::Boolean(expected_verdict(student)),
        )])
    }

    fn field<'a>(payload: &'a Value, key: &str) -> &'a Value {
        payload
            .as_map()
            .expect("a map payload")
            .iter()
            .find(|(name, _)| name.as_str() == Some(key))
            .map(|(_, value)| value)
            .unwrap_or_else(|| panic!("a `{key}` field"))
    }

    fn student_variables(student: &Student) -> Value {
        let grades: Vec<Value> = student
            .grades
            .iter()
            .map(|(course, score)| {
                Value::Map(vec![
                    (Value::from("course"), Value::from(*course)),
                    (Value::from("score"), Value::from(*score)),
                ])
            })
            .collect();
        Value::Map(vec![
            (Value::from("grades"), Value::Array(grades)),
            (
                Value::from("student"),
                Value::Map(vec![
                    (Value::from("id"), Value::from(student.id.as_str())),
                    (Value::from("program"), Value::from(student.program)),
                ]),
            ),
        ])
    }

    fn init(client: &mut UnixStream, id: u64) {
        let registrations: Vec<Value> = ["weight", "passMark", "record"]
            .iter()
            .map(|name| {
                Value::Map(vec![
                    (Value::from("name"), Value::from(*name)),
                    (Value::from("blanket"), Value::from(true)),
                ])
            })
            .collect();
        wire::send(
            client,
            &Envelope::new(
                CorrelationId(id),
                MessageType::Init,
                Value::Map(vec![
                    (Value::from("config"), Value::Map(vec![])),
                    (Value::from("registrations"), Value::Array(registrations)),
                ]),
            ),
        );
        let reply = wire::reply(client).expect("a reply");
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
    }

    /// Registers the rule, returning its 16-character handle.
    fn register(client: &mut UnixStream, id: u64) -> String {
        wire::send(
            client,
            &Envelope::new(
                CorrelationId(id),
                MessageType::CodeRegister,
                Value::Map(vec![(Value::from("source"), Value::from(RULE))]),
            ),
        );
        let reply = wire::reply(client).expect("a reply");
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        let handle = field(&reply.payload, "handle")
            .as_str()
            .expect("a string handle")
            .to_owned();
        assert_eq!(handle.len(), 16, "{handle}");
        handle
    }

    fn cached_request(id: u64, handle: &str, student: &Student) -> Envelope<Value> {
        Envelope::new(
            CorrelationId(id),
            MessageType::CachedExecutionStart,
            Value::Map(vec![
                (Value::from("handle"), Value::from(handle)),
                (Value::from("variables"), student_variables(student)),
            ]),
        )
    }

    fn direct_request(id: u64, student: &Student) -> Envelope<Value> {
        Envelope::new(
            CorrelationId(id),
            MessageType::ExecutionStart,
            Value::Map(vec![
                (Value::from("source"), Value::from(RULE)),
                (Value::from("variables"), student_variables(student)),
            ]),
        )
    }

    /// The Backend's answer to one `Call` of the rule's three host functions. Each argument
    /// arrives as a Value Secret holder (`{__secret, value}`, FR-28) because it was read from a
    /// place — the Backend reads the value inside.
    fn answer(call: &Envelope<Value>) -> Value {
        let name = field(&call.payload, "name")
            .as_str()
            .expect("a function name");
        let arguments: Vec<&Value> = field(&call.payload, "arguments")
            .as_array()
            .expect("an argument array")
            .iter()
            .map(unwrap_holder)
            .collect();
        match (name, arguments.as_slice()) {
            ("weight", [course]) => {
                Value::from(course_weight(course.as_str().expect("a course name")))
            }
            ("passMark", [program]) => {
                Value::from(pass_mark(program.as_str().expect("a program name")))
            }
            ("record", [id, Value::Boolean(_)]) => {
                assert!(id.as_str().is_some(), "record's id is a string");
                Value::Nil
            }
            (name, arguments) => panic!("an unexpected Call to `{name}` with {arguments:?}"),
        }
    }

    /// The value a holder `{__secret, value}` carries, or the value itself when it is no holder.
    fn unwrap_holder(value: &Value) -> &Value {
        match value.as_map() {
            Some(entries)
                if entries.len() == 2
                    && entries
                        .iter()
                        .any(|(key, _)| key.as_str() == Some("__secret")) =>
            {
                field(value, "value")
            }
            _ => value,
        }
    }

    /// Sends one execution and answers its host calls as a Backend until its own reply arrives.
    /// Returns the reply's payload and the number of `Call`s it made.
    fn run_and_serve_calls(client: &mut UnixStream, request: Envelope<Value>) -> (Value, usize) {
        let expected = request.id.map(CorrelationId::get);
        wire::send(client, &request);
        let mut calls = 0;
        loop {
            let reply = wire::reply(client).expect("a reply");
            match reply.message_type {
                MessageType::Call => {
                    calls += 1;
                    let value = answer(&reply);
                    let call_id = reply.id.expect("a Call carries its id").get();
                    wire::send(
                        client,
                        &Envelope::new(
                            CorrelationId(call_id),
                            MessageType::Result,
                            Value::Map(vec![(Value::from("value"), value)]),
                        ),
                    );
                }
                MessageType::Result => {
                    assert_eq!(
                        reply.id.map(CorrelationId::get),
                        expected,
                        "the reply names its execution"
                    );
                    return (reply.payload, calls);
                }
                other => panic!("the run failed with {other:?}: {:?}", reply.payload),
            }
        }
    }

    /// Story 4.4: the cached path over the socket returns the verdict Rust computes for every
    /// student, in every rotation, with exactly 10 host calls per run, and both outcomes occur.
    /// This module verifies a copy of the bench's workload (a test cannot import a bench's
    /// modules), so the copy must stay the one `benches/cache.rs` measures.
    #[test]
    fn this_workload_is_the_one_the_cache_bench_measures() {
        let bench = include_str!("../benches/cache.rs");
        assert!(
            bench.contains(&format!("pub const RULE: &str = {RULE:?};")),
            "the bench's `RULE` differs from this copy"
        );
        for (course, weight) in COURSES {
            assert!(
                bench.contains(&format!("(\"{course}\", {weight}),")),
                "the bench's `weight` table differs for `{course}`"
            );
        }
        assert!(bench.contains(&format!("pub const STUDENTS: usize = {STUDENTS};")));
        assert!(bench.contains(&format!(
            "pub const HOST_CALLS_PER_RUN: usize = {HOST_CALLS_PER_RUN};"
        )));
        assert!(bench.contains("(i * 37 + j * 11) % 101"));
    }

    #[test]
    fn the_rule_over_the_socket_is_verified_for_every_student_with_ten_host_calls() {
        let sandbox = Sandbox::new();
        let serving = Serving::start(&sandbox, &sandbox.valid());
        let mut client = serving.connect();
        init(&mut client, 1);
        let handle = register(&mut client, 2);
        let students = students();
        let mut id = 3;
        let mut verdicts = Vec::new();
        for _round in 0..3 {
            for student in &students {
                let (payload, calls) =
                    run_and_serve_calls(&mut client, cached_request(id, &handle, student));
                id += 1;
                assert_eq!(payload, expected_payload(student), "{}", student.id);
                assert_eq!(calls, HOST_CALLS_PER_RUN, "{}", student.id);
                verdicts.push(expected_verdict(student));
            }
        }
        assert!(
            verdicts.contains(&true) && verdicts.contains(&false),
            "the workload must exercise both outcomes"
        );
        serving.stop().exit(0);
    }

    /// Story 4.4: the cold figure (`CodeRegister` then its first run), the warm figure (a cache
    /// hit) and the direct figure (`ExecutionStart`) all return the same verified verdict with 10
    /// host calls each; `CodeUnregister` then frees the handle, as the untimed cleanup requires.
    #[test]
    fn cold_warm_and_direct_runs_agree_and_code_unregister_frees_the_handle() {
        let sandbox = Sandbox::new();
        let serving = Serving::start(&sandbox, &sandbox.valid());
        let mut client = serving.connect();
        init(&mut client, 1);
        let students = students();
        let student = &students[1];

        let handle = register(&mut client, 2);
        let (cold, cold_calls) =
            run_and_serve_calls(&mut client, cached_request(3, &handle, student));
        let (warm, warm_calls) =
            run_and_serve_calls(&mut client, cached_request(4, &handle, student));
        let (direct, direct_calls) = run_and_serve_calls(&mut client, direct_request(5, student));

        let expected = expected_payload(student);
        assert_eq!(cold, expected, "cold");
        assert_eq!(warm, expected, "warm");
        assert_eq!(direct, expected, "direct");
        assert_eq!(cold_calls, HOST_CALLS_PER_RUN);
        assert_eq!(warm_calls, HOST_CALLS_PER_RUN);
        assert_eq!(direct_calls, HOST_CALLS_PER_RUN);

        wire::send(&mut client, &unregister_request(6, &handle));
        let reply = wire::reply(&mut client).expect("a reply");
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        assert_eq!(
            reply.payload,
            Value::Map(vec![]),
            "CodeUnregister answers {{}}"
        );

        wire::send(&mut client, &cached_request(7, &handle, student));
        assert_eq!(
            wire::refusal(&mut client),
            (Some(7), "protocol.unknown_script_handle".to_owned())
        );
        serving.stop().exit(0);
    }

    /// Story 4.4: the steady-state workload's shape: eight connections, each on its own thread,
    /// all running the rule concurrently, with every reply verified and 10 host calls per run.
    #[test]
    fn eight_concurrent_connections_each_verify_every_run_of_the_rule() {
        let sandbox = Sandbox::new();
        let serving = Serving::start(&sandbox, &sandbox.valid());
        let clients: Vec<UnixStream> = (0..CONCURRENCY).map(|_| serving.connect()).collect();
        std::thread::scope(|scope| {
            for (index, mut client) in clients.into_iter().enumerate() {
                scope.spawn(move || {
                    init(&mut client, 1);
                    let handle = register(&mut client, 2);
                    let students = students();
                    for run in 0..RUNS_PER_CONNECTION {
                        let student = &students[(index + run) % students.len()];
                        let (payload, calls) = run_and_serve_calls(
                            &mut client,
                            cached_request(3 + run as u64, &handle, student),
                        );
                        assert_eq!(payload, expected_payload(student), "{}", student.id);
                        assert_eq!(calls, HOST_CALLS_PER_RUN, "{}", student.id);
                    }
                });
            }
        });
        serving.stop().exit(0);
    }

    /// Story 5.9: the remote path. The TCP+TLS leg of the `cache` bench runs the same rule as the
    /// UDS leg over a real TLS connection (trusting only the fixture CA, server name `localhost`),
    /// and the bench's figures are read from `benches/cache.rs`. The stream-generic helpers below
    /// are copies of `init`, `register` and `run_and_serve_calls`, which take a `UnixStream` only.
    mod remote_path {
        use super::*;
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::os::unix::net::UnixStream;
        use std::sync::Arc;

        use rustls::pki_types::pem::PemObject;

        use super::super::conformance::harness::{Daemon, fixture, frames, tcp_section, tls};
        use super::super::{Run, Sandbox, nothing};

        /// A loopback port that was free just before the call (the bench's pick-then-bind).
        fn picked_port() -> u16 {
            TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        }

        /// The bench's `TCP+TLS` System Config: UDS plus `[transport.tcp]` on `port`.
        fn config_with_tcp(sandbox: &Sandbox, port: u16) -> String {
            format!(
                "{}{}",
                sandbox.valid(),
                tcp_section(&format!("127.0.0.1:{port}"))
            )
        }

        /// `init` over any stream: `Init` with the rule's three blanket grants.
        fn init_on<S: Read + Write>(stream: &mut S, id: u64) {
            let registrations: Vec<Value> = ["weight", "passMark", "record"]
                .iter()
                .map(|name| {
                    Value::Map(vec![
                        (Value::from("name"), Value::from(*name)),
                        (Value::from("blanket"), Value::from(true)),
                    ])
                })
                .collect();
            frames::send(
                stream,
                &Envelope::new(
                    CorrelationId(id),
                    MessageType::Init,
                    Value::Map(vec![
                        (Value::from("config"), Value::Map(vec![])),
                        (Value::from("registrations"), Value::Array(registrations)),
                    ]),
                ),
            );
            let reply = frames::reply(stream).expect("an Init reply");
            assert_eq!(
                reply.message_type,
                MessageType::Result,
                "{:?}",
                reply.payload
            );
        }

        /// `register` over any stream: the rule's handle, 16 characters long.
        fn register_on<S: Read + Write>(stream: &mut S, id: u64) -> String {
            frames::send(
                stream,
                &Envelope::new(
                    CorrelationId(id),
                    MessageType::CodeRegister,
                    Value::Map(vec![(Value::from("source"), Value::from(RULE))]),
                ),
            );
            let reply = frames::reply(stream).expect("a reply");
            assert_eq!(
                reply.message_type,
                MessageType::Result,
                "{:?}",
                reply.payload
            );
            let handle = field(&reply.payload, "handle")
                .as_str()
                .expect("a string handle")
                .to_owned();
            assert_eq!(handle.len(), 16, "{handle}");
            handle
        }

        /// `run_and_serve_calls` over any stream: answers every `Call` as a Backend and returns the
        /// execution's payload with its `Call` count.
        fn serve_run<S: Read + Write>(stream: &mut S, request: Envelope<Value>) -> (Value, usize) {
            let expected = request.id.map(CorrelationId::get);
            frames::send(stream, &request);
            let mut calls = 0;
            loop {
                let reply = frames::reply(stream).expect("a reply");
                match reply.message_type {
                    MessageType::Call => {
                        calls += 1;
                        let value = answer(&reply);
                        let call_id = reply.id.expect("a Call carries its id").get();
                        frames::send(
                            stream,
                            &Envelope::new(
                                CorrelationId(call_id),
                                MessageType::Result,
                                Value::Map(vec![(Value::from("value"), value)]),
                            ),
                        );
                    }
                    MessageType::Result => {
                        assert_eq!(
                            reply.id.map(CorrelationId::get),
                            expected,
                            "the reply names its execution"
                        );
                        return (reply.payload, calls);
                    }
                    other => panic!("the run failed with {other:?}: {:?}", reply.payload),
                }
            }
        }

        /// Story 5.9, the quick-mode check of the TCP+TLS leg: over a real TLS connection, every
        /// student's verdict is the one Rust computes, with exactly 10 host calls per run, in
        /// three rotations.
        #[test]
        fn the_rule_over_tls_is_verified_for_every_student_with_ten_host_calls() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let mut client = tls(daemon.tcp_address());
            init_on(&mut client, 1);
            let handle = register_on(&mut client, 2);
            let students = students();
            let mut id = 3;
            let mut verdicts = Vec::new();
            for _round in 0..3 {
                for student in &students {
                    let (payload, calls) =
                        serve_run(&mut client, cached_request(id, &handle, student));
                    id += 1;
                    assert_eq!(payload, expected_payload(student), "{}", student.id);
                    assert_eq!(calls, HOST_CALLS_PER_RUN, "{}", student.id);
                    verdicts.push(expected_verdict(student));
                }
            }
            assert!(
                verdicts.contains(&true) && verdicts.contains(&false),
                "the workload must exercise both outcomes"
            );
            drop(client);
            daemon.stop().exit(0);
        }

        /// Story 5.9: the cold figure over TLS, `CodeRegister` then its first run, then the
        /// untimed `CodeUnregister` — each run verified, and the handle freed afterwards.
        #[test]
        fn cold_and_warm_runs_over_tls_agree_and_code_unregister_frees_the_handle() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let mut client = tls(daemon.tcp_address());
            init_on(&mut client, 1);
            let students = students();
            let student = &students[1];
            let handle = register_on(&mut client, 2);
            let (cold, cold_calls) = serve_run(&mut client, cached_request(3, &handle, student));
            let (warm, warm_calls) = serve_run(&mut client, cached_request(4, &handle, student));
            let expected = expected_payload(student);
            assert_eq!(cold, expected, "cold over TLS");
            assert_eq!(warm, expected, "warm over TLS");
            assert_eq!(cold_calls, HOST_CALLS_PER_RUN);
            assert_eq!(warm_calls, HOST_CALLS_PER_RUN);

            frames::send(
                &mut client,
                &Envelope::new(
                    CorrelationId(5),
                    MessageType::CodeUnregister,
                    Value::Map(vec![(Value::from("handle"), Value::from(handle.as_str()))]),
                ),
            );
            let reply = frames::reply(&mut client).expect("a reply");
            assert_eq!(
                reply.message_type,
                MessageType::Result,
                "{:?}",
                reply.payload
            );
            assert_eq!(reply.payload, Value::Map(vec![]));

            frames::send(&mut client, &cached_request(6, &handle, student));
            let refusal = frames::reply(&mut client).expect("a reply");
            assert_eq!(refusal.message_type, MessageType::Error);
            assert_eq!(refusal.id, Some(CorrelationId(6)));
            drop(client);
            daemon.stop().exit(0);
        }

        /// Story 5.9, the TCP+TLS steady state in quick mode: eight connections, each on its own
        /// thread, one TLS connection per thread, every run verified with 10 host calls.
        #[test]
        fn eight_concurrent_tls_connections_each_verify_every_run_of_the_rule() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let addr = daemon.tcp_address();
            std::thread::scope(|scope| {
                for index in 0..CONCURRENCY {
                    scope.spawn(move || {
                        let mut client = tls(addr);
                        init_on(&mut client, 1);
                        let handle = register_on(&mut client, 2);
                        let students = students();
                        for run in 0..RUNS_PER_CONNECTION {
                            let student = &students[(index + run) % students.len()];
                            let (payload, calls) = serve_run(
                                &mut client,
                                cached_request(3 + run as u64, &handle, student),
                            );
                            assert_eq!(payload, expected_payload(student), "{}", student.id);
                            assert_eq!(calls, HOST_CALLS_PER_RUN, "{}", student.id);
                        }
                    });
                }
            });
            daemon.stop().exit(0);
        }

        /// Story 5.9: both legs of the one Daemon give the same verified verdict for every
        /// student, so the TCP+TLS figures measure the same work as the UDS ones.
        #[test]
        fn the_uds_and_tls_legs_answer_the_same_verified_verdict_for_every_student() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let mut uds: UnixStream = daemon.uds(&sandbox.socket());
            let mut over_tls = tls(daemon.tcp_address());
            init_on(&mut uds, 1);
            init_on(&mut over_tls, 1);
            let uds_handle = register_on(&mut uds, 2);
            let tls_handle = register_on(&mut over_tls, 2);
            for (index, student) in students().iter().enumerate() {
                let id = 3 + index as u64;
                let (uds_payload, uds_calls) =
                    serve_run(&mut uds, cached_request(id, &uds_handle, student));
                let (tls_payload, tls_calls) =
                    serve_run(&mut over_tls, cached_request(id, &tls_handle, student));
                assert_eq!(uds_payload, expected_payload(student), "UDS {}", student.id);
                assert_eq!(tls_payload, uds_payload, "TLS {}", student.id);
                assert_eq!(uds_calls, HOST_CALLS_PER_RUN);
                assert_eq!(tls_calls, uds_calls, "{}", student.id);
            }
            drop(uds);
            drop(over_tls);
            daemon.stop().exit(0);
        }

        /// Story 5.9: the TCP+TLS leg's port is the one picked just before start, and the Daemon
        /// listens on it.
        #[test]
        fn the_daemon_listens_on_the_port_picked_just_before_start() {
            let sandbox = Sandbox::new();
            let port = picked_port();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, port));
            assert_eq!(daemon.tcp_address().port(), port);
            let mut client = tls(daemon.tcp_address());
            init_on(&mut client, 1);
            drop(client);
            daemon.stop().exit(0);
        }

        /// Story 5.9, the matrix row "Port taken between pick and bind": another process holds
        /// the port, so the Daemon exits with failure and nothing is published.
        #[test]
        fn a_port_taken_between_pick_and_bind_fails_the_daemon_with_exit_one() {
            let sandbox = Sandbox::new();
            let holder = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = holder.local_addr().unwrap().port();
            let run = daemon_with(&sandbox, &config_with_tcp(&sandbox, port));
            run.exit(1);
            assert!(
                run.stderr
                    .starts_with(&format!("error: 127.0.0.1:{port}: cannot listen for TCP: ")),
                "{}",
                run.stderr
            );
            drop(holder);
        }

        /// Runs the Daemon in-process until it stops on its own (a startup failure).
        fn daemon_with(sandbox: &Sandbox, config_text: &str) -> Run {
            let config = sandbox.file("config.toml", config_text);
            super::super::daemon(&["--config", config.to_str().unwrap()], &nothing(sandbox))
        }

        /// Story 5.9, the bench's TLS parameters: the negotiated protocol version and cipher suite
        /// read off a completed handshake print as `TLSv1_3, TLS13_…`.
        #[test]
        fn a_completed_handshake_names_its_version_and_suite_in_the_bench_format() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let mut client = tls(daemon.tcp_address());
            while client.conn.is_handshaking() {
                client.conn.complete_io(&mut client.sock).unwrap();
            }
            let version = client
                .conn
                .protocol_version()
                .expect("a negotiated version");
            let suite = client
                .conn
                .negotiated_cipher_suite()
                .expect("a negotiated suite");
            let text = format!("{version:?}, {:?}", suite.suite());
            assert!(text.starts_with("TLSv1_3, TLS13_"), "{text}");
            drop(client);
            daemon.stop().exit(0);
        }

        /// Story 5.9: the client trusts only the fixture CA, so an empty root store refuses the
        /// Daemon's certificate.
        #[test]
        fn a_client_that_trusts_no_root_is_refused_by_the_handshake() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let config = Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
            );
            let mut connection = rustls::ClientConnection::new(
                config,
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            )
            .unwrap();
            let mut tcp = TcpStream::connect(daemon.tcp_address()).unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            assert!(connection.complete_io(&mut tcp).is_err());
            drop(tcp);
            daemon.stop().exit(0);
        }

        /// Story 5.9: the verification is by name too: the fixture certificate is for `localhost`
        /// and `127.0.0.1`, so another name is refused even with the fixture CA trusted.
        #[test]
        fn a_name_other_than_localhost_is_refused_by_the_handshake() {
            let sandbox = Sandbox::new();
            let daemon = Daemon::start(&sandbox, &config_with_tcp(&sandbox, picked_port()));
            let mut roots = rustls::RootCertStore::empty();
            for certificate in
                rustls::pki_types::CertificateDer::pem_file_iter(fixture("ca.pem")).unwrap()
            {
                roots.add(certificate.unwrap()).unwrap();
            }
            let config = Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
            );
            let mut connection = rustls::ClientConnection::new(
                config,
                rustls::pki_types::ServerName::try_from("example.org").unwrap(),
            )
            .unwrap();
            let mut tcp = TcpStream::connect(daemon.tcp_address()).unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            assert!(connection.complete_io(&mut tcp).is_err());
            drop(tcp);
            daemon.stop().exit(0);
        }

        /// Story 5.9: the bench's source names what this module pins: the group, its figures, the
        /// TCP+TLS steady state and its label, the topology text, the TLS client's settings, and the
        /// JSON and comparison fields. These are names only; the run itself is verified by the
        /// bench's own quick mode, which `cargo test` runs.
        #[test]
        fn the_bench_declares_the_remote_path_legs_steady_state_report_and_topology() {
            let bench = include_str!("../benches/cache.rs");
            for needle in [
                "benchmark_group(\"remote_path\")",
                "tcp_tls_cold_first_run",
                "tcp_tls_warm_cache_hit",
                "noise_threshold(report::VARIANCE.p50)",
                "daemon.connect_tls()",
                "steady::measure(|| daemon.connect_tls(), workload::RULE",
                "report::TCP_TLS_SIDE",
                "Hexput Daemon, Cached Execution over TCP+TLS (loopback)",
                "tcp_tls_steady_state",
                "tcp_tls_summary",
                "tcp_tls: Delta",
                "NoPrevious",
                "TCP+TLS/UDS",
                "render_side_by_side",
                "self_check",
                "legs",
                "tcp_tls",
                "127.0.0.1 (loopback)",
                "ring provider, server authentication only",
                "same machine over the loopback interface",
                "no network hop",
                "same machine: Daemon in-process on its own runtime, clients on threads of the bench process",
                "the Daemon never listened",
                "127.0.0.1:0",
                "TcpListener",
                "[transport.tcp]",
                "\"ca.pem\"",
                "\"server.pem\"",
                "\"server.key\"",
                "CARGO_MANIFEST_DIR",
                "builder_with_provider(",
                "with_safe_default_protocol_versions",
                "\"localhost\"",
                "complete_io",
                "is_handshaking()",
                "set_nodelay(true)",
                "Duration::from_secs(60)",
                "trait Duplex",
                "Box<dyn Duplex>",
                "flush()",
            ] {
                assert!(bench.contains(needle), "the bench lacks `{needle}`");
            }
        }

        /// Story 5.9: the bench's module doc carries the Story 5.9 figures and the loopback
        /// topology, and the transport table keeps the two legs apart.
        #[test]
        fn the_bench_module_doc_names_the_remote_figures_and_the_loopback_topology() {
            let bench = include_str!("../benches/cache.rs");
            let doc: String = bench
                .lines()
                .take_while(|line| line.starts_with("//!"))
                .collect();
            assert!(doc.contains("Story 5.9"), "the module doc names Story 5.9");
            assert!(doc.contains("remote_path/tcp_tls_cold_first_run"));
            assert!(doc.contains("remote_path/tcp_tls_warm_cache_hit"));
            assert!(doc.contains("127.0.0.1"), "the loopback address is named");
            assert!(doc.contains("loopback"), "the topology is named loopback");
        }

        /// Story 5.9: the TCP+TLS steady state runs after Hexput's UDS one and before Rhai's, inside
        /// the same filter gate as `steady_state`, so a filter that skips the steady state skips it.
        #[test]
        fn the_tcp_tls_steady_state_is_ordered_between_the_uds_and_rhai_legs_and_filtered_with_them()
         {
            let bench = include_str!("../benches/cache.rs");
            let gate = bench
                .find("report::runs_steady_state(")
                .expect("the steady state gate");
            let skipped = bench
                .find("steady_state skipped by the filter")
                .expect("the skip message");
            let uds = bench
                .find("steady::measure(|| daemon.connect(), workload::RULE")
                .expect("the UDS steady state");
            let tls = bench
                .find("steady::measure(|| daemon.connect_tls(), workload::RULE")
                .expect("the TCP+TLS steady state");
            let rhai = bench
                .find("steady::measure(\n            || rhai_server.connect(),")
                .or_else(|| bench.find("|| rhai_server.connect()"))
                .expect("the Rhai steady state");
            assert!(
                gate < uds && uds < tls && tls < rhai,
                "the order is UDS, TCP+TLS, Rhai"
            );
            assert!(tls < skipped, "the TCP+TLS steady state is inside the gate");
        }

        /// Story 5.9: the remote figures sit in their own `remote_path` group, not inside
        /// `cache_win`, and the UDS group is still the one the 4.4 drift guards read.
        #[test]
        fn the_remote_figures_are_a_group_of_their_own_beside_the_uds_ones() {
            let bench = include_str!("../benches/cache.rs");
            let uds = bench
                .find("benchmark_group(\"cache_win\")")
                .expect("cache_win");
            let remote = bench
                .find("benchmark_group(\"remote_path\")")
                .expect("remote_path");
            let rhai = bench
                .find("benchmark_group(\"rhai_compare\")")
                .expect("rhai_compare");
            assert!(rhai < remote, "remote_path follows rhai_compare");
            assert!(
                !bench[uds..rhai].contains("tcp_tls"),
                "no TCP+TLS figure sits in the UDS group"
            );
        }

        /// Story 5.9: the Rhai server still reaches the one boxed `Client` constructor.
        #[test]
        fn the_rhai_server_still_builds_its_client_through_client_new() {
            let rhai = include_str!("../benches/cache/rhai.rs");
            assert!(rhai.contains("Client::new("));
        }
    }

    // --- Story 4.5: the Rhai comparator over the same workload, and the in-process engine drivers ---

    /// Story 4.5: the Rhai comparator (spec `cache` bench, constant `RHAI_RULE`), its native host
    /// functions, and the in-process Hexput drivers of the `engine` section. Every driver runs the
    /// same 16 students and must return the verdict Rust computes, with 10 host calls per run.
    /// Like the 4.4 module, this copies the bench's workload; the drift guards pin the copies.
    mod rhai_comparator {
        use super::*;
        use hexput_enforce::{
            Budget, DEFAULT_ALLOCATIONS, DEFAULT_OUTPUT_SIZE, DEFAULT_RPC_CALLS,
            DEFAULT_SIDE_EFFECTS, Exceeded, Limits,
        };
        use hexput_exec::{
            SLICE,
            wire::{measure_held, result_payload_size},
        };
        use hexput_interpreter::{
            Argument, Array as HexArray, Execution, Meter, Object as HexObject, Outcome, Program,
            Value as HexValue,
        };
        use hexput_shared::diagnostics::Span;
        use rhai::{AST, Array, Dynamic, Engine, ImmutableString, Map, Scope};
        use std::path::Path;
        use std::process::Command;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        const RHAI_RULE: &str = "let total = 0.0; let weights = 0.0;\nfor grade in grades { let w = weight(grade.course); total = total + grade.score * w; weights = weights + w; }\nlet passed = total / weights >= passMark(student.program);\nrecord(student.id, passed);\npassed\n";

        /// The root manifest's directory, the workspace root.
        fn workspace_root() -> std::path::PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .expect("the workspace root")
        }

        /// The `[section]` headers under which a manifest names `dependency` (as `dependency = …`
        /// or `dependency.workspace = …`).
        fn sections_naming(manifest: &str, dependency: &str) -> Vec<String> {
            let mut section = String::new();
            let mut found = Vec::new();
            for line in manifest.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    section = trimmed.to_owned();
                    continue;
                }
                let named = trimmed.strip_prefix(dependency).is_some_and(|rest| {
                    matches!(rest.trim_start().chars().next(), Some('=') | Some('.'))
                });
                if named {
                    found.push(section.clone());
                }
            }
            found
        }

        /// Story 4.5: `rhai` is pinned at 1.26.1 in the workspace, reaches `hexput-tests` as a
        /// dev-dependency only, and no other crate names it at all.
        #[test]
        fn rhai_is_pinned_at_one_point_two_six_one_and_is_a_dev_dependency_only() {
            let root = workspace_root();
            let workspace =
                std::fs::read_to_string(root.join("Cargo.toml")).expect("root manifest");
            assert!(
                workspace
                    .lines()
                    .any(|line| line.trim() == "rhai = \"1.26.1\""),
                "the workspace pins `rhai = \"1.26.1\"`"
            );
            assert_eq!(
                sections_naming(&workspace, "rhai"),
                vec!["[workspace.dependencies]".to_owned()]
            );
            // The manifest line is a caret requirement, so the lock is what holds 1.26.1: a later
            // `cargo update` that moved it would measure another Rhai under the "rhai 1.26.1" label.
            let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("Cargo.lock");
            let locked: Vec<&str> = lock
                .split("[[package]]")
                .filter(|package| package.lines().any(|line| line.trim() == "name = \"rhai\""))
                .filter_map(|package| {
                    package
                        .lines()
                        .find_map(|line| line.trim().strip_prefix("version = "))
                })
                .collect();
            assert_eq!(
                locked,
                vec!["\"1.26.1\""],
                "Cargo.lock resolves rhai at exactly 1.26.1"
            );

            let tests =
                std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                    .expect("hexput-tests' manifest");
            assert_eq!(
                sections_naming(&tests, "rhai"),
                vec!["[dev-dependencies]".to_owned()],
                "hexput-tests reaches rhai as a dev-dependency only"
            );

            for entry in std::fs::read_dir(root.join("crates")).expect("the crates directory") {
                let directory = entry.expect("a crate directory").path();
                if directory.file_name().and_then(|name| name.to_str()) == Some("hexput-tests") {
                    continue;
                }
                let manifest = std::fs::read_to_string(directory.join("Cargo.toml"))
                    .unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
                assert!(
                    sections_naming(&manifest, "rhai").is_empty(),
                    "{} names rhai; only hexput-tests may, as a dev-dependency",
                    directory.display()
                );
            }
        }

        /// Story 4.5: the crate-graph check still passes with rhai added. That rhai reaches no
        /// production crate is the manifest test above (only `hexput-tests` names it, as a
        /// dev-dependency); this one checks that adding it moved no pinned edge.
        #[test]
        fn the_crate_graph_check_still_passes_with_rhai_added() {
            let output = Command::new("python3")
                .arg("scripts/check-crate-graph.py")
                .current_dir(workspace_root())
                .output()
                .expect("python3 runs the crate-graph check");
            assert!(
                output.status.success(),
                "check-crate-graph.py failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        /// Story 4.5: the Rhai rule the bench measures is the one this module runs, and the bench
        /// wires the Rhai module in with the same workload constants the Hexput copy checks.
        #[test]
        fn the_rhai_rule_is_the_one_the_bench_measures() {
            let rhai = include_str!("../benches/cache/rhai.rs");
            assert!(
                rhai.contains(&format!("pub const RHAI_RULE: &str = {RHAI_RULE:?};")),
                "the bench's `RHAI_RULE` differs from this copy"
            );
            assert!(
                include_str!("../benches/cache.rs").contains("mod rhai;"),
                "the bench declares the Rhai module"
            );
        }

        /// Story 4.5: the comparator suite's text in the bench, the Rhai server's shape and the
        /// engine section's drivers, pinned by name. This pins names only: the drivers below are
        /// copies that check the same sequence, and the bench's own drivers and Rhai server are
        /// verified run by run by the bench's quick mode, which `cargo test` runs.
        #[test]
        fn the_bench_declares_the_comparator_group_engine_section_and_report_fields() {
            let bench = include_str!("../benches/cache.rs");
            let rhai = include_str!("../benches/cache/rhai.rs");
            let all = format!("{bench}{rhai}");
            for needle in [
                "rhai_compare",
                "hexput_cold_first_run",
                "rhai_cold_first_run",
                "hexput_warm_cache_hit",
                "rhai_warm_cache_hit",
                "noise_threshold(report::VARIANCE.p50)",
                "ENGINE_ROUNDS: usize = 5;",
                "ENGINE_RUNS: usize = 10_000;",
                "budget enforcement overhead: +",
                "rhai 1.26.1, Engine::new() defaults, thread-per-connection UDS server in the bench",
                "rhai_source",
                "rhai_steady_state",
                "engine_rounds",
                "engine_runs",
                "hexput_unmetered_ns",
                "hexput_enforced_ns",
                "rhai_engine_ns",
                "budget_overhead_ns",
                "budget_overhead_pct",
                "hexput_unmetered",
                "hexput_enforced",
                "rhai_engine",
                "Meter::UNMETERED",
                "Budget::with_limits(Limits::default())",
                "memory_ceiling()",
                "allocation_ceiling()",
                "charge_cpu",
                "charge_rpc_call",
                "charge_output",
                "result_payload_size",
                "measure_held",
                "argument_depth()",
                "CodeRegister",
                "CachedExecutionStart",
                "CodeUnregister",
                "eval_ast_with_scope",
                "UnixListener",
                "Engine::new()",
                "encode_frame",
                "MAX_FRAME_LEN",
            ] {
                assert!(all.contains(needle), "the bench lacks `{needle}`");
            }
            assert!(
                !rhai.contains("set_max_"),
                "the Rhai comparator runs at Engine::new()'s defaults, with no limit set"
            );
        }

        /// Story 4.5: the Rhai engine of the comparator: native host functions that count their
        /// calls and log each `record`, with the rule compiled once.
        struct RhaiEngine {
            engine: Engine,
            ast: AST,
            calls: Arc<AtomicUsize>,
            recorded: Arc<Mutex<Vec<(String, bool)>>>,
        }

        /// One Rhai run: its verdict, the `record` calls it made and its host calls.
        struct RhaiRun {
            verdict: bool,
            recorded: Vec<(String, bool)>,
            calls: usize,
        }

        impl RhaiEngine {
            /// `Engine::new()` defaults, the rule compiled once, the three host functions native.
            fn new() -> Self {
                let calls = Arc::new(AtomicUsize::new(0));
                let recorded = Arc::new(Mutex::new(Vec::new()));
                let mut engine = Engine::new();
                let counted = Arc::clone(&calls);
                engine.register_fn("weight", move |course: ImmutableString| -> f64 {
                    counted.fetch_add(1, Ordering::SeqCst);
                    course_weight(course.as_str()) as f64
                });
                let counted = Arc::clone(&calls);
                engine.register_fn("passMark", move |program: ImmutableString| -> f64 {
                    counted.fetch_add(1, Ordering::SeqCst);
                    pass_mark(program.as_str()) as f64
                });
                let counted = Arc::clone(&calls);
                let sink = Arc::clone(&recorded);
                engine.register_fn("record", move |id: ImmutableString, passed: bool| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    sink.lock()
                        .expect("the record log")
                        .push((id.to_string(), passed));
                });
                let ast = engine.compile(RHAI_RULE).expect("the Rhai rule compiles");
                Self {
                    engine,
                    ast,
                    calls,
                    recorded,
                }
            }

            /// One run in a fresh `Scope`, with the student's variables cloned in.
            fn run(&self, grades: &Array, student: &Map) -> RhaiRun {
                self.calls.store(0, Ordering::SeqCst);
                self.recorded.lock().expect("the record log").clear();
                let mut scope = Scope::new();
                scope.push("grades", grades.clone());
                scope.push("student", student.clone());
                let verdict = self
                    .engine
                    .eval_ast_with_scope::<bool>(&mut scope, &self.ast)
                    .expect("the Rhai rule runs");
                RhaiRun {
                    verdict,
                    recorded: std::mem::take(&mut *self.recorded.lock().expect("the record log")),
                    calls: self.calls.load(Ordering::SeqCst),
                }
            }
        }

        /// A student's variables as Rhai sees them: every number an `f64`, converted once.
        fn rhai_variables(student: &Student) -> (Array, Map) {
            let grades: Array = student
                .grades
                .iter()
                .map(|(course, score)| {
                    let mut grade = Map::new();
                    grade.insert("course".into(), (*course).into());
                    grade.insert("score".into(), (*score as f64).into());
                    Dynamic::from(grade)
                })
                .collect();
            let mut record = Map::new();
            record.insert("id".into(), student.id.as_str().into());
            record.insert("program".into(), student.program.into());
            (grades, record)
        }

        /// Story 4.5: the Rhai rule returns the verdict Rust computes for every student, makes
        /// exactly the 10 host calls, records the right verdict under the right id, and shares no
        /// state between runs (a fresh `Scope` each time, so three rounds agree).
        #[test]
        fn the_rhai_rule_gives_the_verified_verdict_and_ten_host_calls_for_every_student() {
            let engine = RhaiEngine::new();
            let students = students();
            let variables: Vec<(Array, Map)> = students.iter().map(rhai_variables).collect();
            let mut verdicts = Vec::new();
            for _round in 0..3 {
                for (student, (grades, record)) in students.iter().zip(&variables) {
                    let run = engine.run(grades, record);
                    let expected = expected_verdict(student);
                    assert_eq!(run.verdict, expected, "rhai {}", student.id);
                    assert_eq!(run.calls, HOST_CALLS_PER_RUN, "rhai {}", student.id);
                    assert_eq!(
                        run.recorded,
                        vec![(student.id.clone(), expected)],
                        "rhai {}",
                        student.id
                    );
                    verdicts.push(run.verdict);
                }
            }
            assert!(
                verdicts.contains(&true) && verdicts.contains(&false),
                "the workload must exercise both outcomes"
            );
        }

        /// Story 4.5: Hexput over the socket (the same client and registration path as the bench)
        /// and Rhai in-process reach the same verified verdict for every student, each with 10
        /// calls. The Rhai server over the socket is run by the bench's quick mode.
        #[test]
        fn hexput_over_the_socket_and_rhai_in_process_reach_the_same_verified_verdict() {
            let sandbox = Sandbox::new();
            let serving = Serving::start(&sandbox, &sandbox.valid());
            let mut client = serving.connect();
            init(&mut client, 1);
            let handle = register(&mut client, 2);
            let engine = RhaiEngine::new();
            let students = students();
            for (id, student) in (3..).zip(&students) {
                let (payload, calls) =
                    run_and_serve_calls(&mut client, cached_request(id, &handle, student));
                assert_eq!(payload, expected_payload(student), "hexput {}", student.id);
                assert_eq!(calls, HOST_CALLS_PER_RUN, "hexput {}", student.id);

                let (grades, record) = rhai_variables(student);
                let run = engine.run(&grades, &record);
                assert_eq!(
                    run.verdict,
                    expected_verdict(student),
                    "rhai {}",
                    student.id
                );
                assert_eq!(run.calls, HOST_CALLS_PER_RUN, "rhai {}", student.id);
            }
            serving.stop().exit(0);
        }

        /// The Hexput side of the `engine` section: one run's result, its host calls, the `record`
        /// calls it made, and the budget it was charged against when it is enforced.
        struct Driven {
            verdict: bool,
            calls: usize,
            recorded: Vec<(String, bool)>,
            budget: Option<Budget>,
        }

        /// A student's starting variables as the interpreter takes them, converted once.
        fn hexput_variables(student: &Student) -> Vec<(&'static str, HexValue)> {
            let grades = HexArray::from_values(
                student
                    .grades
                    .iter()
                    .map(|(course, score)| {
                        HexValue::Object(HexObject::from_entries([
                            ("course", HexValue::String(Arc::from(*course))),
                            ("score", HexValue::Number(*score as f64)),
                        ]))
                    })
                    .collect(),
            );
            let record = HexObject::from_entries([
                ("id", HexValue::String(Arc::from(student.id.as_str()))),
                ("program", HexValue::String(Arc::from(student.program))),
            ]);
            vec![
                ("grades", HexValue::Array(grades)),
                ("student", HexValue::Object(record)),
            ]
        }

        /// Answers the rule's three host calls natively, with no wire, recording each `record`.
        fn answer(
            name: &str,
            arguments: &[Argument],
            recorded: &mut Vec<(String, bool)>,
        ) -> HexValue {
            let values: Vec<&HexValue> = arguments.iter().map(|argument| &argument.value).collect();
            match (name, values.as_slice()) {
                ("weight", [HexValue::String(course)]) => {
                    HexValue::Number(course_weight(course) as f64)
                }
                ("passMark", [HexValue::String(program)]) => {
                    HexValue::Number(pass_mark(program) as f64)
                }
                ("record", [HexValue::String(id), HexValue::Bool(passed)]) => {
                    recorded.push((id.to_string(), *passed));
                    HexValue::Null
                }
                _ => panic!(
                    "an unexpected host call to `{name}` with {} argument(s)",
                    values.len()
                ),
            }
        }

        /// A budget that refused a charge ends the run, naming the dimension.
        fn within(result: Result<(), Exceeded>) {
            if let Err(exceeded) = result {
                panic!(
                    "the enforced run exceeded its {} budget",
                    exceeded.dimension().as_str()
                );
            }
        }

        fn charge_cpu(budget: &mut Option<Budget>, elapsed: Duration, span: Span) {
            if let Some(budget) = budget {
                within(budget.charge_cpu(elapsed, span));
            }
        }

        /// Runs the rule in-process. Unmetered, it is `Execution` with `Meter::UNMETERED`. Enforced,
        /// it is the Executor's synchronous enforcement: the same slice and ceilings, CPU charged
        /// at every pause, host call and finish, one RPC call per host call, output at the finish.
        fn drive_hexput(
            program: &Arc<Program>,
            variables: &[(&'static str, HexValue)],
            enforced: bool,
        ) -> Driven {
            let mut budget = enforced.then(|| Budget::with_limits(Limits::default()));
            let meter = match &budget {
                Some(budget) => Meter {
                    slice: Some(SLICE),
                    memory_ceiling: Some(budget.memory_ceiling()),
                    allocation_ceiling: Some(budget.allocation_ceiling()),
                },
                None => Meter::UNMETERED,
            };
            let mut execution =
                Execution::with_variables(Arc::clone(program), variables.iter().cloned())
                    .expect("the starting variables bind")
                    .metered(meter);
            let mut recorded = Vec::new();
            let mut calls = 0;
            let mut started = Instant::now();
            loop {
                match execution.run().expect("the rule runs") {
                    Outcome::Paused(paused) => {
                        charge_cpu(&mut budget, started.elapsed(), paused.span());
                        execution = paused.resume();
                        started = Instant::now();
                    }
                    Outcome::HostCall(call) => {
                        charge_cpu(&mut budget, started.elapsed(), call.span());
                        if let Some(budget) = budget.as_mut() {
                            let measuring = Instant::now();
                            let depth = budget.limits().argument_depth();
                            let mut frame = hexput_port::MAX_FRAME_LEN;
                            for argument in call.arguments() {
                                assert!(
                                    measure_held(
                                        &argument.value,
                                        argument.secret.as_ref(),
                                        depth,
                                        &mut frame
                                    )
                                    .is_ok(),
                                    "a sendable argument"
                                );
                            }
                            within(budget.charge_rpc_call(call.span()));
                            within(budget.charge_cpu(measuring.elapsed(), call.span()));
                        }
                        calls += 1;
                        let value = answer(call.name(), call.arguments(), &mut recorded);
                        execution = call.resume(&value);
                        started = Instant::now();
                    }
                    Outcome::Finished(finished) => {
                        let span = program.span;
                        charge_cpu(&mut budget, started.elapsed(), span);
                        if let Some(budget) = &budget {
                            let bytes = result_payload_size(
                                &finished.result,
                                &finished.modifications,
                                budget.limits().output_size(),
                            );
                            within(budget.charge_output(bytes, span));
                        }
                        let verdict = finished
                            .result
                            .value
                            .as_bool()
                            .expect("the rule returns a bool");
                        return Driven {
                            verdict,
                            calls,
                            recorded,
                            budget,
                        };
                    }
                    Outcome::OutOfMemory(_) => panic!("the rule ran out of memory"),
                    Outcome::AllocationsExceeded(_) => panic!("the rule made too many allocations"),
                }
            }
        }

        /// Story 4.5: both in-process Hexput drivers, unmetered and enforced, return the verified
        /// verdict for all 16 students with 10 host calls and one correct `record` each. The
        /// enforced driver panics on any `Exceeded`, so a refused charge fails this test.
        #[test]
        fn the_in_process_hexput_drivers_verify_every_student_unmetered_and_enforced() {
            let program = Arc::new(hexput_parser::parse(RULE).expect("the rule parses"));
            for student in &students() {
                let variables = hexput_variables(student);
                for enforced in [false, true] {
                    let driven = drive_hexput(&program, &variables, enforced);
                    let expected = expected_verdict(student);
                    assert_eq!(
                        driven.verdict, expected,
                        "{} enforced={enforced}",
                        student.id
                    );
                    assert_eq!(
                        driven.calls, HOST_CALLS_PER_RUN,
                        "{} enforced={enforced}",
                        student.id
                    );
                    assert_eq!(
                        driven.recorded,
                        vec![(student.id.clone(), expected)],
                        "{} enforced={enforced}",
                        student.id
                    );
                }
            }
        }

        /// Story 4.5: the enforced driver charges its run against the default budget and every
        /// host call counts once toward the RPC and side-effect dimensions; the defaults in force
        /// are the ones the headline UDS figures run with, unchanged.
        #[test]
        fn the_enforced_driver_charges_ten_calls_against_the_unchanged_default_budget() {
            let program = Arc::new(hexput_parser::parse(RULE).expect("the rule parses"));
            let students = students();
            let variables = hexput_variables(&students[1]);
            let driven = drive_hexput(&program, &variables, true);
            let budget = driven.budget.expect("the enforced run keeps its budget");
            assert_eq!(budget.rpc_calls_used(), HOST_CALLS_PER_RUN as u64);
            assert_eq!(budget.side_effects_used(), HOST_CALLS_PER_RUN as u64);
            let limits = budget.limits();
            assert_eq!(limits.rpc_calls(), DEFAULT_RPC_CALLS);
            assert_eq!(limits.side_effects(), DEFAULT_SIDE_EFFECTS);
            assert_eq!(limits.allocations(), DEFAULT_ALLOCATIONS);
            assert_eq!(limits.output_size(), DEFAULT_OUTPUT_SIZE);
        }

        /// Story 4.5: the unmetered driver keeps no budget at all, so its figure is the interpreter
        /// alone and the overhead line is exactly the enforced driver's extra work.
        #[test]
        fn the_unmetered_driver_keeps_no_budget() {
            let program = Arc::new(hexput_parser::parse(RULE).expect("the rule parses"));
            let students = students();
            let driven = drive_hexput(&program, &hexput_variables(&students[0]), false);
            assert!(driven.budget.is_none());
            assert_eq!(driven.calls, HOST_CALLS_PER_RUN);
        }
    }
}

// --- Story 5.1: TCP served with mandatory TLS, beside the Unix Domain Socket ---

/// A Daemon serving TCP+TLS (and UDS, where its config names it) over real sockets. The TLS
/// client is a blocking rustls stream trusting the fixture CA; the fixtures are generated as
/// described in `transport.rs`'s `fixture` doc comment.
#[cfg(unix)]
mod tls_serving {
    use std::io::{ErrorKind, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    use super::conformance::harness::{
        Daemon, Wss, fixture, frames, tcp_section, tcp_section_with, tls, websocket_section,
        websocket_section_with,
    };
    use super::{Sandbox, daemon, nothing};

    /// The Client ID an Init reply carries (its `reconnect_secret`, Story 5.5, is checked by the
    /// connection and conformance tests; here it must merely follow the Client ID).
    fn client_id(init: &Envelope<Value>) -> String {
        assert_eq!(init.message_type, MessageType::Result, "{:?}", init.payload);
        let fields = init.payload.as_map().expect("a map payload");
        assert_eq!(
            fields.len(),
            2,
            "`client_id` then `reconnect_secret`: {fields:?}"
        );
        assert_eq!(fields[0].0.as_str(), Some("client_id"));
        assert_eq!(fields[1].0.as_str(), Some("reconnect_secret"));
        fields[0].1.as_str().expect("a string Client ID").to_owned()
    }

    /// The integer under `value` in a `Result` payload.
    fn value_of(reply: &Envelope<Value>) -> i64 {
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        reply
            .payload
            .as_map()
            .and_then(|fields| {
                fields
                    .iter()
                    .find(|(key, _)| key.as_str() == Some("value"))
                    .and_then(|(_, value)| value.as_i64())
            })
            .unwrap_or_else(|| panic!("a `value` integer in {:?}", reply.payload))
    }

    /// Init, then `return 1 + 2;`: the two replies a client gets.
    fn init_and_run<S: Read + Write>(stream: &mut S) -> (Envelope<Value>, Envelope<Value>) {
        frames::send(stream, &frames::init(1));
        let init = frames::reply(stream).expect("an Init reply");
        frames::send(stream, &frames::request(2, "return 1 + 2;"));
        let run = frames::reply(stream).expect("an execution reply");
        (init, run)
    }

    #[test]
    fn a_tls_client_gets_the_same_replies_as_a_uds_client() {
        let sandbox = Sandbox::new();
        let config = format!("{}{}", sandbox.valid(), tcp_section("127.0.0.1:0"));
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.tcp_address();
        assert_ne!(addr.port(), 0, "the actual bound address is logged");

        let (uds_init, uds_run) = init_and_run(&mut daemon.uds(&sandbox.socket()));
        let (tls_init, tls_run) = init_and_run(&mut tls(addr));

        assert_eq!(value_of(&uds_run), 3);
        assert_eq!(value_of(&tls_run), 3);
        assert_eq!(tls_run.id, Some(CorrelationId(2)));
        assert_eq!(tls_run.id, uds_run.id);
        assert_eq!(tls_run.message_type, uds_run.message_type);
        assert_eq!(tls_run.payload, uds_run.payload, "identical to UDS");
        assert_eq!(tls_init.message_type, uds_init.message_type);
        assert_eq!(client_id(&tls_init).len(), 32);
        assert_ne!(
            client_id(&tls_init),
            client_id(&uds_init),
            "one Session each"
        );
        daemon.stop().exit(0);
    }

    #[test]
    fn a_bare_ip_bind_listens_on_the_default_port_7476() {
        // The one test that needs a fixed port: skipped, not failed, when something else on the
        // machine (a running Daemon, another test run) already holds it.
        if let Err(error) = TcpListener::bind(("127.0.0.1", 7476)) {
            eprintln!("skipped: 127.0.0.1:7476 is unavailable: {error}");
            return;
        }
        let sandbox = Sandbox::new();
        let daemon = Daemon::start(&sandbox, &tcp_section("127.0.0.1"));
        let addr = daemon.tcp_address();
        assert_eq!(addr.port(), 7476, "{addr}");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(
            daemon
                .log_with("listening on TCP with TLS")
                .lines()
                .any(|line| line.contains("address=127.0.0.1:7476")),
            "the log names the default port"
        );

        let (init, run) = init_and_run(&mut tls(SocketAddr::from(([127, 0, 0, 1], 7476))));
        assert_eq!(client_id(&init).len(), 32);
        assert_eq!(value_of(&run), 3);
        daemon.stop().exit(0);
    }

    #[test]
    fn a_plaintext_probe_is_never_answered_is_logged_at_debug_and_disturbs_no_tls_client() {
        let sandbox = Sandbox::new();
        let config = format!("log_level = \"debug\"\n{}", tcp_section("127.0.0.1:0"));
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.tcp_address();

        let mut plain = TcpStream::connect(addr).unwrap();
        plain
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let probe_addr = plain.local_addr().unwrap();
        frames::send(&mut plain, &frames::init(1));

        // No Hexput frame ever comes back; the socket is closed. At most a TLS alert record
        // (content type 0x15) is sent.
        let mut bytes = Vec::new();
        match plain.read_to_end(&mut bytes) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
            Err(error) => panic!("the socket was not closed: {error}"),
        }
        assert!(bytes.is_empty() || bytes[0] == 0x15, "{bytes:02x?}");

        let log = daemon.log_with("TLS handshake failed");
        let line = log
            .lines()
            .find(|line| line.contains("TLS handshake failed"))
            .unwrap();
        assert!(line.contains(" DEBUG "), "{line}");
        assert!(
            line.contains(&probe_addr.to_string()),
            "the peer address is logged: {line}"
        );

        // The Daemon is unaffected: a TLS client still completes Init and a run.
        let (init, run) = init_and_run(&mut tls(addr));
        assert_eq!(init.message_type, MessageType::Result);
        assert_eq!(value_of(&run), 3);
        daemon.stop().exit(0);
    }

    #[test]
    fn a_silent_tcp_peer_stalls_no_accept_on_either_transport() {
        // The handshake runs in the connection's task, never in the accept loop: while one peer
        // sits connected and silent for the whole 10 s handshake timeout, TLS and UDS clients
        // are served at once.
        let sandbox = Sandbox::new();
        let config = format!("{}{}", sandbox.valid(), tcp_section("127.0.0.1:0"));
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.tcp_address();

        let _silent = TcpStream::connect(addr).unwrap();
        let started = Instant::now();
        let (_, tls_run) = init_and_run(&mut tls(addr));
        let (_, uds_run) = init_and_run(&mut daemon.uds(&sandbox.socket()));
        assert_eq!(value_of(&tls_run), 3);
        assert_eq!(value_of(&uds_run), 3);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "served while the silent peer's handshake is pending: {:?}",
            started.elapsed()
        );
        daemon.stop().exit(0);
    }

    #[test]
    fn a_tls_client_that_stops_sending_still_gets_its_replies() {
        // As on a Unix Domain Socket, a Backend that sends its request and then ends its side
        // — with `close_notify` or with a bare TCP FIN — is a clean close: the execution in
        // flight still finishes and its reply is written.
        let slow = "let i = 0; while (i < 50000) { i = i + 1; }; return i;";
        let sandbox = Sandbox::new();
        let daemon = Daemon::start(&sandbox, &tcp_section("127.0.0.1:0"));
        let addr = daemon.tcp_address();

        for close_notify in [true, false] {
            let mut stream = tls(addr);
            frames::send(&mut stream, &frames::init(1));
            assert_eq!(
                frames::reply(&mut stream)
                    .expect("an Init reply")
                    .message_type,
                MessageType::Result
            );
            frames::send(&mut stream, &frames::request(2, slow));
            stream.flush().unwrap();
            if close_notify {
                stream.conn.send_close_notify();
                stream.flush().unwrap();
            }
            stream.sock.shutdown(std::net::Shutdown::Write).unwrap();

            let reply = frames::reply(&mut stream).expect("the reply is still written");
            assert_eq!(
                reply.id,
                Some(CorrelationId(2)),
                "close_notify: {close_notify}"
            );
            assert_eq!(value_of(&reply), 50_000, "close_notify: {close_notify}");
        }
        daemon.stop().exit(0);
    }

    #[test]
    fn shutdown_stops_every_listener_and_removes_the_socket() {
        let sandbox = Sandbox::new();
        let config = format!("{}{}", sandbox.valid(), tcp_section("127.0.0.1:0"));
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.tcp_address();
        let _uds = daemon.uds(&sandbox.socket());
        let (_, run) = init_and_run(&mut tls(addr));
        assert_eq!(value_of(&run), 3);

        daemon.stop().exit(0);
        assert!(
            TcpStream::connect(addr).is_err(),
            "no longer listening on TCP"
        );
        assert!(!sandbox.socket().exists(), "the socket is removed");
    }

    #[test]
    fn every_tls_startup_failure_exits_one_naming_the_file_and_leaves_nothing_behind() {
        let sandbox = Sandbox::new();
        let (cert, key) = (fixture("server.pem"), fixture("server.key"));
        let key_text = std::fs::read_to_string(&key).unwrap();
        let cert_text = std::fs::read_to_string(&cert).unwrap();
        let no_certificate = sandbox.file("no-certificate.pem", &key_text);
        let no_key = sandbox.file("no-key.key", &cert_text);
        let malformed = sandbox.file(
            "malformed.pem",
            "-----BEGIN CERTIFICATE-----\n!!! not base64 !!!\n-----END CERTIFICATE-----\n",
        );
        let absent_cert = sandbox.dir.join("absent.pem");
        let absent_key = sandbox.dir.join("absent.key");
        let other_key = fixture("other.key");

        // (certificate, key, the file the error must name)
        let cases = [
            (absent_cert.clone(), key.clone(), absent_cert.clone()),
            (cert.clone(), absent_key.clone(), absent_key.clone()),
            (sandbox.dir.clone(), key.clone(), sandbox.dir.clone()),
            (no_certificate.clone(), key.clone(), no_certificate.clone()),
            (cert.clone(), no_key.clone(), no_key.clone()),
            (malformed.clone(), key.clone(), malformed.clone()),
            (cert.clone(), other_key.clone(), other_key.clone()),
        ];
        for (cert_path, key_path, named) in cases {
            let config = sandbox.file(
                "tls.toml",
                &format!(
                    "{}{}",
                    sandbox.valid(),
                    tcp_section_with("127.0.0.1:0", &cert_path, &key_path)
                ),
            );
            let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
            run.exit(1);
            assert!(
                run.stderr
                    .starts_with(&format!("error: {}: ", named.display())),
                "{}: {}",
                named.display(),
                run.stderr
            );
            assert_eq!(run.stderr.lines().count(), 1, "one line: {}", run.stderr);
            assert!(!run.log.contains("listening on TCP"), "{}", run.log);
            assert!(
                !sandbox.socket().exists(),
                "nothing is bound: {}",
                named.display()
            );
        }
    }

    #[test]
    fn a_tcp_address_in_use_exits_one_and_removes_the_socket_bound_before_it() {
        let sandbox = Sandbox::new();
        let holder = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = holder.local_addr().unwrap();
        let config = sandbox.file(
            "config.toml",
            &format!("{}{}", sandbox.valid(), tcp_section(&addr.to_string())),
        );
        let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
        run.exit(1);
        assert!(
            run.stderr
                .starts_with(&format!("error: {addr}: cannot listen for TCP: ")),
            "{}",
            run.stderr
        );
        assert!(!sandbox.socket().exists(), "the UDS socket is removed");
        drop(holder);
    }

    /// Init, then `return 1 + 2;`, over a WebSocket client.
    fn wss_init_and_run(client: &mut Wss) -> (Envelope<Value>, Envelope<Value>) {
        client.send(&frames::init(1));
        let init = client.reply().expect("an Init reply");
        client.send(&frames::request(2, "return 1 + 2;"));
        let run = client.reply().expect("an execution reply");
        (init, run)
    }

    #[test]
    fn a_wss_client_gets_the_same_replies_as_a_uds_client() {
        let sandbox = Sandbox::new();
        let config = format!("{}{}", sandbox.valid(), websocket_section("127.0.0.1:0"));
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.bound_address("WebSocket");
        assert_ne!(addr.port(), 0, "the actual bound address is logged");

        let (uds_init, uds_run) = init_and_run(&mut daemon.uds(&sandbox.socket()));
        let (wss_init, wss_run) = wss_init_and_run(&mut Wss::connect(addr));

        assert_eq!(value_of(&uds_run), 3);
        assert_eq!(value_of(&wss_run), 3);
        assert_eq!(wss_run.id, Some(CorrelationId(2)));
        assert_eq!(wss_run.id, uds_run.id);
        assert_eq!(wss_run.message_type, uds_run.message_type);
        assert_eq!(wss_run.payload, uds_run.payload, "identical to UDS");
        assert_eq!(wss_init.message_type, uds_init.message_type);
        assert_eq!(client_id(&wss_init).len(), 32);
        assert_ne!(
            client_id(&wss_init),
            client_id(&uds_init),
            "one Session each"
        );
        daemon.stop().exit(0);
    }

    #[test]
    fn uds_tcp_and_websocket_are_served_by_one_daemon_each_answering() {
        let sandbox = Sandbox::new();
        let config = format!(
            "{}{}{}",
            sandbox.valid(),
            tcp_section("127.0.0.1:0"),
            websocket_section("127.0.0.1:0")
        );
        let daemon = Daemon::start(&sandbox, &config);
        let tcp = daemon.tcp_address();
        let websocket = daemon.bound_address("WebSocket");
        assert_ne!(tcp, websocket, "two listeners, two ports");

        let (uds_init, uds_run) = init_and_run(&mut daemon.uds(&sandbox.socket()));
        let (tcp_init, tcp_run) = init_and_run(&mut tls(tcp));
        let (wss_init, wss_run) = wss_init_and_run(&mut Wss::connect(websocket));

        for run in [&uds_run, &tcp_run, &wss_run] {
            assert_eq!(value_of(run), 3);
            assert_eq!(
                run.payload, uds_run.payload,
                "the same reply on every transport"
            );
        }
        let ids = [&uds_init, &tcp_init, &wss_init].map(client_id);
        assert!(
            ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
            "{ids:?}"
        );
        daemon.stop().exit(0);
    }

    #[test]
    fn a_bare_ip_websocket_bind_listens_on_the_default_port_7478() {
        // The one test that needs the fixed WebSocket port: skipped, not failed, when something
        // else on the machine already holds it.
        if let Err(error) = TcpListener::bind(("127.0.0.1", 7478)) {
            eprintln!("skipped: 127.0.0.1:7478 is unavailable: {error}");
            return;
        }
        let sandbox = Sandbox::new();
        let daemon = Daemon::start(&sandbox, &websocket_section("127.0.0.1"));
        let addr = daemon.bound_address("WebSocket");
        assert_eq!(addr.port(), 7478, "{addr}");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert!(
            daemon
                .log_with("listening on WebSocket with TLS")
                .lines()
                .any(|line| line.contains("address=127.0.0.1:7478")),
            "the log names the default port"
        );

        let (init, run) =
            wss_init_and_run(&mut Wss::connect(SocketAddr::from(([127, 0, 0, 1], 7478))));
        assert_eq!(client_id(&init).len(), 32);
        assert_eq!(value_of(&run), 3);
        daemon.stop().exit(0);
    }

    #[test]
    fn a_plaintext_websocket_probe_is_never_upgraded_is_logged_at_debug_and_disturbs_no_wss_client()
    {
        let sandbox = Sandbox::new();
        let config = format!(
            "log_level = \"debug\"\n{}",
            websocket_section("127.0.0.1:0")
        );
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.bound_address("WebSocket");

        // A cleartext HTTP upgrade request, exactly what a browser or proxy would send.
        let mut plain = TcpStream::connect(addr).unwrap();
        plain
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let probe_addr = plain.local_addr().unwrap();
        plain
            .write_all(
                b"GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
                  Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();

        // No upgrade response and no frame ever come back; the socket is closed. At most a TLS
        // alert record (content type 0x15) is sent.
        let mut bytes = Vec::new();
        match plain.read_to_end(&mut bytes) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
            Err(error) => panic!("the socket was not closed: {error}"),
        }
        assert!(
            !String::from_utf8_lossy(&bytes).contains("101"),
            "{bytes:02x?}"
        );
        assert!(bytes.is_empty() || bytes[0] == 0x15, "{bytes:02x?}");

        let log = daemon.log_with("WebSocket handshake failed");
        let line = log
            .lines()
            .find(|line| line.contains("WebSocket handshake failed"))
            .unwrap();
        assert!(line.contains(" DEBUG "), "{line}");
        assert!(
            line.contains(&probe_addr.to_string()),
            "the peer address is logged: {line}"
        );

        // The Daemon is unaffected: a WSS client still completes Init and a run.
        let (init, run) = wss_init_and_run(&mut Wss::connect(addr));
        assert_eq!(init.message_type, MessageType::Result);
        assert_eq!(value_of(&run), 3);
        daemon.stop().exit(0);
    }

    #[test]
    fn a_text_message_is_answered_with_a_malformed_frame_error_and_reading_continues() {
        let sandbox = Sandbox::new();
        let daemon = Daemon::start(&sandbox, &websocket_section("127.0.0.1:0"));
        let mut client = Wss::connect(daemon.bound_address("WebSocket"));

        client.send_text("hello");
        let refused = client.reply().expect("an Error reply");
        assert_eq!(refused.id, None, "a text message names no request");
        assert_eq!(super::error_code_of(&refused), "protocol.malformed_frame");

        // The connection is still read: a binary request after it is answered normally.
        let (init, run) = wss_init_and_run(&mut client);
        assert_eq!(init.message_type, MessageType::Result);
        assert_eq!(value_of(&run), 3);
        daemon.stop().exit(0);
    }

    #[test]
    fn an_intermediary_drop_detaches_the_connection_and_the_daemon_keeps_serving() {
        let sandbox = Sandbox::new();
        let config = format!(
            "log_level = \"debug\"\n{}{}",
            sandbox.valid(),
            websocket_section("127.0.0.1:0")
        );
        let daemon = Daemon::start(&sandbox, &config);
        let addr = daemon.bound_address("WebSocket");

        // Init, then the TCP stream vanishes with no Close frame and no `close_notify`.
        let mut dropped = Wss::connect(addr);
        dropped.send(&frames::init(1));
        assert_eq!(
            dropped.reply().expect("an Init reply").message_type,
            MessageType::Result
        );
        dropped.vanish();
        daemon.log_with("connection detached from its Session");

        // A new WSS client and a UDS client are still served.
        let (_, wss_run) = wss_init_and_run(&mut Wss::connect(addr));
        let (_, uds_run) = init_and_run(&mut daemon.uds(&sandbox.socket()));
        assert_eq!(value_of(&wss_run), 3);
        assert_eq!(value_of(&uds_run), 3);
        daemon.stop().exit(0);
    }

    #[test]
    fn shutdown_stops_the_websocket_listener_too() {
        let sandbox = Sandbox::new();
        let daemon = Daemon::start(&sandbox, &websocket_section("127.0.0.1:0"));
        let addr = daemon.bound_address("WebSocket");
        let (_, run) = wss_init_and_run(&mut Wss::connect(addr));
        assert_eq!(value_of(&run), 3);

        daemon.stop().exit(0);
        assert!(
            TcpStream::connect(addr).is_err(),
            "no longer listening on WebSocket"
        );
    }

    #[test]
    fn every_websocket_startup_failure_exits_one_naming_the_file_and_leaves_nothing_behind() {
        let sandbox = Sandbox::new();
        let (cert, key) = (fixture("server.pem"), fixture("server.key"));
        let key_text = std::fs::read_to_string(&key).unwrap();
        let cert_text = std::fs::read_to_string(&cert).unwrap();
        let no_certificate = sandbox.file("ws-no-certificate.pem", &key_text);
        let no_key = sandbox.file("ws-no-key.key", &cert_text);
        let malformed = sandbox.file(
            "ws-malformed.pem",
            "-----BEGIN CERTIFICATE-----\n!!! not base64 !!!\n-----END CERTIFICATE-----\n",
        );
        let absent_cert = sandbox.dir.join("ws-absent.pem");
        let absent_key = sandbox.dir.join("ws-absent.key");
        let other_key = fixture("other.key");

        // (certificate, key, the file the error must name)
        let cases = [
            (absent_cert.clone(), key.clone(), absent_cert.clone()),
            (cert.clone(), absent_key.clone(), absent_key.clone()),
            (sandbox.dir.clone(), key.clone(), sandbox.dir.clone()),
            (no_certificate.clone(), key.clone(), no_certificate.clone()),
            (cert.clone(), no_key.clone(), no_key.clone()),
            (malformed.clone(), key.clone(), malformed.clone()),
            (cert.clone(), other_key.clone(), other_key.clone()),
        ];
        for (cert_path, key_path, named) in cases {
            let config = sandbox.file(
                "ws-tls.toml",
                &format!(
                    "{}{}",
                    sandbox.valid(),
                    websocket_section_with("127.0.0.1:0", &cert_path, &key_path)
                ),
            );
            let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
            run.exit(1);
            assert!(
                run.stderr
                    .starts_with(&format!("error: {}: ", named.display())),
                "{}: {}",
                named.display(),
                run.stderr
            );
            assert_eq!(run.stderr.lines().count(), 1, "one line: {}", run.stderr);
            assert!(!run.log.contains("listening on"), "{}", run.log);
            assert!(
                !sandbox.socket().exists(),
                "nothing is bound: {}",
                named.display()
            );
        }
    }

    #[test]
    fn a_websocket_section_without_its_tls_pair_exits_one_naming_the_field() {
        let sandbox = Sandbox::new();
        for text in [
            "[transport.websocket]\nbind = \"127.0.0.1:0\"\n",
            "[transport.websocket]\nbind = \"127.0.0.1:0\"\ntls_cert = \"c\"\n",
            "[transport.websocket]\nbind = \"127.0.0.1:0\"\ntls_key = \"k\"\n",
        ] {
            let config = sandbox.file("ws-no-tls.toml", &format!("{}{text}", sandbox.valid()));
            let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
            run.exit(1);
            assert!(
                run.stderr
                    .starts_with(&format!("error: {}:", config.display())),
                "{}",
                run.stderr
            );
            assert!(run.stderr.contains("missing field `tls_"), "{}", run.stderr);
            assert!(!run.log.contains("listening on"), "{}", run.log);
            assert!(!sandbox.socket().exists(), "nothing is bound: {text}");
        }
    }

    #[test]
    fn a_websocket_address_in_use_exits_one_and_unbinds_the_listeners_before_it() {
        let sandbox = Sandbox::new();
        let holder = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = holder.local_addr().unwrap();
        // A free TCP port, bound by the Daemon before WebSocket is tried.
        let tcp = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let config = sandbox.file(
            "config.toml",
            &format!(
                "{}{}{}",
                sandbox.valid(),
                tcp_section(&tcp.to_string()),
                websocket_section(&taken.to_string())
            ),
        );
        let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
        run.exit(1);
        assert!(
            run.stderr
                .starts_with(&format!("error: {taken}: cannot listen for WebSocket: ")),
            "{}",
            run.stderr
        );
        assert!(!sandbox.socket().exists(), "the UDS socket is removed");
        assert!(
            TcpStream::connect(tcp).is_err(),
            "the TCP listener is unbound"
        );
        drop(holder);
    }
}

// --- Story 5.3: the Named Pipe on Windows, and the platform rule everywhere ---

/// The `Init` every Named Pipe test sends: no Config and no registrations.
fn init_envelope(id: u64) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::Init,
        Value::Map(vec![
            (Value::from("config"), Value::Map(vec![])),
            (Value::from("registrations"), Value::Array(vec![])),
        ]),
    )
}

/// An `ExecutionStart` for `source`, with no variables.
fn execution_envelope(id: u64, source: &str) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::ExecutionStart,
        Value::Map(vec![
            (Value::from("source"), Value::from(source)),
            (Value::from("variables"), Value::Map(vec![])),
        ]),
    )
}

/// The `Result {value: n}` a Script answering `n` is owed.
fn value_reply(id: u64, n: u64) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::Result,
        Value::Map(vec![(Value::from("value"), Value::from(n))]),
    )
}

/// A `[transport.named_pipe]` section for `pipe`, a TOML literal string.
fn named_pipe_section(pipe: &str) -> String {
    format!("[transport.named_pipe]\npath = '{pipe}'\n")
}

/// Story 5.3: the Named Pipe is Windows-only. On any other host a configured pipe is skipped with
/// a warning, logged right after the System Config loads, and UDS serves the local role.
#[cfg(unix)]
#[test]
fn a_pipe_on_a_non_windows_host_is_warned_about_and_uds_still_serves() {
    let sandbox = Sandbox::new();
    let pipe = r"\\.\pipe\hexput-test-unix-only";
    let config = format!("{}{}", sandbox.valid(), named_pipe_section(pipe));
    let serving = Serving::start(&sandbox, &config);
    let mut client = serving.connect();
    wire::send(&mut client, &init_envelope(1));
    let reply = wire::reply(&mut client).expect("a reply");
    assert_eq!(reply.message_type, hexput_port::MessageType::Result);
    wire::send(&mut client, &execution_envelope(2, "return 1 + 2;"));
    assert_eq!(wire::reply(&mut client), Some(value_reply(2, 3)));

    let run = serving.stop();
    run.exit(0);
    let loaded = run
        .log
        .find("System Config loaded")
        .expect("the config loads");
    let warning = run
        .log
        .find("[transport.named_pipe] is Windows-only; not serving it on this platform")
        .unwrap_or_else(|| panic!("no warning:\n{}", run.log));
    let listening = run
        .log
        .find("listening on a Unix Domain Socket")
        .expect("UDS is bound");
    assert!(loaded < warning && warning < listening, "{}", run.log);
    let line = run
        .log
        .lines()
        .find(|line| line.contains("is Windows-only"))
        .expect("the warning line");
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains(&format!("path={pipe}")), "{line}");
}

/// Story 5.3: a configuration whose only transport is the Named Pipe has nothing servable on a
/// non-Windows host. It exits 1 before anything is bound, naming the Windows-only section.
#[cfg(unix)]
#[test]
fn a_pipe_only_config_on_a_non_windows_host_exits_one_and_binds_nothing() {
    let sandbox = Sandbox::new();
    let config = sandbox.file(
        "config.toml",
        &named_pipe_section(r"\\.\pipe\hexput-test-unix-only"),
    );
    let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
    run.exit(1);
    assert_eq!(
        run.stderr,
        format!(
            "error: {}: no configured transport is served on this platform ([transport.named_pipe] is Windows-only)\n",
            config.display()
        )
    );
    assert!(!run.log.contains("daemon started"), "{}", run.log);
    assert!(!run.log.contains("listening on"), "{}", run.log);
}

/// Story 5.3: the Named Pipe, served by a real Daemon over a real pipe. Each test has its own
/// pipe name; the blocking client retries `ERROR_PIPE_BUSY` and the not-yet-listening instant.
#[cfg(windows)]
mod named_pipe_serving {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io::{ErrorKind, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use hexput_daemon::run_with;
    use hexput_port::{CorrelationId, Envelope, MessageType, Value, decode, encode, encode_frame};
    use hexput_transport::named_pipe::NamedPipeListener;

    use super::{
        Run, Sandbox, SharedLog, daemon, execution_envelope, init_envelope, named_pipe_section,
        nothing, value_reply,
    };

    /// `ERROR_PIPE_BUSY`: every instance is connected, or the next one is not yet created.
    const ERROR_PIPE_BUSY: i32 = 231;

    /// How long a client or a log line may take before the test gives up.
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// A pipe name no other test, or process, uses.
    pub(super) fn pipe_name() -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        format!(
            r"\\.\pipe\hexput-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// One attempt at a blocking client on `pipe`, read and write.
    pub(super) fn open_once(pipe: &str) -> std::io::Result<File> {
        OpenOptions::new().read(true).write(true).open(pipe)
    }

    /// Not listening yet, or every instance busy: both are for the client to retry.
    pub(super) fn retryable(error: &std::io::Error) -> bool {
        error.kind() == ErrorKind::NotFound || error.raw_os_error() == Some(ERROR_PIPE_BUSY)
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("tls")
            .join(name)
    }

    /// A Daemon serving `config_text` on its own thread until [`Serving::stop`], with its log
    /// readable while it runs.
    struct Serving {
        stop: tokio::sync::oneshot::Sender<()>,
        thread: std::thread::JoinHandle<Run>,
        log: SharedLog,
        pipe: String,
    }

    impl Serving {
        fn start(sandbox: &Sandbox, pipe: &str, config_text: &str) -> Self {
            let config = sandbox.file("config.toml", config_text);
            let discovery = nothing(sandbox);
            let log = SharedLog::default();
            let thread_log = log.clone();
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let thread = std::thread::spawn(move || {
                let argv: Vec<OsString> =
                    vec!["hexput-daemon".into(), "--config".into(), config.into()];
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let code = run_with(
                    argv,
                    &discovery,
                    &mut out,
                    &mut err,
                    thread_log.clone(),
                    async {
                        let _ = stopped.await;
                    },
                );
                Run {
                    code: format!("{code:?}"),
                    stdout: String::from_utf8(out).unwrap(),
                    stderr: String::from_utf8(err).unwrap(),
                    log: thread_log.text(),
                }
            });
            Self {
                stop,
                thread,
                log,
                pipe: pipe.to_owned(),
            }
        }

        /// A client on the pipe, waiting until the Daemon is listening on it.
        fn connect(&self) -> File {
            let started = Instant::now();
            loop {
                match open_once(&self.pipe) {
                    Ok(pipe) => return pipe,
                    Err(error)
                        if retryable(&error)
                            && started.elapsed() < TIMEOUT
                            && !self.thread.is_finished() =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("the Daemon never listened on {}: {error}", self.pipe),
                }
            }
        }

        /// Waits until the Daemon's log contains `text`.
        fn wait_for(&self, text: &str) {
            let started = Instant::now();
            while !self.log.text().contains(text) {
                assert!(
                    started.elapsed() < TIMEOUT,
                    "{text:?} was never logged:\n{}",
                    self.log.text()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn stop(self) -> Run {
            let _ = self.stop.send(());
            self.thread.join().expect("the Daemon thread")
        }
    }

    fn send(pipe: &mut File, envelope: &Envelope<Value>) {
        pipe.write_all(&encode_frame(&encode(envelope).unwrap()).unwrap())
            .unwrap();
    }

    /// The next frame, prefix included, or `None` once the Daemon has closed the pipe.
    fn frame(pipe: &mut File) -> Option<Vec<u8>> {
        let mut prefix = [0; 4];
        match pipe.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe
                ) =>
            {
                return None;
            }
            Err(error) => panic!("no reply: {error}"),
        }
        let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
        pipe.read_exact(&mut body).unwrap();
        let mut bytes = prefix.to_vec();
        bytes.extend(body);
        Some(bytes)
    }

    /// The next reply, or `None` once the Daemon has closed the pipe.
    fn reply(pipe: &mut File) -> Option<Envelope<Value>> {
        frame(pipe).map(|bytes| decode(&bytes[4..]).unwrap())
    }

    /// The `(id, code)` of the next reply, which must be an `Error`.
    fn refusal(pipe: &mut File) -> (Option<u64>, String) {
        let reply = reply(pipe).expect("a reply");
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

    /// Sends `Init` as `id` and returns the Client ID it is answered with.
    fn init(pipe: &mut File, id: u64) -> String {
        send(pipe, &init_envelope(id));
        let reply = reply(pipe).expect("a reply");
        assert_eq!(reply.id, Some(CorrelationId(id)));
        assert_eq!(
            reply.message_type,
            MessageType::Result,
            "{:?}",
            reply.payload
        );
        let fields = reply.payload.as_map().expect("a map payload");
        assert_eq!(
            fields.len(),
            2,
            "`client_id` then `reconnect_secret`: {fields:?}"
        );
        assert_eq!(fields[0].0.as_str(), Some("client_id"));
        assert_eq!(fields[1].0.as_str(), Some("reconnect_secret"));
        let client_id = fields[0].1.as_str().expect("a string Client ID").to_owned();
        assert_eq!(client_id.len(), 32);
        client_id
    }

    #[test]
    fn the_pipe_round_trip_answers_like_the_wire_and_logs_its_path() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        let serving = Serving::start(&sandbox, &pipe, &named_pipe_section(&pipe));
        let mut client = serving.connect();
        init(&mut client, 1);
        send(&mut client, &execution_envelope(2, "return 1 + 2;"));

        // The frame on the pipe is exactly the shared framing of the reply, byte for byte.
        let expected = value_reply(2, 3);
        let bytes = frame(&mut client).expect("a reply");
        assert_eq!(bytes, encode_frame(&encode(&expected).unwrap()).unwrap());

        let run = serving.stop();
        run.exit(0);
        assert!(
            run.log
                .contains(&format!("listening on a Named Pipe path={pipe}")),
            "{}",
            run.log
        );
        assert!(run.log.contains("daemon stopping"), "{}", run.log);
    }

    #[test]
    fn shutdown_ends_the_daemon_and_the_pipe_is_gone() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        let serving = Serving::start(&sandbox, &pipe, &named_pipe_section(&pipe));
        drop(serving.connect());
        let run = serving.stop();
        run.exit(0);
        let error = open_once(&pipe).expect_err("the pipe is gone after shutdown");
        assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
    }

    #[test]
    fn three_clients_at_once_are_each_answered_on_their_own_instance() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        let serving = Serving::start(&sandbox, &pipe, &named_pipe_section(&pipe));
        let mut clients: Vec<File> = (0..3).map(|_| serving.connect()).collect();

        let ids: Vec<String> = clients
            .iter_mut()
            .enumerate()
            .map(|(n, client)| init(client, 10 + n as u64))
            .collect();
        let distinct: HashSet<&String> = ids.iter().collect();
        assert_eq!(distinct.len(), 3, "each connection has its own Client ID");

        for (n, client) in clients.iter_mut().enumerate() {
            let source = format!("return {};", 100 + n);
            send(client, &execution_envelope(20 + n as u64, &source));
        }
        for (n, client) in clients.iter_mut().enumerate() {
            assert_eq!(
                reply(client),
                Some(value_reply(20 + n as u64, 100 + n as u64)),
                "client {n}"
            );
        }
        serving.stop().exit(0);
    }

    #[test]
    fn a_pipe_already_served_exits_one_naming_it_and_listens_on_nothing() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        // Another listener holds the name first. Its instance needs a runtime to exist.
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let holder = runtime.block_on(async { NamedPipeListener::bind(&pipe) });
        let _holder = match holder {
            Ok(listener) => listener,
            Err(error) => panic!("the name is free: {error}"),
        };

        let config = sandbox.file("config.toml", &named_pipe_section(&pipe));
        let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
        run.exit(1);
        assert_eq!(
            run.stderr,
            format!("error: {pipe}: pipe already in use: another process is serving it\n")
        );
        assert!(!run.log.contains("daemon started"), "{}", run.log);
        assert!(
            !run.log.contains("listening on a Named Pipe"),
            "{}",
            run.log
        );
    }

    #[test]
    fn a_client_leaving_abruptly_is_detached_and_a_new_client_is_still_served() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        // The detach is logged at `debug`, so this test's Daemon logs at that level.
        let config = format!("log_level = \"debug\"\n{}", named_pipe_section(&pipe));
        let serving = Serving::start(&sandbox, &pipe, &config);

        let mut first = serving.connect();
        init(&mut first, 1);
        drop(first);
        serving.wait_for("connection detached from its Session");

        let mut second = serving.connect();
        init(&mut second, 2);
        let run = serving.stop();
        run.exit(0);
        assert!(
            run.log.contains("connection detached from its Session"),
            "{}",
            run.log
        );
    }

    #[test]
    fn a_malformed_frame_is_answered_and_the_connection_keeps_serving() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        let serving = Serving::start(&sandbox, &pipe, &named_pipe_section(&pipe));
        let mut client = serving.connect();
        // 0xc1 is never valid MessagePack, so this frame is malformed.
        client.write_all(&encode_frame(&[0xc1]).unwrap()).unwrap();
        assert_eq!(
            refusal(&mut client),
            (None, "protocol.malformed_frame".to_owned())
        );
        // The connection is still served: the next frame is answered as usual.
        send(&mut client, &init_envelope(6));
        let reply = reply(&mut client).expect("a reply");
        assert_eq!(reply.id, Some(CorrelationId(6)));
        assert_eq!(reply.message_type, MessageType::Result);
        serving.stop().exit(0);
    }

    #[test]
    fn a_pipe_and_tcp_are_both_served_by_one_daemon() {
        let sandbox = Sandbox::new();
        let pipe = pipe_name();
        // A free TCP port, which the Daemon binds with its TLS identity.
        let tcp = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let config = format!(
            "{}[transport.tcp]\nbind = '{tcp}'\ntls_cert = '{}'\ntls_key = '{}'\n",
            named_pipe_section(&pipe),
            fixture("server.pem").display(),
            fixture("server.key").display(),
        );
        let serving = Serving::start(&sandbox, &pipe, &config);

        let mut client = serving.connect();
        assert_eq!(init(&mut client, 1).len(), 32);

        // TCP accepts a connection too. The TLS handshake waits for a ClientHello this test
        // never sends, so only the accept is checked.
        let started = Instant::now();
        loop {
            match TcpStream::connect(tcp) {
                Ok(_) => break,
                Err(error) => {
                    assert!(started.elapsed() < TIMEOUT, "TCP never listened: {error}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        serving.stop().exit(0);
    }

    #[test]
    fn uds_only_on_windows_exits_one_naming_the_unix_only_section() {
        let sandbox = Sandbox::new();
        let config = sandbox.file("config.toml", &sandbox.valid());
        let run = daemon(&["--config", config.to_str().unwrap()], &nothing(&sandbox));
        run.exit(1);
        assert_eq!(
            run.stderr,
            format!(
                "error: {}: no configured transport is served on this platform ([transport.uds] is Unix-only)\n",
                config.display()
            )
        );
        assert!(
            run.log
                .contains("[transport.uds] is Unix-only; not serving it on this platform"),
            "{}",
            run.log
        );
        assert!(!sandbox.socket().exists(), "nothing is bound");
    }
}

// --- Story 5.8: a Session outlives its last Connection by session_ttl_secs ---

/// An `Init` envelope's reply payload carries the Client ID and the reconnect secret, in order.
#[cfg(unix)]
fn credentials_of(reply: &hexput_port::Envelope<hexput_port::Value>) -> (String, String) {
    use hexput_port::MessageType;

    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "{:?}",
        reply.payload
    );
    let fields = reply.payload.as_map().expect("a map payload");
    (
        fields[0].1.as_str().expect("a Client ID").to_owned(),
        fields[1].1.as_str().expect("a reconnect secret").to_owned(),
    )
}

/// A `Reconnect` request as `id` carrying `client_id` and `secret`.
#[cfg(unix)]
fn reconnect_request(
    id: u64,
    client_id: &str,
    secret: &str,
) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::Reconnect,
        Value::Map(vec![
            (Value::from("client_id"), Value::from(client_id)),
            (Value::from("reconnect_secret"), Value::from(secret)),
        ]),
    )
}

/// Story 5.8, over the socket: with `session_ttl_secs = 2` a Session whose last Connection left
/// stays resumable for two seconds. A reconnect inside them resumes it with its cached Script
/// intact, and the first countdown never tears it down while that Connection is attached. Once the Session expires, a reconnect is `protocol.reconnect_denied` exactly like an
/// unknown Client ID, and an `Init` on that same connection creates a fresh Session.
#[cfg(unix)]
#[test]
fn a_session_expires_one_ttl_after_its_last_connection_and_a_reconnect_before_that_resumes_it() {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    let sandbox = Sandbox::new();
    let config = format!(
        "log_level = \"debug\"\nsession_ttl_secs = 2\n{}",
        sandbox.valid()
    );
    let serving = Serving::start(&sandbox, &config);

    let mut first = serving.connect();
    wire::send(&mut first, &init_envelope(1));
    let (client_id, secret) = credentials_of(&wire::reply(&mut first).expect("an init reply"));

    let register = Envelope::new(
        CorrelationId(2),
        MessageType::CodeRegister,
        Value::Map(vec![(Value::from("source"), Value::from("return 42;"))]),
    );
    wire::send(&mut first, &register);
    let registered = wire::reply(&mut first).expect("a registration reply");
    let handle = registered
        .payload
        .as_map()
        .expect("a map payload")
        .iter()
        .find(|(key, _)| key.as_str() == Some("handle"))
        .and_then(|(_, value)| value.as_str())
        .expect("a handle")
        .to_owned();

    // The last Connection leaves: the countdown starts.
    drop(first);
    serving.log_with("the Session expires after its TTL unless a Connection reconnects");

    // Inside the TTL, a reconnect resumes the Session and its cached Script still runs.
    let mut second = serving.connect();
    wire::send(&mut second, &reconnect_request(3, &client_id, &secret));
    let resumed = wire::reply(&mut second).expect("a reconnect reply");
    assert_eq!(
        resumed.message_type,
        MessageType::Result,
        "{:?}",
        resumed.payload
    );
    wire::send(&mut second, &cached_run_request(4, &handle));
    let answer = wire::reply(&mut second).expect("an execution reply");
    assert_eq!(
        answer.message_type,
        MessageType::Result,
        "{:?}",
        answer.payload
    );
    // Past the first countdown's end, while `second` is attached: it did nothing.
    const EXPIRED: &str = "Session expired after its TTL; torn down";
    const STARTED: &str = "the Session expires after its TTL unless a Connection reconnects";
    std::thread::sleep(Duration::from_millis(2_500));
    assert!(
        !serving.log.text().contains(EXPIRED),
        "the first countdown tore down an attached Session"
    );
    wire::send(&mut second, &cached_run_request(8, &handle));
    let again = wire::reply(&mut second).expect("an execution reply");
    assert_eq!(
        again.message_type,
        MessageType::Result,
        "{:?}",
        again.payload
    );

    // The last Connection leaves again: a new countdown, which is the one that expires it.
    drop(second);
    let log = serving.log_with(EXPIRED);
    assert_eq!(log.matches(EXPIRED).count(), 1, "{log}");
    assert_eq!(log.matches(STARTED).count(), 2, "{log}");
    assert!(
        log.rfind(STARTED) < log.find(EXPIRED),
        "the expiry follows the second countdown's start: {log}"
    );

    let mut third = serving.connect();
    wire::send(&mut third, &reconnect_request(5, &client_id, &secret));
    let torn = wire::reply(&mut third).expect("a denial");
    wire::send(
        &mut third,
        &reconnect_request(6, "00000000000000000000000000000000", &secret),
    );
    let unknown = wire::reply(&mut third).expect("a denial");
    assert_eq!(torn.message_type, MessageType::Error);
    assert_eq!(
        torn.payload, unknown.payload,
        "identical to an unknown Client ID"
    );
    let code = torn
        .payload
        .as_map()
        .expect("an error payload is a map")
        .iter()
        .find(|(key, _)| key.as_str() == Some("code"))
        .and_then(|(_, value)| value.as_str())
        .expect("a code");
    assert_eq!(code, "protocol.reconnect_denied");

    // Not fatal: the same connection may `Init` a fresh Session.
    wire::send(&mut third, &init_envelope(7));
    let (fresh, _) = credentials_of(&wire::reply(&mut third).expect("an init reply"));
    assert_ne!(fresh, client_id);
    drop(third);

    let run = serving.stop();
    run.exit(0);
}

// --- Story 6.2: a Plugin registration over the real socket ---

/// Story 6.2: an `Init` with no Config and no registrations, as the Plugin socket tests send it.
#[cfg(unix)]
fn plugin_init(id: u64) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::Init,
        Value::Map(vec![
            (Value::from("config"), Value::Map(vec![])),
            (Value::from("registrations"), Value::Array(vec![])),
        ]),
    )
}

/// Story 6.2: a `PluginRegister` request for `source`, declaring no custom Events (Story 6.3:
/// `events` is required, so `{}` is the explicit "none").
#[cfg(unix)]
fn plugin_register_envelope(id: u64, source: &str) -> hexput_port::Envelope<hexput_port::Value> {
    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    Envelope::new(
        CorrelationId(id),
        MessageType::PluginRegister,
        Value::Map(vec![
            (Value::from("source"), Value::from(source)),
            (Value::from("events"), Value::Map(vec![])),
        ]),
    )
}

/// Story 6.2: a successful `PluginRegister` reply, which must be exactly `{name}`; returns the
/// name.
#[cfg(unix)]
fn plugin_name_reply(reply: &hexput_port::Envelope<hexput_port::Value>) -> String {
    use hexput_port::{MessageType, Value};

    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "{:?}",
        reply.payload
    );
    let Value::Map(fields) = &reply.payload else {
        panic!("a Result payload is a map");
    };
    assert_eq!(fields.len(), 1, "exactly `name`: {fields:?}");
    assert_eq!(fields[0].0.as_str(), Some("name"));
    fields[0].1.as_str().expect("a string name").to_owned()
}

#[cfg(unix)]
#[test]
fn a_plugin_registration_over_the_socket_is_answered_with_its_name_and_names_are_per_session() {
    use hexput_port::CorrelationId;

    let source = "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { return 1; }";
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());

    let mut first = serving.connect();
    wire::send(&mut first, &plugin_init(1));
    assert_eq!(
        wire::reply(&mut first).expect("a reply").message_type,
        hexput_port::MessageType::Result
    );
    wire::send(&mut first, &plugin_register_envelope(2, source));
    let reply = wire::reply(&mut first).expect("a reply");
    assert_eq!(reply.id, Some(CorrelationId(2)));
    assert_eq!(plugin_name_reply(&reply), "x");

    // The same name again on the same Session is refused, and the first is unchanged.
    wire::send(&mut first, &plugin_register_envelope(3, source));
    assert_eq!(
        wire::refusal(&mut first),
        (Some(3), "protocol.duplicate_plugin_name".to_owned())
    );

    // Another Session may register the same name.
    let mut second = serving.connect();
    wire::send(&mut second, &plugin_init(1));
    assert_eq!(
        wire::reply(&mut second).expect("a reply").message_type,
        hexput_port::MessageType::Result
    );
    wire::send(&mut second, &plugin_register_envelope(2, source));
    assert_eq!(
        plugin_name_reply(&wire::reply(&mut second).expect("a reply")),
        "x"
    );

    serving.stop().exit(0);
}

#[cfg(unix)]
#[test]
fn a_plugin_registration_before_init_over_the_socket_is_init_not_completed() {
    let sandbox = Sandbox::new();
    let serving = Serving::start(&sandbox, &sandbox.valid());
    let mut client = serving.connect();
    wire::send(
        &mut client,
        &plugin_register_envelope(41, "plugin { name = \"x\" }"),
    );
    assert_eq!(
        wire::refusal(&mut client),
        (Some(41), "protocol.init_not_completed".to_owned())
    );
    serving.stop().exit(0);
}
