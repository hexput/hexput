//! Stories 2.3–2.7: the core's side of a connection, driven through an in-memory `Port`.
//!
//! No socket is involved anywhere in this file. That is the point: `hexput_connection::serve`
//! is generic over the Port, so exercising it with an adapter that is not a transport at all is
//! what shows the core makes no transport-specific decision (AD-1).

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hexput_port::{
    CorrelationId, Envelope, Inbound, MessageType, Outbound, Port, ProtocolCode, ProtocolError,
    ProtocolFailure, Received, Value,
};
use hexput_session::{ClientId, Sessions};

/// Everything the core wrote.
#[derive(Clone, Default)]
struct Sent(Arc<Mutex<Vec<Envelope<Value>>>>);

/// Runs once, when the script has run out and before the Port reports `Closed` — while the
/// connection is still attached — given every reply written so far.
type Probe = Box<dyn FnOnce(&[Envelope<Value>]) + Send>;

/// A Port that yields a fixed script of `Received` items, then `Closed`.
struct Scripted {
    script: Arc<Mutex<VecDeque<Received>>>,
    probe: Option<Probe>,
    sent: Sent,
    /// A write that fails: the attempt number (from 0) and how it fails.
    fail: Option<(usize, io::ErrorKind)>,
}

struct ScriptedIn(Arc<Mutex<VecDeque<Received>>>, Option<Probe>, Sent);
struct ScriptedOut {
    sent: Sent,
    attempts: usize,
    fail: Option<(usize, io::ErrorKind)>,
}

impl Port for Scripted {
    type Inbound = ScriptedIn;
    type Outbound = ScriptedOut;

    fn split(self) -> (ScriptedIn, ScriptedOut) {
        (
            ScriptedIn(self.script, self.probe, self.sent.clone()),
            ScriptedOut {
                sent: self.sent,
                attempts: 0,
                fail: self.fail,
            },
        )
    }
}

impl Inbound for ScriptedIn {
    async fn recv(&mut self) -> Received {
        let next = self.0.lock().unwrap().pop_front();
        next.unwrap_or_else(|| {
            if let Some(probe) = self.1.take() {
                probe(&self.2.0.lock().unwrap());
            }
            Received::Closed(None)
        })
    }
}

impl Outbound for ScriptedOut {
    async fn send(&mut self, envelope: Envelope<Value>) -> io::Result<()> {
        let attempt = self.attempts;
        self.attempts += 1;
        match self.fail {
            // An envelope too large to frame fails alone; nothing was written.
            Some((at, io::ErrorKind::InvalidInput)) if attempt == at => {
                Err(io::Error::from(io::ErrorKind::InvalidInput))
            }
            // A broken connection stays broken.
            Some((at, kind)) if kind != io::ErrorKind::InvalidInput && attempt >= at => {
                Err(io::Error::from(kind))
            }
            _ => {
                self.sent.0.lock().unwrap().push(envelope);
                Ok(())
            }
        }
    }
}

/// Serve `script` to completion; return what was written and what was left unread.
fn serve(
    script: Vec<Received>,
    fail: Option<(usize, io::ErrorKind)>,
) -> (Vec<Envelope<Value>>, usize) {
    serve_with(&Arc::new(Sessions::new()), script, fail)
}

/// [`serve_with`], running `probe` after the last scripted item while the connection is open.
fn serve_probed(
    sessions: &Arc<Sessions>,
    script: Vec<Received>,
    probe: impl FnOnce(&[Envelope<Value>]) + Send + 'static,
) -> Vec<Envelope<Value>> {
    run(sessions, script, None, Some(Box::new(probe))).0
}

/// [`serve`] against a registry the test keeps, to inspect after the connection ends.
fn serve_with(
    sessions: &Arc<Sessions>,
    script: Vec<Received>,
    fail: Option<(usize, io::ErrorKind)>,
) -> (Vec<Envelope<Value>>, usize) {
    run(sessions, script, fail, None)
}

fn run(
    sessions: &Arc<Sessions>,
    script: Vec<Received>,
    fail: Option<(usize, io::ErrorKind)>,
    probe: Option<Probe>,
) -> (Vec<Envelope<Value>>, usize) {
    let (written, left, _) = run_timed(sessions, script, fail, probe);
    (written, left)
}

/// [`run`], also returning how long `serve` took to return. That excludes the runtime's drop,
/// which waits for any abandoned execution still running on the blocking pool.
fn run_timed(
    sessions: &Arc<Sessions>,
    script: Vec<Received>,
    fail: Option<(usize, io::ErrorKind)>,
    probe: Option<Probe>,
) -> (Vec<Envelope<Value>>, usize, Duration) {
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let sent = Sent::default();
    let port = Scripted {
        script: Arc::clone(&script),
        probe,
        sent: sent.clone(),
        fail,
    };
    let dispatch = discarding_dispatch();
    let worker_dispatch = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .on_thread_start(move || {
            std::mem::forget(tracing::dispatcher::set_default(&worker_dispatch));
        })
        .build()
        .unwrap();
    let started = Instant::now();
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(hexput_connection::serve(port, Arc::clone(sessions)));
    });
    let elapsed = started.elapsed();
    drop(runtime);
    let written = sent.0.lock().unwrap().clone();
    let left = script.lock().unwrap().len();
    (written, left, elapsed)
}

/// A subscriber that enables every event and span and writes them nowhere, installed around every
/// `serve` in this file (and on its blocking threads).
///
/// `tracing` caches whether a callsite is enabled process-wide, when the callsite is first hit.
/// A callsite first hit with no subscriber at all, while another test is installing its own
/// scoped subscriber, can be cached as disabled for good — which would strip the spans from
/// what [`at_a_quiet_level_a_warning_keeps_its_span_fields`] observes. With a subscriber live
/// wherever `hexput-connection` runs, no callsite is ever registered without one.
fn discarding_dispatch() -> tracing::Dispatch {
    tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(io::sink)
            .finish(),
    )
}

fn message(id: u64, message_type: MessageType) -> Received {
    Received::Message(Envelope::new(CorrelationId(id), message_type, Value::Nil))
}

fn string(s: &str) -> Value {
    Value::from(s)
}

/// A well-formed `Init` payload registering `registrations`, each `(name, blanket)` stating its
/// blanket grant explicitly.
fn init_payload(registrations: &[(&str, bool)]) -> Value {
    let registrations = registrations
        .iter()
        .map(|(name, blanket)| {
            Value::Map(vec![
                (string("name"), string(name)),
                (string("blanket"), Value::from(*blanket)),
            ])
        })
        .collect();
    Value::Map(vec![
        (string("config"), Value::Map(vec![])),
        (string("registrations"), Value::Array(registrations)),
    ])
}

fn init(id: u64, payload: Value) -> Received {
    Received::Message(Envelope::new(CorrelationId(id), MessageType::Init, payload))
}

/// The Client ID a `Result` reply to `Init` carries.
fn client_id_of(response: &Envelope<Value>) -> ClientId {
    assert_eq!(
        response.message_type,
        MessageType::Result,
        "{:?}",
        response.payload
    );
    let Value::Map(fields) = &response.payload else {
        panic!("a Result payload is a map: {:?}", response.payload);
    };
    assert_eq!(fields.len(), 1, "exactly `client_id`: {fields:?}");
    assert_eq!(fields[0].0.as_str(), Some("client_id"));
    let text = fields[0].1.as_str().expect("a string Client ID");
    assert_eq!(text.len(), 32);
    assert!(
        text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{text}"
    );
    text.parse().unwrap()
}

/// The `message` of an `Error` response's payload.
fn message_of(response: &Envelope<Value>) -> String {
    let Value::Map(fields) = &response.payload else {
        panic!("an error payload is a map: {:?}", response.payload);
    };
    fields
        .iter()
        .find(|(key, _)| key.as_str() == Some("message"))
        .and_then(|(_, value)| value.as_str())
        .expect("a message field")
        .to_owned()
}

fn malformed(id: Option<u64>, code: ProtocolCode) -> Received {
    Received::Malformed(ProtocolFailure {
        id: id.map(CorrelationId),
        error: ProtocolError::new(code, "broken"),
    })
}

/// `answers` in correlation-id order, for replies whose arrival order is not defined.
fn by_id(
    mut answers: Vec<(Option<CorrelationId>, String)>,
) -> Vec<(Option<CorrelationId>, String)> {
    answers.sort_by_key(|(id, _)| id.map(CorrelationId::get));
    answers
}

/// `"Result"` for a `Result` reply, otherwise the `Error`'s code.
fn outcome(response: &Envelope<Value>) -> String {
    if response.message_type == MessageType::Result {
        "Result".to_owned()
    } else {
        code_of(response)
    }
}

/// The `code` of an `Error` response's payload.
fn code_of(response: &Envelope<Value>) -> String {
    assert_eq!(response.message_type, MessageType::Error);
    let Value::Map(fields) = &response.payload else {
        panic!("an error payload is a map: {:?}", response.payload);
    };
    fields
        .iter()
        .find(|(key, _)| key.as_str() == Some("code"))
        .and_then(|(_, value)| value.as_str())
        .expect("a code field")
        .to_owned()
}

#[test]
fn every_request_before_init_is_refused() {
    let sessions = Arc::new(Sessions::new());
    let (sent, _) = serve_with(
        &sessions,
        vec![
            message(1, MessageType::ExecutionStart),
            // A nil payload is an init missing both parts.
            message(2, MessageType::Init),
            message(3, MessageType::Result),
            message(4, MessageType::Error),
        ],
        None,
    );
    let answers: Vec<_> = sent.iter().map(|r| (r.id, code_of(r))).collect();
    assert_eq!(
        answers,
        [
            (
                Some(CorrelationId(1)),
                "protocol.init_not_completed".to_owned()
            ),
            (
                Some(CorrelationId(2)),
                "protocol.invalid_payload".to_owned()
            ),
            // A stray `Result`/`Error` is refused with a nil id: its id is in the Daemon's
            // call-id space, never the Backend's (Story 3.1).
            (None, "protocol.unexpected_message".to_owned()),
            (None, "protocol.unexpected_message".to_owned()),
        ]
    );
}

#[test]
fn an_error_with_a_nil_id_is_answered_with_a_nil_id() {
    let request = Envelope {
        id: None,
        ..Envelope::new(CorrelationId(0), MessageType::Error, Value::Nil)
    };
    let (sent, _) = serve(vec![Received::Message(request)], None);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].id, None);
    assert_eq!(code_of(&sent[0]), "protocol.unexpected_message");
}

#[test]
fn a_malformed_frame_is_answered_and_the_connection_keeps_reading() {
    let (sent, left) = serve(
        vec![
            malformed(Some(7), ProtocolCode::InvalidEnvelope),
            malformed(None, ProtocolCode::MalformedFrame),
            message(8, MessageType::ExecutionStart),
        ],
        None,
    );
    let answers: Vec<_> = sent.iter().map(|r| (r.id, code_of(r))).collect();
    assert_eq!(
        answers,
        [
            (
                Some(CorrelationId(7)),
                "protocol.invalid_envelope".to_owned()
            ),
            (None, "protocol.malformed_frame".to_owned()),
            (
                Some(CorrelationId(8)),
                "protocol.init_not_completed".to_owned()
            ),
        ]
    );
    assert_eq!(left, 0);
}

#[test]
fn a_fatal_protocol_error_is_answered_then_the_connection_closes() {
    let (sent, left) = serve(
        vec![
            malformed(None, ProtocolCode::FrameTooLarge),
            message(9, MessageType::ExecutionStart),
        ],
        None,
    );
    assert_eq!(sent.len(), 1);
    assert_eq!(code_of(&sent[0]), "protocol.frame_too_large");
    assert_eq!(left, 1, "nothing is read after a fatal error");
}

#[test]
fn a_closed_connection_gets_no_reply() {
    let (sent, left) = serve(
        vec![
            Received::Closed(Some(io::Error::from(io::ErrorKind::ConnectionReset))),
            message(1, MessageType::ExecutionStart),
        ],
        None,
    );
    assert!(sent.is_empty());
    assert_eq!(left, 1, "serving stops at the close");
}

#[test]
fn a_failed_write_ends_the_connection() {
    let (sent, left) = serve(
        vec![
            message(1, MessageType::ExecutionStart),
            message(2, MessageType::ExecutionStart),
            message(3, MessageType::ExecutionStart),
        ],
        Some((1, io::ErrorKind::BrokenPipe)),
    );
    assert_eq!(sent.len(), 1);
    assert_eq!(
        left, 1,
        "the request whose reply failed was the last one read"
    );
}

#[test]
fn a_reply_too_large_to_frame_is_answered_as_too_large_and_the_connection_stays_open() {
    let (sent, left) = serve(
        vec![
            message(1, MessageType::ExecutionStart),
            message(2, MessageType::ExecutionStart),
        ],
        Some((0, io::ErrorKind::InvalidInput)),
    );
    let answers: Vec<_> = sent.iter().map(|r| (r.id, code_of(r))).collect();
    assert_eq!(
        answers,
        [
            (
                Some(CorrelationId(1)),
                "protocol.response_too_large".to_owned()
            ),
            (
                Some(CorrelationId(2)),
                "protocol.init_not_completed".to_owned()
            ),
        ],
        "the unsendable reply is replaced, with its id, and the next request is still answered"
    );
    assert_eq!(left, 0);
}

// --- Stories 2.4 + 2.5: init, the gate, and detach ---

/// What the registry held for a Session while its connection was still open:
/// `(live Sessions, attached Connections, registration names)`.
type Snapshot = (usize, Option<usize>, Option<Vec<(String, bool)>>);

/// Serve `script`, then — before the connection closes — record what the registry holds for the
/// Session whose Client ID reply number `init_reply` carried.
fn serve_and_look(
    sessions: &Arc<Sessions>,
    script: Vec<Received>,
    init_reply: usize,
) -> (Vec<Envelope<Value>>, Snapshot) {
    let snapshot = Arc::new(Mutex::new(None));
    let sent = {
        let (sessions, snapshot) = (Arc::clone(sessions), Arc::clone(&snapshot));
        serve_probed(&Arc::clone(&sessions), script, move |sent| {
            let id = client_id_of(&sent[init_reply]);
            *snapshot.lock().unwrap() = Some((
                sessions.len(),
                sessions.attached(id),
                sessions.registrations(id).map(|registered| {
                    registered
                        .iter()
                        .map(|r| (r.name().to_owned(), r.blanket()))
                        .collect()
                }),
            ));
        })
    };
    let snapshot = snapshot.lock().unwrap().take().expect("the probe ran");
    (sent, snapshot)
}

#[test]
fn init_creates_a_session_with_this_connection_attached_and_answers_its_client_id() {
    let sessions = Arc::new(Sessions::new());
    let (sent, seen) = serve_and_look(
        &sessions,
        vec![init(5, init_payload(&[("getUser", true)]))],
        0,
    );
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].id, Some(CorrelationId(5)));
    assert_eq!(seen, (1, Some(1), Some(vec![("getUser".to_owned(), true)])));
    // The connection closed, it was the last attached, so its Session went with it.
    let client_id = client_id_of(&sent[0]);
    assert!(!sessions.contains(client_id));
    assert!(sessions.is_empty());
}

#[test]
fn an_init_registering_no_functions_is_valid() {
    let sessions = Arc::new(Sessions::new());
    let (sent, seen) = serve_and_look(&sessions, vec![init(1, init_payload(&[]))], 0);
    assert_eq!(sent[0].message_type, MessageType::Result);
    assert_eq!(seen, (1, Some(1), Some(vec![])));
}

#[test]
fn an_invalid_init_creates_no_session_and_names_what_is_wrong() {
    let cases: Vec<(Value, &str)> = vec![
        (Value::Nil, "missing `config` and `registrations`"),
        (Value::Map(vec![]), "missing `config` and `registrations`"),
        (
            Value::Map(vec![(string("registrations"), Value::Array(vec![]))]),
            "missing `config`",
        ),
        (
            Value::Map(vec![(string("config"), Value::Map(vec![]))]),
            "missing `registrations`",
        ),
        (
            init_payload(&[("getUser", true), ("getUser", true)]),
            "`getUser`",
        ),
        // Story 3.2: a blanket grant is a boolean or nothing.
        (
            Value::Map(vec![
                (string("config"), Value::Map(vec![])),
                (
                    string("registrations"),
                    Value::Array(vec![
                        Value::Map(vec![(string("name"), string("a"))]),
                        Value::Map(vec![
                            (string("name"), string("getOrder")),
                            (string("blanket"), string("yes")),
                        ]),
                    ]),
                ),
            ]),
            "`registrations[1].blanket` is not a boolean",
        ),
    ];
    for (payload, expected) in cases {
        let sessions = Arc::new(Sessions::new());
        // Nothing is attached after a refusal, so the ExecutionStart that follows is still gated.
        let (sent, _) = serve_with(
            &sessions,
            vec![init(3, payload), message(4, MessageType::ExecutionStart)],
            None,
        );
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].id, Some(CorrelationId(3)));
        assert_eq!(code_of(&sent[0]), "protocol.invalid_payload");
        let message = message_of(&sent[0]);
        assert!(message.contains(expected), "{message:?} names {expected:?}");
        assert_eq!(code_of(&sent[1]), "protocol.init_not_completed");
        assert!(sessions.is_empty(), "no Session after {message:?}");
    }
}

#[test]
fn after_init_execution_runs_and_a_second_init_is_refused() {
    let sessions = Arc::new(Sessions::new());
    let (sent, seen) = serve_and_look(
        &sessions,
        vec![
            message(1, MessageType::ExecutionStart),
            init(2, init_payload(&[("getUser", true)])),
            execution(3, "return 1;"),
            init(4, init_payload(&[("other", true)])),
            message(5, MessageType::Result),
        ],
        1,
    );
    let answers: Vec<_> = sent
        .iter()
        .map(|r| {
            let code = if r.message_type == MessageType::Result {
                "Result".to_owned()
            } else {
                code_of(r)
            };
            (r.id, code)
        })
        .collect();
    // The execution's reply is written when it finishes, so where it lands among the inline
    // answers is not defined (Story 2.7): compare by id.
    let answers = by_id(answers);
    assert_eq!(
        answers,
        [
            // The stray `Result` is refused with a nil id, which sorts first.
            (None, "protocol.unexpected_message".to_owned()),
            (
                Some(CorrelationId(1)),
                "protocol.init_not_completed".to_owned()
            ),
            (Some(CorrelationId(2)), "Result".to_owned()),
            (Some(CorrelationId(3)), "Result".to_owned()),
            (
                Some(CorrelationId(4)),
                "protocol.already_initialized".to_owned()
            ),
        ]
    );
    // The second init left the first Session as it was, and created no other.
    assert_eq!(seen, (1, Some(1), Some(vec![("getUser".to_owned(), true)])));
    assert!(sessions.is_empty());
}

#[test]
fn two_connections_get_two_sessions_each_reply_on_its_own_connection() {
    let sessions = Arc::new(Sessions::new());
    let first = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&first);
    let registry = Arc::clone(&sessions);
    // B inits while A is still attached, from inside A's probe: both Sessions are live at once.
    let a_sent = serve_probed(
        &sessions,
        vec![init(1, init_payload(&[("a", true)]))],
        move |a_sent| {
            // On its own thread: a runtime cannot be started inside A's.
            let (b_sent, b_seen) = std::thread::spawn(move || {
                serve_and_look(&registry, vec![init(1, init_payload(&[("b", true)]))], 0)
            })
            .join()
            .unwrap();
            *slot.lock().unwrap() = Some((client_id_of(&a_sent[0]), b_sent, b_seen));
        },
    );
    let (a_id, b_sent, b_seen) = first.lock().unwrap().take().unwrap();
    assert_eq!(a_sent.len(), 1, "A received only its own reply");
    assert_eq!(b_sent.len(), 1, "B received only its own reply");
    let b_id = client_id_of(&b_sent[0]);
    assert_ne!(a_id, b_id);
    assert_eq!(b_seen, (2, Some(1), Some(vec![("b".to_owned(), true)])));
    assert!(sessions.is_empty());
}

#[test]
fn every_way_a_connection_ends_detaches_and_tears_down_its_session() {
    /// What follows the init, and which write fails.
    type Ending = (Vec<Received>, Option<(usize, io::ErrorKind)>);
    let ends: Vec<Ending> = vec![
        // The peer closes cleanly.
        (vec![], None),
        // The stream is lost.
        (
            vec![Received::Closed(Some(io::Error::from(
                io::ErrorKind::ConnectionReset,
            )))],
            None,
        ),
        // A fatal frame.
        (vec![malformed(None, ProtocolCode::FrameTooLarge)], None),
        // A write fails.
        (
            vec![message(2, MessageType::ExecutionStart)],
            Some((1, io::ErrorKind::BrokenPipe)),
        ),
    ];
    for (tail, fail) in ends {
        let sessions = Arc::new(Sessions::new());
        let mut script = vec![init(1, init_payload(&[("getUser", true)]))];
        script.extend(tail);
        let (sent, _) = serve_with(&sessions, script, fail);
        let client_id = client_id_of(&sent[0]);
        assert!(!sessions.contains(client_id));
        assert!(sessions.is_empty());
    }
}

// --- Story 2.6: Direct Execution ---

fn execution_payload(source: &str, variables: Vec<(&str, Value)>) -> Value {
    Value::Map(vec![
        (string("source"), string(source)),
        (
            string("variables"),
            Value::Map(variables.into_iter().map(|(k, v)| (string(k), v)).collect()),
        ),
    ])
}

fn execution(id: u64, source: &str) -> Received {
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::ExecutionStart,
        execution_payload(source, vec![]),
    ))
}

#[test]
fn an_initialized_execution_is_answered_on_this_connection_with_its_id() {
    let (sent, _) = serve(
        vec![
            init(1, init_payload(&[])),
            Received::Message(Envelope::new(
                CorrelationId(7),
                MessageType::ExecutionStart,
                execution_payload("return a + 1;", vec![("a", Value::from(2))]),
            )),
        ],
        None,
    );
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].id, Some(CorrelationId(7)));
    assert_eq!(sent[1].message_type, MessageType::Result);
    assert_eq!(
        sent[1].payload,
        Value::Map(vec![(string("value"), Value::from(3))])
    );
}

#[test]
fn a_failing_script_is_answered_and_the_connection_keeps_serving() {
    let (sent, left) = serve(
        vec![
            init(1, init_payload(&[])),
            execution(2, "let = ;"),
            execution(3, "return 1 / 0;"),
            Received::Message(Envelope::new(
                CorrelationId(4),
                MessageType::ExecutionStart,
                Value::Nil,
            )),
            execution(5, "return \"still here\";"),
        ],
        None,
    );
    assert_eq!(left, 0);
    // Replies arrive in completion order, which four independent executions do not define.
    let answers = by_id(sent[1..].iter().map(|r| (r.id, outcome(r))).collect());
    assert_eq!(
        answers,
        [
            (Some(CorrelationId(2)), "syntax.expected_syntax".to_owned()),
            (
                Some(CorrelationId(3)),
                "arithmetic.division_by_zero".to_owned()
            ),
            (
                Some(CorrelationId(4)),
                "protocol.invalid_payload".to_owned()
            ),
            (Some(CorrelationId(5)), "Result".to_owned()),
        ]
    );
}

// --- Story 2.7: slow executions block nothing ---

/// How many turns the slow Script's loop takes — sized so it runs well over a fast Script in the
/// test profile: about half a second in a debug build, against microseconds for a fast one.
const SLOW_TURNS: i64 = 150_000;

/// A slow execution: a counted loop, never a sleep, returning [`SLOW_TURNS`].
fn slow(id: u64) -> Received {
    execution(
        id,
        &format!("let i = 0; while (i < {SLOW_TURNS}) {{ i = i + 1; }}; return i;"),
    )
}

/// The ids of `sent`, in the order they were written.
fn ids(sent: &[Envelope<Value>]) -> Vec<u64> {
    sent.iter().map(|r| r.id.expect("an id").get()).collect()
}

#[test]
fn a_fast_execution_is_answered_before_a_slow_one_submitted_earlier() {
    let (sent, left) = serve(
        vec![
            init(1, init_payload(&[])),
            slow(2),
            execution(3, "return 3;"),
        ],
        None,
    );
    assert_eq!(left, 0);
    assert_eq!(
        ids(&sent),
        [1, 3, 2],
        "completion order, not submission order"
    );
    assert_eq!(
        sent[1].payload,
        Value::Map(vec![(string("value"), Value::from(3))])
    );
    assert_eq!(
        sent[2].payload,
        Value::Map(vec![(string("value"), Value::from(SLOW_TURNS))]),
        "the slow one still completes, with its own id"
    );
}

#[test]
fn while_a_slow_execution_runs_every_other_message_is_answered_at_once() {
    let (sent, left) = serve(
        vec![
            init(1, init_payload(&[])),
            slow(2),
            init(3, init_payload(&[])),
            malformed(Some(4), ProtocolCode::InvalidEnvelope),
            execution(5, "return 5;"),
        ],
        None,
    );
    assert_eq!(left, 0);
    assert_eq!(ids(&sent), [1, 3, 4, 5, 2]);
    let outcomes: Vec<_> = sent.iter().map(outcome).collect();
    assert_eq!(
        outcomes,
        [
            "Result",
            "protocol.already_initialized",
            "protocol.invalid_envelope",
            "Result",
            "Result"
        ]
    );
}

#[test]
fn many_executions_in_flight_are_each_answered_once_with_their_ids() {
    let mut script = vec![init(1, init_payload(&[])), slow(2)];
    script.extend((10..40).map(|k| execution(k, &format!("return {k};"))));
    let (sent, left) = serve(script, None);
    assert_eq!(left, 0);
    assert_eq!(
        sent.len(),
        32,
        "the init, the slow one and thirty fast ones"
    );
    let mut answered = ids(&sent[1..]);
    answered.sort_unstable();
    let expected: Vec<u64> = std::iter::once(2).chain(10..40).collect();
    assert_eq!(answered, expected, "every one answered exactly once");
    for reply in &sent[1..] {
        let id = reply.id.unwrap().get();
        let value = if id == 2 { SLOW_TURNS } else { id as i64 };
        assert_eq!(
            reply.payload,
            Value::Map(vec![(string("value"), Value::from(value))]),
            "request {id} got its own result"
        );
    }
    assert_eq!(ids(&sent).last(), Some(&2), "the slow one finishes last");
}

#[test]
fn a_failing_execution_among_slow_ones_is_answered_first_and_the_slow_one_completes() {
    let (sent, _) = serve(
        vec![
            init(1, init_payload(&[])),
            slow(2),
            execution(3, "return 1 / 0;"),
        ],
        None,
    );
    assert_eq!(ids(&sent), [1, 3, 2]);
    assert_eq!(code_of(&sent[1]), "arithmetic.division_by_zero");
    assert_eq!(sent[2].message_type, MessageType::Result);
}

#[test]
fn a_failed_write_abandons_executions_still_in_flight_and_detaches() {
    let sessions = Arc::new(Sessions::new());
    // How long the slow Script takes when it is waited for.
    let (waited, _, full) = run_timed(
        &sessions,
        vec![init(1, init_payload(&[])), slow(2)],
        None,
        None,
    );
    assert_eq!(ids(&waited), [1, 2]);

    // The fast reply's write fails: the connection ends without waiting for the slow one.
    let (sent, left, abandoned) = run_timed(
        &sessions,
        vec![
            init(1, init_payload(&[])),
            slow(2),
            execution(3, "return 3;"),
        ],
        Some((1, io::ErrorKind::BrokenPipe)),
        None,
    );
    assert_eq!(ids(&sent), [1], "nothing after the failed write");
    assert_eq!(left, 0);
    assert!(
        abandoned < full / 2,
        "serve returned in {abandoned:?}, not after the slow Script's {full:?}"
    );
    assert!(
        sessions.is_empty(),
        "the Session was detached and torn down"
    );
}

#[test]
fn a_lost_stream_abandons_executions_still_in_flight_and_detaches() {
    let sessions = Arc::new(Sessions::new());
    // How long the slow Script takes when it is waited for.
    let (waited, _, full) = run_timed(
        &sessions,
        vec![init(1, init_payload(&[])), slow(2)],
        None,
        None,
    );
    assert_eq!(ids(&waited), [1, 2]);

    let (sent, left, abandoned) = run_timed(
        &sessions,
        vec![
            init(1, init_payload(&[])),
            slow(2),
            Received::Closed(Some(io::Error::from(io::ErrorKind::ConnectionReset))),
        ],
        None,
        None,
    );
    assert_eq!(ids(&sent), [1], "nothing is written to a lost stream");
    assert_eq!(left, 0);
    assert!(
        abandoned < full / 2,
        "serve returned in {abandoned:?}, not after the slow Script's {full:?}"
    );
    assert!(
        sessions.is_empty(),
        "the Session was detached and torn down"
    );
}

#[test]
fn a_fatal_frame_stops_reading_but_in_flight_executions_are_still_answered() {
    let sessions = Arc::new(Sessions::new());
    let (sent, left) = serve_with(
        &sessions,
        vec![
            init(1, init_payload(&[])),
            slow(2),
            malformed(None, ProtocolCode::FrameTooLarge),
            execution(3, "return 3;"),
        ],
        None,
    );
    let answers: Vec<_> = sent.iter().map(|r| (r.id, outcome(r))).collect();
    assert_eq!(
        answers,
        [
            (Some(CorrelationId(1)), "Result".to_owned()),
            (None, "protocol.frame_too_large".to_owned()),
            (Some(CorrelationId(2)), "Result".to_owned()),
        ]
    );
    assert_eq!(left, 1, "nothing is read after the fatal frame");
    assert!(
        sessions.is_empty(),
        "the Session was detached after the reply"
    );
}

#[test]
fn a_clean_close_waits_for_in_flight_executions_and_detaches_after() {
    let sessions = Arc::new(Sessions::new());
    let seen = Arc::new(Mutex::new(None));
    let (registry, slot) = (Arc::clone(&sessions), Arc::clone(&seen));
    let sent = serve_probed(
        &sessions,
        vec![init(1, init_payload(&[])), slow(2)],
        move |sent| {
            // The script ran out while the slow Script was still running.
            *slot.lock().unwrap() = Some((ids(sent), registry.len()));
        },
    );
    assert_eq!(seen.lock().unwrap().take(), Some((vec![1], 1)));
    assert_eq!(
        ids(&sent),
        [1, 2],
        "its reply was still written after the close"
    );
    assert!(
        sessions.is_empty(),
        "and only then was the Session detached"
    );
}

// --- Story 2.8: spans survive a quiet log level ---

/// A Port whose every write fails as unframable, so the connection logs its one `warn`.
struct Unframable(Arc<Mutex<VecDeque<Received>>>);
struct UnframableIn(Arc<Mutex<VecDeque<Received>>>);
struct UnframableOut;

impl Port for Unframable {
    type Inbound = UnframableIn;
    type Outbound = UnframableOut;

    fn split(self) -> (UnframableIn, UnframableOut) {
        (UnframableIn(self.0), UnframableOut)
    }
}

impl Inbound for UnframableIn {
    async fn recv(&mut self) -> Received {
        let next = self.0.lock().unwrap().pop_front();
        next.unwrap_or(Received::Closed(None))
    }
}

impl Outbound for UnframableOut {
    async fn send(&mut self, _: Envelope<Value>) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::InvalidInput))
    }
}

/// A log sink the test reads back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The matrix's quiet-level row: at `warn`, `debug` events are filtered out, but the spans are
/// still enabled, so the `warn` events that do pass keep `connection`, `client_id` and `id`.
#[test]
fn at_a_quiet_level_a_warning_keeps_its_span_fields() {
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::WARN)
        .with_writer(Mutex::new(writer))
        .finish();
    let script = vec![
        message(1, MessageType::ExecutionStart),
        init(2, init_payload(&[])),
        message(3, MessageType::Result),
        // Its reply is written from the `Finished` arm, once the execution completes.
        execution(4, "return 1;"),
    ];
    let port = Unframable(Arc::new(Mutex::new(VecDeque::from(script))));
    let sessions = Arc::new(Sessions::new());
    let dispatch = tracing::Dispatch::new(subscriber);
    let worker_dispatch = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .on_thread_start(move || {
            std::mem::forget(tracing::dispatcher::set_default(&worker_dispatch));
        })
        .build()
        .unwrap();
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(hexput_connection::serve(port, Arc::clone(&sessions)));
    });
    assert!(sessions.is_empty(), "the connection detached");

    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    // One warning per reply: nothing below `warn` got through.
    assert_eq!(events.len(), 4, "{text}");
    let mut issued = None;
    for (event, request) in events.iter().zip(["1", "2", "3", "4"]) {
        assert_eq!(event["level"], "WARN", "{event}");
        let spans = event["spans"].as_array().expect("span fields");
        assert_eq!(spans[0]["name"], "connection");
        assert!(spans[0]["connection"].is_u64(), "{event}");
        assert_eq!(spans[1]["name"], "request");
        assert_eq!(spans[1]["id"], request, "{event}");
        let client_id = spans[0]["client_id"].as_str().expect("a client_id field");
        if request == "1" {
            assert_eq!(client_id, "none", "before init: {event}");
        } else {
            assert_eq!(client_id.len(), ClientId::TEXT_LEN, "{event}");
            assert_eq!(*issued.get_or_insert(client_id.to_owned()), client_id);
        }
    }
}

// --- Story 3.1: host calls on the connection ---

/// A Port wired to the test: it reads what the test sends and hands the test everything written.
/// Dropping the test's sender is the peer closing. With `refuse_calls`, every `Call` fails to
/// write as unframable, as a too-large frame would.
struct Wired {
    from_test: tokio::sync::mpsc::UnboundedReceiver<Received>,
    to_test: tokio::sync::mpsc::UnboundedSender<Envelope<Value>>,
    refuse_calls: bool,
}
struct WiredIn(tokio::sync::mpsc::UnboundedReceiver<Received>);
struct WiredOut(tokio::sync::mpsc::UnboundedSender<Envelope<Value>>, bool);

impl Port for Wired {
    type Inbound = WiredIn;
    type Outbound = WiredOut;

    fn split(self) -> (WiredIn, WiredOut) {
        (
            WiredIn(self.from_test),
            WiredOut(self.to_test, self.refuse_calls),
        )
    }
}

impl Inbound for WiredIn {
    async fn recv(&mut self) -> Received {
        // `mpsc::UnboundedReceiver::recv` is cancel-safe, as the trait requires.
        self.0.recv().await.unwrap_or(Received::Closed(None))
    }
}

impl Outbound for WiredOut {
    async fn send(&mut self, envelope: Envelope<Value>) -> io::Result<()> {
        if self.1 && envelope.message_type == MessageType::Call {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        // The test may have stopped listening; the write still "succeeds", as a socket would.
        let _ = self.0.send(envelope);
        Ok(())
    }
}

/// The test's side of a [`Wired`] connection: a stand-in Backend.
struct Backend {
    to_daemon: Option<tokio::sync::mpsc::UnboundedSender<Received>>,
    from_daemon: tokio::sync::mpsc::UnboundedReceiver<Envelope<Value>>,
}

impl Backend {
    fn send(&self, received: Received) {
        self.to_daemon
            .as_ref()
            .expect("still connected")
            .send(received)
            .unwrap();
    }

    fn reply(&self, id: CorrelationId, message_type: MessageType, payload: Value) {
        self.send(Received::Message(Envelope::new(id, message_type, payload)));
    }

    /// The next envelope the Daemon writes.
    async fn next(&mut self) -> Envelope<Value> {
        tokio::time::timeout(Duration::from_secs(10), self.from_daemon.recv())
            .await
            .expect("the Daemon wrote nothing within 10s")
            .expect("the connection is still open")
    }

    /// The next envelope, which must be a `Call`: its id, name and arguments.
    async fn call(&mut self) -> (CorrelationId, String, Vec<Value>) {
        let call = self.next().await;
        assert_eq!(call.message_type, MessageType::Call, "{call:?}");
        let Value::Map(fields) = call.payload else {
            panic!("a Call payload is a map");
        };
        assert_eq!(
            fields.len(),
            2,
            "exactly `name` and `arguments`: {fields:?}"
        );
        assert_eq!(fields[0].0.as_str(), Some("name"));
        assert_eq!(fields[1].0.as_str(), Some("arguments"));
        let name = fields[0].1.as_str().expect("a string name").to_owned();
        let Value::Array(arguments) = fields[1].1.clone() else {
            panic!("`arguments` is an array");
        };
        (call.id.expect("a Call has an id"), name, arguments)
    }

    /// The peer closes the connection.
    fn close(&mut self) {
        self.to_daemon = None;
    }
}

/// Serve one wired connection on `runtime` while `drive` plays the Backend. `drive` gets the
/// Client ID's init reply already read; `registered` is what the init registered, as
/// `(name, blanket)` pairs.
fn hosted<F, Fut>(
    runtime: &tokio::runtime::Runtime,
    registered: &[(&str, bool)],
    refuse_calls: bool,
    drive: F,
) -> Arc<Sessions>
where
    F: FnOnce(Backend) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(&sessions);
    let init_payload = init_payload(registered);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (backend, serving) = connect(&registry, init_payload, refuse_calls).await;
            drive(backend).await;
            finished(serving).await;
        });
    });
    sessions
}

/// Open one wired connection on `sessions` and complete its init with `init_payload`: the
/// stand-in Backend, with the init reply read, and the running `serve`.
async fn connect(
    sessions: &Arc<Sessions>,
    init_payload: Value,
    refuse_calls: bool,
) -> (Backend, tokio::task::JoinHandle<()>) {
    let (to_daemon, from_test) = tokio::sync::mpsc::unbounded_channel();
    let (to_test, from_daemon) = tokio::sync::mpsc::unbounded_channel();
    let port = Wired {
        from_test,
        to_test,
        refuse_calls,
    };
    let serving = tokio::spawn(hexput_connection::serve(port, Arc::clone(sessions)));
    let mut backend = Backend {
        to_daemon: Some(to_daemon),
        from_daemon,
    };
    backend.reply(CorrelationId(0), MessageType::Init, init_payload);
    let reply = backend.next().await;
    client_id_of(&reply);
    (backend, serving)
}

/// Wait for a connection's `serve` to return once its Backend left.
async fn finished(serving: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("serve returned once the Backend left")
        .unwrap();
}

fn wired_runtime() -> tokio::runtime::Runtime {
    let dispatch = discarding_dispatch();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .on_thread_start(move || {
            std::mem::forget(tracing::dispatcher::set_default(&dispatch));
        })
        .build()
        .unwrap()
}

fn value_of(response: &Envelope<Value>) -> Value {
    assert_eq!(
        response.message_type,
        MessageType::Result,
        "{:?}",
        response.payload
    );
    let Value::Map(fields) = &response.payload else {
        panic!("a Result payload is a map");
    };
    assert_eq!(fields.len(), 1, "exactly `value`");
    fields[0].1.clone()
}

#[test]
fn a_host_call_round_trips_and_the_script_resumes_with_the_backend_value() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            // The Backend's own ids and the Daemon's call ids are separate spaces: this execution's
            // id is the same number as the Daemon's first call id, and nothing is confused.
            backend.send(execution(0, "return getOrder(7).total;"));
            let (id, name, arguments) = backend.call().await;
            assert_eq!(id, CorrelationId(0));
            assert_eq!(name, "getOrder");
            assert_eq!(arguments, [Value::from(7)]);
            backend.reply(
                id,
                MessageType::Result,
                Value::Map(vec![(
                    string("value"),
                    Value::Map(vec![(string("total"), Value::from(3))]),
                )]),
            );
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(0)));
            assert_eq!(value_of(&reply), Value::from(3));
            backend.close();
        },
    );
    assert!(sessions.is_empty());
}

#[test]
fn a_backend_error_fails_only_its_script() {
    hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "let x = 7;\nreturn getOrder(x);"));
            let (id, _, _) = backend.call().await;
            backend.reply(
                id,
                MessageType::Error,
                Value::Map(vec![(string("message"), string("no such order"))]),
            );
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "host.function_failed");
            assert!(message_of(&reply).contains("no such order"));
            // The connection, and the next execution, are unaffected.
            backend.send(execution(2, "return 2;"));
            let reply = backend.next().await;
            assert_eq!(value_of(&reply), Value::from(2));
            backend.close();
        },
    );
}

#[test]
fn an_unregistered_or_shadowed_call_sends_nothing() {
    hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return nope(1);"));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "capability.unknown_function");
            backend.send(execution(
                2,
                "fn getOrder(x) { return x; }; return getOrder(1);",
            ));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(1));
            backend.send(execution(3, "return getOrder(fn() {});"));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "type.function_argument");
            backend.close();
            // Nothing else was written: no `Call` at any point.
            assert!(backend.from_daemon.recv().await.is_none());
        },
    );
}

#[test]
fn concurrent_calls_are_routed_by_id_whatever_order_the_replies_come_in() {
    hosted(
        &wired_runtime(),
        &[("echo", true)],
        false,
        |mut backend| async move {
            backend.send(execution(10, "return echo(\"first\");"));
            let first = backend.call().await;
            backend.send(execution(20, "return echo(\"second\");"));
            let second = backend.call().await;
            assert_ne!(first.0, second.0, "each call has its own id");
            // Answer the later call first.
            for (id, _, arguments) in [second, first] {
                backend.reply(
                    id,
                    MessageType::Result,
                    Value::Map(vec![(string("value"), arguments[0].clone())]),
                );
            }
            let mut replies = [backend.next().await, backend.next().await];
            replies.sort_by_key(|r| r.id.unwrap().get());
            assert_eq!(value_of(&replies[0]), string("first"));
            assert_eq!(value_of(&replies[1]), string("second"));
            backend.close();
        },
    );
}

#[test]
fn a_peer_that_closes_mid_call_fails_the_call_with_no_reply_and_is_answered_then_detached() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(execution(5, "return getOrder(1);"));
            let _ = backend.call().await;
            backend.close();
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(5)));
            assert_eq!(code_of(&reply), "host.no_reply");
        },
    );
    assert!(sessions.is_empty(), "the connection detached");
}

#[test]
fn a_stray_reply_after_init_is_an_unexpected_message() {
    hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            // Refused with a nil id: 42 names no call of the Daemon's, and echoing it would read
            // as a failure of the Backend's own request 42.
            backend.reply(CorrelationId(42), MessageType::Result, Value::Nil);
            let reply = backend.next().await;
            assert_eq!(reply.id, None);
            assert_eq!(code_of(&reply), "protocol.unexpected_message");
            // A reply to a call already answered is stray too.
            backend.send(execution(1, "return getOrder(1);"));
            let (id, _, _) = backend.call().await;
            let answer = Value::Map(vec![(string("value"), Value::from(1))]);
            backend.reply(id, MessageType::Result, answer);
            assert_eq!(value_of(&backend.next().await), Value::from(1));
            backend.reply(id, MessageType::Error, Value::Nil);
            let reply = backend.next().await;
            assert_eq!(reply.id, None);
            assert_eq!(code_of(&reply), "protocol.unexpected_message");
            // And a Backend never sends a `Call` of its own; that refusal echoes its id.
            backend.reply(CorrelationId(7), MessageType::Call, Value::Nil);
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(7)));
            assert_eq!(code_of(&reply), "protocol.unexpected_message");
            backend.close();
        },
    );
}

#[test]
fn a_call_that_cannot_be_framed_fails_only_its_script() {
    hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        true,
        |mut backend| async move {
            backend.send(execution(1, "return getOrder(1);"));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "host.function_failed");
            backend.send(execution(2, "return 2;"));
            assert_eq!(value_of(&backend.next().await), Value::from(2));
            backend.close();
        },
    );
}

/// The acceptance criterion: an execution waiting on a reply holds no blocking-pool thread. With
/// a pool of exactly one thread, a second execution can only finish while the first waits if the
/// first holds none.
#[test]
fn an_execution_waiting_on_a_reply_holds_no_thread_and_another_completes_meanwhile() {
    let dispatch = discarding_dispatch();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .on_thread_start(move || {
            std::mem::forget(tracing::dispatcher::set_default(&dispatch));
        })
        .build()
        .unwrap();
    hosted(
        &runtime,
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return getOrder(1) + 1;"));
            let (id, _, _) = backend.call().await;
            // The call is outstanding; a second execution runs to completion meanwhile.
            backend.send(execution(
                2,
                "let i = 0; while (i < 1000) { i = i + 1; }; return i;",
            ));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(1000));
            backend.reply(
                id,
                MessageType::Result,
                Value::Map(vec![(string("value"), Value::from(41))]),
            );
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(value_of(&reply), Value::from(42));
            backend.close();
        },
    );
}

#[test]
fn a_fatal_frame_mid_call_fails_the_call_with_no_reply_and_serve_returns() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(execution(5, "return getOrder(1);"));
            let _ = backend.call().await;
            backend.send(malformed(None, ProtocolCode::FrameTooLarge));
            let refusal = backend.next().await;
            assert_eq!(refusal.id, None);
            assert_eq!(code_of(&refusal), "protocol.frame_too_large");
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(5)));
            assert_eq!(code_of(&reply), "host.no_reply");
            // `hosted` asserts `serve` returns; the peer is still connected here.
        },
    );
    assert!(sessions.is_empty(), "the connection detached");
}

#[test]
fn a_call_made_only_after_the_connection_stopped_reading_fails_at_once() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            // The Script is still looping when the peer closes, so its call is made only after
            // the connection stopped reading — while the call table is still alive.
            backend.send(execution(
                6,
                &format!(
                    "let i = 0; while (i < {SLOW_TURNS}) {{ i = i + 1; }}; return getOrder(i);"
                ),
            ));
            backend.close();
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(6)));
            assert_eq!(code_of(&reply), "host.no_reply");
        },
    );
    assert!(sessions.is_empty(), "the connection detached");
}

// --- Story 3.2: blanket grants at registration ---

#[test]
fn a_blanket_granted_call_writes_exactly_one_call_and_nothing_else() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return getOrder(1);"));
            // The first envelope is the `Call` itself: no authorization request precedes it.
            let (id, name, arguments) = backend.call().await;
            assert_eq!(name, "getOrder");
            assert_eq!(arguments, [Value::from(1)]);
            backend.reply(
                id,
                MessageType::Result,
                Value::Map(vec![(string("value"), string("order"))]),
            );
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(value_of(&reply), string("order"));
            backend.close();
            // Nothing else was written for it.
            assert!(backend.from_daemon.recv().await.is_none());
        },
    );
    assert!(sessions.is_empty());
}

/// An `Init` payload registering one `getOrder` whose registration map is `extra` plus its name.
fn init_with_registration(extra: Vec<(Value, Value)>) -> Value {
    let mut registration = vec![(string("name"), string("getOrder"))];
    registration.extend(extra);
    Value::Map(vec![
        (string("config"), Value::Map(vec![])),
        (
            string("registrations"),
            Value::Array(vec![Value::Map(registration)]),
        ),
    ])
}

#[test]
fn a_function_registered_without_a_grant_is_refused_like_an_unregistered_one() {
    let runtime = wired_runtime();
    let withheld = [
        // `blanket` absent: no blanket grant.
        init_with_registration(vec![]),
        init_with_registration(vec![(string("blanket"), Value::from(false))]),
    ];
    let dispatch = discarding_dispatch();
    for init_payload in withheld {
        tracing::dispatcher::with_default(&dispatch, || {
            runtime.block_on(async {
                let sessions = Arc::new(Sessions::new());
                let (mut backend, serving) = connect(&sessions, init_payload, false).await;
                backend.send(execution(1, "return getOrder(1);"));
                let refused = backend.next().await;
                assert_eq!(refused.id, Some(CorrelationId(1)));
                assert_eq!(code_of(&refused), "capability.unknown_function");
                backend.send(execution(2, "return nope(1);"));
                let unregistered = backend.next().await;
                assert_eq!(code_of(&unregistered), "capability.unknown_function");
                backend.close();
                assert!(
                    backend.from_daemon.recv().await.is_none(),
                    "no `Call` was written"
                );
                finished(serving).await;
                // The Script cannot tell the two apart: the same error, bar the callee's name.
                assert_eq!(
                    message_of(&refused),
                    message_of(&unregistered).replace("nope", "getOrder")
                );
            });
        });
    }
}

#[test]
fn grants_never_leak_across_sessions() {
    let runtime = wired_runtime();
    let dispatch = discarding_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let sessions = Arc::new(Sessions::new());
            // Three Sessions live at once on one registry: A grants `getOrder`, B registers it
            // without the grant, C does not register it at all.
            let (mut a, a_serving) =
                connect(&sessions, init_payload(&[("getOrder", true)]), false).await;
            let (mut b, b_serving) =
                connect(&sessions, init_payload(&[("getOrder", false)]), false).await;
            let (mut c, c_serving) = connect(&sessions, init_payload(&[]), false).await;
            assert_eq!(sessions.len(), 3);

            for other in [&mut b, &mut c] {
                other.send(execution(1, "return getOrder(1);"));
                let reply = other.next().await;
                assert_eq!(code_of(&reply), "capability.unknown_function");
            }

            a.send(execution(1, "return getOrder(1);"));
            let (id, name, _) = a.call().await;
            assert_eq!(name, "getOrder");
            a.reply(
                id,
                MessageType::Result,
                Value::Map(vec![(string("value"), Value::from(5))]),
            );
            assert_eq!(value_of(&a.next().await), Value::from(5));

            for (mut backend, serving) in [(a, a_serving), (b, b_serving), (c, c_serving)] {
                backend.close();
                assert!(backend.from_daemon.recv().await.is_none());
                finished(serving).await;
            }
            assert!(sessions.is_empty());
        });
    });
}

#[test]
fn a_refused_call_is_logged_at_debug_under_its_request() {
    let log = Captured::default();
    let writer = log.clone();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Mutex::new(writer))
            .finish(),
    );
    let runtime = wired_runtime();
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let sessions = Arc::new(Sessions::new());
            let (mut backend, serving) =
                connect(&sessions, init_payload(&[("getOrder", false)]), false).await;
            backend.send(execution(7, "return getOrder(1);"));
            assert_eq!(
                code_of(&backend.next().await),
                "capability.unknown_function"
            );
            backend.send(execution(8, "return nope(1);"));
            assert_eq!(
                code_of(&backend.next().await),
                "capability.unknown_function"
            );
            backend.close();
            finished(serving).await;
        });
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let refusals: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["fields"]["message"] == "refused a host call")
        .collect();
    assert_eq!(refusals.len(), 2, "{text}");
    for (event, (request, function, reason)) in refusals.iter().zip([
        ("7", "getOrder", "not_granted"),
        ("8", "nope", "unregistered"),
    ]) {
        assert_eq!(event["level"], "DEBUG", "{event}");
        assert_eq!(event["fields"]["function"], function, "{event}");
        assert_eq!(event["fields"]["reason"], reason, "{event}");
        // Logged from the execution's own task, so it names its connection and request.
        assert_eq!(event["span"]["name"], "request", "{event}");
        assert_eq!(event["span"]["id"], request, "{event}");
        let spans = event["spans"].as_array().expect("span fields");
        assert_eq!(spans[0]["name"], "connection", "{event}");
        assert_eq!(
            spans[0]["client_id"].as_str().map(str::len),
            Some(ClientId::TEXT_LEN),
            "{event}"
        );
    }
}
