//! Story 4.4: measure the cache win (FR-5, SM-1) — parse a Script once, execute it many times.
//!
//! The bench starts a real Daemon in-process (`hexput_daemon::run_with` on its own thread and
//! its own Tokio runtime) over a real Unix Domain Socket, and drives it as a Backend would: each
//! client is a blocking `UnixStream` that sends `Init` with blanket grants for the rule's three
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
//! The three Criterion figures use one connection with one execution in flight.
//!
//! **Stated variance:** p50 and throughput within ±10%, p99 within ±20%. The bench prints whether
//! this run's spread across rounds is within it, and — when the previous report was made on a
//! machine with the same fingerprint (CPU model, parallelism, OS, arch) — whether each figure is
//! within it of the previous run's. Criterion compares its own figures against its previous run
//! with a 10% noise threshold. All of this is informational: no figure is a gate.
//!
//! The machine and workload conditions are printed before any figure and, after a full run,
//! written with the steady-state figures to `<target>/hexput-bench/cache-win.json`.
//!
//! **Quick mode:** without `--bench` (that is, under `cargo test`), Criterion runs each figure
//! once and `steady_state` runs one round of 20 runs per connection with no warm-up. Every result
//! is still checked, the report module's own checks run, and no report is written or compared.
//!
//! A positional filter goes to Criterion; `steady_state` runs only with no filter or a filter
//! that is a substring of `steady_state`.

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

#[cfg(not(unix))]
fn main() {
    println!("the cache bench needs a Unix Domain Socket; skipped on this platform");
}

#[cfg(unix)]
fn main() {
    use std::time::Instant;

    use criterion::Criterion;

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
    let conditions = report::Conditions::gather(quick);
    println!("{}", conditions.render());

    let daemon = daemon::Daemon::start();
    let students = workload::students();

    let mut criterion = Criterion::default().without_plots().configure_from_args();
    {
        let mut client = daemon.connect();
        workload::init(&mut client);
        let handle = workload::register(&mut client);

        let mut group = criterion.benchmark_group("cache_win");
        group.noise_threshold(report::VARIANCE.p50);

        let mut next = 0usize;
        group.bench_function("cold_first_run", |bencher| {
            bencher.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let student = &students[next % students.len()];
                    next += 1;
                    let started = Instant::now();
                    let handle = workload::register(&mut client);
                    let registered = started.elapsed();
                    let payload = workload::cached_payload(&handle, student);
                    total += registered + workload::run_cached(&mut client, payload, student);
                    workload::unregister(&mut client, &handle);
                }
                total
            });
        });
        group.bench_function("warm_cache_hit", |bencher| {
            bencher.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let student = &students[next % students.len()];
                    next += 1;
                    let payload = workload::cached_payload(&handle, student);
                    total += workload::run_cached(&mut client, payload, student);
                }
                total
            });
        });
        group.bench_function("direct_execution", |bencher| {
            bencher.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
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
    criterion.final_summary();

    if report::runs_steady_state(filter.as_deref(), &args) {
        let (rounds, runs, warm_up) = if quick {
            (1, QUICK_RUNS_PER_CONNECTION, 0)
        } else {
            (ROUNDS, RUNS_PER_CONNECTION, WARM_UP_RUNS)
        };
        let measured = steady::measure(&daemon, rounds, runs, warm_up);
        let summary = report::Summary::of(&measured);
        println!("{}", report::render_steady_state(&measured, &summary));
        if !quick {
            let comparison = report::compare(previous.as_deref(), &conditions, &summary);
            println!("{}", comparison.render());
            let json = report::to_json(&conditions, &measured, &summary);
            match report::write(&path, &json) {
                Ok(()) => println!("report written to {}", path.display()),
                Err(error) => eprintln!("could not write {}: {error}", path.display()),
            }
        }
    } else {
        println!("steady_state skipped by the filter");
    }

    daemon.stop();
}

/// The Daemon under measurement: in-process, on its own thread, over a socket in a fresh temp dir.
#[cfg(unix)]
mod daemon {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use hexput_daemon::{Discovery, run_with};

    use crate::wire::Client;

    pub struct Daemon {
        stop: tokio::sync::oneshot::Sender<()>,
        thread: std::thread::JoinHandle<(ExitCode, String)>,
        dir: PathBuf,
    }

    impl Daemon {
        /// Starts a Daemon whose System Config names a socket inside a fresh temp dir, logging at
        /// `error` so logging stays off the measured path.
        pub fn start() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let dir = std::env::temp_dir()
                .join(format!("hexput-bench-{}-{nanos:08x}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temporary directory");
            let config = dir.join("config.toml");
            std::fs::write(
                &config,
                format!(
                    "log_level = \"error\"\n\n[transport.uds]\npath = {:?}\n",
                    dir.join("d.sock").to_str().expect("a UTF-8 temp path")
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
            Self { stop, thread, dir }
        }

        /// A client connection, waiting until the Daemon listens.
        pub fn connect(&self) -> Client {
            let socket = self.dir.join("d.sock");
            let started = Instant::now();
            loop {
                match std::os::unix::net::UnixStream::connect(&socket) {
                    Ok(stream) => return Client::new(stream),
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

/// A blocking Backend client: framed MessagePack envelopes over the socket.
#[cfg(unix)]
mod wire {
    use std::io::{BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use hexput_port::{CorrelationId, Envelope, MessageType, Value, decode, encode, encode_frame};

    pub struct Client {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
        next_id: u64,
    }

    impl Client {
        pub fn new(stream: UnixStream) -> Self {
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("a read timeout");
            let reader = BufReader::new(stream.try_clone().expect("a second handle"));
            Self {
                reader,
                writer: stream,
                next_id: 1,
            }
        }

        /// A request envelope with this connection's next correlation id.
        pub fn envelope(&mut self, message_type: MessageType, payload: Value) -> Envelope<Value> {
            let id = self.next_id;
            self.next_id += 1;
            Envelope::new(CorrelationId(id), message_type, payload)
        }

        pub fn send(&mut self, envelope: &Envelope<Value>) {
            let frame = encode_frame(&encode(envelope).expect("an encodable envelope"))
                .expect("a framable envelope");
            self.writer.write_all(&frame).expect("a sent frame");
        }

        /// The next message from the Daemon; the stream must not end.
        pub fn receive(&mut self) -> Envelope<Value> {
            let mut prefix = [0; 4];
            self.reader
                .read_exact(&mut prefix)
                .expect("a message from the Daemon");
            let len = u32::from_be_bytes(prefix) as usize;
            assert!(
                len <= hexput_port::MAX_FRAME_LEN,
                "a frame of {len} bytes is over the limit"
            );
            let mut body = vec![0; len];
            self.reader.read_exact(&mut body).expect("a whole frame");
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
        id: String,
        program: &'static str,
        grades: Vec<(&'static str, i64)>,
        /// The starting variables, built once; each run sends a copy.
        variables: Value,
        /// The verdict Rust computes, as the exact `Result` payload expected.
        expected: Value,
    }

    fn course_weight(course: &str) -> i64 {
        COURSES
            .iter()
            .find(|(name, _)| *name == course)
            .map(|(_, weight)| *weight)
            .unwrap_or_else(|| panic!("an unknown course `{course}`"))
    }

    fn pass_mark(program: &str) -> i64 {
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
                    expected: Value::Nil,
                };
                student.variables = variables(&student);
                student.expected = Value::Map(vec![(
                    Value::from("value"),
                    Value::Boolean(verdict(&student)),
                )]);
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
        let payload = client.request(
            MessageType::CodeRegister,
            Value::Map(vec![(Value::from("source"), Value::from(RULE))]),
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
                    let value = answer(&message);
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
            ("record", [id, Value::Boolean(_)]) if id.as_str().is_some() => Value::Nil,
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

    use crate::daemon::Daemon;
    use crate::report::Round;
    use crate::{CONCURRENCY, workload};

    /// Runs `rounds` rounds of `runs` timed runs on each of [`CONCURRENCY`] connections, after
    /// `warm_up` untimed runs each, and returns each round's figures.
    pub fn measure(daemon: &Daemon, rounds: usize, runs: usize, warm_up: usize) -> Vec<Round> {
        let students = workload::students();
        let clients: Vec<_> = (0..CONCURRENCY).map(|_| daemon.connect()).collect();
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
                                let handle = workload::register(&mut client);
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

/// The conditions, the steady-state figures and their reproducibility — pure apart from
/// gathering the conditions and writing the file, so quick mode checks it with [`self_check`].
#[cfg(unix)]
mod report {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::{Value as Json, json};

    use crate::{
        CONCURRENCY, QUICK_RUNS_PER_CONNECTION, ROUNDS, RUNS_PER_CONNECTION, WARM_UP_RUNS,
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

    /// `steady_state` runs with no filter, or a filter that is a substring of its name, and not
    /// when Criterion only lists or profiles its benchmarks.
    pub fn runs_steady_state(filter: Option<&str>, args: &[String]) -> bool {
        !args
            .iter()
            .any(|arg| arg == "--list" || arg.split('=').next() == Some("--profile-time"))
            && filter.is_none_or(|filter| "steady_state".contains(filter))
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
        /// The build actually measured, never assumed.
        pub profile: &'static str,
    }

    impl Conditions {
        pub fn gather(quick: bool) -> Self {
            let cpu = std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|text| cpu_model(&text))
                .unwrap_or_else(|| "unknown".to_owned());
            let (rounds, runs_per_connection, warm_up_runs) = if quick {
                (1, QUICK_RUNS_PER_CONNECTION, 0)
            } else {
                (ROUNDS, RUNS_PER_CONNECTION, WARM_UP_RUNS)
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
                profile: profile(quick, cfg!(debug_assertions)),
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
                "host_calls_per_run": crate::workload::HOST_CALLS_PER_RUN,
                "students": crate::workload::STUDENTS,
                "concurrency": CONCURRENCY,
                "rounds": self.rounds,
                "runs_per_connection": self.runs_per_connection,
                "warm_up_runs": self.warm_up_runs,
                "budget": "default Resource Budget",
                "transport": "UDS, Daemon in-process on its own runtime",
                "profile": self.profile,
            })
        }

        pub fn render(&self) -> String {
            format!(
                "== cache win: conditions ==\n\
                 machine:  {} ({} threads), {}/{}\n\
                 toolchain: {}\n\
                 revision: {}\n\
                 timestamp: {}\n\
                 workload: the pass/fail rule, {} host calls per run, {} students rotating\n\
                 steady state: {} connections, {} round(s) of {} runs each, {} warm-up runs\n\
                 budget:   default Resource Budget, no overrides\n\
                 transport: UDS, Daemon in-process on its own runtime (clients share the machine)\n\
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

    pub fn render_steady_state(rounds: &[Round], summary: &Summary) -> String {
        let mut text =
            format!("== steady_state: {CONCURRENCY} concurrent connections, Cached Execution ==\n");
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

    /// This run against the previous report.
    #[derive(Debug, PartialEq)]
    pub enum Comparison {
        /// No previous file, an unreadable one, or one from another machine or workload.
        NoPrevious,
        Previous {
            /// The previous run's median figures.
            previous: Round,
            /// Each figure's relative change from the previous run.
            change: [f64; 3],
        },
    }

    impl Comparison {
        pub fn render(&self) -> String {
            match self {
                Self::NoPrevious => "against the previous run: no comparable previous run".into(),
                Self::Previous { previous, change } => {
                    let figures = previous.figures();
                    let lines: Vec<String> = (0..3)
                        .map(|index| {
                            format!(
                                "  {}: previous {:.1}, change {:+.1}% ({} ±{:.0}%)",
                                NAMES[index],
                                figures[index],
                                change[index] * 100.0,
                                verdict(change[index], limits()[index]),
                                limits()[index] * 100.0
                            )
                        })
                        .collect();
                    format!(
                        "against the previous run on this machine:\n{}",
                        lines.join("\n")
                    )
                }
            }
        }
    }

    /// Compares `summary` with the previous report's text, when it parses and its machine has
    /// this run's fingerprint.
    pub fn compare(
        previous: Option<&str>,
        conditions: &Conditions,
        summary: &Summary,
    ) -> Comparison {
        let Some(json) = previous.and_then(|text| serde_json::from_str::<Json>(text).ok()) else {
            return Comparison::NoPrevious;
        };
        let fingerprint = conditions.fingerprint();
        let machine = json.get("machine");
        let same_machine = ["cpu", "parallelism", "os", "arch"]
            .iter()
            .all(|key| machine.and_then(|machine| machine.get(key)) == fingerprint.get(key));
        // A previous run of another workload (script, concurrency, rounds, runs, profile) is
        // not comparable either.
        let same_workload = json.get("workload") == Some(&conditions.workload());
        let Some(previous) = json
            .get("steady_state")
            .and_then(|steady| steady.get("summary"))
            .and_then(Round::from_json)
            .filter(|_| same_machine && same_workload)
        else {
            return Comparison::NoPrevious;
        };
        let (before, after) = (previous.figures(), summary.median.figures());
        Comparison::Previous {
            previous,
            change: [0, 1, 2].map(|index| relative(before[index], after[index])),
        }
    }

    pub fn to_json(conditions: &Conditions, rounds: &[Round], summary: &Summary) -> Json {
        json!({
            "machine": conditions.machine(),
            "workload": conditions.workload(),
            "variance": {
                "p50": VARIANCE.p50,
                "p99": VARIANCE.p99,
                "throughput": VARIANCE.throughput,
            },
            "steady_state": {
                "rounds": rounds.iter().map(Round::json).collect::<Vec<_>>(),
                "summary": summary.median.json(),
                "spread": {
                    "p50": summary.spread[0],
                    "p99": summary.spread[1],
                    "throughput": summary.spread[2],
                },
            },
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
            profile: "bench",
        };
        let previous = to_json(&conditions, &rounds, &summary).to_string();
        let faster = Summary::of(&rounds.map(|round| Round {
            p50_us: round.p50_us * 1.2,
            ..round
        }));
        match compare(Some(&previous), &conditions, &faster) {
            Comparison::Previous {
                previous: before,
                change,
            } => {
                assert_eq!(before, summary.median);
                assert!((change[0] - 0.2).abs() < 1e-9);
                assert_eq!(change[2], 0.0);
                let text = compare(Some(&previous), &conditions, &faster).render();
                assert!(text.contains("p50: previous 100.0, change +20.0% (outside ±10%)"));
                assert!(text.contains("throughput: previous 1000.0, change +0.0% (within ±10%)"));
            }
            Comparison::NoPrevious => panic!("the same machine is comparable"),
        }
        let other = Conditions {
            cpu: "another cpu".into(),
            ..conditions.clone()
        };
        assert_eq!(
            compare(Some(&previous), &other, &summary),
            Comparison::NoPrevious
        );
        assert_eq!(compare(None, &other, &summary), Comparison::NoPrevious);
        assert_eq!(
            compare(Some("{not json"), &other, &summary),
            Comparison::NoPrevious
        );
        assert_eq!(
            compare(Some("{\"machine\": 3}"), &other, &summary),
            Comparison::NoPrevious
        );
        // Corrupt or partial reports from this very machine are not comparable either.
        assert_eq!(
            compare(Some("{\"machine\": 3}"), &conditions, &summary),
            Comparison::NoPrevious
        );
        let mut partial = to_json(&conditions, &rounds, &summary);
        partial["steady_state"]
            .as_object_mut()
            .expect("a steady_state object")
            .remove("summary");
        assert_eq!(
            compare(Some(&partial.to_string()), &conditions, &summary),
            Comparison::NoPrevious
        );
        let mut garbled = to_json(&conditions, &rounds, &summary);
        garbled["steady_state"]["summary"]["p99_us"] = json!("fast");
        assert_eq!(
            compare(Some(&garbled.to_string()), &conditions, &summary),
            Comparison::NoPrevious
        );
        // Nor is a run of another workload on this machine.
        let fewer = Conditions {
            runs_per_connection: 10,
            ..conditions.clone()
        };
        assert_eq!(
            compare(Some(&previous), &fewer, &summary),
            Comparison::NoPrevious
        );
        let unoptimised = Conditions {
            profile: profile(false, true),
            ..fewer
        };
        assert_eq!(
            compare(Some(&previous), &unoptimised, &summary),
            Comparison::NoPrevious
        );

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
