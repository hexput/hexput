//! Stories 2.3–2.5: the core's side of a connection, driven through an in-memory `Port`.
//!
//! No socket is involved anywhere in this file. That is the point: `hexput_connection::serve`
//! is generic over the Port, so exercising it with an adapter that is not a transport at all is
//! what shows the core makes no transport-specific decision (AD-1).

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

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
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let sent = Sent::default();
    let port = Scripted {
        script: Arc::clone(&script),
        probe,
        sent: sent.clone(),
        fail,
    };
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(hexput_connection::serve(port, Arc::clone(sessions)));
    let written = sent.0.lock().unwrap().clone();
    let left = script.lock().unwrap().len();
    (written, left)
}

fn message(id: u64, message_type: MessageType) -> Received {
    Received::Message(Envelope::new(CorrelationId(id), message_type, Value::Nil))
}

fn string(s: &str) -> Value {
    Value::from(s)
}

/// A well-formed `Init` payload registering `names`.
fn init_payload(names: &[&str]) -> Value {
    let registrations = names
        .iter()
        .map(|name| Value::Map(vec![(string("name"), string(name))]))
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
fn every_request_before_init_is_refused_with_its_id_echoed() {
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
            (
                Some(CorrelationId(3)),
                "protocol.unexpected_message".to_owned()
            ),
            (
                Some(CorrelationId(4)),
                "protocol.unexpected_message".to_owned()
            ),
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
type Snapshot = (usize, Option<usize>, Option<Vec<String>>);

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
                sessions.registration_names(id),
            ));
        })
    };
    let snapshot = snapshot.lock().unwrap().take().expect("the probe ran");
    (sent, snapshot)
}

#[test]
fn init_creates_a_session_with_this_connection_attached_and_answers_its_client_id() {
    let sessions = Arc::new(Sessions::new());
    let (sent, seen) = serve_and_look(&sessions, vec![init(5, init_payload(&["getUser"]))], 0);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].id, Some(CorrelationId(5)));
    assert_eq!(seen, (1, Some(1), Some(vec!["getUser".to_owned()])));
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
        (init_payload(&["getUser", "getUser"]), "`getUser`"),
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
            init(2, init_payload(&["getUser"])),
            execution(3, "return 1;"),
            init(4, init_payload(&["other"])),
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
    assert_eq!(
        answers,
        [
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
            (
                Some(CorrelationId(5)),
                "protocol.unexpected_message".to_owned()
            ),
        ]
    );
    // The second init left the first Session as it was, and created no other.
    assert_eq!(seen, (1, Some(1), Some(vec!["getUser".to_owned()])));
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
        vec![init(1, init_payload(&["a"]))],
        move |a_sent| {
            // On its own thread: a runtime cannot be started inside A's.
            let (b_sent, b_seen) = std::thread::spawn(move || {
                serve_and_look(&registry, vec![init(1, init_payload(&["b"]))], 0)
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
    assert_eq!(b_seen, (2, Some(1), Some(vec!["b".to_owned()])));
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
        let mut script = vec![init(1, init_payload(&["getUser"]))];
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
    let answers: Vec<_> = sent[1..]
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
