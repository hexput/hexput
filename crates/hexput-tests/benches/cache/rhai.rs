//! Story 4.5: the Rhai comparator — the same rule on Rhai, measured two ways.
//!
//! - [`RhaiServer`]: a small server inside the bench that speaks the Daemon's wire over a Unix
//!   Domain Socket — the same framing, `Init`, `CodeRegister`, `CachedExecutionStart`,
//!   `CodeUnregister` and a `Call` round trip per host call — so the bench's one Backend client,
//!   students and verification drive it exactly as they drive the Daemon. The engine and the
//!   server around it differ — one thread and one `Engine::new()` per connection here, against the
//!   Daemon's Tokio runtime and Executor — and so does one thing on the wire: the Daemon sends each
//!   `Call` argument in a Value Secret holder (FR-28), this server sends plain values.
//! - [`EngineDriver`]: Rhai's engine in-process, with native host functions, for the `engine`
//!   section beside the in-process Hexput drivers.
//!
//! Rhai runs at its defaults (`Engine::new()`, no limit set), as a Rhai user would embed it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ::rhai::{AST, Dynamic, Engine, ImmutableString, Map, Scope};
use hexput_port::{
    CorrelationId, Envelope, MAX_FRAME_LEN, MessageType, Value, decode, encode, encode_frame,
};

use crate::wire::{Client, field};

/// The measured rule in Rhai syntax: the same steps as [`crate::workload::RULE`], and every
/// number crosses as an `f64` (Rhai `FLOAT`), so the arithmetic is the same f64 operations in the
/// same order.
pub const RHAI_RULE: &str = "let total = 0.0; let weights = 0.0;\nfor grade in grades { let w = weight(grade.course); total = total + grade.score * w; weights = weights + w; }\nlet passed = total / weights >= passMark(student.program);\nrecord(student.id, passed);\npassed\n";

/// The rule's host functions: each takes this many arguments.
const HOST_FUNCTIONS: [(&str, usize); 3] = [("weight", 1), ("passMark", 1), ("record", 2)];

/// The Rhai server under measurement: a `UnixListener` in a fresh temp dir, one thread per
/// connection. Mirrors [`crate::daemon::Daemon`].
pub struct RhaiServer {
    dir: PathBuf,
    socket: PathBuf,
    stopping: Arc<AtomicBool>,
    accept: JoinHandle<Vec<JoinHandle<()>>>,
}

impl RhaiServer {
    /// Binds the socket and starts accepting connections.
    pub fn start() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "hexput-bench-rhai-{}-{nanos:08x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let socket = dir.join("r.sock");
        let listener = UnixListener::bind(&socket).expect("the Rhai server's socket");
        let stopping = Arc::new(AtomicBool::new(false));
        let accept = {
            let stopping = Arc::clone(&stopping);
            std::thread::spawn(move || {
                let mut connections = Vec::new();
                for (number, stream) in listener.incoming().enumerate() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let stream = stream.expect("an accepted Rhai connection");
                    connections.push(std::thread::spawn(move || serve(stream, number as u64)));
                }
                connections
            })
        };
        Self {
            dir,
            socket,
            stopping,
            accept,
        }
    }

    /// A client connection; the socket is bound before `start` returns.
    pub fn connect(&self) -> Client {
        let stream = UnixStream::connect(&self.socket).expect("a Rhai server connection");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .expect("a read timeout");
        Client::new(stream)
    }

    /// Ends the accept thread (woken by one last connection), waits for every connection — each
    /// ends when its client hangs up — re-raising a connection's panic, and removes the temp dir.
    pub fn stop(self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wakes the blocking `accept`; the accept thread sees `stopping` and drops it.
        let _ = UnixStream::connect(&self.socket);
        let connections = self.accept.join().expect("the Rhai accept thread");
        let _ = std::fs::remove_dir_all(&self.dir);
        for connection in connections {
            if let Err(panic) = connection.join() {
                std::panic::resume_unwind(panic);
            }
        }
    }
}

/// One connection's framed stream, shared by the request loop and the host functions.
struct Stream {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// The `Call` id counter, this connection's own.
    next_call: u64,
    /// The id of the `CachedExecutionStart` running, carried in each `Call`.
    execution: u64,
}

impl Stream {
    fn send(&mut self, envelope: &Envelope<Value>) {
        let frame = encode_frame(&encode(envelope).expect("an encodable envelope"))
            .expect("a framable envelope");
        self.writer.write_all(&frame).expect("a sent frame");
    }

    /// The next message, or `None` once the client hung up between messages.
    fn receive(&mut self) -> Option<Envelope<Value>> {
        let mut prefix = [0; 4];
        match self.reader.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return None,
            Err(error) => panic!("a Rhai connection failed: {error}"),
        }
        let len = u32::from_be_bytes(prefix) as usize;
        assert!(
            len <= MAX_FRAME_LEN,
            "a frame of {len} bytes is over the limit"
        );
        let mut body = vec![0; len];
        self.reader.read_exact(&mut body).expect("a whole frame");
        Some(decode(&body).expect("a decodable envelope"))
    }

    /// One host call: `Call {name, arguments, execution}`, then that id's `Result {value}`.
    fn call(&mut self, name: &str, arguments: Vec<Value>) -> Dynamic {
        let id = self.next_call;
        self.next_call += 1;
        let call = Envelope::new(
            CorrelationId(id),
            MessageType::Call,
            Value::Map(vec![
                (Value::from("name"), Value::from(name)),
                (Value::from("arguments"), Value::Array(arguments)),
                (Value::from("execution"), Value::from(self.execution)),
            ]),
        );
        self.send(&call);
        let reply = self.receive().expect("a reply to the Call");
        assert!(
            reply.message_type == MessageType::Result && reply.id == call.id,
            "`{name}` was answered with {:?} (id {:?}): {:?}",
            reply.message_type,
            reply.id,
            reply.payload
        );
        to_dynamic(field(&reply.payload, "value"))
    }
}

/// Serves one connection until its client hangs up: one `Engine::new()`, the rule's host
/// functions each a `Call` round trip on this stream, and the compiled `AST`s by handle.
fn serve(stream: UnixStream, number: u64) {
    let reader = BufReader::new(stream.try_clone().expect("a second handle"));
    let stream = Arc::new(Mutex::new(Stream {
        reader,
        writer: stream,
        next_call: 1,
        execution: 0,
    }));
    let mut engine = Engine::new();
    for (name, arity) in HOST_FUNCTIONS {
        let stream = Arc::clone(&stream);
        let call = move |arguments: Vec<Value>| {
            stream
                .lock()
                .expect("the connection's stream")
                .call(name, arguments)
        };
        match arity {
            1 => {
                engine.register_fn(name, move |a: Dynamic| call(vec![to_wire(&a)]));
            }
            _ => {
                engine.register_fn(name, move |a: Dynamic, b: Dynamic| {
                    call(vec![to_wire(&a), to_wire(&b)])
                });
            }
        }
    }
    let mut scripts: HashMap<String, AST> = HashMap::new();
    let mut next_handle = 1u64;
    loop {
        // The lock is released before the Script runs: its host functions take it.
        let Some(request) = stream.lock().expect("the connection's stream").receive() else {
            return;
        };
        let id = request.id.expect("a request carries its id");
        let reply = match request.message_type {
            // The Daemon's Init reply shape; the bench never presents the secret back.
            MessageType::Init => Value::Map(vec![
                (
                    Value::from("client_id"),
                    Value::from(format!("{number:032x}")),
                ),
                (
                    Value::from("reconnect_secret"),
                    Value::from(format!("{number:064x}")),
                ),
            ]),
            MessageType::CodeRegister => {
                let source = field(&request.payload, "source")
                    .as_str()
                    .expect("a string source");
                // Compiled once; every run of the handle reuses this `AST`.
                let ast = engine.compile(source).expect("the Rhai rule compiles");
                let handle = format!("{next_handle:016x}");
                next_handle += 1;
                scripts.insert(handle.clone(), ast);
                Value::Map(vec![(Value::from("handle"), Value::from(handle))])
            }
            MessageType::CachedExecutionStart => {
                let handle = field(&request.payload, "handle")
                    .as_str()
                    .expect("a string handle");
                let ast = scripts
                    .get(handle)
                    .unwrap_or_else(|| panic!("an unknown handle `{handle}`"));
                let mut scope = Scope::new();
                for (name, value) in field(&request.payload, "variables")
                    .as_map()
                    .expect("a variables map")
                {
                    scope.push_dynamic(name.as_str().expect("a variable name"), to_dynamic(value));
                }
                stream.lock().expect("the connection's stream").execution = id.get();
                let result = engine
                    .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
                    .unwrap_or_else(|error| panic!("the Rhai rule failed: {error}"));
                Value::Map(vec![(Value::from("value"), to_wire(&result))])
            }
            MessageType::CodeUnregister => {
                let handle = field(&request.payload, "handle")
                    .as_str()
                    .expect("a string handle");
                assert!(
                    scripts.remove(handle).is_some(),
                    "an unknown handle `{handle}`"
                );
                Value::Map(vec![])
            }
            other => panic!("the Rhai server serves no {other:?}"),
        };
        stream
            .lock()
            .expect("the connection's stream")
            .send(&Envelope::new(
                CorrelationId(id.get()),
                MessageType::Result,
                reply,
            ));
    }
}

/// A wire value as Rhai sees it: every number a `FLOAT`, a string, a bool, `nil` as `()`, an
/// array or a map with string keys. Anything else is a panic.
pub fn to_dynamic(value: &Value) -> Dynamic {
    match value {
        Value::Nil => Dynamic::UNIT,
        Value::Boolean(boolean) => Dynamic::from_bool(*boolean),
        Value::Integer(_) | Value::F32(_) | Value::F64(_) => {
            Dynamic::from_float(value.as_f64().expect("a number"))
        }
        Value::String(text) => Dynamic::from(ImmutableString::from(
            text.as_str().expect("a UTF-8 string"),
        )),
        Value::Array(items) => Dynamic::from_array(items.iter().map(to_dynamic).collect()),
        Value::Map(entries) => Dynamic::from_map(
            entries
                .iter()
                .map(|(key, value)| {
                    (
                        key.as_str().expect("a string key").into(),
                        to_dynamic(value),
                    )
                })
                .collect::<Map>(),
        ),
        other => panic!("no Rhai value for {other:?}"),
    }
}

/// A Rhai value back on the wire: `()` as `nil`, a bool, a `FLOAT` as float64, an `INT` as an
/// integer, a string, an array or a map. Anything else is a panic.
pub fn to_wire(value: &Dynamic) -> Value {
    if value.is_unit() {
        Value::Nil
    } else if let Ok(boolean) = value.as_bool() {
        Value::Boolean(boolean)
    } else if let Ok(number) = value.as_float() {
        Value::F64(number)
    } else if let Ok(number) = value.as_int() {
        Value::from(number)
    } else if value.is_string() {
        Value::from(value.as_immutable_string_ref().expect("a string").as_str())
    } else if value.is_array() {
        Value::Array(
            value
                .as_array_ref()
                .expect("an array")
                .iter()
                .map(to_wire)
                .collect(),
        )
    } else if value.is_map() {
        Value::Map(
            value
                .as_map_ref()
                .expect("a map")
                .iter()
                .map(|(key, value)| (Value::from(key.as_str()), to_wire(value)))
                .collect(),
        )
    } else {
        panic!("no wire value for a Rhai `{}`", value.type_name())
    }
}

/// How many `record` calls a run made, and the last one's arguments.
type Recorded = (usize, Option<(ImmutableString, bool)>);

/// One in-process Rhai run: its verdict, its host calls and its last `record`.
pub struct EngineRun {
    pub verdict: bool,
    pub calls: usize,
    pub records: usize,
    pub recorded: Option<(ImmutableString, bool)>,
}

/// Rhai's engine in-process, for the `engine` section: one `Engine::new()` with native
/// `weight`/`passMark`/`record` (no wire), the rule compiled once into an `AST`, and a fresh
/// `Scope` per run.
pub struct EngineDriver {
    engine: Engine,
    ast: AST,
    calls: Rc<Cell<usize>>,
    recorded: Rc<RefCell<Recorded>>,
}

impl EngineDriver {
    pub fn new() -> Self {
        let calls = Rc::new(Cell::new(0));
        let recorded = Rc::new(RefCell::new((0, None)));
        let mut engine = Engine::new();
        let counted = Rc::clone(&calls);
        engine.register_fn("weight", move |course: ImmutableString| -> f64 {
            counted.set(counted.get() + 1);
            crate::workload::course_weight(course.as_str()) as f64
        });
        let counted = Rc::clone(&calls);
        engine.register_fn("passMark", move |program: ImmutableString| -> f64 {
            counted.set(counted.get() + 1);
            crate::workload::pass_mark(program.as_str()) as f64
        });
        let counted = Rc::clone(&calls);
        let sink = Rc::clone(&recorded);
        engine.register_fn("record", move |id: ImmutableString, passed: bool| {
            counted.set(counted.get() + 1);
            let mut sink = sink.borrow_mut();
            sink.0 += 1;
            sink.1 = Some((id, passed));
        });
        let ast = engine.compile(RHAI_RULE).expect("the Rhai rule compiles");
        Self {
            engine,
            ast,
            calls,
            recorded,
        }
    }

    /// A student's starting variables, converted once from the wire map the bench sends.
    pub fn variables(wire: &Value) -> Vec<(String, Dynamic)> {
        wire.as_map()
            .expect("a variables map")
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().expect("a variable name").to_owned(),
                    to_dynamic(value),
                )
            })
            .collect()
    }

    /// One run in a fresh `Scope`, the variables cloned in.
    pub fn run(&self, variables: &[(String, Dynamic)]) -> EngineRun {
        self.calls.set(0);
        *self.recorded.borrow_mut() = (0, None);
        let mut scope = Scope::new();
        for (name, value) in variables {
            scope.push_dynamic(name.as_str(), value.clone());
        }
        let verdict = self
            .engine
            .eval_ast_with_scope::<bool>(&mut scope, &self.ast)
            .unwrap_or_else(|error| panic!("the Rhai rule failed: {error}"));
        let (records, recorded) = self.recorded.take();
        EngineRun {
            verdict,
            calls: self.calls.get(),
            records,
            recorded,
        }
    }
}

/// The conversion's own checks, run by `report::self_check` in quick mode: the round trip
/// through `Dynamic` for every kind, and an integer arriving as a `FLOAT`.
pub fn self_check() {
    let wire = Value::Map(vec![
        (Value::from("a"), Value::Nil),
        (Value::from("b"), Value::Boolean(true)),
        (Value::from("c"), Value::F64(2.5)),
        (Value::from("d"), Value::from("text")),
        (
            Value::from("e"),
            Value::Array(vec![
                Value::F64(-1.0),
                Value::Map(vec![(Value::from("k"), Value::Boolean(false))]),
                Value::Array(vec![]),
            ]),
        ),
    ]);
    assert_eq!(to_wire(&to_dynamic(&wire)), wire, "the round trip");
    assert_eq!(to_wire(&to_dynamic(&Value::from(7))), Value::F64(7.0));
    assert_eq!(to_dynamic(&Value::from(-3)).as_float(), Ok(-3.0));
    assert_eq!(to_wire(&Dynamic::from_int(7)), Value::from(7));
    let rule = Engine::new()
        .compile(RHAI_RULE)
        .map(|_| ())
        .map_err(|error| error.to_string());
    assert_eq!(rule, Ok(()), "the Rhai rule compiles");
}
