//! Story 4.4: measure the cache win (FR-5, SM-1) — parse a Script once, execute it many times.
//! Story 4.5: compare that hot path against Rhai, and report the Resource Budget's cost as its
//! own line (SM-1, NFR3).
//! Story 5.9: benchmark the remote path (TCP+TLS) separately from the local one (UDS), so the
//! transport's latency is never conflated with the runtime's (NFR3).
//!
//! The bench starts a real Daemon in-process (`hexput_daemon::run_with` on its own thread and
//! its own Tokio runtime) serving a real Unix Domain Socket and a real TCP+TLS listener, and
//! drives it as a Backend would: each client is a blocking stream (a `UnixStream`, or a rustls
//! client stream over a `TcpStream`) that sends `Init` with blanket grants for the rule's three
//! host functions and answers every `Call` with `Result {value}`. Every execution runs under the
//! default Resource Budget and capability checks — nothing is overridden, raised or collapsed.
//!
//! The workload is the anchor case, [`workload::RULE`]: a weighted pass/fail rule over eight
//! grades, making 10 host calls per run (8 × `weight`, `passMark`, `record`). Runs rotate through
//! 16 deterministic students, so the variables are fresh on every run, and every reply is checked
//! against the verdict Rust computes with the same f64 operations in the same order, along with
//! its exact `Call` count. A mismatch panics, so a broken run never publishes a number.
//!
//! What each figure means:
//!
//! - `cache_win/cold_first_run` (Criterion): a Script never seen before — `CodeRegister` (the
//!   parse) plus the first `CachedExecutionStart` on its new handle, timed together. The handle
//!   is then unregistered, untimed, so the cache stays bounded.
//! - `cache_win/warm_cache_hit` (Criterion): `CachedExecutionStart` on one registered handle.
//!   The difference from `cold_first_run` is the cache's win.
//! - `cache_win/direct_execution` (Criterion): `ExecutionStart` with the source, parsed on every
//!   run — what every run would cost without the cache.
//! - `steady_state` (measured by hand, because Criterion reports no p99): [`CONCURRENCY`]
//!   connections each run a closed loop of Cached Executions. Each round reports p50 and p99
//!   (nearest-rank, from sending `CachedExecutionStart` to its final `Result`) and throughput
//!   (runs ÷ the round's wall time); the summary is the median of the rounds.
//!
//! - `rhai_compare/{hexput,rhai}_cold_first_run` and `rhai_compare/{hexput,rhai}_warm_cache_hit`
//!   (Criterion, Story 4.5): the same cold and warm figures on the Daemon and on a Rhai server
//!   inside the bench ([`rhai::RhaiServer`]) that speaks the same wire over a UDS — the same
//!   framing, `CodeRegister` (Rhai compiles once into an `AST`), `CachedExecutionStart` and a
//!   `Call` round trip per host call — driven by the same client, students and verification. The
//!   rule is [`rhai::RHAI_RULE`], the same steps with every number an f64. One wire difference
//!   remains: the Daemon sends each `Call` argument in a Value Secret holder (FR-28), which the
//!   client unwraps, while the Rhai server sends plain values — so Hexput's frames are larger.
//! - `steady_state` also runs the same closed loops against the Rhai server, right after
//!   Hexput's, and prints the two summaries side by side (p50, p99, throughput; Hexput, Rhai and
//!   the Hexput/Rhai ratio).
//! - `engine` (hand-timed, Story 4.5): the rule in-process on three drivers over the same 16
//!   students, host calls answered natively — `hexput_unmetered` (the interpreter alone),
//!   `hexput_enforced` (the same with the Executor's synchronous enforcement: CPU slicing and
//!   charges, the memory and allocation ceilings, the RPC-call/side-effect count, the output
//!   size) and `rhai_engine`. [`ENGINE_ROUNDS`] rounds of [`ENGINE_RUNS`] runs per driver; the
//!   median of the rounds' mean ns/run gives the line "budget enforcement overhead". The UDS
//!   headline figures above all run with the full default budget and are never adjusted by it.
//! - `remote_path/tcp_tls_cold_first_run` and `remote_path/tcp_tls_warm_cache_hit` (Criterion,
//!   Story 5.9): the `cache_win/cold_first_run` and `cache_win/warm_cache_hit` figures, defined
//!   exactly alike, over one TCP+TLS connection to the same Daemon instead of the UDS. The UDS
//!   counterparts are the `cache_win` figures themselves; they are not repeated in this group.
//! - `steady_state` also runs the same closed loops over TCP+TLS, between Hexput's UDS loops and
//!   Rhai's, prints its own rounds and summary, and then a transport table: p50, p99 and
//!   throughput for `UDS` and `TCP+TLS` side by side with the `TCP+TLS/UDS` ratio. Both legs' raw
//!   figures are always shown; no figure averages or merges the two, and the report keeps
//!   `steady_state` (UDS) and `tcp_tls_steady_state` as separate sections.
//!
//! **Topology:** every leg runs on one machine. The UDS leg is the Daemon in-process with its
//! clients on threads of the bench process. The TCP+TLS leg connects to `127.0.0.1` over the
//! loopback interface — there is no network hop, so its figures are the cost of the TCP and TLS
//! stacks, not network latency. The client trusts only the fixture CA, verifies the name
//! `localhost`, uses the `ring` provider with server authentication only, and the negotiated
//! protocol version and cipher suite are recorded in the conditions. A cross-machine figure is
//! not measured here; the per-leg topology in the conditions is where one would be recorded.
//! Every connection, TCP and TLS handshake included, is set up before its figure's clock starts,
//! so no TCP+TLS figure includes connection setup — a Backend that reconnects often pays that
//! on top. The legs run one after the other (UDS, then TCP+TLS, then Rhai) against one Daemon
//! that still holds the earlier legs' Sessions until their TTL, so the `TCP+TLS/UDS` ratio
//! carries any drift between the two runs as well as the transport's cost.
//!
//! The Criterion figures use one connection with one execution in flight. Rhai runs at
//! `Engine::new()`'s defaults, one thread per connection, as a Rhai user would embed it.
//!
//! **Stated variance:** p50 and throughput within ±10%, p99 within ±20%. The bench prints whether
//! this run's spread across rounds is within it, and — when the previous report was made on a
//! machine with the same fingerprint (CPU model, parallelism, OS, arch) — whether each figure is
//! within it of the previous run's. Criterion compares its own figures against its previous run
//! with a 10% noise threshold. All of this is informational: no figure is a gate.
//!
//! The machine and workload conditions are printed before any figure and, after a full run,
//! written with the three steady states (UDS, TCP+TLS, Rhai) and the engine figures (`null` when
//! that section was filtered out) to `<target>/hexput-bench/cache-win.json`. The comparison with
//! the previous run covers Hexput's UDS and TCP+TLS and Rhai's steady-state summaries.
//!
//! **Quick mode:** without `--bench` (that is, under `cargo test`), Criterion runs each figure
//! once, each steady state runs one round of 20 runs per connection with no warm-up, and the
//! engine section one round of 20 runs. Every result is still checked, the report module's own
//! checks run, and no report is written or compared.
//!
//! A positional filter goes to Criterion; `steady_state` (all three legs) and `engine` each run
//! only with no filter or a filter that is a substring of their name. The report is read, compared and
//! written only when `steady_state` ran (so `-- engine` writes none).

/// Connections in the steady-state section.
#[cfg(unix)]
const CONCURRENCY: usize = 8;
/// Timed rounds in the steady-state section.
#[cfg(unix)]
const ROUNDS: usize = 5;
/// Timed runs per connection per round.
#[cfg(unix)]
const RUNS_PER_CONNECTION: usize = 1000;
/// Untimed runs per connection before the first round.
#[cfg(unix)]
const WARM_UP_RUNS: usize = 100;
/// Quick mode's shortened steady state: one round of this many runs, no warm-up.
#[cfg(unix)]
const QUICK_RUNS_PER_CONNECTION: usize = 20;
/// Rounds in the `engine` section.
#[cfg(unix)]
const ENGINE_ROUNDS: usize = 5;
/// Runs of each driver per `engine` round.
#[cfg(unix)]
const ENGINE_RUNS: usize = 10_000;
/// Untimed runs of each driver before the first `engine` round.
#[cfg(unix)]
const ENGINE_WARM_UP_RUNS: usize = 1000;
/// Quick mode's shortened `engine` section: one round of this many runs, no warm-up.
#[cfg(unix)]
const QUICK_ENGINE_RUNS: usize = 20;

/// The Rhai comparator: the server over UDS, the in-process engine and the conversion. A crate
/// root's modules resolve beside it (`benches/`), so the path names `benches/cache/`, where Cargo
/// discovers no bench target (it has no `main.rs`).
#[cfg(unix)]
#[path = "cache/rhai.rs"]
mod rhai;

#[cfg(not(unix))]
fn main() {
    println!("the cache bench needs a Unix Domain Socket; skipped on this platform");
}

#[cfg(unix)]
fn main() {
    use std::time::{Duration, Instant};

    use criterion::Criterion;

    use crate::wire::Client;
    use crate::workload::Student;

    let args: Vec<String> = std::env::args().collect();
    let quick = report::is_quick(&args);
    let filter = report::filter(&args);

    if quick {
        report::self_check();
    }

    let path = report::report_path();
    let previous = if quick {
        None
    } else {
        std::fs::read_to_string(&path).ok()
    };
    let daemon = daemon::Daemon::start();
    let rhai_server = rhai::RhaiServer::start();
    // The conditions name the TLS parameters the TCP+TLS leg negotiates, still before any figure.
    let tls_parameters = daemon.tls_parameters();
    if quick {
        // The very string the conditions and `workload.legs` record — the bench's own, not a copy.
        assert!(
            tls_parameters.starts_with("TLSv1_3, TLS13_"),
            "unexpected TLS parameters: {tls_parameters}"
        );
    }
    let conditions = report::Conditions::gather(quick, tls_parameters);
    println!("{}", conditions.render());
    let students = workload::students();

    // A Script never seen before: `CodeRegister` plus its first run, timed together; the
    // unregister is untimed, so the cache stays bounded.
    let cold = |client: &mut Client, source: &str, iters: u64, next: &mut usize| {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            let student: &Student = &students[*next % students.len()];
            *next += 1;
            let started = Instant::now();
            let handle = workload::register_source(client, source);
            let registered = started.elapsed();
            let payload = workload::cached_payload(&handle, student);
            total += registered + workload::run_cached(client, payload, student);
            workload::unregister(client, &handle);
        }
        total
    };
    // A cache hit: `CachedExecutionStart` on one registered handle.
    let warm = |client: &mut Client, handle: &str, iters: u64, next: &mut usize| {
        let mut total = Duration::ZERO;
        for _ in 0..iters {
            let student = &students[*next % students.len()];
            *next += 1;
            let payload = workload::cached_payload(handle, student);
            total += workload::run_cached(client, payload, student);
        }
        total
    };

    let mut criterion = Criterion::default().without_plots().configure_from_args();
    {
        let mut client = daemon.connect();
        workload::init(&mut client);
        let handle = workload::register(&mut client);

        let mut group = criterion.benchmark_group("cache_win");
        group.noise_threshold(report::VARIANCE.p50);

        let mut next = 0usize;
        group.bench_function("cold_first_run", |bencher| {
            bencher.iter_custom(|iters| cold(&mut client, workload::RULE, iters, &mut next));
        });
        group.bench_function("warm_cache_hit", |bencher| {
            bencher.iter_custom(|iters| warm(&mut client, &handle, iters, &mut next));
        });
        group.bench_function("direct_execution", |bencher| {
            bencher.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let student = &students[next % students.len()];
                    next += 1;
                    let payload = workload::direct_payload(student);
                    total += workload::run_direct(&mut client, payload, student);
                }
                total
            });
        });
        group.finish();
    }
    {
        // Story 4.5: the same figures on the Rhai server, through the same client — one
        // connection each, one execution in flight, the same student rotation.
        let mut hexput = daemon.connect();
        workload::init(&mut hexput);
        let hexput_handle = workload::register_source(&mut hexput, workload::RULE);
        let mut rhai_client = rhai_server.connect();
        workload::init(&mut rhai_client);
        let rhai_handle = workload::register_source(&mut rhai_client, rhai::RHAI_RULE);

        let mut group = criterion.benchmark_group("rhai_compare");
        group.noise_threshold(report::VARIANCE.p50);

        let mut next = 0usize;
        group.bench_function("hexput_cold_first_run", |bencher| {
            bencher.iter_custom(|iters| cold(&mut hexput, workload::RULE, iters, &mut next));
        });
        group.bench_function("rhai_cold_first_run", |bencher| {
            bencher.iter_custom(|iters| cold(&mut rhai_client, rhai::RHAI_RULE, iters, &mut next));
        });
        group.bench_function("hexput_warm_cache_hit", |bencher| {
            bencher.iter_custom(|iters| warm(&mut hexput, &hexput_handle, iters, &mut next));
        });
        group.bench_function("rhai_warm_cache_hit", |bencher| {
            bencher.iter_custom(|iters| warm(&mut rhai_client, &rhai_handle, iters, &mut next));
        });
        group.finish();
    }
    {
        // Story 5.9: the remote path — the `cache_win` cold and warm figures, defined exactly
        // alike, over one TCP+TLS connection to the same Daemon, one execution in flight.
        let mut client = daemon.connect_tls();
        workload::init(&mut client);
        let handle = workload::register(&mut client);

        let mut group = criterion.benchmark_group("remote_path");
        group.noise_threshold(report::VARIANCE.p50);

        let mut next = 0usize;
        group.bench_function("tcp_tls_cold_first_run", |bencher| {
            bencher.iter_custom(|iters| cold(&mut client, workload::RULE, iters, &mut next));
        });
        group.bench_function("tcp_tls_warm_cache_hit", |bencher| {
            bencher.iter_custom(|iters| warm(&mut client, &handle, iters, &mut next));
        });
        group.finish();
    }
    criterion.final_summary();

    let steady = if report::runs_steady_state(filter.as_deref(), &args) {
        let (rounds, runs, warm) = if quick {
            (1, QUICK_RUNS_PER_CONNECTION, 0)
        } else {
            (ROUNDS, RUNS_PER_CONNECTION, WARM_UP_RUNS)
        };
        let hexput = steady::measure(|| daemon.connect(), workload::RULE, rounds, runs, warm);
        let hexput_summary = report::Summary::of(&hexput);
        println!(
            "{}",
            report::render_steady_state(report::HEXPUT_SIDE, &hexput, &hexput_summary)
        );
        // Story 5.9: the same closed loops over TCP+TLS, reported on their own.
        let tls = steady::measure(|| daemon.connect_tls(), workload::RULE, rounds, runs, warm);
        let tls_summary = report::Summary::of(&tls);
        println!(
            "{}",
            report::render_steady_state(report::TCP_TLS_SIDE, &tls, &tls_summary)
        );
        println!(
            "{}",
            report::render_transport(&hexput_summary, &tls_summary)
        );
        let rhai = steady::measure(
            || rhai_server.connect(),
            rhai::RHAI_RULE,
            rounds,
            runs,
            warm,
        );
        let rhai_summary = report::Summary::of(&rhai);
        println!(
            "{}",
            report::render_steady_state(report::RHAI_SIDE, &rhai, &rhai_summary)
        );
        println!(
            "{}",
            report::render_side_by_side(
                &report::HEXPUT_RHAI_TABLE,
                &hexput_summary.median,
                &rhai_summary.median
            )
        );
        Some(report::Steady {
            hexput,
            hexput_summary,
            tcp_tls: tls,
            tcp_tls_summary: tls_summary,
            rhai,
            rhai_summary,
        })
    } else {
        println!("steady_state skipped by the filter");
        None
    };

    let engine = if report::runs_engine(filter.as_deref(), &args) {
        let (rounds, runs, warm_up) = if quick {
            (1, QUICK_ENGINE_RUNS, 0)
        } else {
            (ENGINE_ROUNDS, ENGINE_RUNS, ENGINE_WARM_UP_RUNS)
        };
        let measured = engine::measure(&students, rounds, runs, warm_up);
        let summary = report::EngineFigures::median(&measured);
        println!("{}", report::render_engine(&measured, &summary));
        Some(summary)
    } else {
        println!("engine skipped by the filter");
        None
    };

    // Quick mode: the report written from this run's steady states must compare each leg with
    // itself, so `main`'s wiring of the legs is checked, not just the report's functions.
    if let Some(steady) = &steady
        && quick
    {
        steady.check_round_trip(&conditions);
    }

    // The report is read, compared and written only after a full steady state.
    if let Some(steady) = &steady
        && !quick
    {
        let comparison = steady.compare(previous.as_deref(), &conditions);
        println!("{}", comparison.render());
        let json = report::to_json(&conditions, steady, engine.as_ref());
        match report::write(&path, &json) {
            Ok(()) => println!("report written to {}", path.display()),
            Err(error) => eprintln!("could not write {}: {error}", path.display()),
        }
    }

    daemon.stop();
    rhai_server.stop();
}

/// The Daemon under measurement: in-process, on its own thread, serving a socket in a fresh temp
/// dir and a TCP+TLS listener on a loopback port.
#[cfg(unix)]
mod daemon {
    use std::ffi::OsString;
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use hexput_daemon::{Discovery, run_with};
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, ServerName};
    use rustls::{ClientConfig, ClientConnection, StreamOwned};

    use crate::wire::Client;

    /// A blocking TLS client stream over TCP.
    type TlsStream = StreamOwned<ClientConnection, TcpStream>;

    pub struct Daemon {
        stop: tokio::sync::oneshot::Sender<()>,
        thread: std::thread::JoinHandle<(ExitCode, String)>,
        dir: PathBuf,
        /// The loopback address of the TCP+TLS listener.
        tcp: SocketAddr,
        /// The one TLS client configuration every TCP+TLS connection uses.
        tls: Arc<ClientConfig>,
    }

    /// A checked-in TLS fixture: the CA, and the `localhost`/`127.0.0.1` leaf with its key.
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("tls")
            .join(name)
    }

    /// A loopback port that is free just before the Daemon starts: bound to port 0, read, and
    /// released. Should another process take it before the Daemon binds, the Daemon exits with
    /// failure and `connect_tls` panics — never a wrong number.
    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free loopback port")
            .port()
    }

    /// The client configuration: the `ring` provider, rustls's safe default protocol versions,
    /// the fixture CA as the only root, and no client authentication.
    fn client_config() -> Arc<ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_file_iter(fixture("ca.pem")).expect("the CA file") {
            roots
                .add(certificate.expect("a CA certificate"))
                .expect("a trusted root");
        }
        Arc::new(
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("the ring provider's protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }

    impl Daemon {
        /// Starts a Daemon whose System Config names a socket inside a fresh temp dir and a
        /// TCP+TLS listener on a free loopback port with the fixture identity, logging at
        /// `error` so logging stays off the measured path.
        pub fn start() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let dir = std::env::temp_dir()
                .join(format!("hexput-bench-{}-{nanos:08x}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temporary directory");
            let tcp = SocketAddr::from(([127, 0, 0, 1], free_port()));
            let config = dir.join("config.toml");
            std::fs::write(
                &config,
                format!(
                    "log_level = \"error\"\n\n[transport.uds]\npath = {:?}\n\n\
                     [transport.tcp]\nbind = {:?}\ntls_cert = {:?}\ntls_key = {:?}\n",
                    dir.join("d.sock").to_str().expect("a UTF-8 temp path"),
                    tcp.to_string(),
                    fixture("server.pem")
                        .to_str()
                        .expect("a UTF-8 fixture path"),
                    fixture("server.key")
                        .to_str()
                        .expect("a UTF-8 fixture path"),
                ),
            )
            .expect("a System Config file");
            let discovery = Discovery::new(None, dir.join("no-default.toml"));
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
                    std::io::sink(),
                    async {
                        let _ = stopped.await;
                    },
                );
                (code, String::from_utf8_lossy(&err).into_owned())
            });
            Self {
                stop,
                thread,
                dir,
                tcp,
                tls: client_config(),
            }
        }

        /// Retries `attempt` until the Daemon listens; panics once it has stopped or 10 s passed.
        fn until_listening<T>(&self, attempt: impl Fn() -> std::io::Result<T>) -> T {
            let started = Instant::now();
            loop {
                match attempt() {
                    Ok(connected) => return connected,
                    Err(error) => {
                        assert!(
                            started.elapsed() < Duration::from_secs(10)
                                && !self.thread.is_finished(),
                            "the Daemon never listened: {error}"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }

        /// A UDS client connection, waiting until the Daemon listens.
        pub fn connect(&self) -> Client {
            let socket = self.dir.join("d.sock");
            let stream = self.until_listening(|| std::os::unix::net::UnixStream::connect(&socket));
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("a read timeout");
            Client::new(stream)
        }

        /// A TCP+TLS client connection to the loopback listener, waiting until the Daemon
        /// listens, with its handshake completed.
        pub fn connect_tls(&self) -> Client {
            Client::new(self.tls_stream())
        }

        /// The TLS parameters the TCP+TLS leg negotiates, read off one completed handshake:
        /// the protocol version and the cipher suite, e.g. `TLSv1_3, TLS13_AES_256_GCM_SHA384`.
        pub fn tls_parameters(&self) -> String {
            let stream = self.tls_stream();
            let version = stream
                .conn
                .protocol_version()
                .expect("a negotiated protocol version");
            let suite = stream
                .conn
                .negotiated_cipher_suite()
                .expect("a negotiated cipher suite");
            format!("{version:?}, {:?}", suite.suite())
        }

        /// A TLS stream to the loopback listener: `TCP_NODELAY`, a 60 s read timeout, the
        /// server name `localhost`, and the handshake driven to completion before it returns.
        fn tls_stream(&self) -> TlsStream {
            let tcp = self.until_listening(|| TcpStream::connect(self.tcp));
            tcp.set_nodelay(true).expect("TCP_NODELAY");
            tcp.set_read_timeout(Some(Duration::from_secs(60)))
                .expect("a read timeout");
            let connection = ClientConnection::new(
                Arc::clone(&self.tls),
                ServerName::try_from("localhost").expect("a server name"),
            )
            .expect("a TLS client connection");
            let mut stream = StreamOwned::new(connection, tcp);
            while stream.conn.is_handshaking() {
                stream
                    .conn
                    .complete_io(&mut stream.sock)
                    .expect("a completed TLS handshake");
            }
            stream
        }

        /// Stops the Daemon, which must exit with success.
        pub fn stop(self) {
            let _ = self.stop.send(());
            let (code, stderr) = self.thread.join().expect("the Daemon thread");
            let _ = std::fs::remove_dir_all(&self.dir);
            assert_eq!(
                format!("{code:?}"),
                format!("{:?}", ExitCode::SUCCESS),
                "the Daemon failed: {stderr}"
            );
        }
    }
}

/// A blocking Backend client: framed MessagePack envelopes over any byte stream — a UDS, or TLS
/// over TCP — so every leg runs the one verified client.
#[cfg(unix)]
mod wire {
    use std::io::{BufReader, Read, Write};

    use hexput_port::{CorrelationId, Envelope, MessageType, Value, decode, encode, encode_frame};

    /// A connected byte stream the client reads and writes: a `UnixStream`, or a rustls client
    /// stream over a `TcpStream`. Read timeouts are set by whoever connects it.
    pub trait Duplex: Read + Write + Send {}

    impl<T: Read + Write + Send> Duplex for T {}

    pub struct Client {
        /// The stream, read through the buffer and written through `get_mut()`.
        stream: BufReader<Box<dyn Duplex>>,
        next_id: u64,
    }

    impl Client {
        pub fn new(stream: impl Duplex + 'static) -> Self {
            Self {
                stream: BufReader::new(Box::new(stream)),
                next_id: 1,
            }
        }

        /// A request envelope with this connection's next correlation id.
        pub fn envelope(&mut self, message_type: MessageType, payload: Value) -> Envelope<Value> {
            let id = self.next_id;
            self.next_id += 1;
            Envelope::new(CorrelationId(id), message_type, payload)
        }

        /// Writes one frame and flushes it, so a TLS record is never held back.
        pub fn send(&mut self, envelope: &Envelope<Value>) {
            let frame = encode_frame(&encode(envelope).expect("an encodable envelope"))
                .expect("a framable envelope");
            let writer = self.stream.get_mut();
            writer.write_all(&frame).expect("a sent frame");
            writer.flush().expect("a flushed frame");
        }

        /// The next message from the Daemon; the stream must not end.
        pub fn receive(&mut self) -> Envelope<Value> {
            let mut prefix = [0; 4];
            self.stream
                .read_exact(&mut prefix)
                .expect("a message from the Daemon");
            let len = u32::from_be_bytes(prefix) as usize;
            assert!(
                len <= hexput_port::MAX_FRAME_LEN,
                "a frame of {len} bytes is over the limit"
            );
            let mut body = vec![0; len];
            self.stream.read_exact(&mut body).expect("a whole frame");
            decode(&body).expect("a decodable envelope")
        }

        /// Sends a request that makes no host call and returns its `Result` payload.
        pub fn request(&mut self, message_type: MessageType, payload: Value) -> Value {
            let request = self.envelope(message_type, payload);
            self.send(&request);
            let reply = self.receive();
            assert_eq!(
                reply.message_type,
                MessageType::Result,
                "{message_type:?} failed: {:?}",
                reply.payload
            );
            assert_eq!(reply.id, request.id, "the reply names its request");
            reply.payload
        }
    }

    /// The value under `key` in a map payload.
    pub fn field<'a>(payload: &'a Value, key: &str) -> &'a Value {
        payload
            .as_map()
            .expect("a map payload")
            .iter()
            .find(|(name, _)| name.as_str() == Some(key))
            .map(|(_, value)| value)
            .unwrap_or_else(|| panic!("a `{key}` field in {payload:?}"))
    }
}

/// The anchor-case workload, its Backend, and the verification of every run.
#[cfg(unix)]
mod workload {
    use std::time::{Duration, Instant};

    use hexput_port::{CorrelationId, Envelope, MessageType, Value};

    use crate::wire::{Client, field};

    /// The measured Script: a weighted pass/fail rule over a student's grades.
    pub const RULE: &str = "let total = 0;\nlet weights = 0;\nfor (grade in grades) {\n  let w = weight(grade.course);\n  total = total + grade.score * w;\n  weights = weights + w;\n};\nlet passed = total / weights >= passMark(student.program);\nrecord(student.id, passed);\nreturn passed;\n";
    /// Host calls per run: 8 × `weight`, `passMark`, `record`.
    pub const HOST_CALLS_PER_RUN: usize = 10;
    /// Students the runs rotate through.
    pub const STUDENTS: usize = 16;
    /// The `weight` table.
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

    pub struct Student {
        pub id: String,
        pub program: &'static str,
        pub grades: Vec<(&'static str, i64)>,
        /// The starting variables, built once; each run sends a copy.
        pub variables: Value,
        /// The verdict Rust computes.
        pub passes: bool,
        /// The verdict Rust computes, as the exact `Result` payload expected.
        expected: Value,
    }

    pub fn course_weight(course: &str) -> i64 {
        COURSES
            .iter()
            .find(|(name, _)| *name == course)
            .map(|(_, weight)| *weight)
            .unwrap_or_else(|| panic!("an unknown course `{course}`"))
    }

    pub fn pass_mark(program: &str) -> i64 {
        match program {
            "science" => 50,
            "engineering" => 60,
            other => panic!("an unknown program `{other}`"),
        }
    }

    /// The 16 deterministic students: score `j` of student `i` is `(i*37 + j*11) % 101`, and the
    /// programs alternate.
    pub fn students() -> Vec<Student> {
        (0..STUDENTS as i64)
            .map(|i| {
                let mut student = Student {
                    id: format!("student-{i:02}"),
                    program: if i % 2 == 0 { "science" } else { "engineering" },
                    grades: (0..8i64)
                        .map(|j| (COURSES[((i + j) % 8) as usize].0, (i * 37 + j * 11) % 101))
                        .collect(),
                    variables: Value::Nil,
                    passes: false,
                    expected: Value::Nil,
                };
                student.variables = variables(&student);
                student.passes = verdict(&student);
                student.expected =
                    Value::Map(vec![(Value::from("value"), Value::Boolean(student.passes))]);
                student
            })
            .collect()
    }

    /// The verdict, with the same f64 operations in the same order as the Script.
    fn verdict(student: &Student) -> bool {
        let mut total = 0.0_f64;
        let mut weights = 0.0_f64;
        for (course, score) in &student.grades {
            let w = course_weight(course) as f64;
            total += *score as f64 * w;
            weights += w;
        }
        total / weights >= pass_mark(student.program) as f64
    }

    fn variables(student: &Student) -> Value {
        let grades = student
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

    /// `Init` with no Config overrides and a blanket grant for each of the rule's host functions,
    /// so no `Authorize` question is ever sent.
    pub fn init(client: &mut Client) {
        let registrations = ["weight", "passMark", "record"]
            .iter()
            .map(|name| {
                Value::Map(vec![
                    (Value::from("name"), Value::from(*name)),
                    (Value::from("blanket"), Value::from(true)),
                ])
            })
            .collect();
        client.request(
            MessageType::Init,
            Value::Map(vec![
                (Value::from("config"), Value::Map(vec![])),
                (Value::from("registrations"), Value::Array(registrations)),
            ]),
        );
    }

    /// `CodeRegister` of the rule; returns its handle.
    pub fn register(client: &mut Client) -> String {
        register_source(client, RULE)
    }

    /// `CodeRegister` of `source` — [`RULE`] on the Daemon, `RHAI_RULE` on the Rhai server;
    /// returns its handle.
    pub fn register_source(client: &mut Client, source: &str) -> String {
        let payload = client.request(
            MessageType::CodeRegister,
            Value::Map(vec![(Value::from("source"), Value::from(source))]),
        );
        field(&payload, "handle")
            .as_str()
            .expect("a string handle")
            .to_owned()
    }

    /// `CodeUnregister` of `handle`, answered `Result {}`.
    pub fn unregister(client: &mut Client, handle: &str) {
        let payload = client.request(
            MessageType::CodeUnregister,
            Value::Map(vec![(Value::from("handle"), Value::from(handle))]),
        );
        assert_eq!(payload, Value::Map(vec![]), "CodeUnregister answers {{}}");
    }

    pub fn cached_payload(handle: &str, student: &Student) -> Value {
        Value::Map(vec![
            (Value::from("handle"), Value::from(handle)),
            (Value::from("variables"), student.variables.clone()),
        ])
    }

    pub fn direct_payload(student: &Student) -> Value {
        Value::Map(vec![
            (Value::from("source"), Value::from(RULE)),
            (Value::from("variables"), student.variables.clone()),
        ])
    }

    pub fn run_cached(client: &mut Client, payload: Value, student: &Student) -> Duration {
        run(client, MessageType::CachedExecutionStart, payload, student)
    }

    pub fn run_direct(client: &mut Client, payload: Value, student: &Student) -> Duration {
        run(client, MessageType::ExecutionStart, payload, student)
    }

    /// Sends one execution, answers its `Call`s as the Backend, and returns the time from the
    /// send to its final `Result`. Panics unless the reply is the expected verdict after exactly
    /// [`HOST_CALLS_PER_RUN`] calls.
    fn run(
        client: &mut Client,
        message_type: MessageType,
        payload: Value,
        student: &Student,
    ) -> Duration {
        let request = client.envelope(message_type, payload);
        let started = Instant::now();
        client.send(&request);
        let mut calls = 0;
        let (elapsed, reply) = loop {
            let message = client.receive();
            match message.message_type {
                MessageType::Call => {
                    calls += 1;
                    let value = answer(&message, student);
                    let call_id = message.id.expect("a Call carries its id");
                    client.send(&Envelope::new(
                        CorrelationId(call_id.get()),
                        MessageType::Result,
                        Value::Map(vec![(Value::from("value"), value)]),
                    ));
                }
                MessageType::Result if message.id == request.id => {
                    break (started.elapsed(), message.payload);
                }
                other => panic!(
                    "{} failed with {other:?} (id {:?}): {:?}",
                    student.id, message.id, message.payload
                ),
            }
        };
        assert_eq!(
            reply, student.expected,
            "{}: a wrong verdict; no number is published",
            student.id
        );
        assert_eq!(
            calls, HOST_CALLS_PER_RUN,
            "{}: a wrong number of host calls",
            student.id
        );
        elapsed
    }

    /// The Backend's answer to one `Call` of the rule's three host functions. Each argument
    /// arrives as a Value Secret holder (`{__secret, value}`, FR-28) because it was read from a
    /// place — the Backend reads the value inside; the Rhai server sends plain values. `record`
    /// must name this student and its verdict.
    fn answer(call: &Envelope<Value>, student: &Student) -> Value {
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
            ("record", [id, Value::Boolean(passed)])
                if id.as_str() == Some(student.id.as_str()) && *passed == student.passes =>
            {
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
}

/// The steady-state section: concurrent closed loops of Cached Executions, timed by hand.
#[cfg(unix)]
mod steady {
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    use crate::report::Round;
    use crate::wire::Client;
    use crate::{CONCURRENCY, workload};

    /// Runs `rounds` rounds of `runs` timed runs on each of [`CONCURRENCY`] connections opened
    /// by `connect`, after `warm_up` untimed runs each, with `source` registered on each, and
    /// returns each round's figures. The Daemon and the Rhai server are measured alike.
    pub fn measure(
        connect: impl Fn() -> Client,
        source: &str,
        rounds: usize,
        runs: usize,
        warm_up: usize,
    ) -> Vec<Round> {
        let students = workload::students();
        let clients: Vec<_> = (0..CONCURRENCY).map(|_| connect()).collect();
        // The connections and the main thread meet before and after every round, so the main
        // thread's clock spans exactly the round's runs.
        let barrier = Barrier::new(CONCURRENCY + 1);
        std::thread::scope(|scope| {
            let workers: Vec<_> = clients
                .into_iter()
                .enumerate()
                .map(|(index, mut client)| {
                    let (students, barrier) = (&students, &barrier);
                    scope.spawn(move || {
                        // A connection that panics (a wrong verdict, a wrong `Call` count, a read
                        // timeout) still meets the barrier for every wait it has left, so the
                        // others and the main thread finish and `join` reports the panic instead
                        // of the run hanging.
                        let waited = std::cell::Cell::new(0usize);
                        let wait = || {
                            barrier.wait();
                            waited.set(waited.get() + 1);
                        };
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                workload::init(&mut client);
                                let handle = workload::register_source(&mut client, source);
                                let mut turn = index;
                                let mut next = || {
                                    turn += 1;
                                    &students[turn % students.len()]
                                };
                                for _ in 0..warm_up {
                                    let student = next();
                                    let payload = workload::cached_payload(&handle, student);
                                    workload::run_cached(&mut client, payload, student);
                                }
                                let mut latencies: Vec<Vec<Duration>> = Vec::with_capacity(rounds);
                                for _ in 0..rounds {
                                    let mut round = Vec::with_capacity(runs);
                                    wait();
                                    for _ in 0..runs {
                                        let student = next();
                                        let payload = workload::cached_payload(&handle, student);
                                        round.push(workload::run_cached(
                                            &mut client,
                                            payload,
                                            student,
                                        ));
                                    }
                                    wait();
                                    latencies.push(round);
                                }
                                latencies
                            }));
                        outcome.unwrap_or_else(|panic| {
                            for _ in waited.get()..2 * rounds {
                                barrier.wait();
                            }
                            std::panic::resume_unwind(panic)
                        })
                    })
                })
                .collect();
            let mut walls = Vec::with_capacity(rounds);
            for _ in 0..rounds {
                barrier.wait();
                let started = Instant::now();
                barrier.wait();
                walls.push(started.elapsed());
            }
            let mut per_round: Vec<Vec<Duration>> = vec![Vec::new(); rounds];
            for worker in workers {
                let latencies = worker.join().expect("a steady-state connection");
                for (round, mut measured) in latencies.into_iter().enumerate() {
                    per_round[round].append(&mut measured);
                }
            }
            per_round
                .into_iter()
                .zip(walls)
                .map(|(latencies, wall)| Round::of(latencies, wall))
                .collect()
        })
    }
}

/// The `engine` section: the rule in-process on three drivers, hand-timed, so the Resource
/// Budget's cost shows as its own line beside Rhai's engine.
#[cfg(unix)]
mod engine {
    use std::sync::Arc;
    use std::time::Instant;

    use hexput_enforce::{Budget, Exceeded, Limits};
    use hexput_exec::wire::{Path, measure_held, result_payload_size, to_hexput};
    use hexput_interpreter::{Argument, Execution, Held, Meter, Outcome, Program, Value};
    use hexput_shared::diagnostics::Span;

    use crate::report::EngineFigures;
    use crate::rhai::EngineDriver;
    use crate::workload::{self, HOST_CALLS_PER_RUN, Student};

    /// One Hexput run's result: its verdict, its host calls and its `record` calls (each checked
    /// against the student as it is made).
    struct Driven {
        verdict: bool,
        calls: usize,
        records: usize,
    }

    /// The rule's host calls answered natively — no wire. `record` must name this student and
    /// its verdict.
    fn answer(name: &str, arguments: &[Argument], student: &Student, records: &mut usize) -> Value {
        match (name, arguments) {
            ("weight", [course]) => match &course.value {
                Value::String(course) => Value::Number(workload::course_weight(course) as f64),
                other => panic!("`weight` of {other:?}"),
            },
            ("passMark", [program]) => match &program.value {
                Value::String(program) => Value::Number(workload::pass_mark(program) as f64),
                other => panic!("`passMark` of {other:?}"),
            },
            ("record", [id, passed]) => {
                assert!(
                    matches!(&id.value, Value::String(id) if **id == *student.id)
                        && passed.value.as_bool() == Some(student.passes),
                    "{}: a wrong `record`",
                    student.id
                );
                *records += 1;
                Value::Null
            }
            (name, arguments) => panic!(
                "an unexpected host call to `{name}` with {} argument(s)",
                arguments.len()
            ),
        }
    }

    /// A refused charge is a panic: the enforced figure never comes from a run the Executor
    /// would have stopped.
    fn within(result: Result<(), Exceeded>) {
        if let Err(exceeded) = result {
            panic!(
                "an enforced run exceeded its {} budget",
                exceeded.dimension().as_str()
            );
        }
    }

    /// One in-process run of the rule. Unmetered: `Execution` with `Meter::UNMETERED`, each host
    /// call resumed at once. Enforced: the Executor's synchronous enforcement, in the order
    /// `hexput_exec::execute_held` applies it — the same slice and ceilings from
    /// `Budget::with_limits(Limits::default())`, CPU charged with an `Instant`-timed elapsed at
    /// every pause, host call and finish, each host call's arguments measured against the
    /// argument depth limit and the frame (that work charged as CPU too), one RPC call (and side
    /// effect) per host call, and the exact output size at the finish. Not mirrored, as it is not
    /// budgeting: converting the arguments to wire values (there is no wire here), the feature
    /// toggles, the method table and the capability decision.
    fn drive(
        program: &Arc<Program>,
        variables: &[(String, Held)],
        enforced: bool,
        student: &Student,
    ) -> Driven {
        let mut budget = enforced.then(|| Budget::with_limits(Limits::default()));
        let meter = match &budget {
            Some(budget) => Meter {
                slice: Some(hexput_exec::SLICE),
                memory_ceiling: Some(budget.memory_ceiling()),
                allocation_ceiling: Some(budget.allocation_ceiling()),
            },
            None => Meter::UNMETERED,
        };
        let whole = program.span;
        let mut started = Instant::now();
        let mut execution =
            Execution::with_variables(Arc::clone(program), variables.iter().cloned())
                .expect("the starting variables bind")
                .metered(meter);
        let charge_cpu = |budget: &mut Option<Budget>, started: Instant, span: Span| {
            if let Some(budget) = budget {
                within(budget.charge_cpu(started.elapsed(), span));
            }
        };
        let (mut calls, mut records) = (0, 0);
        loop {
            match execution.run().expect("the rule runs") {
                Outcome::Paused(paused) => {
                    charge_cpu(&mut budget, started, paused.span());
                    started = Instant::now();
                    execution = paused.resume();
                }
                Outcome::HostCall(call) => {
                    charge_cpu(&mut budget, started, call.span());
                    if let Some(budget) = budget.as_mut() {
                        // `advance`: the arguments measured, the call counted, the work charged.
                        let measuring = Instant::now();
                        let depth = budget.limits().argument_depth();
                        let mut frame = hexput_port::MAX_FRAME_LEN;
                        for argument in call.arguments() {
                            measure_held(
                                &argument.value,
                                argument.secret.as_ref(),
                                depth,
                                &mut frame,
                            )
                            .unwrap_or_else(|_| panic!("{}: an unsendable argument", student.id));
                        }
                        within(budget.charge_rpc_call(call.span()));
                        within(budget.charge_cpu(measuring.elapsed(), call.span()));
                    }
                    calls += 1;
                    let value = answer(call.name(), call.arguments(), student, &mut records);
                    started = Instant::now();
                    execution = call.resume(&value);
                }
                Outcome::Finished(finished) => {
                    charge_cpu(&mut budget, started, whole);
                    if let Some(budget) = &budget {
                        let limit = budget.limits().output_size();
                        let size =
                            result_payload_size(&finished.result, &finished.modifications, limit);
                        within(budget.charge_output(size, whole));
                    }
                    let verdict = finished
                        .result
                        .value
                        .as_bool()
                        .expect("the rule returns a bool");
                    return Driven {
                        verdict,
                        calls,
                        records,
                    };
                }
                Outcome::OutOfMemory(_) => panic!("{}: the rule ran out of memory", student.id),
                Outcome::AllocationsExceeded(_) => {
                    panic!("{}: the rule made too many allocations", student.id)
                }
            }
        }
    }

    /// Panics unless a run returned the student's verdict after exactly the rule's host calls,
    /// one of them a `record`.
    fn verify(student: &Student, driver: &str, verdict: bool, calls: usize, records: usize) {
        assert_eq!(
            verdict, student.passes,
            "{driver} {}: a wrong verdict; no number is published",
            student.id
        );
        assert_eq!(
            (calls, records),
            (HOST_CALLS_PER_RUN, 1),
            "{driver} {}: a wrong number of host calls",
            student.id
        );
    }

    /// Runs `warm_up` untimed runs of each driver, then `rounds` rounds; each runs `runs` runs of
    /// each driver in turn over the students — the round's first driver rotating — and gives each
    /// driver's mean ns/run. Every run is verified.
    pub fn measure(
        students: &[Student],
        rounds: usize,
        runs: usize,
        warm_up: usize,
    ) -> Vec<EngineFigures> {
        let program = Arc::new(hexput_parser::parse(workload::RULE).expect("the rule parses"));
        // The variables are converted once per student and cloned per run.
        let hexput: Vec<Vec<(String, Held)>> = students
            .iter()
            .map(|student| {
                student
                    .variables
                    .as_map()
                    .expect("a variables map")
                    .iter()
                    .map(|(name, value)| {
                        let name = name.as_str().expect("a variable name");
                        let held = to_hexput(value, &mut Path::variable(name))
                            .expect("a convertible variable");
                        (name.to_owned(), held)
                    })
                    .collect()
            })
            .collect();
        let rhai_engine = EngineDriver::new();
        let rhai: Vec<_> = students
            .iter()
            .map(|student| EngineDriver::variables(&student.variables))
            .collect();

        let (program, hexput) = (&program, &hexput);
        let hexput_driver = |enforced: bool, name: &'static str| {
            move |index: usize| {
                let student = &students[index];
                let driven = drive(program, &hexput[index], enforced, student);
                verify(student, name, driven.verdict, driven.calls, driven.records);
            }
        };
        let mut hexput_unmetered = hexput_driver(false, "hexput_unmetered");
        let mut hexput_enforced = hexput_driver(true, "hexput_enforced");
        let mut rhai_engine_driver = |index: usize| {
            let student = &students[index];
            let run = rhai_engine.run(&rhai[index]);
            assert!(
                run.recorded.as_ref().is_some_and(
                    |(id, passed)| id.as_str() == student.id && *passed == student.passes
                ),
                "rhai_engine {}: a wrong `record`",
                student.id
            );
            verify(student, "rhai_engine", run.verdict, run.calls, run.records);
        };
        // In `EngineFigures` order: unmetered, enforced, Rhai.
        let mut drivers: [&mut dyn FnMut(usize); 3] = [
            &mut hexput_unmetered,
            &mut hexput_enforced,
            &mut rhai_engine_driver,
        ];
        // Untimed warm-up runs of every driver, so no driver pays for cold caches.
        for driver in drivers.iter_mut() {
            for index in 0..warm_up {
                (*driver)(index % students.len());
            }
        }
        (0..rounds)
            .map(|round| {
                // Each round starts with the next driver, so no driver always runs first.
                let mut mean_ns = [0.0_f64; 3];
                for offset in 0..drivers.len() {
                    let which = (round + offset) % drivers.len();
                    let started = Instant::now();
                    for index in 0..runs {
                        (drivers[which])(index % students.len());
                    }
                    mean_ns[which] = started.elapsed().as_secs_f64() * 1e9 / runs as f64;
                }
                EngineFigures {
                    hexput_unmetered: mean_ns[0],
                    hexput_enforced: mean_ns[1],
                    rhai_engine: mean_ns[2],
                }
            })
            .collect()
    }
}

/// The conditions, the steady-state figures and their reproducibility — pure apart from
/// gathering the conditions and writing the file, so quick mode checks it with [`self_check`].
#[cfg(unix)]
mod report {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::{Value as Json, json};

    use crate::{
        CONCURRENCY, ENGINE_ROUNDS, ENGINE_RUNS, ENGINE_WARM_UP_RUNS, QUICK_ENGINE_RUNS,
        QUICK_RUNS_PER_CONNECTION, ROUNDS, RUNS_PER_CONNECTION, WARM_UP_RUNS,
    };

    /// What the Rhai side is, recorded in the conditions.
    pub const COMPARATOR: &str =
        "rhai 1.26.1, Engine::new() defaults, thread-per-connection UDS server in the bench";

    /// The steady-state sides' labels.
    pub const HEXPUT_SIDE: &str = "Hexput Daemon, Cached Execution";
    pub const TCP_TLS_SIDE: &str = "Hexput Daemon, Cached Execution over TCP+TLS (loopback)";
    pub const RHAI_SIDE: &str = "Rhai server, compiled AST";

    /// Where the UDS leg (and the Rhai server's) runs.
    pub const UDS_TOPOLOGY: &str = "same machine: Daemon in-process on its own runtime, clients on threads of the bench process";
    /// Where the TCP+TLS leg runs: one machine, over loopback, so never network latency.
    pub const TCP_TLS_TOPOLOGY: &str = "same machine over the loopback interface: no network hop, \
         so the figure is the TCP and TLS stack cost, not network latency; Daemon in-process, \
         clients on threads of the bench process";
    /// The TCP+TLS leg's address.
    pub const TCP_TLS_ADDRESS: &str = "127.0.0.1 (loopback)";

    /// A two-column table of steady-state medians: its title, its column labels (left, right,
    /// ratio), which way the ratio divides, and how to read it.
    pub struct Table {
        pub title: &'static str,
        pub columns: [&'static str; 3],
        /// The ratio is right ÷ left when set, else left ÷ right.
        pub right_over_left: bool,
        pub reading: &'static str,
    }

    /// Hexput (UDS) against Rhai (UDS).
    pub const HEXPUT_RHAI_TABLE: Table = Table {
        title: "side by side (median of rounds, same client and concurrency)",
        columns: ["Hexput", "Rhai", "Hexput/Rhai"],
        right_over_left: false,
        reading: "Hexput/Rhai below 1x: a lower p50/p99 (better) but a lower throughput (worse)",
    };

    /// Hexput over UDS against Hexput over TCP+TLS: both raw figures, never one combined.
    pub const TRANSPORT_TABLE: Table = Table {
        title: "transport (Hexput, median of rounds, same client and concurrency; each leg's raw \
                figures, none combined)",
        columns: ["UDS", "TCP+TLS", "TCP+TLS/UDS"],
        right_over_left: true,
        reading: "TCP+TLS/UDS: above 1x on p50/p99 or below 1x on throughput is the TCP and TLS \
                  stacks' cost over loopback (worse); connections are set up before timing, so \
                  no figure includes the TCP connect or the TLS handshake",
    };

    /// The stated variance, as fractions.
    pub struct Variance {
        pub p50: f64,
        pub p99: f64,
        pub throughput: f64,
    }

    pub const VARIANCE: Variance = Variance {
        p50: 0.10,
        p99: 0.20,
        throughput: 0.10,
    };

    /// Quick mode: no `--bench`, or `--bench --test` (Criterion's own test mode).
    pub fn is_quick(args: &[String]) -> bool {
        let flag = |name: &str| args.iter().skip(1).any(|arg| arg == name);
        !flag("--bench") || flag("--test")
    }

    /// Criterion options that take a value in the next argument.
    const VALUE_OPTIONS: [&str; 19] = [
        "-c",
        "--color",
        "--colour",
        "-s",
        "--save-baseline",
        "-b",
        "--baseline",
        "--baseline-lenient",
        "--load-baseline",
        "--format",
        "--profile-time",
        "--sample-size",
        "--warm-up-time",
        "--measurement-time",
        "--nresamples",
        "--noise-threshold",
        "--confidence-level",
        "--significance-level",
        "--plotting-backend",
    ];

    /// The positional filter Criterion also reads, if any.
    pub fn filter(args: &[String]) -> Option<String> {
        let mut rest = args.iter().skip(1);
        while let Some(arg) = rest.next() {
            if arg == "--" {
                return rest.next().cloned();
            }
            if arg.starts_with('-') {
                if VALUE_OPTIONS.contains(&arg.as_str()) || arg == "--output-format" {
                    rest.next();
                }
                continue;
            }
            return Some(arg.clone());
        }
        None
    }

    /// A hand-timed section runs with no filter, or a filter that is a substring of its name,
    /// and not when Criterion only lists or profiles its benchmarks.
    fn runs_section(name: &str, filter: Option<&str>, args: &[String]) -> bool {
        !args
            .iter()
            .any(|arg| arg == "--list" || arg.split('=').next() == Some("--profile-time"))
            && filter.is_none_or(|filter| name.contains(filter))
    }

    /// Whether the `steady_state` section (Hexput's and Rhai's) runs.
    pub fn runs_steady_state(filter: Option<&str>, args: &[String]) -> bool {
        runs_section("steady_state", filter, args)
    }

    /// Whether the `engine` section runs.
    pub fn runs_engine(filter: Option<&str>, args: &[String]) -> bool {
        runs_section("engine", filter, args)
    }

    /// `<target>/hexput-bench/cache-win.json`: `CARGO_TARGET_DIR`, else the workspace's `target`.
    pub fn report_path() -> PathBuf {
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // `<CARGO_MANIFEST_DIR>/../../target`, spelled without the `..`s.
                let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
                manifest
                    .ancestors()
                    .nth(2)
                    .unwrap_or(manifest)
                    .join("target")
            });
        target.join("hexput-bench").join("cache-win.json")
    }

    pub fn write(path: &Path, json: &Json) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(json).map_err(std::io::Error::other)?;
        std::fs::write(path, text + "\n")
    }

    /// The machine and workload conditions of one run.
    #[derive(Clone)]
    pub struct Conditions {
        pub cpu: String,
        pub parallelism: u64,
        pub os: String,
        pub arch: String,
        pub rustc: String,
        pub git_revision: String,
        pub timestamp: u64,
        pub rounds: usize,
        pub runs_per_connection: usize,
        pub warm_up_runs: usize,
        pub engine_rounds: usize,
        pub engine_runs: usize,
        pub engine_warm_up_runs: usize,
        /// The build actually measured, never assumed.
        pub profile: &'static str,
        /// The TCP+TLS leg's negotiated protocol version and cipher suite.
        pub tls: String,
    }

    impl Conditions {
        pub fn gather(quick: bool, tls: String) -> Self {
            let cpu = std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|text| cpu_model(&text))
                .unwrap_or_else(|| "unknown".to_owned());
            let (rounds, runs_per_connection, warm_up_runs) = if quick {
                (1, QUICK_RUNS_PER_CONNECTION, 0)
            } else {
                (ROUNDS, RUNS_PER_CONNECTION, WARM_UP_RUNS)
            };
            let (engine_rounds, engine_runs, engine_warm_up_runs) = if quick {
                (1, QUICK_ENGINE_RUNS, 0)
            } else {
                (ENGINE_ROUNDS, ENGINE_RUNS, ENGINE_WARM_UP_RUNS)
            };
            Self {
                cpu,
                parallelism: std::thread::available_parallelism()
                    .map_or(0, |count| count.get() as u64),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                rustc: command("rustc", &["--version"]),
                git_revision: command("git", &["rev-parse", "HEAD"]),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs()),
                rounds,
                runs_per_connection,
                warm_up_runs,
                engine_rounds,
                engine_runs,
                engine_warm_up_runs,
                profile: profile(quick, cfg!(debug_assertions)),
                tls,
            }
        }

        /// The machine fingerprint a previous report must match to be compared.
        pub fn fingerprint(&self) -> Json {
            json!({
                "cpu": self.cpu,
                "parallelism": self.parallelism,
                "os": self.os,
                "arch": self.arch,
            })
        }

        pub fn machine(&self) -> Json {
            json!({
                "cpu": self.cpu,
                "parallelism": self.parallelism,
                "os": self.os,
                "arch": self.arch,
                "rustc": self.rustc,
                "git_revision": self.git_revision,
                "timestamp": self.timestamp,
            })
        }

        pub fn workload(&self) -> Json {
            json!({
                "script": crate::workload::RULE,
                "rhai_source": crate::rhai::RHAI_RULE,
                "comparator": COMPARATOR,
                "host_calls_per_run": crate::workload::HOST_CALLS_PER_RUN,
                "students": crate::workload::STUDENTS,
                "concurrency": CONCURRENCY,
                "rounds": self.rounds,
                "runs_per_connection": self.runs_per_connection,
                "warm_up_runs": self.warm_up_runs,
                "engine_rounds": self.engine_rounds,
                "engine_runs": self.engine_runs,
                "engine_warm_up_runs": self.engine_warm_up_runs,
                "budget": "default Resource Budget",
                "legs": self.legs(),
                "profile": self.profile,
            })
        }

        /// Each leg's transport and machine topology, kept apart.
        pub fn legs(&self) -> Json {
            json!({
                "uds": {"transport": "UDS", "topology": UDS_TOPOLOGY},
                "tcp_tls": {
                    "transport": "TCP+TLS",
                    "address": TCP_TLS_ADDRESS,
                    "tls": self.tls_setup(),
                    "topology": TCP_TLS_TOPOLOGY,
                },
                "rhai": {"transport": "UDS", "topology": UDS_TOPOLOGY},
            })
        }

        /// The negotiated parameters and how the client authenticates.
        fn tls_setup(&self) -> String {
            format!("{}, ring provider, server authentication only", self.tls)
        }

        pub fn render(&self) -> String {
            format!(
                "== cache win: conditions ==\n\
                 machine:  {} ({} threads), {}/{}\n\
                 toolchain: {}\n\
                 revision: {}\n\
                 timestamp: {}\n\
                 workload: the pass/fail rule, {} host calls per run, {} students rotating\n\
                 steady state: {} connections per leg, {} round(s) of {} runs each, {} warm-up \
                 runs\n\
                 budget:   default Resource Budget, no overrides — every UDS headline figure runs \
                 with all six dimensions enforced, unadjusted\n\
                 leg uds:     UDS; {}\n\
                 leg tcp_tls: TCP+TLS to {}, {}; {}\n\
                 leg rhai:    UDS; {}\n\
                 comparator: {} (same client, students and verification; the Daemon's `Call` \
                 arguments carry Value Secret holders, the Rhai server's plain values)\n\
                 engine:   {} round(s) of {} in-process runs per driver (the first driver \
                 rotating), {} warm-up runs each\n\
                 profile:  {}\n\
                 variance: p50 and throughput ±{:.0}%, p99 ±{:.0}% (informational, never a gate)",
                self.cpu,
                self.parallelism,
                self.os,
                self.arch,
                self.rustc,
                self.git_revision,
                self.timestamp,
                crate::workload::HOST_CALLS_PER_RUN,
                crate::workload::STUDENTS,
                CONCURRENCY,
                self.rounds,
                self.runs_per_connection,
                self.warm_up_runs,
                UDS_TOPOLOGY,
                TCP_TLS_ADDRESS,
                self.tls_setup(),
                TCP_TLS_TOPOLOGY,
                UDS_TOPOLOGY,
                COMPARATOR,
                self.engine_rounds,
                self.engine_runs,
                self.engine_warm_up_runs,
                self.profile,
                VARIANCE.p50 * 100.0,
                VARIANCE.p99 * 100.0,
            )
        }
    }

    /// The build profile a run is measured under: quick mode is a test build, and a bench run
    /// with debug assertions on is not the optimised `bench` profile.
    fn profile(quick: bool, debug_assertions: bool) -> &'static str {
        match (quick, debug_assertions) {
            (true, _) => "test (quick mode)",
            (false, false) => "bench",
            (false, true) => "unoptimised (debug assertions on)",
        }
    }

    fn cpu_model(cpuinfo: &str) -> Option<String> {
        cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_owned())
        })
    }

    /// The trimmed stdout of a successful command, else `"unknown"`.
    fn command(program: &str, args: &[&str]) -> String {
        std::process::Command::new(program)
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    /// One round's (or the summary's) figures.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Round {
        pub p50_us: f64,
        pub p99_us: f64,
        pub throughput: f64,
    }

    impl Round {
        /// The figures of one round: its latencies and its wall time.
        pub fn of(mut latencies: Vec<Duration>, wall: Duration) -> Self {
            latencies.sort_unstable();
            let us = |duration: Duration| duration.as_secs_f64() * 1e6;
            Self {
                p50_us: us(percentile(&latencies, 50.0)),
                p99_us: us(percentile(&latencies, 99.0)),
                throughput: latencies.len() as f64 / wall.as_secs_f64().max(f64::MIN_POSITIVE),
            }
        }

        fn figures(&self) -> [f64; 3] {
            [self.p50_us, self.p99_us, self.throughput]
        }

        fn json(&self) -> Json {
            json!({
                "p50_us": self.p50_us,
                "p99_us": self.p99_us,
                "throughput_per_sec": self.throughput,
            })
        }

        fn from_json(json: &Json) -> Option<Self> {
            Some(Self {
                p50_us: json.get("p50_us")?.as_f64()?,
                p99_us: json.get("p99_us")?.as_f64()?,
                throughput: json.get("throughput_per_sec")?.as_f64()?,
            })
        }
    }

    /// The nearest-rank percentile of sorted samples: the `ceil(p/100 × n)`-th smallest.
    pub fn percentile(sorted: &[Duration], p: f64) -> Duration {
        assert!(!sorted.is_empty(), "a percentile of no samples");
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[rank.clamp(1, sorted.len()) - 1]
    }

    fn median(mut values: Vec<f64>) -> f64 {
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        if values.len() % 2 == 1 {
            values[middle]
        } else {
            (values[middle - 1] + values[middle]) / 2.0
        }
    }

    /// The relative change from `from` to `to`.
    fn relative(from: f64, to: f64) -> f64 {
        if from == 0.0 {
            if to == 0.0 { 0.0 } else { f64::INFINITY }
        } else {
            (to - from) / from
        }
    }

    /// The median of the rounds, and each figure's largest relative deviation from it.
    #[derive(Debug, PartialEq)]
    pub struct Summary {
        pub median: Round,
        /// The spread, as fractions (not a `Round` of latencies).
        pub spread: [f64; 3],
    }

    impl Summary {
        pub fn of(rounds: &[Round]) -> Self {
            let figure = |index: usize| median(rounds.iter().map(|r| r.figures()[index]).collect());
            let median = Round {
                p50_us: figure(0),
                p99_us: figure(1),
                throughput: figure(2),
            };
            let central = median.figures();
            let mut spread = [0.0_f64; 3];
            for round in rounds {
                for (index, value) in round.figures().into_iter().enumerate() {
                    spread[index] = spread[index].max(relative(central[index], value).abs());
                }
            }
            Self { median, spread }
        }
    }

    const NAMES: [&str; 3] = ["p50", "p99", "throughput"];

    fn limits() -> [f64; 3] {
        [VARIANCE.p50, VARIANCE.p99, VARIANCE.throughput]
    }

    fn verdict(deviation: f64, limit: f64) -> &'static str {
        if deviation.abs() <= limit {
            "within"
        } else {
            "outside"
        }
    }

    pub fn render_steady_state(label: &str, rounds: &[Round], summary: &Summary) -> String {
        let mut text =
            format!("== steady_state: {label}, {CONCURRENCY} concurrent connections ==\n");
        for (index, round) in rounds.iter().enumerate() {
            text += &format!(
                "round {}/{}: p50 {:.1} µs  p99 {:.1} µs  throughput {:.0} runs/s\n",
                index + 1,
                rounds.len(),
                round.p50_us,
                round.p99_us,
                round.throughput
            );
        }
        text += &format!(
            "summary (median of {} round(s)): p50 {:.1} µs  p99 {:.1} µs  throughput {:.0} runs/s\n",
            rounds.len(),
            summary.median.p50_us,
            summary.median.p99_us,
            summary.median.throughput
        );
        let spread: Vec<String> = (0..3)
            .map(|index| {
                format!(
                    "{} ±{:.1}% ({} ±{:.0}%)",
                    NAMES[index],
                    summary.spread[index] * 100.0,
                    verdict(summary.spread[index], limits()[index]),
                    limits()[index] * 100.0
                )
            })
            .collect();
        text += &format!("spread across rounds: {}", spread.join(", "));
        text
    }

    /// `numerator` over `denominator`: how many times the denominator's figure the numerator's is.
    pub fn ratio(numerator: f64, denominator: f64) -> f64 {
        if denominator == 0.0 {
            if numerator == 0.0 { 1.0 } else { f64::INFINITY }
        } else {
            numerator / denominator
        }
    }

    /// Two steady-state summaries side by side: rows p50, p99 and throughput; columns `left`,
    /// `right` and their ratio, labelled by `table`. Both raw figures are always shown.
    pub fn render_side_by_side(table: &Table, left: &Round, right: &Round) -> String {
        let units = ["µs", "µs", "runs/s"];
        let [left_label, right_label, ratio_label] = table.columns;
        let mut text = format!(
            "== steady_state: {} ==\n\
             {:<12}{:>16}{:>16}{:>14}\n",
            table.title, "", left_label, right_label, ratio_label
        );
        let (left, right) = (left.figures(), right.figures());
        for index in 0..3 {
            let quotient = if table.right_over_left {
                ratio(right[index], left[index])
            } else {
                ratio(left[index], right[index])
            };
            text += &format!(
                "{:<12}{:>16}{:>16}{:>14}\n",
                NAMES[index],
                format!("{:.1} {}", left[index], units[index]),
                format!("{:.1} {}", right[index], units[index]),
                format!("{quotient:.2}x"),
            );
        }
        text += table.reading;
        text
    }

    /// The three steady states of one run, each its own: Hexput over UDS, Hexput over TCP+TLS,
    /// and Rhai over UDS.
    pub struct Steady {
        pub hexput: Vec<Round>,
        pub hexput_summary: Summary,
        pub tcp_tls: Vec<Round>,
        pub tcp_tls_summary: Summary,
        pub rhai: Vec<Round>,
        pub rhai_summary: Summary,
    }

    impl Steady {
        /// This run's three summaries against the previous report, each against its own leg.
        pub fn compare(&self, previous: Option<&str>, conditions: &Conditions) -> Comparison {
            compare(
                previous,
                conditions,
                &self.hexput_summary,
                &self.tcp_tls_summary,
                &self.rhai_summary,
            )
        }

        /// Checks, in quick mode, that this run's own report compares each leg with itself:
        /// written by `to_json` and read back by `Steady::compare`, every delta's previous figures
        /// are that leg's median and no other's.
        pub fn check_round_trip(&self, conditions: &Conditions) {
            let json = to_json(conditions, self, None).to_string();
            let Comparison::Previous {
                hexput,
                tcp_tls,
                rhai,
            } = self.compare(Some(&json), conditions)
            else {
                panic!("this run's own report is not comparable with itself");
            };
            // Equal up to the JSON float parse, which may round the last bit.
            let same = |delta: &Delta, summary: &Summary, leg: &str| {
                let (read, own) = (delta.previous.figures(), summary.median.figures());
                assert!(
                    (0..3)
                        .all(|index| (read[index] - own[index]).abs() <= own[index].abs() * 1e-12),
                    "the {leg} leg compared with {read:?}, not its own {own:?}"
                );
            };
            same(&hexput, &self.hexput_summary, "UDS");
            same(&tcp_tls, &self.tcp_tls_summary, "TCP+TLS");
            same(&rhai, &self.rhai_summary, "Rhai");
        }
    }

    /// The transport table: Hexput over UDS on the left, over TCP+TLS on the right.
    pub fn render_transport(uds: &Summary, tcp_tls: &Summary) -> String {
        render_side_by_side(&TRANSPORT_TABLE, &uds.median, &tcp_tls.median)
    }

    /// The `engine` section's figures: each driver's mean ns/run (one round's, or the median of
    /// the rounds).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct EngineFigures {
        pub hexput_unmetered: f64,
        pub hexput_enforced: f64,
        pub rhai_engine: f64,
    }

    impl EngineFigures {
        /// Each driver's median across the rounds.
        pub fn median(rounds: &[Self]) -> Self {
            Self {
                hexput_unmetered: median(rounds.iter().map(|r| r.hexput_unmetered).collect()),
                hexput_enforced: median(rounds.iter().map(|r| r.hexput_enforced).collect()),
                rhai_engine: median(rounds.iter().map(|r| r.rhai_engine).collect()),
            }
        }

        /// Each driver's largest relative deviation across `rounds` from `self` (their median),
        /// as fractions, in field order — this section's own spread, beside the steady state's.
        pub fn spread(&self, rounds: &[Self]) -> [f64; 3] {
            let central = [
                self.hexput_unmetered,
                self.hexput_enforced,
                self.rhai_engine,
            ];
            let mut spread = [0.0_f64; 3];
            for round in rounds {
                let figures = [
                    round.hexput_unmetered,
                    round.hexput_enforced,
                    round.rhai_engine,
                ];
                for index in 0..3 {
                    spread[index] =
                        spread[index].max(relative(central[index], figures[index]).abs());
                }
            }
            spread
        }

        /// The budget enforcement overhead: enforced − unmetered in ns/run, and that relative to
        /// unmetered in percent.
        pub fn overhead(&self) -> (f64, f64) {
            let ns = self.hexput_enforced - self.hexput_unmetered;
            (
                ns,
                relative(self.hexput_unmetered, self.hexput_enforced) * 100.0,
            )
        }

        /// The overhead line.
        pub fn overhead_line(&self) -> String {
            let (ns, pct) = self.overhead();
            if ns >= 0.0 {
                format!("budget enforcement overhead: +{ns:.1} ns/run (+{pct:.1}%)")
            } else {
                // Enforcement cannot make a run faster; a negative difference is noise.
                format!(
                    "budget enforcement overhead: -{:.1} ns/run (-{:.1}%), within measurement noise",
                    -ns, -pct
                )
            }
        }

        fn json(&self) -> Json {
            let (ns, pct) = self.overhead();
            json!({
                "hexput_unmetered_ns": self.hexput_unmetered,
                "hexput_enforced_ns": self.hexput_enforced,
                "rhai_engine_ns": self.rhai_engine,
                "budget_overhead_ns": ns,
                "budget_overhead_pct": pct,
            })
        }
    }

    pub fn render_engine(rounds: &[EngineFigures], summary: &EngineFigures) -> String {
        let mut text = String::from(
            "== engine: the rule in-process, host calls answered natively (no wire) ==\n",
        );
        for (index, round) in rounds.iter().enumerate() {
            text += &format!(
                "round {}/{}: hexput_unmetered {:.0} ns/run  hexput_enforced {:.0} ns/run  \
                 rhai_engine {:.0} ns/run\n",
                index + 1,
                rounds.len(),
                round.hexput_unmetered,
                round.hexput_enforced,
                round.rhai_engine
            );
        }
        text += &format!(
            "summary (median of {} round(s)): hexput_unmetered {:.0} ns/run  hexput_enforced \
             {:.0} ns/run  rhai_engine {:.0} ns/run\n",
            rounds.len(),
            summary.hexput_unmetered,
            summary.hexput_enforced,
            summary.rhai_engine
        );
        let [unmetered, enforced, rhai] = summary.spread(rounds);
        text += &format!(
            "spread across rounds: hexput_unmetered ±{:.1}%, hexput_enforced ±{:.1}%, \
             rhai_engine ±{:.1}% (an overhead smaller than these is within the noise)\n",
            unmetered * 100.0,
            enforced * 100.0,
            rhai * 100.0
        );
        text += &summary.overhead_line();
        text += "\n  covers: the Executor's synchronous enforcement — CPU-time slicing and \
                 charging, the memory and allocation ceilings, the argument depth and frame \
                 measure, the RPC-call/side-effect count and the output-size measure; not the \
                 async hops, the arguments' wire conversion or the capability lookups, which are \
                 not budgeting\n  the UDS headline figures above run with the full default \
                 budget and are not adjusted by this line";
        text
    }

    /// One side's figures against the previous run's.
    #[derive(Debug, PartialEq)]
    pub struct Delta {
        /// The previous run's median figures.
        pub previous: Round,
        /// Each figure's relative change from the previous run.
        pub change: [f64; 3],
    }

    impl Delta {
        fn of(previous: Round, summary: &Summary) -> Self {
            let (before, after) = (previous.figures(), summary.median.figures());
            Self {
                previous,
                change: [0, 1, 2].map(|index| relative(before[index], after[index])),
            }
        }

        fn render(&self, label: &str) -> String {
            let figures = self.previous.figures();
            let lines: Vec<String> = (0..3)
                .map(|index| {
                    format!(
                        "  {}: previous {:.1}, change {:+.1}% ({} ±{:.0}%)",
                        NAMES[index],
                        figures[index],
                        self.change[index] * 100.0,
                        verdict(self.change[index], limits()[index]),
                        limits()[index] * 100.0
                    )
                })
                .collect();
            format!("{label}:\n{}", lines.join("\n"))
        }
    }

    /// This run against the previous report.
    #[derive(Debug, PartialEq)]
    pub enum Comparison {
        /// No previous file, an unreadable one, or one from another machine or workload.
        NoPrevious,
        Previous {
            hexput: Delta,
            tcp_tls: Delta,
            rhai: Delta,
        },
    }

    impl Comparison {
        pub fn render(&self) -> String {
            match self {
                Self::NoPrevious => "against the previous run: no comparable previous run".into(),
                Self::Previous {
                    hexput,
                    tcp_tls,
                    rhai,
                } => format!(
                    "against the previous run on this machine:\n{}\n{}\n{}",
                    hexput.render(HEXPUT_SIDE),
                    tcp_tls.render(TCP_TLS_SIDE),
                    rhai.render(RHAI_SIDE)
                ),
            }
        }
    }

    /// Compares the three summaries with the previous report's text, when it parses, its machine
    /// has this run's fingerprint, its workload is this run's and it has all three sections — a
    /// Story 4.5 report, with no `tcp_tls_steady_state`, is not comparable.
    pub fn compare(
        previous: Option<&str>,
        conditions: &Conditions,
        hexput: &Summary,
        tcp_tls: &Summary,
        rhai: &Summary,
    ) -> Comparison {
        let Some(json) = previous.and_then(|text| serde_json::from_str::<Json>(text).ok()) else {
            return Comparison::NoPrevious;
        };
        let fingerprint = conditions.fingerprint();
        let machine = json.get("machine");
        let same_machine = ["cpu", "parallelism", "os", "arch"]
            .iter()
            .all(|key| machine.and_then(|machine| machine.get(key)) == fingerprint.get(key));
        // A previous run of another workload (script, Rhai source and comparator, concurrency,
        // rounds, runs, profile) is not comparable either — a Story 4.4 report among them.
        let same_workload = json.get("workload") == Some(&conditions.workload());
        let summary = |section: &str| {
            json.get(section)
                .and_then(|steady| steady.get("summary"))
                .and_then(Round::from_json)
        };
        match (
            summary("steady_state"),
            summary("tcp_tls_steady_state"),
            summary("rhai_steady_state"),
        ) {
            (Some(previous_hexput), Some(previous_tcp_tls), Some(previous_rhai))
                if same_machine && same_workload =>
            {
                Comparison::Previous {
                    hexput: Delta::of(previous_hexput, hexput),
                    tcp_tls: Delta::of(previous_tcp_tls, tcp_tls),
                    rhai: Delta::of(previous_rhai, rhai),
                }
            }
            _ => Comparison::NoPrevious,
        }
    }

    fn steady_json(rounds: &[Round], summary: &Summary) -> Json {
        json!({
            "rounds": rounds.iter().map(Round::json).collect::<Vec<_>>(),
            "summary": summary.median.json(),
            "spread": {
                "p50": summary.spread[0],
                "p99": summary.spread[1],
                "throughput": summary.spread[2],
            },
        })
    }

    /// The report: the conditions, the three steady states (each its own section) and — `null` when the section was
    /// filtered out — the engine figures with the overhead.
    pub fn to_json(
        conditions: &Conditions,
        steady: &Steady,
        engine: Option<&EngineFigures>,
    ) -> Json {
        json!({
            "machine": conditions.machine(),
            "workload": conditions.workload(),
            "variance": {
                "p50": VARIANCE.p50,
                "p99": VARIANCE.p99,
                "throughput": VARIANCE.throughput,
            },
            "steady_state": steady_json(&steady.hexput, &steady.hexput_summary),
            "tcp_tls_steady_state": steady_json(&steady.tcp_tls, &steady.tcp_tls_summary),
            "rhai_steady_state": steady_json(&steady.rhai, &steady.rhai_summary),
            "engine": engine.map_or(Json::Null, EngineFigures::json),
        })
    }

    /// The report's own checks, run in quick mode. A bench target is built without `--test`
    /// (`harness = false`), so `#[cfg(test)]` code would never run; these run under
    /// `cargo test` through the bench's quick mode instead.
    pub fn self_check() {
        let args = |list: &[&str]| -> Vec<String> {
            std::iter::once("cache")
                .chain(list.iter().copied())
                .map(str::to_owned)
                .collect()
        };

        // Quick mode and the filter.
        assert!(is_quick(&args(&[])));
        assert!(!is_quick(&args(&["--bench"])));
        assert!(is_quick(&args(&["--bench", "--test"])));
        assert_eq!(filter(&args(&["--bench"])), None);
        assert_eq!(filter(&args(&["--bench", "warm"])).as_deref(), Some("warm"));
        assert_eq!(
            filter(&args(&["--save-baseline", "main", "--bench", "cold"])).as_deref(),
            Some("cold")
        );
        assert_eq!(
            filter(&args(&["--measurement-time=3", "steady"])).as_deref(),
            Some("steady")
        );
        assert!(runs_steady_state(None, &args(&[])));
        assert!(runs_steady_state(Some("steady"), &args(&[])));
        assert!(runs_steady_state(Some("steady_state"), &args(&[])));
        assert!(!runs_steady_state(Some("warm"), &args(&[])));
        assert!(!runs_steady_state(Some("cache_win"), &args(&[])));
        // `-- remote_path` runs only the two TCP+TLS Criterion figures: no hand-timed section.
        assert!(!runs_steady_state(Some("remote_path"), &args(&[])));
        assert!(!runs_engine(Some("remote_path"), &args(&[])));
        assert!(!runs_steady_state(Some("tcp_tls"), &args(&[])));
        assert!(!runs_steady_state(None, &args(&["--list"])));
        assert!(!runs_steady_state(None, &args(&["--profile-time", "5"])));
        assert!(!runs_steady_state(None, &args(&["--profile-time=5"])));

        // Nearest-rank percentiles.
        let samples: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        assert_eq!(percentile(&samples, 50.0), Duration::from_micros(50));
        assert_eq!(percentile(&samples, 99.0), Duration::from_micros(99));
        let few: Vec<Duration> = (1..=3).map(Duration::from_micros).collect();
        assert_eq!(percentile(&few, 50.0), Duration::from_micros(2));
        assert_eq!(percentile(&few, 99.0), Duration::from_micros(3));
        assert_eq!(percentile(&few[..1], 50.0), Duration::from_micros(1));
        let round = Round::of(
            vec![Duration::from_micros(30), Duration::from_micros(10)],
            Duration::from_secs(1),
        );
        assert_eq!(round.p50_us, 10.0);
        assert_eq!(round.p99_us, 30.0);
        assert_eq!(round.throughput, 2.0);

        // The median and the spread.
        let rounds = [100.0, 90.0, 110.0, 100.0, 104.0].map(|p50_us| Round {
            p50_us,
            p99_us: p50_us * 2.0,
            throughput: 1000.0,
        });
        let summary = Summary::of(&rounds);
        assert_eq!(summary.median.p50_us, 100.0);
        assert_eq!(summary.median.p99_us, 200.0);
        assert!((summary.spread[0] - 0.10).abs() < 1e-9);
        assert_eq!(summary.spread[2], 0.0);
        assert_eq!(verdict(summary.spread[0] - 1e-12, VARIANCE.p50), "within");
        assert_eq!(verdict(0.11, VARIANCE.p50), "outside");
        assert_eq!(verdict(-0.15, VARIANCE.p99), "within");
        let even = Summary::of(&rounds[..2]);
        assert_eq!(even.median.p50_us, 95.0);

        // The comparison: same machine, another machine, no file, a corrupt file.
        let conditions = Conditions {
            cpu: "a test cpu".into(),
            parallelism: 8,
            os: "linux".into(),
            arch: "x86_64".into(),
            rustc: "rustc 1.0".into(),
            git_revision: "abc".into(),
            timestamp: 1,
            rounds: ROUNDS,
            runs_per_connection: RUNS_PER_CONNECTION,
            warm_up_runs: WARM_UP_RUNS,
            engine_rounds: ENGINE_ROUNDS,
            engine_runs: ENGINE_RUNS,
            engine_warm_up_runs: ENGINE_WARM_UP_RUNS,
            profile: "bench",
            tls: "TLSv1_3, TLS13_AES_256_GCM_SHA384".into(),
        };
        // Rhai's rounds: half Hexput's latency, twice its throughput.
        let rhai_rounds = rounds.map(|round| Round {
            p50_us: round.p50_us / 2.0,
            p99_us: round.p99_us / 2.0,
            throughput: round.throughput * 2.0,
        });
        let rhai_summary = Summary::of(&rhai_rounds);
        // The TCP+TLS rounds: one and a half times Hexput's UDS latency, 80% of its throughput.
        let tls_rounds = rounds.map(|round| Round {
            p50_us: round.p50_us * 1.5,
            p99_us: round.p99_us * 1.5,
            throughput: round.throughput * 0.8,
        });
        let tls_summary = Summary::of(&tls_rounds);
        let steady_of = |hexput: &[Round], tcp_tls: &[Round], rhai: &[Round]| Steady {
            hexput: hexput.to_vec(),
            hexput_summary: Summary::of(hexput),
            tcp_tls: tcp_tls.to_vec(),
            tcp_tls_summary: Summary::of(tcp_tls),
            rhai: rhai.to_vec(),
            rhai_summary: Summary::of(rhai),
        };
        let steady = steady_of(&rounds, &tls_rounds, &rhai_rounds);
        let engine = EngineFigures {
            hexput_unmetered: 1000.0,
            hexput_enforced: 1100.0,
            rhai_engine: 800.0,
        };
        let previous = to_json(&conditions, &steady, Some(&engine)).to_string();
        let faster = Summary::of(&rounds.map(|round| Round {
            p50_us: round.p50_us * 1.2,
            ..round
        }));
        // Rhai's figures are compared too: its p99 moved by 30% here.
        let rhai_moved = Summary::of(&rhai_rounds.map(|round| Round {
            p99_us: round.p99_us * 1.3,
            ..round
        }));
        // And the TCP+TLS leg's, on its own: its throughput fell by 25% here.
        let tls_moved = Summary::of(&tls_rounds.map(|round| Round {
            throughput: round.throughput * 0.75,
            ..round
        }));
        match compare(
            Some(&previous),
            &conditions,
            &faster,
            &tls_moved,
            &rhai_moved,
        ) {
            Comparison::Previous {
                hexput,
                tcp_tls,
                rhai,
            } => {
                assert_eq!(hexput.previous, summary.median);
                assert!((hexput.change[0] - 0.2).abs() < 1e-9);
                assert_eq!(hexput.change[2], 0.0);
                assert_eq!(rhai.previous, rhai_summary.median);
                assert_eq!(rhai.change[0], 0.0);
                assert!((rhai.change[1] - 0.3).abs() < 1e-9);
                // The TCP+TLS delta is against the previous TCP+TLS summary, never the UDS one.
                assert_eq!(tcp_tls.previous, tls_summary.median);
                assert_eq!(tcp_tls.change[0], 0.0);
                assert!((tcp_tls.change[2] + 0.25).abs() < 1e-9);
                let text = compare(
                    Some(&previous),
                    &conditions,
                    &faster,
                    &tls_moved,
                    &rhai_moved,
                )
                .render();
                assert!(text.contains("p50: previous 100.0, change +20.0% (outside ±10%)"));
                assert!(text.contains("throughput: previous 1000.0, change +0.0% (within ±10%)"));
                assert!(text.contains(&format!(
                    "{RHAI_SIDE}:\n  p50: previous 50.0, change +0.0% (within ±10%)\n  \
                     p99: previous 100.0, change +30.0% (outside ±20%)"
                )));
                assert!(text.contains(&format!(
                    "{TCP_TLS_SIDE}:\n  p50: previous 150.0, change +0.0% (within ±10%)\n  \
                     p99: previous 300.0, change +0.0% (within ±20%)\n  \
                     throughput: previous 800.0, change -25.0% (outside ±10%)"
                )));
                // Ordered UDS, TCP+TLS, Rhai — the order the steady states run in.
                let at = |label: &str| text.find(&format!("{label}:\n")).expect(label);
                assert!(at(HEXPUT_SIDE) < at(TCP_TLS_SIDE) && at(TCP_TLS_SIDE) < at(RHAI_SIDE));
            }
            Comparison::NoPrevious => panic!("the same machine is comparable"),
        }
        let other = Conditions {
            cpu: "another cpu".into(),
            ..conditions.clone()
        };
        let unchanged = |conditions: &Conditions, text: Option<&str>| {
            compare(text, conditions, &summary, &tls_summary, &rhai_summary)
        };
        assert_eq!(unchanged(&other, Some(&previous)), Comparison::NoPrevious);
        assert_eq!(unchanged(&other, None), Comparison::NoPrevious);
        assert_eq!(unchanged(&other, Some("{not json")), Comparison::NoPrevious);
        assert_eq!(
            unchanged(&other, Some("{\"machine\": 3}")),
            Comparison::NoPrevious
        );
        // Corrupt or partial reports from this very machine are not comparable either.
        assert_eq!(
            unchanged(&conditions, Some("{\"machine\": 3}")),
            Comparison::NoPrevious
        );
        for section in ["steady_state", "tcp_tls_steady_state", "rhai_steady_state"] {
            let mut partial = to_json(&conditions, &steady, Some(&engine));
            partial[section]
                .as_object_mut()
                .expect("a steady-state object")
                .remove("summary");
            assert_eq!(
                unchanged(&conditions, Some(&partial.to_string())),
                Comparison::NoPrevious,
                "{section} without a summary"
            );
            let mut garbled = to_json(&conditions, &steady, Some(&engine));
            garbled[section]["summary"]["p99_us"] = json!("fast");
            assert_eq!(
                unchanged(&conditions, Some(&garbled.to_string())),
                Comparison::NoPrevious,
                "{section} with a garbled summary"
            );
        }
        // A Story 4.4 report — no comparator fields, no Rhai summary — is not comparable.
        let mut old = to_json(&conditions, &steady, None);
        let workload = old["workload"].as_object_mut().expect("a workload object");
        for key in ["rhai_source", "comparator", "engine_rounds", "engine_runs"] {
            workload.remove(key);
        }
        old.as_object_mut()
            .expect("a report object")
            .remove("rhai_steady_state");
        assert_eq!(
            unchanged(&conditions, Some(&old.to_string())),
            Comparison::NoPrevious
        );
        // A Story 4.5 report — one `transport` string instead of `legs`, no TCP+TLS section —
        // is not comparable either; nor is a report that lacks only the TCP+TLS section.
        let mut story_4_5 = to_json(&conditions, &steady, Some(&engine));
        let workload = story_4_5["workload"]
            .as_object_mut()
            .expect("a workload object");
        workload.remove("legs");
        workload.insert(
            "transport".into(),
            json!("UDS, Daemon in-process on its own runtime"),
        );
        story_4_5
            .as_object_mut()
            .expect("a report object")
            .remove("tcp_tls_steady_state");
        assert_eq!(
            unchanged(&conditions, Some(&story_4_5.to_string())),
            Comparison::NoPrevious
        );
        let mut without_tls = to_json(&conditions, &steady, Some(&engine));
        without_tls
            .as_object_mut()
            .expect("a report object")
            .remove("tcp_tls_steady_state");
        assert_eq!(
            unchanged(&conditions, Some(&without_tls.to_string())),
            Comparison::NoPrevious
        );
        // Another negotiated TLS suite is another workload.
        let other_suite = Conditions {
            tls: "TLSv1_3, TLS13_CHACHA20_POLY1305_SHA256".into(),
            ..conditions.clone()
        };
        assert_eq!(
            unchanged(&other_suite, Some(&previous)),
            Comparison::NoPrevious
        );
        // A report whose engine section was filtered out still compares its steady states.
        let without_engine = to_json(&conditions, &steady, None);
        assert_eq!(without_engine["engine"], Json::Null);
        assert!(matches!(
            unchanged(&conditions, Some(&without_engine.to_string())),
            Comparison::Previous { .. }
        ));
        // Nor is a run of another workload on this machine.
        let fewer = Conditions {
            runs_per_connection: 10,
            ..conditions.clone()
        };
        assert_eq!(unchanged(&fewer, Some(&previous)), Comparison::NoPrevious);
        let shorter_engine = Conditions {
            engine_runs: QUICK_ENGINE_RUNS,
            ..conditions.clone()
        };
        assert_eq!(
            unchanged(&shorter_engine, Some(&previous)),
            Comparison::NoPrevious
        );
        let unoptimised = Conditions {
            profile: profile(false, true),
            ..fewer
        };
        assert_eq!(
            unchanged(&unoptimised, Some(&previous)),
            Comparison::NoPrevious
        );

        // The report's comparator fields and engine figures.
        let report: Json = serde_json::from_str(&previous).expect("the report parses");
        assert_eq!(
            report["workload"]["rhai_source"],
            json!(crate::rhai::RHAI_RULE)
        );
        assert_eq!(report["workload"]["comparator"], json!(COMPARATOR));
        assert_eq!(report["workload"]["engine_rounds"], json!(ENGINE_ROUNDS));
        assert_eq!(report["workload"]["engine_runs"], json!(ENGINE_RUNS));
        assert_eq!(
            report["rhai_steady_state"]["summary"]["p50_us"],
            json!(50.0)
        );
        assert_eq!(
            report["rhai_steady_state"]["rounds"]
                .as_array()
                .map(Vec::len),
            Some(5)
        );
        assert_eq!(
            report["engine"],
            json!({
                "hexput_unmetered_ns": 1000.0,
                "hexput_enforced_ns": 1100.0,
                "rhai_engine_ns": 800.0,
                "budget_overhead_ns": 100.0,
                "budget_overhead_pct": 10.0,
            })
        );

        // The three steady states are separate sections; no figure combines two legs.
        let sections = report.as_object().expect("a report object");
        for section in ["steady_state", "tcp_tls_steady_state", "rhai_steady_state"] {
            assert!(sections.contains_key(section), "{section}");
        }
        assert_eq!(report["steady_state"]["summary"]["p50_us"], json!(100.0));
        assert_eq!(
            report["tcp_tls_steady_state"]["summary"],
            json!({"p50_us": 150.0, "p99_us": 300.0, "throughput_per_sec": 800.0})
        );
        assert_eq!(
            report["tcp_tls_steady_state"]["rounds"]
                .as_array()
                .map(Vec::len),
            Some(5)
        );
        assert_eq!(
            report["tcp_tls_steady_state"]["spread"]["throughput"],
            json!(0.0)
        );

        // Each leg's transport and topology, in the workload and the rendered conditions.
        let legs = &report["workload"]["legs"];
        assert_eq!(
            report["workload"].get("transport"),
            None,
            "`legs` replaces it"
        );
        assert_eq!(
            legs["uds"],
            json!({"transport": "UDS", "topology": UDS_TOPOLOGY})
        );
        assert_eq!(legs["rhai"], legs["uds"]);
        assert_eq!(
            legs["tcp_tls"],
            json!({
                "transport": "TCP+TLS",
                "address": "127.0.0.1 (loopback)",
                "tls": "TLSv1_3, TLS13_AES_256_GCM_SHA384, ring provider, server authentication only",
                "topology": TCP_TLS_TOPOLOGY,
            })
        );
        assert!(TCP_TLS_TOPOLOGY.contains("not network latency"));
        let rendered_conditions = conditions.render();
        assert!(rendered_conditions.contains(&format!("leg uds:     UDS; {UDS_TOPOLOGY}\n")));
        assert!(rendered_conditions.contains(&format!(
            "leg tcp_tls: TCP+TLS to 127.0.0.1 (loopback), TLSv1_3, \
             TLS13_AES_256_GCM_SHA384, ring provider, server authentication only; \
             {TCP_TLS_TOPOLOGY}\n"
        )));
        assert!(rendered_conditions.contains(&format!("leg rhai:    UDS; {UDS_TOPOLOGY}\n")));

        // The side-by-side table and its ratio.
        assert_eq!(ratio(150.0, 100.0), 1.5);
        assert_eq!(ratio(0.0, 0.0), 1.0);
        assert_eq!(ratio(1.0, 0.0), f64::INFINITY);
        let table = render_side_by_side(&HEXPUT_RHAI_TABLE, &summary.median, &rhai_summary.median);
        assert!(table.starts_with(
            "== steady_state: side by side (median of rounds, same client and concurrency) ==\n"
        ));
        assert!(
            table.contains("Hexput") && table.contains("Rhai") && table.contains("Hexput/Rhai")
        );
        assert!(
            table.ends_with("a lower throughput (worse)"),
            "the ratio's reading"
        );
        let row = |name: &str| {
            table
                .lines()
                .find(|line| line.starts_with(name))
                .unwrap_or_else(|| panic!("a `{name}` row"))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(row("p50"), "p50 100.0 µs 50.0 µs 2.00x");
        assert_eq!(row("p99"), "p99 200.0 µs 100.0 µs 2.00x");
        assert_eq!(
            row("throughput"),
            "throughput 1000.0 runs/s 2000.0 runs/s 0.50x"
        );

        // The transport table: UDS and TCP+TLS raw, then TCP+TLS over UDS.
        let transport = render_transport(&summary, &tls_summary);
        let header = transport
            .lines()
            .nth(1)
            .expect("a header row")
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(header, ["UDS", "TCP+TLS", "TCP+TLS/UDS"]);
        assert!(transport.starts_with("== steady_state: transport ("));
        assert!(transport.ends_with("no figure includes the TCP connect or the TLS handshake"));
        let transport_row = |name: &str| {
            transport
                .lines()
                .find(|line| line.starts_with(name))
                .unwrap_or_else(|| panic!("a `{name}` row"))
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(transport_row("p50"), "p50 100.0 µs 150.0 µs 1.50x");
        assert_eq!(transport_row("p99"), "p99 200.0 µs 300.0 µs 1.50x");
        assert_eq!(
            transport_row("throughput"),
            "throughput 1000.0 runs/s 800.0 runs/s 0.80x"
        );

        // The overhead arithmetic and its line.
        assert_eq!(engine.overhead(), (100.0, 10.0));
        assert_eq!(
            engine.overhead_line(),
            "budget enforcement overhead: +100.0 ns/run (+10.0%)"
        );
        let noisy = EngineFigures {
            hexput_enforced: 990.0,
            ..engine
        };
        assert_eq!(noisy.overhead(), (-10.0, -1.0));
        assert!(
            noisy
                .overhead_line()
                .starts_with("budget enforcement overhead: -10.0 ns/run (-1.0%)")
        );
        let engine_rounds = [
            engine,
            EngineFigures {
                hexput_unmetered: 900.0,
                hexput_enforced: 1300.0,
                rhai_engine: 700.0,
            },
            EngineFigures {
                hexput_unmetered: 1200.0,
                hexput_enforced: 1000.0,
                rhai_engine: 900.0,
            },
        ];
        let median_engine = EngineFigures::median(&engine_rounds);
        assert_eq!(median_engine, engine, "each driver's own median");
        let rendered = render_engine(&engine_rounds, &median_engine);
        assert!(rendered.contains("round 3/3"));
        // Unmetered 900/1000/1200 around 1000: ±20%; enforced 1100/1300/1000 around 1100.
        let spread = median_engine.spread(&engine_rounds);
        assert!((spread[0] - 0.2).abs() < 1e-9, "{spread:?}");
        assert!((spread[1] - 200.0 / 1100.0).abs() < 1e-9, "{spread:?}");
        assert!(rendered.contains("spread across rounds: hexput_unmetered ±20.0%"));
        assert!(rendered.contains("budget enforcement overhead: +100.0 ns/run (+10.0%)"));
        assert!(rendered.contains("full default budget"));

        // The engine filter rule.
        assert!(runs_engine(None, &args(&[])));
        assert!(runs_engine(Some("engine"), &args(&[])));
        assert!(runs_engine(Some("eng"), &args(&[])));
        assert!(!runs_engine(Some("steady"), &args(&[])));
        assert!(!runs_engine(Some("rhai_compare"), &args(&[])));
        assert!(!runs_engine(None, &args(&["--list"])));
        assert!(!runs_engine(None, &args(&["--profile-time", "5"])));
        assert!(!runs_steady_state(Some("engine"), &args(&[])));

        // The wire↔Dynamic conversion and the Rhai rule.
        crate::rhai::self_check();

        // The profile.
        assert_eq!(profile(false, false), "bench");
        assert_eq!(profile(true, false), "test (quick mode)");
        assert_eq!(profile(true, true), "test (quick mode)");
        assert_eq!(profile(false, true), "unoptimised (debug assertions on)");
        assert_eq!(
            Comparison::NoPrevious.render(),
            "against the previous run: no comparable previous run"
        );

        // The CPU model.
        assert_eq!(
            cpu_model("processor\t: 0\nmodel name\t: Some CPU @ 3GHz\n").as_deref(),
            Some("Some CPU @ 3GHz")
        );
        assert_eq!(cpu_model("processor\t: 0\n"), None);
    }
}
