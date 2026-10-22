//! Stories 2.3–2.8 and Epic 3's wire-facing stories: the core's side of a connection, driven
//! through an in-memory `Port`.
//!
//! No socket is involved anywhere in this file. That is the point: `hexput_connection::serve`
//! is generic over the Port, so exercising it with an adapter that is not a transport at all is
//! what shows the core makes no transport-specific decision (AD-1).

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hexput_plugin::Plugins;
use hexput_port::{
    CorrelationId, Envelope, Inbound, MessageType, Outbound, Port, ProtocolCode, ProtocolError,
    ProtocolFailure, Received, Value,
};
use hexput_script::AstCache;
use hexput_session::{ClientId, ReconnectSecret, Sessions, Setting, Settings};
use hexput_shared::ids::{PluginId, ScriptHandle};

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
    run_timed_with_cache(
        sessions,
        &Arc::new(AstCache::with_capacity(10_000)),
        script,
        fail,
        probe,
    )
}

/// [`run_timed`] sharing `cache` (Story 4.1), which the test keeps to inspect after the connection.
fn run_timed_with_cache(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    script: Vec<Received>,
    fail: Option<(usize, io::ErrorKind)>,
    probe: Option<Probe>,
) -> (Vec<Envelope<Value>>, usize, Duration) {
    run_timed_with_plugins(
        sessions,
        cache,
        &Arc::new(Plugins::new()),
        script,
        fail,
        probe,
    )
}

/// [`run_timed_with_cache`] sharing `plugins` (Story 6.2), which the test keeps to inspect.
fn run_timed_with_plugins(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    plugins: &Arc<Plugins>,
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
        runtime.block_on(hexput_connection::serve(
            port,
            Arc::clone(sessions),
            Arc::clone(cache),
            Arc::clone(plugins),
        ));
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
    init_configured(Value::Map(vec![]), registrations)
}

/// [`init_payload`] with `config` as its Config (Story 3.7).
fn init_configured(config: Value, registrations: &[(&str, bool)]) -> Value {
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
        (string("config"), config),
        (string("registrations"), Value::Array(registrations)),
    ])
}

fn init(id: u64, payload: Value) -> Received {
    Received::Message(Envelope::new(CorrelationId(id), MessageType::Init, payload))
}

/// The Client ID a `Result` reply to `Init` carries.
fn client_id_of(response: &Envelope<Value>) -> ClientId {
    init_reply_of(response).0
}

/// The Client ID and the reconnect secret a `Result` reply to `Init` carries (Story 5.5): exactly
/// `client_id` then `reconnect_secret`, the secret 64 lowercase hex characters.
fn init_reply_of(response: &Envelope<Value>) -> (ClientId, String) {
    assert_eq!(
        response.message_type,
        MessageType::Result,
        "{:?}",
        response.payload
    );
    let Value::Map(fields) = &response.payload else {
        panic!("a Result payload is a map: {:?}", response.payload);
    };
    assert_eq!(
        fields.len(),
        2,
        "exactly `client_id` and `reconnect_secret`: {fields:?}"
    );
    assert_eq!(fields[0].0.as_str(), Some("client_id"));
    assert_eq!(fields[1].0.as_str(), Some("reconnect_secret"));
    let text = fields[0].1.as_str().expect("a string Client ID");
    assert_eq!(text.len(), 32);
    assert!(
        text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{text}"
    );
    let secret = fields[1].1.as_str().expect("a string reconnect secret");
    assert_eq!(secret.len(), 64, "{secret}");
    assert!(
        secret
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{secret}"
    );
    (text.parse().unwrap(), secret.to_owned())
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
/// test profile: about a fifth of a second in a debug build, against microseconds for a fast one,
/// and far inside the CPU time budget (Story 3.5) however loaded the machine running the tests.
const SLOW_TURNS: i64 = 50_000;

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
        runtime.block_on(hexput_connection::serve(
            port,
            Arc::clone(&sessions),
            Arc::new(AstCache::with_capacity(10_000)),
            Arc::new(Plugins::new()),
        ));
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
        // As a real adapter does: an envelope that cannot be framed is refused as invalid input.
        let framable = hexput_port::encode(&envelope)
            .ok()
            .is_some_and(|body| hexput_port::encode_frame(&body).is_ok());
        if !framable {
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
        self.request(MessageType::Call).await
    }

    /// The next envelope, which must be an `Authorize` question for the per-call handler: its id,
    /// name and arguments.
    async fn question(&mut self) -> (CorrelationId, String, Vec<Value>) {
        self.request(MessageType::Authorize).await
    }

    /// Answer the question `id` with `Result {value: allowed}`.
    fn allow(&self, id: CorrelationId, allowed: Value) {
        self.reply(
            id,
            MessageType::Result,
            Value::Map(vec![(string("value"), allowed)]),
        );
    }

    /// The next envelope, which must be a `message_type` request from the Daemon with the payload
    /// `{name, arguments, execution}`: its id, name and arguments — each argument, and everything
    /// inside it, asserted to travel as a Value Secret holder (Story 3.11), then stripped of them.
    async fn request(&mut self, message_type: MessageType) -> (CorrelationId, String, Vec<Value>) {
        let (id, name, arguments, _) = self.request_from(message_type).await;
        (id, name, arguments.iter().map(unheld).collect())
    }

    /// The next envelope, which must be a `message_type` request from the Daemon with the payload
    /// `{name, arguments, execution}`: its id, name, arguments, and the id of the `ExecutionStart`
    /// that made it.
    async fn request_from(
        &mut self,
        message_type: MessageType,
    ) -> (CorrelationId, String, Vec<Value>, u64) {
        let call = self.next().await;
        assert_eq!(call.message_type, message_type, "{call:?}");
        let Value::Map(fields) = call.payload else {
            panic!("a Call payload is a map");
        };
        assert_eq!(
            fields.len(),
            3,
            "exactly `name`, `arguments` and `execution`: {fields:?}"
        );
        assert_eq!(fields[0].0.as_str(), Some("name"));
        assert_eq!(fields[1].0.as_str(), Some("arguments"));
        assert_eq!(fields[2].0.as_str(), Some("execution"));
        let name = fields[0].1.as_str().expect("a string name").to_owned();
        let Value::Array(arguments) = fields[1].1.clone() else {
            panic!("`arguments` is an array");
        };
        let execution = fields[2].1.as_u64().expect("`execution` is a request id");
        (
            call.id.expect("a Daemon request has an id"),
            name,
            arguments,
            execution,
        )
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
    let cache = Arc::new(AstCache::with_capacity(10_000));
    hosted_on(runtime, &sessions, &cache, registered, refuse_calls, drive);
    sessions
}

/// [`hosted`] on `sessions` and `cache`, which the test keeps to inspect after the connection
/// (Story 4.3). A Session torn down when its connection ends is one built with
/// [`torn_down_by`].
fn hosted_on<F, Fut>(
    runtime: &tokio::runtime::Runtime,
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    registered: &[(&str, bool)],
    refuse_calls: bool,
    drive: F,
) where
    F: FnOnce(Backend) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(sessions);
    let cache = Arc::clone(cache);
    let init_payload = init_payload(registered);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (backend, serving, _) =
                connect_on(&registry, &cache, init_payload, refuse_calls).await;
            drive(backend).await;
            finished(serving).await;
        });
    });
}

/// A Session registry whose teardown releases the Session's entries in `cache`, wired the way the
/// Daemon wires it (Story 4.3).
fn torn_down_by(cache: &Arc<AstCache>) -> Arc<Sessions> {
    let cache = Arc::clone(cache);
    Arc::new(Sessions::new().with_teardown(move |client_id| cache.close(client_id)))
}

/// Open one wired connection on `sessions` and complete its init with `init_payload`: the
/// stand-in Backend, with the init reply read, and the running `serve`.
async fn connect(
    sessions: &Arc<Sessions>,
    init_payload: Value,
    refuse_calls: bool,
) -> (Backend, tokio::task::JoinHandle<()>) {
    let (backend, serving, _) = connect_as(sessions, init_payload, refuse_calls).await;
    (backend, serving)
}

/// [`connect`], also returning the Client ID the init issued.
async fn connect_as(
    sessions: &Arc<Sessions>,
    init_payload: Value,
    refuse_calls: bool,
) -> (Backend, tokio::task::JoinHandle<()>, ClientId) {
    connect_on(
        sessions,
        &Arc::new(AstCache::with_capacity(10_000)),
        init_payload,
        refuse_calls,
    )
    .await
}

/// [`connect_as`] sharing `cache`, which the test keeps to inspect (Story 4.3).
async fn connect_on(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    init_payload: Value,
    refuse_calls: bool,
) -> (Backend, tokio::task::JoinHandle<()>, ClientId) {
    connect_with_plugins(
        sessions,
        cache,
        &Arc::new(Plugins::new()),
        init_payload,
        refuse_calls,
    )
    .await
}

/// [`connect_on`] sharing `plugins` (Story 6.2), which the test keeps to inspect.
async fn connect_with_plugins(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    plugins: &Arc<Plugins>,
    init_payload: Value,
    refuse_calls: bool,
) -> (Backend, tokio::task::JoinHandle<()>, ClientId) {
    let (to_daemon, from_test) = tokio::sync::mpsc::unbounded_channel();
    let (to_test, from_daemon) = tokio::sync::mpsc::unbounded_channel();
    let port = Wired {
        from_test,
        to_test,
        refuse_calls,
    };
    let serving = tokio::spawn(hexput_connection::serve(
        port,
        Arc::clone(sessions),
        Arc::clone(cache),
        Arc::clone(plugins),
    ));
    let mut backend = Backend {
        to_daemon: Some(to_daemon),
        from_daemon,
    };
    backend.reply(CorrelationId(0), MessageType::Init, init_payload);
    let reply = backend.next().await;
    let client_id = client_id_of(&reply);
    (backend, serving, client_id)
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

/// A wire value with every Value Secret holder replaced by its value, after asserting that the
/// value, and every value inside it, is one (Story 3.11: everything a `Call` sends is).
fn unheld(value: &Value) -> Value {
    let Value::Map(fields) = value else {
        panic!("not a Value Secret holder: {value:?}");
    };
    assert_eq!(
        fields.len(),
        2,
        "a holder is exactly `__secret` and `value`: {value:?}"
    );
    assert_eq!(fields[0].0, string("__secret"));
    assert_eq!(fields[1].0, string("value"));
    let Value::Map(secret) = &fields[0].1 else {
        panic!("`__secret` is a map: {value:?}");
    };
    assert_eq!(secret[0].0, string("ref"));
    assert!(secret[0].1.as_str().is_some_and(|r| !r.is_empty()));
    match &fields[1].1 {
        Value::Array(items) => Value::Array(items.iter().map(unheld).collect()),
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), unheld(item)))
                .collect(),
        ),
        other => other.clone(),
    }
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
fn every_call_and_question_names_the_execution_that_made_it() {
    hosted(
        &wired_runtime(),
        &[("echo", true), ("guarded", false)],
        false,
        |mut backend| async move {
            // Two executions in flight on one connection: each request says which is asking.
            backend.send(execution(10, "return echo(1);"));
            let (first, _, _, from_first) = backend.request_from(MessageType::Call).await;
            backend.send(execution(20, "return guarded(2);"));
            let (question, _, _, from_second) = backend.request_from(MessageType::Authorize).await;
            assert_eq!((from_first, from_second), (10, 20));
            backend.allow(question, Value::Boolean(true));
            let (second, _, _, from_second) = backend.request_from(MessageType::Call).await;
            assert_eq!(
                from_second, 20,
                "the Call after the question names the same execution"
            );
            for id in [first, second] {
                backend.reply(
                    id,
                    MessageType::Result,
                    Value::Map(vec![(string("value"), Value::Nil)]),
                );
            }
            let _ = [backend.next().await, backend.next().await];
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
            // And a Backend never sends a `Call` or an `Authorize` of its own; that refusal echoes
            // its id.
            for message_type in [MessageType::Call, MessageType::Authorize] {
                backend.reply(CorrelationId(7), message_type, Value::Nil);
                let reply = backend.next().await;
                assert_eq!(reply.id, Some(CorrelationId(7)));
                assert_eq!(code_of(&reply), "protocol.unexpected_message");
            }
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
fn a_function_registered_without_a_grant_asks_and_a_refusal_is_like_an_unregistered_name() {
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
                let (id, name, arguments) = backend.question().await;
                assert_eq!(name, "getOrder");
                assert_eq!(arguments, [Value::from(1)]);
                backend.allow(id, Value::from(false));
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

            // B's handler is asked, on B's connection alone, and refuses.
            b.send(execution(1, "return getOrder(1);"));
            let (id, _, _) = b.question().await;
            b.allow(id, Value::from(false));
            assert_eq!(code_of(&b.next().await), "capability.unknown_function");
            c.send(execution(1, "return getOrder(1);"));
            assert_eq!(code_of(&c.next().await), "capability.unknown_function");

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
            let (id, _, _) = backend.question().await;
            backend.allow(id, Value::from(false));
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
    for (event, (request, function, reason)) in refusals
        .iter()
        .zip([("7", "getOrder", "refused"), ("8", "nope", "unregistered")])
    {
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

// --- Story 3.3: the per-call handler ---

#[test]
fn a_handler_that_allows_is_asked_first_then_the_call_is_made() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", false)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return getOrder(7, \"a\");"));
            let (question, name, arguments) = backend.question().await;
            assert_eq!(name, "getOrder");
            assert_eq!(arguments, [Value::from(7), string("a")]);
            backend.allow(question, Value::from(true));
            let (call, name, arguments) = backend.call().await;
            assert_eq!(name, "getOrder");
            assert_eq!(arguments, [Value::from(7), string("a")]);
            // One per-connection counter issues both ids.
            assert_eq!(call, CorrelationId(question.get() + 1));
            backend.reply(
                call,
                MessageType::Result,
                Value::Map(vec![(string("value"), string("order"))]),
            );
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(value_of(&reply), string("order"));
            // Asked again on the next call: nothing is cached.
            backend.send(execution(2, "return getOrder(8);"));
            let (question, _, _) = backend.question().await;
            backend.allow(question, Value::from(false));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(code_of(&reply), "capability.unknown_function");
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` for it"
            );
        },
    );
    assert!(sessions.is_empty());
}

#[test]
fn every_handler_denial_sends_no_call_and_is_the_same_error() {
    hosted(
        &wired_runtime(),
        &[("getOrder", false)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return nope(1);"));
            let unregistered = backend.next().await;
            let answers = [
                (
                    MessageType::Result,
                    Value::Map(vec![(string("value"), string("yes"))]),
                ),
                (
                    MessageType::Result,
                    Value::Map(vec![(string("value"), Value::from(1))]),
                ),
                (
                    MessageType::Result,
                    Value::Map(vec![(string("value"), Value::Nil)]),
                ),
                (MessageType::Result, Value::Nil),
                (
                    MessageType::Error,
                    Value::Map(vec![(string("message"), string("denied"))]),
                ),
            ];
            for (n, (message_type, payload)) in (2..).zip(answers) {
                backend.send(execution(n, "return getOrder(1);"));
                let (question, _, _) = backend.question().await;
                backend.reply(question, message_type, payload);
                let reply = backend.next().await;
                assert_eq!(reply.id, Some(CorrelationId(n)));
                assert_eq!(reply.message_type, MessageType::Error, "{reply:?}");
                assert_eq!(code_of(&reply), "capability.unknown_function");
                assert_eq!(
                    message_of(&reply),
                    message_of(&unregistered).replace("nope", "getOrder")
                );
            }
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

#[test]
fn a_question_pending_when_the_peer_closes_is_denied() {
    let sessions = hosted(
        &wired_runtime(),
        &[("getOrder", false)],
        false,
        |mut backend| async move {
            backend.send(execution(3, "return getOrder(1);"));
            let _ = backend.question().await;
            backend.close();
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(code_of(&reply), "capability.unknown_function");
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
    assert!(sessions.is_empty(), "the connection detached");
}

#[test]
fn a_silent_handler_is_denied_after_the_timeout_and_its_late_answer_is_dropped() {
    // Story 3.7: the Session's Config sets the timeout, here far below the 5 s default.
    let timeout = Duration::from_millis(100);
    configured(
        &wired_runtime(),
        by_key(&[("authorization_timeout_ms", Value::from(100))]),
        &[("getOrder", false)],
        |mut backend| async move {
            let started = tokio::time::Instant::now();
            backend.send(execution(1, "return getOrder(1);"));
            let (question, _, _) = backend.question().await;
            let reply = backend.next().await;
            let elapsed = started.elapsed();
            assert!(elapsed >= timeout, "{elapsed:?}");
            assert!(elapsed < hexput_exec::AUTHORIZATION_TIMEOUT, "{elapsed:?}");
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "capability.unknown_function");
            // The late answer reaches no one: no `Call`, and no `unexpected_message` either.
            backend.allow(question, Value::from(true));
            backend.send(execution(2, "return 2;"));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)), "{reply:?}");
            assert_eq!(value_of(&reply), Value::from(2));
            backend.close();
            assert!(backend.from_daemon.recv().await.is_none());
        },
    );
}

/// The acceptance criterion: a question waiting for its answer holds no thread. With a pool of
/// exactly one blocking thread, a second execution can only finish meanwhile if the first holds
/// none.
#[test]
fn a_question_waiting_for_its_answer_holds_no_thread() {
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
        &[("getOrder", false)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return getOrder(1) + 1;"));
            let (question, _, _) = backend.question().await;
            backend.send(execution(
                2,
                "let i = 0; while (i < 1000) { i = i + 1; }; return i;",
            ));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(1000));
            backend.allow(question, Value::from(true));
            let (call, _, _) = backend.call().await;
            backend.reply(
                call,
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
fn an_ambient_call_over_the_wire_is_refused_with_nothing_written_but_the_error() {
    hosted(
        &wired_runtime(),
        &[("getOrder", true), ("guarded", false)],
        false,
        |mut backend| async move {
            for (id, name) in [
                (1, "process"),
                (2, "fetch"),
                (3, "require"),
                (4, "globalThis"),
            ] {
                backend.send(execution(id, &format!("return {name}(\"/etc/passwd\");")));
                // The next envelope is the refusal itself: no `Call`, no `Authorize` before it.
                let reply = backend.next().await;
                assert_eq!(reply.message_type, MessageType::Error, "{name}: {reply:?}");
                assert_eq!(reply.id, Some(CorrelationId(id)));
                assert_eq!(code_of(&reply), "capability.unknown_function", "{name}");
            }
            backend.close();
        },
    );
}

// --- Story 3.5: a runaway stops at its budget and disturbs no one ---

#[test]
fn a_runaway_gets_its_budget_error_and_its_neighbours_complete() {
    let dispatch = discarding_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        wired_runtime().block_on(async {
            let sessions = Arc::new(Sessions::new());
            let (mut a, serving_a) = connect(&sessions, init_payload(&[]), false).await;
            let (mut b, serving_b) = connect(&sessions, init_payload(&[]), false).await;
            a.send(execution(1, "while (true) {}"));
            a.send(execution(2, "return 2;"));
            b.send(execution(3, "return 3;"));
            // Its neighbours on the same connection and on another are answered at once.
            let reply = a.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(2));
            let reply = b.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(value_of(&reply), Value::from(3));
            // The runaway ends with its own error once its CPU time is spent.
            let reply = a.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "budget.cpu_time_exceeded");
            // And the connection carries on serving.
            a.send(execution(4, "return 4;"));
            let reply = a.next().await;
            assert_eq!(reply.id, Some(CorrelationId(4)));
            assert_eq!(value_of(&reply), Value::from(4));
            a.close();
            b.close();
            finished(serving_a).await;
            finished(serving_b).await;
        });
    });
}

#[test]
fn a_script_over_its_memory_budget_is_answered_with_a_budget_error() {
    let (sent, _) = serve(
        vec![
            init(1, init_payload(&[])),
            execution(2, "let s = \"x\"; while (true) { s = s + s; }"),
        ],
        None,
    );
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].id, Some(CorrelationId(2)));
    assert_eq!(code_of(&sent[1]), "budget.memory_exceeded");
}

// --- Story 3.6: RPC calls and output size over the wire ---

#[test]
fn an_rpc_flood_is_answered_with_a_budget_error_after_its_hundred_calls() {
    hosted(
        &wired_runtime(),
        &[("ping", true)],
        false,
        |mut backend| async move {
            backend.send(execution(
                1,
                "let n = 0;\nwhile (true) { n = n + 1; ping(n); }",
            ));
            for expected in 1..=100u64 {
                let (id, name, arguments) = backend.call().await;
                assert_eq!(name, "ping");
                assert_eq!(arguments, [Value::from(expected)]);
                backend.reply(
                    id,
                    MessageType::Result,
                    Value::Map(vec![(string("value"), Value::Nil)]),
                );
            }
            // The hundred-and-first call is never written: the next message is the error.
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(reply.message_type, MessageType::Error);
            assert_eq!(code_of(&reply), "budget.rpc_calls_exceeded");
            backend.close();
        },
    );
}

#[test]
fn a_result_past_the_output_size_budget_is_answered_with_a_budget_error() {
    // Two MiB of string: far inside a frame, past the 1 MiB output size budget.
    let (sent, _) = serve(
        vec![
            init(1, init_payload(&[])),
            execution(
                2,
                "let s = \"x\"; let i = 0; while (i < 21) { s = s + s; i = i + 1; }; return s;",
            ),
        ],
        None,
    );
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].id, Some(CorrelationId(2)));
    assert_eq!(code_of(&sent[1]), "budget.output_size_exceeded");
}

// --- Story 3.7: limits from the Session's Config, overridden per execution ---

/// A map with string keys.
fn by_key(fields: &[(&str, Value)]) -> Value {
    Value::Map(
        fields
            .iter()
            .map(|(key, value)| (string(key), value.clone()))
            .collect(),
    )
}

/// A Config or override map setting `budget.<key>` alone.
fn budget_of(key: &str, value: u64) -> Value {
    by_key(&[("budget", by_key(&[(key, Value::from(value))]))])
}

/// An `ExecutionStart` with `overrides`.
fn overridden(id: u64, source: &str, overrides: Value) -> Received {
    let Value::Map(mut fields) = execution_payload(source, vec![]) else {
        unreachable!("an execution payload is a map");
    };
    fields.push((string("overrides"), overrides));
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::ExecutionStart,
        Value::Map(fields),
    ))
}

/// [`hosted`], with the Session's init carrying `config`.
fn configured<F, Fut>(
    runtime: &tokio::runtime::Runtime,
    config: Value,
    registered: &[(&str, bool)],
    drive: F,
) -> Arc<Sessions>
where
    F: FnOnce(Backend) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(&sessions);
    let init_payload = init_configured(config, registered);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (backend, serving) = connect(&registry, init_payload, false).await;
            drive(backend).await;
            finished(serving).await;
        });
    });
    sessions
}

/// Answer the next `count` `Call`s with `Result {value: 1}`.
async fn answer_calls(backend: &mut Backend, count: usize) {
    for _ in 0..count {
        let (id, name, _) = backend.call().await;
        assert_eq!(name, "getOrder");
        backend.reply(
            id,
            MessageType::Result,
            Value::Map(vec![(string("value"), Value::from(1))]),
        );
    }
}

/// A Script making `n` host calls in a row.
/// A Script making `n` calls. Each passes a computed value, so writing `i` afterwards is no write
/// to a referenced place and the result stays exactly `{value}` (Story 3.13).
fn calls(n: usize) -> String {
    format!("let i = 0; while (i < {n}) {{ getOrder(i + 0); i = i + 1; }}; return i;")
}

#[test]
fn the_config_limit_applies_and_an_override_changes_it_for_one_execution_only() {
    let sessions = configured(
        &wired_runtime(),
        budget_of("rpc_calls", 2),
        &[("getOrder", true)],
        |mut backend| async move {
            // Under the Config: two `Call`s, and the third is refused before it is sent.
            backend.send(execution(1, &calls(3)));
            answer_calls(&mut backend, 2).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "budget.rpc_calls_exceeded");

            // Overridden for one execution: all five calls are made.
            backend.send(overridden(2, &calls(5), budget_of("rpc_calls", 5)));
            answer_calls(&mut backend, 5).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(5));

            // The next execution, with no override, is back under the Config.
            backend.send(execution(3, &calls(3)));
            answer_calls(&mut backend, 2).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(code_of(&reply), "budget.rpc_calls_exceeded");
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "nothing else was sent"
            );
        },
    );
    assert!(sessions.is_empty());
}

#[test]
fn an_override_out_of_range_is_refused_naming_its_path_and_nothing_runs() {
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(overridden(
                1,
                "return getOrder(1);",
                budget_of("cpu_time_ms", 0),
            ));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "protocol.invalid_payload");
            assert_eq!(
                message_of(&reply),
                "`overrides.budget.cpu_time_ms` must be an integer from 1 to 60000; found 0"
            );
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

#[test]
fn a_config_out_of_range_or_mistyped_creates_no_session() {
    let cases = [
        (
            budget_of("memory_bytes", 1 << 40),
            "`config.budget.memory_bytes` must be an integer from 1024 to 1073741824; found \
             1099511627776",
        ),
        (
            by_key(&[("budget", by_key(&[("rpc_calls", string("5"))]))]),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a string",
        ),
        (
            by_key(&[("budget", by_key(&[("rpc_calls", Value::F64(1.0))]))]),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a float",
        ),
        (
            by_key(&[("budget", by_key(&[("rpc_calls", Value::from(-1))]))]),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found -1",
        ),
        (
            by_key(&[("budget", by_key(&[("cpu", Value::from(1))]))]),
            "`config.budget.cpu` is not a known setting",
        ),
        (
            by_key(&[("speed", Value::from(1))]),
            "`config.speed` is not a known setting",
        ),
    ];
    for (config, expected) in cases {
        let sessions = Arc::new(Sessions::new());
        let (sent, _) = serve_with(&sessions, vec![init(3, init_configured(config, &[]))], None);
        assert_eq!(sent.len(), 1);
        assert_eq!(code_of(&sent[0]), "protocol.invalid_payload");
        assert_eq!(message_of(&sent[0]), expected);
        assert!(sessions.is_empty(), "no Session after {expected:?}");
    }
}

#[test]
fn the_config_argument_depth_applies_and_an_override_raises_it() {
    configured(
        &wired_runtime(),
        by_key(&[("argument_depth", Value::from(2))]),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(execution(1, "return getOrder([[[1]]]);"));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "depth.argument_too_deep");

            backend.send(overridden(
                2,
                "return getOrder([[[1]]]);",
                by_key(&[("argument_depth", Value::from(3))]),
            ));
            answer_calls(&mut backend, 1).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(1));
            backend.close();
            assert!(backend.from_daemon.recv().await.is_none());
        },
    );
}

#[test]
fn a_result_within_a_raised_output_budget_but_past_a_frame_is_response_too_large() {
    // Deferred from Story 3.6: with the output size budget raised to the frame itself, a result
    // whose `{value}` payload is exactly `MAX_FRAME_LEN` bytes passes the budget, and the
    // envelope around it is what cannot be framed. A string of `MAX_FRAME_LEN - 12` bytes: the
    // payload adds a 1-byte map, a 6-byte `value` key and a 5-byte string header.
    let length = hexput_port::MAX_FRAME_LEN - 12;
    let source = format!(
        "let n = {length}; let piece = \"x\"; let out = \"\"; \
         while (n > 0) {{ if (n % 2 == 1) {{ out = out + piece; }}; \
         piece = piece + piece; n = (n - n % 2) / 2; }}; return out;"
    );
    let overrides = by_key(&[(
        "budget",
        by_key(&[
            (
                "output_size_bytes",
                Value::from(hexput_port::MAX_FRAME_LEN as u64),
            ),
            ("memory_bytes", Value::from(1_u64 << 30)),
            ("cpu_time_ms", Value::from(60_000)),
        ]),
    )]);
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[],
        |mut backend| async move {
            backend.send(overridden(1, &source, overrides));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "protocol.response_too_large");
            // The connection keeps serving.
            backend.send(execution(2, "return 2;"));
            let reply = backend.next().await;
            assert_eq!(value_of(&reply), Value::from(2));
            backend.close();
        },
    );
}

// --- Story 3.8: replacing the Config without reconnecting ---

/// A `ConfigUpdate` with `payload` as its whole payload.
fn config_update_raw(id: u64, payload: Value) -> Received {
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::ConfigUpdate,
        payload,
    ))
}

/// A `ConfigUpdate` carrying `config`.
fn config_update(id: u64, config: Value) -> Received {
    config_update_raw(id, by_key(&[("config", config)]))
}

/// Assert `reply` is the `Result {}` answering the `ConfigUpdate` `id`.
fn assert_updated(reply: &Envelope<Value>, id: u64) {
    assert_eq!(reply.id, Some(CorrelationId(id)));
    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "{:?}",
        reply.payload
    );
    assert_eq!(reply.payload, Value::Map(vec![]), "exactly `Result {{}}`");
}

#[test]
fn an_update_applies_to_the_next_execution_without_reconnecting() {
    configured(
        &wired_runtime(),
        budget_of("rpc_calls", 1),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(config_update(1, budget_of("rpc_calls", 3)));
            assert_updated(&backend.next().await, 1);
            backend.send(execution(2, &calls(3)));
            answer_calls(&mut backend, 3).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(3));
            backend.close();
            assert!(backend.from_daemon.recv().await.is_none());
        },
    );
}

#[test]
fn an_update_replaces_the_config_and_a_left_out_setting_is_back_to_its_default() {
    configured(
        &wired_runtime(),
        by_key(&[
            ("budget", by_key(&[("rpc_calls", Value::from(1))])),
            ("argument_depth", Value::from(2)),
        ]),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(execution(1, "return getOrder([[[1]]]);"));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "depth.argument_too_deep");

            backend.send(config_update(2, budget_of("rpc_calls", 3)));
            assert_updated(&backend.next().await, 2);
            // `argument_depth` is back to its default of 12: a depth of 3 is sent.
            backend.send(execution(3, "return getOrder([[[1]]]);"));
            answer_calls(&mut backend, 1).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(value_of(&reply), Value::from(1));
            backend.close();
        },
    );
}

#[test]
fn a_refused_update_changes_nothing_and_the_next_execution_uses_the_old_limits() {
    configured(
        &wired_runtime(),
        budget_of("rpc_calls", 1),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(config_update(
                1,
                by_key(&[("budget", by_key(&[("rpc_calls", Value::from(-1))]))]),
            ));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "protocol.invalid_payload");
            assert_eq!(
                message_of(&reply),
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found -1"
            );
            // Still one call, as the Config said before the refused update.
            backend.send(execution(2, &calls(2)));
            answer_calls(&mut backend, 1).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(code_of(&reply), "budget.rpc_calls_exceeded");
            backend.close();
        },
    );
}

#[test]
fn a_malformed_update_payload_is_refused_naming_what_is_wrong() {
    let cases = [
        (Value::Nil, "the `ConfigUpdate` payload is missing `config`"),
        (
            Value::Map(vec![]),
            "the `ConfigUpdate` payload is missing `config`",
        ),
        (
            by_key(&[("config", Value::from(1))]),
            "`config` is not a map",
        ),
        (
            by_key(&[("config", Value::Map(vec![])), ("x", Value::from(1))]),
            "the `ConfigUpdate` payload has an unknown key `x`",
        ),
    ];
    for (payload, expected) in cases {
        let sessions = Arc::new(Sessions::new());
        let (sent, _) = serve_with(
            &sessions,
            vec![
                init(1, init_configured(budget_of("rpc_calls", 1), &[])),
                config_update_raw(2, payload),
            ],
            None,
        );
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].id, Some(CorrelationId(2)));
        assert_eq!(code_of(&sent[1]), "protocol.invalid_payload");
        assert_eq!(message_of(&sent[1]), expected);
    }
}

#[test]
fn an_update_before_init_is_refused_and_creates_no_session() {
    let sessions = Arc::new(Sessions::new());
    let (sent, _) = serve_with(
        &sessions,
        vec![config_update(1, budget_of("rpc_calls", 3))],
        None,
    );
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].id, Some(CorrelationId(1)));
    assert_eq!(code_of(&sent[0]), "protocol.init_not_completed");
    // The refusal is the only reply: no `client_id` was ever issued. (`sessions.is_empty()` alone
    // would hold even then, since `serve` detaches on exit.)
    assert!(
        sent.iter()
            .all(|reply| reply.message_type == MessageType::Error),
        "{sent:?}"
    );
    assert!(sessions.is_empty());
}

#[test]
fn a_pipelined_update_is_in_force_for_the_execution_queued_right_behind_it() {
    let sessions = Arc::new(Sessions::new());
    let (sent, _) = serve_with(
        &sessions,
        vec![
            // An output size budget of one byte: `{value: "hello"}` cannot fit it.
            init(1, init_configured(budget_of("output_size_bytes", 1), &[])),
            // Not awaited: the execution is queued right behind the update.
            config_update(2, Value::Map(vec![])),
            execution(3, "return \"hello\";"),
        ],
        None,
    );
    assert_eq!(sent.len(), 3);
    client_id_of(&sent[0]);
    assert_updated(&sent[1], 2);
    assert_eq!(sent[2].id, Some(CorrelationId(3)));
    assert_eq!(value_of(&sent[2]), string("hello"));
}

#[test]
fn an_update_through_one_connection_is_seen_by_every_attached_connection() {
    let runtime = wired_runtime();
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(&sessions);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (mut backend, serving, client_id) = connect_as(
                &registry,
                init_configured(budget_of("rpc_calls", 1), &[("getOrder", true)]),
                false,
            )
            .await;
            // A second Connection on the same Session, as reconnect (Epic 5) will attach one.
            let other = registry.connect();
            assert!(registry.attach(client_id, other));

            backend.send(config_update(1, budget_of("rpc_calls", 3)));
            assert_updated(&backend.next().await, 1);
            let mut expected = Settings::new();
            expected.set(Setting::RpcCalls, 3).unwrap();
            // Whichever Connection dispatches next reads the one live Config.
            let (_, settings) = registry.for_execution(client_id).unwrap();
            assert_eq!(settings, expected);

            // The updating Connection leaves; the other still sees the update.
            backend.close();
            finished(serving).await;
            let (_, settings) = registry.for_execution(client_id).unwrap();
            assert_eq!(settings, expected);
            registry.detach(client_id, other);
        });
    });
    assert!(sessions.is_empty());
}

#[test]
fn an_execution_in_flight_keeps_its_limits_and_the_next_one_gets_the_update() {
    configured(
        &wired_runtime(),
        budget_of("rpc_calls", 2),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(execution(1, &calls(3)));
            // The execution is waiting on its first call when the update arrives.
            let (first, name, _) = backend.call().await;
            assert_eq!(name, "getOrder");
            backend.send(config_update(2, budget_of("rpc_calls", 5)));
            assert_updated(&backend.next().await, 2);
            backend.reply(
                first,
                MessageType::Result,
                Value::Map(vec![(string("value"), Value::from(1))]),
            );
            // Its second call is its last: it still runs under `rpc_calls = 2`.
            answer_calls(&mut backend, 1).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "budget.rpc_calls_exceeded");

            // The next execution runs under the update.
            backend.send(execution(3, &calls(3)));
            answer_calls(&mut backend, 3).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(value_of(&reply), Value::from(3));
            backend.close();
        },
    );
}

#[test]
fn an_override_still_wins_over_an_updated_config_and_changes_nothing_stored() {
    let runtime = wired_runtime();
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(&sessions);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (mut backend, serving, client_id) = connect_as(
                &registry,
                init_configured(budget_of("rpc_calls", 1), &[("getOrder", true)]),
                false,
            )
            .await;
            backend.send(config_update(1, budget_of("rpc_calls", 3)));
            assert_updated(&backend.next().await, 1);

            backend.send(overridden(2, &calls(5), budget_of("rpc_calls", 5)));
            answer_calls(&mut backend, 5).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(2)));
            assert_eq!(value_of(&reply), Value::from(5));

            let mut expected = Settings::new();
            expected.set(Setting::RpcCalls, 3).unwrap();
            assert_eq!(registry.settings(client_id), Some(expected));

            // Without an override, the updated Config's three calls again.
            backend.send(execution(3, &calls(4)));
            answer_calls(&mut backend, 3).await;
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(code_of(&reply), "budget.rpc_calls_exceeded");
            backend.close();
            finished(serving).await;
        });
    });
    assert!(sessions.is_empty());
}

// --- Story 3.9: feature toggles from the Config, overridden per execution ---

/// A Config or override map setting `features.<toggle>` alone.
fn feature_of(toggle: &str, enabled: bool) -> Value {
    by_key(&[("features", by_key(&[(toggle, Value::Boolean(enabled))]))])
}

/// A Script using a `while` loop.
const LOOPING: &str = "let n = 0; while (n < 2) { n = n + 1; }; return n;";

#[test]
fn a_config_toggle_refuses_its_construct_and_an_override_lifts_it_for_one_execution() {
    configured(
        &wired_runtime(),
        feature_of("loops", false),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(execution(1, LOOPING));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "policy.construct_disabled");
            assert_eq!(
                message_of(&reply),
                "`while` is disabled by policy (`features.loops`)"
            );

            backend.send(overridden(2, LOOPING, feature_of("loops", true)));
            let reply = backend.next().await;
            assert_eq!(value_of(&reply), Value::from(2));

            backend.send(execution(3, LOOPING));
            assert_eq!(code_of(&backend.next().await), "policy.construct_disabled");

            // `rpc_calls` refuses even a blanket-granted function, and nothing is written.
            backend.send(overridden(
                4,
                "return getOrder(1);",
                feature_of("rpc_calls", false),
            ));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(4)));
            assert_eq!(code_of(&reply), "policy.construct_disabled");
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

#[test]
fn with_no_toggle_set_every_construct_runs() {
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(execution(
                1,
                "fn f(x) { return x; }; let o = { a: [1] }; let t = 0; \
                 for (k in o) { if (k == \"a\") { t = t + f(getOrder(o.a[0])); }; }; return t;",
            ));
            answer_calls(&mut backend, 1).await;
            let reply = backend.next().await;
            assert_eq!(value_of(&reply), Value::from(1));
            backend.close();
        },
    );
}

#[test]
fn an_override_disabling_a_toggle_changes_nothing_stored() {
    let runtime = wired_runtime();
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(&sessions);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (mut backend, serving, client_id) =
                connect_as(&registry, init_configured(Value::Map(vec![]), &[]), false).await;
            backend.send(overridden(1, LOOPING, feature_of("loops", false)));
            assert_eq!(code_of(&backend.next().await), "policy.construct_disabled");
            assert_eq!(registry.settings(client_id), Some(Settings::new()));
            backend.send(execution(2, LOOPING));
            assert_eq!(value_of(&backend.next().await), Value::from(2));
            backend.close();
            finished(serving).await;
        });
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_config_update_toggles_later_executions_and_a_left_out_toggle_is_enabled_again() {
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[],
        |mut backend| async move {
            backend.send(execution(1, LOOPING));
            assert_eq!(value_of(&backend.next().await), Value::from(2));

            backend.send(config_update(2, feature_of("loops", false)));
            assert_updated(&backend.next().await, 2);
            backend.send(execution(3, LOOPING));
            assert_eq!(code_of(&backend.next().await), "policy.construct_disabled");

            // Replace, never patch: an update that leaves `loops` out enables it again.
            backend.send(config_update(4, Value::Map(vec![])));
            assert_updated(&backend.next().await, 4);
            backend.send(execution(5, LOOPING));
            assert_eq!(value_of(&backend.next().await), Value::from(2));
            backend.close();
        },
    );
}

#[test]
fn an_unknown_or_non_boolean_toggle_creates_no_session() {
    let cases = [
        (
            feature_of("return", false),
            "`config.features.return` is not a known feature toggle (the toggles are loops, \
             conditionals, callbacks, object_literals, array_literals, rpc_calls)",
        ),
        (
            feature_of("variables", false),
            "`config.features.variables` is not a known feature toggle (the toggles are loops, \
             conditionals, callbacks, object_literals, array_literals, rpc_calls)",
        ),
        (
            by_key(&[("features", by_key(&[("loops", Value::from(0))]))]),
            "`config.features.loops` must be a boolean; found 0",
        ),
        (
            by_key(&[("features", by_key(&[("loops", Value::Nil)]))]),
            "`config.features.loops` must be a boolean; found nil",
        ),
    ];
    for (config, expected) in cases {
        let sessions = Arc::new(Sessions::new());
        let (sent, _) = serve_with(&sessions, vec![init(3, init_configured(config, &[]))], None);
        assert_eq!(sent.len(), 1);
        assert_eq!(code_of(&sent[0]), "protocol.invalid_payload");
        assert_eq!(message_of(&sent[0]), expected);
        assert!(sessions.is_empty(), "no Session after {expected:?}");
    }
}

#[test]
fn an_unknown_toggle_in_an_override_runs_nothing() {
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(overridden(
                1,
                "return getOrder(1);",
                feature_of("operators", false),
            ));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "protocol.invalid_payload");
            assert!(
                message_of(&reply)
                    .starts_with("`overrides.features.operators` is not a known feature toggle"),
                "{}",
                message_of(&reply)
            );
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

#[test]
fn a_config_update_with_an_unknown_toggle_is_refused_and_the_stored_toggles_stand() {
    configured(
        &wired_runtime(),
        feature_of("loops", false),
        &[],
        |mut backend| async move {
            backend.send(config_update(1, feature_of("return", false)));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "protocol.invalid_payload");
            assert!(
                message_of(&reply)
                    .starts_with("`config.features.return` is not a known feature toggle"),
                "{}",
                message_of(&reply)
            );
            // `loops` is still off: the refused update changed nothing stored.
            backend.send(execution(2, LOOPING));
            assert_eq!(code_of(&backend.next().await), "policy.construct_disabled");
            backend.close();
        },
    );
}

// --- Story 3.10: the static check mode from the Config, overridden per execution ---

/// A Config or override map setting `check` alone.
fn check_of(mode: Value) -> Value {
    by_key(&[("check", mode)])
}

/// The field `key` of a reply's payload map, if present.
fn field_of(response: &Envelope<Value>, key: &str) -> Option<Value> {
    let Value::Map(fields) = &response.payload else {
        panic!("a payload is a map: {:?}", response.payload);
    };
    fields
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v.clone())
}

/// The `findings` a reply carries, decoded back into bodies.
fn findings_of(response: &Envelope<Value>) -> Vec<hexput_port::ErrorBody> {
    let findings = field_of(response, "findings").expect("a `findings` field");
    hexput_port::rmpv::ext::from_value(findings).expect("findings decode as error bodies")
}

/// A successful reply's `value`, asserting its payload is exactly `{value, findings}`.
fn value_with_findings(response: &Envelope<Value>) -> Value {
    assert_eq!(
        response.message_type,
        MessageType::Result,
        "{:?}",
        response.payload
    );
    let Value::Map(fields) = &response.payload else {
        panic!("a Result payload is a map");
    };
    let keys: Vec<_> = fields.iter().map(|(k, _)| k.as_str().unwrap()).collect();
    assert_eq!(keys, ["value", "findings"]);
    fields[0].1.clone()
}

/// An `ExecutionStart` with starting variables and optional `overrides`.
fn execution_with(
    id: u64,
    source: &str,
    variables: Vec<(&str, Value)>,
    overrides: Option<Value>,
) -> Received {
    let Value::Map(mut fields) = execution_payload(source, variables) else {
        unreachable!("an execution payload is a map");
    };
    if let Some(overrides) = overrides {
        fields.push((string("overrides"), overrides));
    }
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::ExecutionStart,
        Value::Map(fields),
    ))
}

#[test]
fn with_the_check_off_by_default_a_script_runs_unchecked() {
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[],
        |mut backend| async move {
            // An undeclared read on a path never taken: the runtime never reaches it, and no
            // pass reports it.
            backend.send(execution(
                1,
                "if (false) { return x; }; let unused = 1; return 1;",
            ));
            let reply = backend.next().await;
            assert_eq!(value_of(&reply), Value::from(1), "exactly `{{value}}`");
            // The explicit `off` is the same.
            backend.send(overridden(
                2,
                "if (false) { return x; }; return 2;",
                check_of(string("off")),
            ));
            assert_eq!(value_of(&backend.next().await), Value::from(2));
            // A runtime error is still exactly the runtime error.
            backend.send(execution(3, "return x;"));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "reference.undeclared_identifier");
            assert_eq!(field_of(&reply, "findings"), None);
            backend.close();
        },
    );
}

#[test]
fn under_error_a_script_with_an_error_finding_is_rejected_and_no_call_is_sent() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[("f", true)],
        |mut backend| async move {
            backend.send(execution(1, "let unused = 1; f(1); return x;"));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "reference.undeclared_identifier");
            assert_eq!(message_of(&reply), "`x` is not declared");
            let findings = findings_of(&reply);
            let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
            // Every finding, warnings included, in source order.
            assert_eq!(
                codes,
                [
                    "reference.unused_variable",
                    "reference.undeclared_identifier"
                ]
            );
            assert_eq!(findings[0].severity, "warning");
            assert_eq!(findings[1].severity, "error");
            // The rejection's own fields are the first error finding's.
            assert_eq!(findings[1].message, message_of(&reply));
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

#[test]
fn under_error_an_unregistered_call_is_a_finding_at_submission() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[("getOrder", true), ("askFirst", false)],
        |mut backend| async move {
            backend.send(execution(1, "getOrder(1); return g(1);"));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "capability.unknown_function");
            let findings = findings_of(&reply);
            assert_eq!(findings.len(), 1);
            let span = findings[0].span.expect("a spanned finding");
            let source = "getOrder(1); return g(1);";
            assert_eq!(
                &source[span.offset as usize..(span.offset + span.len) as usize],
                "g(1)"
            );
            backend.send(execution(2, "return 5;"));
            assert_eq!(value_of(&backend.next().await), Value::from(5));
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

#[test]
fn a_per_call_registration_is_on_the_callable_list() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[("askFirst", false)],
        |mut backend| async move {
            backend.send(execution(1, "return askFirst(1);"));
            // Not rejected by the check: the Daemon asks the handler before the call.
            let (id, name, _) = backend.question().await;
            assert_eq!(name, "askFirst");
            backend.allow(id, Value::Boolean(false));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "capability.unknown_function");
            assert_eq!(field_of(&reply, "findings"), None, "a runtime refusal");
            backend.close();
        },
    );
}

#[test]
fn under_warn_the_script_runs_and_its_findings_come_with_the_result() {
    configured(
        &wired_runtime(),
        check_of(string("warn")),
        &[],
        |mut backend| async move {
            backend.send(execution(1, "let unused = 1; return 2;"));
            let reply = backend.next().await;
            assert_eq!(value_with_findings(&reply), Value::from(2));
            let findings = findings_of(&reply);
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].severity, "warning");
            assert_eq!(findings[0].code, "reference.unused_variable");

            // Under `warn` an error finding does not reject: the Script runs and fails (or not)
            // on its own.
            backend.send(execution(2, "if (false) { return x; }; return 3;"));
            let reply = backend.next().await;
            assert_eq!(value_with_findings(&reply), Value::from(3));
            assert_eq!(
                findings_of(&reply)[0].code,
                "reference.undeclared_identifier"
            );

            // No findings: exactly `{value}`.
            backend.send(execution(3, "return 4;"));
            assert_eq!(value_of(&backend.next().await), Value::from(4));
            backend.close();
        },
    );
}

#[test]
fn under_error_warnings_alone_run_and_come_with_the_result() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[],
        |mut backend| async move {
            backend.send(execution(1, "let unused = 1; return 2;"));
            let reply = backend.next().await;
            assert_eq!(value_with_findings(&reply), Value::from(2));
            let findings = findings_of(&reply);
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].severity, "warning");
            backend.close();
        },
    );
}

#[test]
fn a_disabled_toggle_is_a_finding_with_the_runtime_message_and_span() {
    let config = by_key(&[
        ("check", string("error")),
        ("features", by_key(&[("loops", Value::Boolean(false))])),
    ]);
    configured(&wired_runtime(), config, &[], |mut backend| async move {
        // On a path never taken: rejected at submission all the same.
        backend.send(execution(1, "if (false) { while (true) { }; }; return 1;"));
        let reply = backend.next().await;
        assert_eq!(code_of(&reply), "policy.construct_disabled");
        assert_eq!(
            message_of(&reply),
            "`while` is disabled by policy (`features.loops`)"
        );

        // On a path taken, the check's body and the runtime's (check off) are the same.
        backend.send(execution(2, LOOPING));
        let checked = backend.next().await;
        backend.send(overridden(3, LOOPING, check_of(string("off"))));
        let ran = backend.next().await;
        assert_eq!(code_of(&ran), "policy.construct_disabled");
        assert_eq!(field_of(&ran, "findings"), None);
        let without_findings = |reply: &Envelope<Value>| {
            let Value::Map(fields) = &reply.payload else {
                unreachable!()
            };
            fields
                .iter()
                .filter(|(k, _)| k.as_str() != Some("findings"))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(without_findings(&checked), without_findings(&ran));
        backend.close();
    });
}

#[test]
fn an_override_checks_one_execution_and_the_stored_mode_stays_off() {
    let runtime = wired_runtime();
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    let registry = Arc::clone(&sessions);
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (mut backend, serving, client_id) =
                connect_as(&registry, init_configured(Value::Map(vec![]), &[]), false).await;
            let unchecked = "if (false) { return x; }; return 1;";
            backend.send(overridden(1, unchecked, check_of(string("error"))));
            assert_eq!(
                code_of(&backend.next().await),
                "reference.undeclared_identifier"
            );
            assert_eq!(registry.settings(client_id), Some(Settings::new()));
            backend.send(execution(2, unchecked));
            assert_eq!(value_of(&backend.next().await), Value::from(1));
            backend.close();
            finished(serving).await;
        });
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_config_update_without_check_turns_it_off_again() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[],
        |mut backend| async move {
            let unchecked = "if (false) { return x; }; return 1;";
            backend.send(execution(1, unchecked));
            assert_eq!(
                code_of(&backend.next().await),
                "reference.undeclared_identifier"
            );
            backend.send(config_update(2, Value::Map(vec![])));
            assert_updated(&backend.next().await, 2);
            backend.send(execution(3, unchecked));
            assert_eq!(value_of(&backend.next().await), Value::from(1));
            backend.send(config_update(4, check_of(string("warn"))));
            assert_updated(&backend.next().await, 4);
            backend.send(execution(5, unchecked));
            assert_eq!(value_with_findings(&backend.next().await), Value::from(1));
            backend.close();
        },
    );
}

#[test]
fn a_bad_check_mode_is_refused_naming_its_path() {
    let cases = [
        (
            string("strict"),
            r#"`config.check` must be one of "off", "warn", "error"; found "strict""#,
        ),
        (
            Value::from(1),
            r#"`config.check` must be one of "off", "warn", "error"; found 1"#,
        ),
        (
            Value::Boolean(true),
            r#"`config.check` must be one of "off", "warn", "error"; found a boolean"#,
        ),
    ];
    for (mode, expected) in cases {
        let sessions = Arc::new(Sessions::new());
        let (sent, _) = serve_with(
            &sessions,
            vec![init(3, init_configured(check_of(mode), &[]))],
            None,
        );
        assert_eq!(code_of(&sent[0]), "protocol.invalid_payload");
        assert_eq!(message_of(&sent[0]), expected);
        assert!(sessions.is_empty());
    }
    configured(
        &wired_runtime(),
        Value::Map(vec![]),
        &[],
        |mut backend| async move {
            backend.send(overridden(1, "return 1;", check_of(string("strict"))));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "protocol.invalid_payload");
            assert!(
                message_of(&reply).starts_with("`overrides.check` must be one of"),
                "{}",
                message_of(&reply)
            );
            backend.close();
        },
    );
}

#[test]
fn a_starting_variable_is_never_a_finding() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[],
        |mut backend| async move {
            backend.send(execution_with(
                1,
                "return total + 1;",
                vec![("total", Value::from(4))],
                None,
            ));
            assert_eq!(value_of(&backend.next().await), Value::from(5));
            backend.close();
        },
    );
}

#[test]
fn a_config_update_with_a_bad_check_is_refused_and_the_stored_mode_stands() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[],
        |mut backend| async move {
            backend.send(config_update(1, check_of(string("strict"))));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "protocol.invalid_payload");
            assert_eq!(
                message_of(&reply),
                r#"`config.check` must be one of "off", "warn", "error"; found "strict""#
            );
            // Still `error`: an untaken undeclared read is rejected.
            backend.send(execution(2, "if (false) { return x; }; return 1;"));
            assert_eq!(
                code_of(&backend.next().await),
                "reference.undeclared_identifier"
            );
            backend.close();
        },
    );
}

#[test]
fn an_error_mode_rejection_asks_no_per_call_handler() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[("askFirst", false)],
        |mut backend| async move {
            backend.send(execution(1, "askFirst(1); return x;"));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(1)));
            assert_eq!(code_of(&reply), "reference.undeclared_identifier");
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Authorize` and no `Call` was written"
            );
        },
    );
}

#[test]
fn a_warn_override_over_an_error_config_runs_a_script_with_an_error_finding() {
    configured(
        &wired_runtime(),
        check_of(string("error")),
        &[],
        |mut backend| async move {
            let source = "if (false) { return x; }; return 1;";
            backend.send(overridden(1, source, check_of(string("warn"))));
            let reply = backend.next().await;
            assert_eq!(value_with_findings(&reply), Value::from(1));
            let findings = findings_of(&reply);
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].code, "reference.undeclared_identifier");
            assert_eq!(findings[0].severity, "error");
            backend.close();
        },
    );
}

#[test]
fn rpc_calls_disabled_under_error_rejects_a_registered_call_and_sends_nothing() {
    let config = by_key(&[
        ("check", string("error")),
        ("features", by_key(&[("rpc_calls", Value::Boolean(false))])),
    ]);
    configured(
        &wired_runtime(),
        config,
        &[("getOrder", true)],
        |mut backend| async move {
            backend.send(execution(1, "if (false) { getOrder(1); }; return 1;"));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "policy.construct_disabled");
            assert_eq!(
                message_of(&reply),
                "a host call to `getOrder` is disabled by policy (`features.rpc_calls`)"
            );
            backend.close();
            assert!(
                backend.from_daemon.recv().await.is_none(),
                "no `Call` was written"
            );
        },
    );
}

// --- Story 3.11: Value Secrets over the wire ---

/// A holder: `{__secret: {…secret}, value}`.
fn holder(secret: Vec<(&str, Value)>, value: Value) -> Value {
    Value::Map(vec![
        (
            string("__secret"),
            Value::Map(secret.into_iter().map(|(k, v)| (string(k), v)).collect()),
        ),
        (string("value"), value),
    ])
}

/// `{name: "a"}` held with `ref` r1, `key` User and a further field `tier`.
fn held_user() -> Value {
    holder(
        vec![
            ("ref", string("r1")),
            ("key", string("User")),
            ("tier", Value::from(3)),
        ],
        Value::Map(vec![(string("name"), string("a"))]),
    )
}

/// The reply to one execution of `source` with `variables`, on a Session with nothing registered.
fn execute_once(source: &str, variables: Vec<(&str, Value)>) -> Envelope<Value> {
    let (sent, _) = serve(
        vec![
            init(1, init_payload(&[])),
            execution_with(2, source, variables, None),
        ],
        None,
    );
    assert_eq!(sent.len(), 2);
    sent[1].clone()
}

/// The `ref` a sent value's holder carries.
fn reference_of(sent: &Value) -> String {
    let Value::Map(fields) = sent else {
        panic!("a holder: {sent:?}");
    };
    let Value::Map(secret) = &fields[0].1 else {
        panic!("a `__secret` map: {sent:?}");
    };
    secret[0].1.as_str().unwrap().to_owned()
}

#[test]
fn a_held_starting_variable_reads_like_its_value() {
    let reply = execute_once("return u.name;", vec![("u", held_user())]);
    assert_eq!(
        reply.payload,
        Value::Map(vec![(string("value"), string("a"))])
    );
}

#[test]
fn a_held_value_returned_goes_back_byte_for_byte() {
    let reply = execute_once("return u;", vec![("u", held_user())]);
    let expected = Value::Map(vec![(string("value"), held_user())]);
    let bytes = |value: &Value| {
        let mut out = Vec::new();
        hexput_port::rmpv::encode::write_value(&mut out, value).unwrap();
        out
    };
    assert_eq!(bytes(&reply.payload), bytes(&expected));
}

#[test]
fn the_secret_is_invisible_and_cannot_be_written() {
    let reply = execute_once(
        "return [u.__secret, u[\"__secret\"], u?.__secret, 5 .__secret];",
        vec![("u", held_user())],
    );
    assert_eq!(
        value_of(&reply),
        Value::Array(vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil])
    );
    let reply = execute_once(
        "u.__secret = 1; u[\"__secret\"] = 2; let o = {__secret: 3, a: 1}; let n = 0; \
         for (k in o) { n = n + 1; }; return [n, o, u, u == u, s == \"x\"];",
        vec![
            ("u", held_user()),
            ("s", holder(vec![("ref", string("r9"))], string("x"))),
        ],
    );
    assert_eq!(
        value_of(&reply),
        Value::Array(vec![
            Value::from(1),
            Value::Map(vec![(string("a"), Value::from(1))]),
            held_user(),
            Value::Boolean(true),
            Value::Boolean(true),
        ])
    );
}

#[test]
fn a_malformed_starting_variable_holder_is_an_invalid_payload() {
    let cases = [
        (
            Value::Map(vec![
                (string("__secret"), Value::from(1)),
                (string("value"), Value::from(2)),
            ]),
            "`variables.u.__secret`",
        ),
        (
            Value::Map(vec![
                (string("__secret"), Value::Map(vec![])),
                (string("value"), Value::from(1)),
            ]),
            "`variables.u.__secret`",
        ),
        (
            holder(
                vec![("ref", string("a"))],
                holder(vec![("ref", string("b"))], Value::from(1)),
            ),
            "`variables.u.value`",
        ),
        (
            Value::Map(vec![
                (
                    string("__secret"),
                    Value::Map(vec![(string("ref"), string("r"))]),
                ),
                (string("value"), Value::from(1)),
                (string("x"), Value::from(2)),
            ]),
            "`variables.u`",
        ),
    ];
    for (variable, path) in cases {
        let reply = execute_once("return 1;", vec![("u", variable)]);
        assert_eq!(code_of(&reply), "protocol.invalid_payload");
        assert!(
            message_of(&reply).starts_with(path),
            "{}",
            message_of(&reply)
        );
    }
}

#[test]
fn every_value_a_call_sends_is_held_and_keeps_its_reference() {
    hosted(
        &wired_runtime(),
        &[("f", true), ("g", true)],
        false,
        |mut backend| async move {
            backend.send(execution_with(
                1,
                "let o = {a: 1}; f(o); f(o); f(u); f(n); let m = n; f(m); \
                 let x = g(); f(x); return 0;",
                vec![
                    ("u", held_user()),
                    ("n", holder(vec![("ref", string("r2"))], Value::from(5))),
                ],
                None,
            ));
            let mut sent = Vec::new();
            for _ in 0..7 {
                let (id, name, arguments, _) = backend.request_from(MessageType::Call).await;
                let answer = if name == "g" {
                    holder(vec![("ref", string("r3"))], Value::from(7))
                } else {
                    Value::Nil
                };
                backend.reply(
                    id,
                    MessageType::Result,
                    Value::Map(vec![(string("value"), answer)]),
                );
                if name == "f" {
                    sent.push(arguments[0].clone());
                }
            }
            let reply = backend.next().await;
            assert_eq!(value_of(&reply), Value::from(0));
            // The same object twice: the same generated IDs, its property's included.
            assert_eq!(sent[0], sent[1]);
            assert!(reference_of(&sent[0]).starts_with("hx:"));
            // The Backend's own secret goes out as it came in.
            let Value::Map(fields) = &sent[2] else {
                panic!("a holder");
            };
            let Value::Map(original) = held_user() else {
                unreachable!()
            };
            assert_eq!(fields[0], original[0], "`__secret` exactly as supplied");
            // A place keeps its Reference ID; its copy is a new place.
            assert_eq!(reference_of(&sent[3]), "r2");
            assert!(reference_of(&sent[4]).starts_with("hx:"));
            // A call's value stored directly keeps the secret it came with.
            assert_eq!(reference_of(&sent[5]), "r3");
            backend.close();
        },
    );
}

#[test]
fn a_reply_with_a_malformed_holder_fails_the_call() {
    hosted(
        &wired_runtime(),
        &[("f", true)],
        false,
        |mut backend| async move {
            backend.send(execution(1, "return f();"));
            let (id, _, _) = backend.call().await;
            backend.reply(
                id,
                MessageType::Result,
                Value::Map(vec![(
                    string("value"),
                    Value::Map(vec![
                        (string("__secret"), Value::from(1)),
                        (string("value"), Value::Nil),
                    ]),
                )]),
            );
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "host.function_failed");
            backend.close();
        },
    );
}

#[test]
fn a_secret_unaware_backend_making_calls_gets_the_plain_result_byte_for_byte() {
    hosted(
        &wired_runtime(),
        &[("f", true)],
        false,
        |mut backend| async move {
            backend.send(execution(
                1,
                "let o = {a: 1}; let n = 2; let a = [o]; f(o); f(n); f(a); f(o); \
                 return [o, n, a, {o: o}];",
            ));
            for _ in 0..4 {
                let (id, _, _) = backend.call().await;
                backend.reply(
                    id,
                    MessageType::Result,
                    Value::Map(vec![(string("value"), Value::Nil)]),
                );
            }
            let reply = backend.next().await;
            let o = Value::Map(vec![(string("a"), Value::from(1))]);
            let expected = Value::Map(vec![(
                string("value"),
                Value::Array(vec![
                    o.clone(),
                    Value::from(2),
                    Value::Array(vec![o.clone()]),
                    Value::Map(vec![(string("o"), o)]),
                ]),
            )]);
            let bytes = |value: &Value| {
                let mut out = Vec::new();
                hexput_port::rmpv::encode::write_value(&mut out, value).unwrap();
                out
            };
            assert_eq!(reply.message_type, MessageType::Result);
            assert_eq!(bytes(&reply.payload), bytes(&expected));
            backend.close();
        },
    );
}

#[test]
fn an_authorize_question_carries_its_arguments_in_holders() {
    hosted(
        &wired_runtime(),
        &[("f", false)],
        false,
        |mut backend| async move {
            backend.send(execution_with(
                1,
                "f(u, 7); return 0;",
                vec![("u", held_user())],
                None,
            ));
            let (question, _, arguments, _) = backend.request_from(MessageType::Authorize).await;
            let Value::Map(original) = held_user() else {
                unreachable!()
            };
            let Value::Map(first) = &arguments[0] else {
                panic!("a holder: {:?}", arguments[0]);
            };
            assert_eq!(first[0], original[0], "the Backend's secret, as supplied");
            assert!(reference_of(&arguments[1]).starts_with("hx:"));
            backend.allow(question, Value::from(true));
            let (call, _, sent, _) = backend.request_from(MessageType::Call).await;
            assert_eq!(
                sent, arguments,
                "the call sends what the question asked about"
            );
            backend.reply(
                call,
                MessageType::Result,
                Value::Map(vec![(string("value"), Value::Nil)]),
            );
            assert_eq!(value_of(&backend.next().await), Value::from(0));
            backend.close();
        },
    );
}

// --- Story 3.12: Registered Methods over the wire ---

/// An `Init` registering `save` under `User` (with `blanket`) and the function `f` (blanket).
fn init_with_method(blanket: bool) -> Value {
    Value::Map(vec![
        (string("config"), Value::Map(vec![])),
        (
            string("registrations"),
            Value::Array(vec![
                Value::Map(vec![
                    (string("name"), string("save")),
                    (string("key"), string("User")),
                    (string("blanket"), Value::from(blanket)),
                ]),
                Value::Map(vec![
                    (string("name"), string("f")),
                    (string("blanket"), Value::from(true)),
                ]),
            ]),
        ),
    ])
}

/// A Daemon request's payload fields, by key.
fn payload_fields(request: &Envelope<Value>) -> Vec<(String, Value)> {
    let Value::Map(fields) = &request.payload else {
        panic!("a request payload is a map: {request:?}");
    };
    fields
        .iter()
        .map(|(key, value)| (key.as_str().unwrap().to_owned(), value.clone()))
        .collect()
}

/// Serve one connection initialized with `init_payload`, driven by `drive`.
fn with_init<F, Fut>(init_payload: Value, drive: F)
where
    F: FnOnce(Backend) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let runtime = wired_runtime();
    let sessions = Arc::new(Sessions::new());
    let dispatch = discarding_dispatch();
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async move {
            let (backend, serving) = connect(&sessions, init_payload, false).await;
            drive(backend).await;
            finished(serving).await;
        });
    });
}

#[test]
fn a_method_call_is_one_call_carrying_the_receiver_with_its_secret() {
    with_init(init_with_method(true), |mut backend| async move {
        backend.send(execution_with(
            4,
            "return u.save(1);",
            vec![("u", held_user())],
            None,
        ));
        let call = backend.next().await;
        assert_eq!(call.message_type, MessageType::Call);
        let fields = payload_fields(&call);
        let keys: Vec<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["name", "arguments", "execution", "receiver"]);
        assert_eq!(fields[0].1, string("save"));
        assert_eq!(fields[2].1, Value::from(4));
        // The receiver's secret exactly as the Backend supplied it.
        let Value::Map(receiver) = &fields[3].1 else {
            panic!("a holder");
        };
        let Value::Map(original) = held_user() else {
            unreachable!()
        };
        assert_eq!(receiver[0], original[0]);
        backend.reply(
            call.id.unwrap(),
            MessageType::Result,
            Value::Map(vec![(string("value"), Value::from(9))]),
        );
        assert_eq!(value_of(&backend.next().await), Value::from(9));
        // A function's call is unchanged: no `receiver` key at all.
        backend.send(execution(5, "return f(1);"));
        let call = backend.next().await;
        let keys: Vec<String> = payload_fields(&call).into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["name", "arguments", "execution"]);
        backend.reply(
            call.id.unwrap(),
            MessageType::Result,
            Value::Map(vec![(string("value"), Value::Nil)]),
        );
        assert_eq!(value_of(&backend.next().await), Value::Nil);
        backend.close();
    });
}

#[test]
fn a_per_call_method_is_asked_about_with_its_receiver_and_refused_like_a_function() {
    with_init(init_with_method(false), |mut backend| async move {
        backend.send(execution_with(
            1,
            "return u.save();",
            vec![("u", held_user())],
            None,
        ));
        let question = backend.next().await;
        assert_eq!(question.message_type, MessageType::Authorize);
        let keys: Vec<String> = payload_fields(&question)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["name", "arguments", "execution", "receiver"]);
        backend.allow(question.id.unwrap(), Value::from(false));
        let reply = backend.next().await;
        assert_eq!(code_of(&reply), "capability.unknown_function");
        backend.close();
    });
}

#[test]
fn unknown_methods_and_overrides_send_nothing() {
    with_init(init_with_method(true), |mut backend| async move {
        backend.send(execution_with(
            1,
            "return u.nope();",
            vec![("u", held_user())],
            None,
        ));
        assert_eq!(
            code_of(&backend.next().await),
            "capability.unknown_function"
        );
        backend.send(execution_with(
            2,
            "u.save = 1; return 0;",
            vec![("u", held_user())],
            None,
        ));
        assert_eq!(code_of(&backend.next().await), "capability.method_override");
        backend.close();
    });
}

#[test]
fn a_bad_method_registration_creates_no_session() {
    for key in [string(""), Value::from(1)] {
        let payload = Value::Map(vec![
            (string("config"), Value::Map(vec![])),
            (
                string("registrations"),
                Value::Array(vec![Value::Map(vec![
                    (string("name"), string("save")),
                    (string("key"), key),
                ])]),
            ),
        ]);
        let sessions = Arc::new(Sessions::new());
        let registry = Arc::clone(&sessions);
        // Probed while the connection is still open: a Session would still be there.
        let sent = serve_probed(&sessions, vec![init(1, payload)], move |_| {
            assert_eq!(registry.len(), 0);
        });
        assert_eq!(code_of(&sent[0]), "protocol.invalid_payload");
    }
}

// --- Story 3.13: modifications both ways over the wire ---

/// `{ref, value}`.
fn modification_entry(reference: &str, value: Value) -> Value {
    Value::Map(vec![
        (string("ref"), string(reference)),
        (string("value"), value),
    ])
}

/// The MessagePack bytes of `value`.
fn wire_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    hexput_port::rmpv::encode::write_value(&mut out, value).unwrap();
    out
}

#[test]
fn a_script_s_write_to_a_referenced_value_comes_back_in_its_result() {
    let reply = execute_once(
        "n = 9; return {ok: true};",
        vec![("n", holder(vec![("ref", string("r1"))], Value::from(8)))],
    );
    let expected = Value::Map(vec![
        (
            string("value"),
            Value::Map(vec![(string("ok"), Value::Boolean(true))]),
        ),
        (
            string("modifications"),
            Value::Array(vec![modification_entry("r1", Value::from(9))]),
        ),
    ]);
    assert_eq!(reply.message_type, MessageType::Result);
    assert_eq!(wire_bytes(&reply.payload), wire_bytes(&expected));
}

#[test]
fn a_referenced_value_only_read_leaves_the_result_exactly_value() {
    let reply = execute_once(
        "let m = n; return u.name;",
        vec![
            ("n", holder(vec![("ref", string("r1"))], Value::from(8))),
            ("u", held_user()),
        ],
    );
    assert_eq!(
        reply.payload,
        Value::Map(vec![(string("value"), string("a"))])
    );
}

#[test]
fn a_failed_script_reports_no_modifications() {
    let reply = execute_once(
        "n = 9; return 1 / 0;",
        vec![("n", holder(vec![("ref", string("r1"))], Value::from(8)))],
    );
    assert_eq!(code_of(&reply), "arithmetic.division_by_zero");
    let Value::Map(fields) = &reply.payload else {
        panic!("an error payload is a map");
    };
    assert!(
        fields
            .iter()
            .all(|(key, _)| key.as_str() != Some("modifications"))
    );
}

#[test]
fn modifications_come_before_the_static_check_s_findings() {
    let reply = {
        let (sent, _) = serve(
            vec![
                init(1, init_payload(&[])),
                execution_with(
                    2,
                    "let unused = 1; n = 9; return 0;",
                    vec![("n", holder(vec![("ref", string("r1"))], Value::from(8)))],
                    Some(Value::Map(vec![(string("check"), string("warn"))])),
                ),
            ],
            None,
        );
        sent[1].clone()
    };
    let Value::Map(fields) = &reply.payload else {
        panic!("a Result payload is a map: {:?}", reply.payload);
    };
    let keys: Vec<&str> = fields
        .iter()
        .map(|(key, _)| key.as_str().unwrap())
        .collect();
    assert_eq!(keys, ["value", "modifications", "findings"]);
}

#[test]
fn a_backend_modification_reaches_the_script_and_its_own_write_comes_back() {
    hosted(
        &wired_runtime(),
        &[("f", true)],
        false,
        |mut backend| async move {
            backend.send(execution_with(
                1,
                "let m = n; let a = {n: 1}; let b = a; let i = 1; f(a, i); i = i + 1; \
                 return [b.n, n, m];",
                vec![("n", holder(vec![("ref", string("r1"))], Value::from(8)))],
                None,
            ));
            let (id, _, arguments, _) = backend.request_from(MessageType::Call).await;
            let a = reference_of(&arguments[0]);
            let i = reference_of(&arguments[1]);
            assert!(a.starts_with("hx:") && i.starts_with("hx:"));
            backend.reply(
                id,
                MessageType::Result,
                Value::Map(vec![
                    (string("value"), Value::Nil),
                    (
                        string("modifications"),
                        Value::Array(vec![
                            modification_entry(&a, Value::Map(vec![(string("n"), Value::from(2))])),
                            modification_entry("r1", Value::from(9)),
                        ]),
                    ),
                ]),
            );
            let reply = backend.next().await;
            assert_eq!(reply.message_type, MessageType::Result, "{reply:?}");
            let Value::Map(fields) = &reply.payload else {
                panic!("a Result payload is a map");
            };
            assert_eq!(
                fields[0].1,
                Value::Array(vec![Value::from(2), Value::from(9), Value::from(8)])
            );
            // Only the Script's own write comes back: `i`, under the ID its call gave it.
            assert_eq!(
                fields[1],
                (
                    string("modifications"),
                    Value::Array(vec![modification_entry(&i, Value::from(2))])
                )
            );
            backend.close();
        },
    );
}

// --- Story 4.1: register a Script once for repeated use ---

/// Statements in the registration that outlasts the fast execution submitted beside it.
const LARGE_REGISTRATION_STATEMENTS: usize = 200_000;

/// [`serve`]'s run against `cache`, which the test keeps to inspect after the connection ends.
fn serve_cached(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    script: Vec<Received>,
) -> (Vec<Envelope<Value>>, usize) {
    let (written, left, _) = run_timed_with_cache(sessions, cache, script, None, None);
    (written, left)
}

/// A `CodeRegister` request for `source`, with no starting-variable names.
fn register(id: u64, source: &str) -> Received {
    register_with(id, source, None)
}

/// A `CodeRegister` request for `source`, naming its starting variables `variables` when given.
fn register_with(id: u64, source: &str, variables: Option<&[&str]>) -> Received {
    let mut fields = vec![(string("source"), string(source))];
    if let Some(variables) = variables {
        fields.push((
            string("variables"),
            Value::Array(variables.iter().map(|name| string(name)).collect()),
        ));
    }
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::CodeRegister,
        Value::Map(fields),
    ))
}

/// The `handle` of a successful `CodeRegister` reply: exactly `handle` when no check ran, and a
/// 16-character lowercase hex text.
fn handle_of(response: &Envelope<Value>) -> ScriptHandle {
    assert_eq!(
        response.message_type,
        MessageType::Result,
        "{:?}",
        response.payload
    );
    let text = field_of(response, "handle")
        .expect("a `handle` field")
        .as_str()
        .expect("a string handle")
        .to_owned();
    assert_eq!(text.len(), 16, "{text}");
    assert!(
        text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{text}"
    );
    text.parse().unwrap()
}

/// The keys of a reply's payload map, in order.
fn keys_of(response: &Envelope<Value>) -> Vec<String> {
    response
        .payload
        .as_map()
        .expect("a payload map")
        .iter()
        .map(|(key, _)| key.as_str().expect("a string key").to_owned())
        .collect()
}

#[test]
fn a_registration_parses_once_and_its_handle_names_the_cached_program() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, left) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, "return 1 + x;")],
    );
    assert_eq!(left, 0);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].id, Some(CorrelationId(2)));
    assert_eq!(keys_of(&sent[1]), ["handle"]);
    let handle = handle_of(&sent[1]);
    let client = client_id_of(&sent[0]);
    let program = cache
        .get(client, handle)
        .expect("the handle names exactly that entry");
    assert_eq!(*program, hexput_parser::parse("return 1 + x;").unwrap());
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn every_registration_gets_a_new_handle_even_for_the_same_source() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[])),
            register(2, "return 1;"),
            register(3, "return 1;"),
        ],
    );
    let (first, second) = (handle_of(&sent[1]), handle_of(&sent[2]));
    assert_ne!(first, second);
    let client = client_id_of(&sent[0]);
    assert_eq!(cache.entry_count(), 2);
    assert!(cache.get(client, first).is_some());
    assert!(cache.get(client, second).is_some());
    let mut handles = cache.handles(client);
    handles.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(handles, expected);
}

#[test]
fn a_source_that_fails_to_parse_is_answered_with_its_diagnostic_and_creates_no_entry() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, "let = ;")],
    );
    assert_eq!(sent[1].id, Some(CorrelationId(2)));
    assert_eq!(sent[1].message_type, MessageType::Error);
    assert!(code_of(&sent[1]).starts_with("syntax."), "{sent:?}");
    assert!(field_of(&sent[1], "span").is_some(), "the parser's span");
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn a_registration_before_init_is_refused_and_creates_no_entry() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(&sessions, &cache, vec![register(1, "return 1;")]);
    assert_eq!(outcome(&sent[0]), "protocol.init_not_completed");
    assert_eq!(cache.entry_count(), 0);
    assert!(sessions.is_empty());
}

#[test]
fn a_malformed_registration_is_refused_naming_what_is_wrong_and_creates_no_entry() {
    let source = |text: &str| (string("source"), string(text));
    // Each case names the key, or the item, its refusal must mention — any of the alternatives.
    let cases: Vec<(Value, &[&str])> = vec![
        (Value::Nil, &["`source`"]),
        (Value::Map(vec![]), &["`source`"]),
        (
            Value::Map(vec![(string("source"), Value::from(1))]),
            &["`source`"],
        ),
        (
            Value::Map(vec![source("1;"), (string("x"), Value::from(1))]),
            &["`x`"],
        ),
        (
            Value::Map(vec![
                source("1;"),
                (string("overrides"), Value::Map(vec![])),
            ]),
            &["`overrides`"],
        ),
        (
            Value::Map(vec![
                source("1;"),
                (string("variables"), Value::Map(vec![])),
            ]),
            &["`variables`"],
        ),
        (
            Value::Map(vec![
                source("1;"),
                (
                    string("variables"),
                    Value::Array(vec![string("a"), string("a")]),
                ),
            ]),
            &["variables[1]", "`a`", "\"a\""],
        ),
        (
            Value::Map(vec![
                source("1;"),
                (string("variables"), Value::Array(vec![string("1a")])),
            ]),
            &["variables[0]", "`1a`", "\"1a\""],
        ),
    ];
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    for (payload, mentions) in cases {
        let (sent, _) = serve_cached(
            &sessions,
            &cache,
            vec![
                init(1, init_payload(&[])),
                Received::Message(Envelope::new(
                    CorrelationId(2),
                    MessageType::CodeRegister,
                    payload.clone(),
                )),
            ],
        );
        assert_eq!(code_of(&sent[1]), "protocol.invalid_payload", "{payload:?}");
        let message = message_of(&sent[1]);
        assert!(
            mentions.iter().any(|m| message.contains(m)),
            "{payload:?} refused as {message:?}, which names none of {mentions:?}"
        );
    }
    assert_eq!(cache.entry_count(), 0);
    assert!(sessions.is_empty());
}

#[test]
fn under_warn_a_registration_is_cached_and_answered_with_its_findings() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_configured(check_of(string("warn")), &[])),
            register_with(2, "let unused = 1; return x;", Some(&["x"])),
        ],
    );
    assert_eq!(keys_of(&sent[1]), ["handle", "findings"]);
    handle_of(&sent[1]);
    let findings = findings_of(&sent[1]);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].code, "reference.unused_variable");
    assert_eq!(findings[0].severity, "warning");
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn under_error_an_error_finding_rejects_the_registration_and_creates_no_entry() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_configured(check_of(string("error")), &[])),
            register(2, "return y;"),
        ],
    );
    assert_eq!(sent[1].message_type, MessageType::Error);
    assert_eq!(code_of(&sent[1]), "reference.undeclared_identifier");
    let findings = findings_of(&sent[1]);
    assert_eq!(
        findings.iter().map(|f| f.code.as_str()).collect::<Vec<_>>(),
        ["reference.undeclared_identifier"]
    );
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn under_error_naming_the_starting_variables_is_accepted_without_findings() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_configured(check_of(string("error")), &[])),
            register_with(2, "return y;", Some(&["y"])),
        ],
    );
    assert_eq!(keys_of(&sent[1]), ["handle"]);
    handle_of(&sent[1]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn under_error_a_registered_function_is_callable_in_a_registration() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(
                1,
                init_configured(check_of(string("error")), &[("f", false)]),
            ),
            register(2, "if (false) { f(1); }; return 1;"),
        ],
    );
    assert_eq!(keys_of(&sent[1]), ["handle"]);
    handle_of(&sent[1]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_registration_reads_the_live_config_its_check_mode_and_toggles() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let loops_off = by_key(&[
        ("check", string("error")),
        ("features", by_key(&[("loops", Value::Boolean(false))])),
    ]);
    let (mut sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[])),
            // Registered under the Config as `Init` left it: the check is off.
            register(2, "return y;"),
            config_update(3, check_of(string("error"))),
            // The update applies to the next registration without reconnecting.
            register(4, "return y;"),
            config_update(5, loops_off),
            register(6, "while (false) { let a = 1; }; return 1;"),
        ],
    );
    // A registration's reply is written when its task finishes; each read the Config at dispatch.
    sent.sort_by_key(|reply| reply.id.map(|id| id.0));
    assert_eq!(ids(&sent), [1, 2, 3, 4, 5, 6]);
    handle_of(&sent[1]);
    assert_updated(&sent[2], 3);
    assert_eq!(code_of(&sent[3]), "reference.undeclared_identifier");
    assert_updated(&sent[4], 5);
    assert_eq!(code_of(&sent[5]), "policy.construct_disabled");
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn with_the_check_off_no_pass_runs_and_the_registration_is_plain() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, "return y;")],
    );
    assert_eq!(keys_of(&sent[1]), ["handle"]);
    handle_of(&sent[1]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn two_sessions_registering_the_same_source_get_entries_only_they_can_find() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let source = "return 1 + x;";
    let (a, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, source)],
    );
    let (b, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, source)],
    );
    let (client_a, client_b) = (client_id_of(&a[0]), client_id_of(&b[0]));
    let (handle_a, handle_b) = (handle_of(&a[1]), handle_of(&b[1]));
    assert_ne!(client_a, client_b);
    assert_ne!(handle_a, handle_b);
    assert!(cache.get(client_a, handle_a).is_some());
    assert!(cache.get(client_b, handle_b).is_some());
    assert!(cache.get(client_a, handle_b).is_none());
    assert!(cache.get(client_b, handle_a).is_none());
    assert_eq!(cache.handles(client_a), vec![handle_a]);
    assert_eq!(cache.handles(client_b), vec![handle_b]);
    assert_eq!(cache.entry_count(), 2);
}

#[test]
fn a_direct_execution_never_touches_the_cache() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), execution(2, "return 1;")],
    );
    assert_eq!(
        sent[1].payload,
        Value::Map(vec![(string("value"), Value::from(1))])
    );
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn a_large_registration_does_not_hold_back_a_fast_execution_on_its_connection() {
    let mut source: String = (0..LARGE_REGISTRATION_STATEMENTS)
        .map(|n| format!("let v{n} = {n};\n"))
        .collect();
    source.push_str("return 0;");
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, left) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[])),
            register(2, &source),
            execution(3, "return 3;"),
        ],
    );
    assert_eq!(left, 0);
    assert_eq!(ids(&sent), [1, 3, 2], "the registration completes last");
    assert_eq!(
        sent[1].payload,
        Value::Map(vec![(string("value"), Value::from(3))])
    );
    handle_of(&sent[2]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_successful_registration_is_logged_at_debug_under_its_request() {
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
    let mut registered = None;
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let sessions = Arc::new(Sessions::new());
            let (mut backend, serving) = connect(&sessions, init_payload(&[]), false).await;
            backend.send(register(7, "return 1;"));
            let reply = backend.next().await;
            registered = Some(handle_of(&reply));
            backend.close();
            finished(serving).await;
        });
    });
    let handle = registered.expect("the registration was answered");
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["fields"]["message"] == "script registered")
        .collect();
    assert_eq!(events.len(), 1, "{text}");
    let event = &events[0];
    assert_eq!(event["level"], "DEBUG", "{event}");
    assert_eq!(event["fields"]["handle"], handle.to_string(), "{event}");
    assert_eq!(event["span"]["name"], "request", "{event}");
    assert_eq!(event["span"]["id"], "7", "{event}");
    let spans = event["spans"].as_array().expect("span fields");
    assert_eq!(spans[0]["name"], "connection", "{event}");
    assert_eq!(
        spans[0]["client_id"].as_str().map(str::len),
        Some(ClientId::TEXT_LEN),
        "{event}"
    );
}

// --- Story 4.2: re-run a registered Script with fresh variables ---

/// A `CachedExecutionStart` request for `handle` (16 hex characters) with `variables`.
fn cached_run(id: u64, handle: &str, variables: Vec<(&str, Value)>) -> Received {
    cached_run_with(id, handle, variables, None)
}

/// [`cached_run`] with `overrides` when given.
fn cached_run_with(
    id: u64,
    handle: &str,
    variables: Vec<(&str, Value)>,
    overrides: Option<Value>,
) -> Received {
    let mut fields = vec![
        (string("handle"), string(handle)),
        (
            string("variables"),
            Value::Map(
                variables
                    .into_iter()
                    .map(|(name, value)| (string(name), value))
                    .collect(),
            ),
        ),
    ];
    if let Some(overrides) = overrides {
        fields.push((string("overrides"), overrides));
    }
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::CachedExecutionStart,
        Value::Map(fields),
    ))
}

#[test]
fn a_cached_run_is_answered_with_its_value_and_its_entry_is_reused() {
    hosted(&wired_runtime(), &[], false, |mut backend| async move {
        backend.send(register(2, "return grade >= 50;"));
        let handle = handle_of(&backend.next().await).to_string();
        backend.send(cached_run(3, &handle, vec![("grade", Value::from(72))]));
        let reply = backend.next().await;
        assert_eq!(reply.id, Some(CorrelationId(3)));
        assert_eq!(value_of(&reply), Value::Boolean(true));
        backend.send(cached_run(4, &handle, vec![("grade", Value::from(12))]));
        assert_eq!(value_of(&backend.next().await), Value::Boolean(false));
        backend.close();
    });
}

#[test]
fn a_cached_run_before_init_is_refused_as_init_not_completed() {
    let (sent, _) = serve(vec![cached_run(1, "0000000000000001", vec![])], None);
    assert_eq!(code_of(&sent[0]), "protocol.init_not_completed");
}

#[test]
fn a_cached_run_of_an_unknown_handle_names_it_and_says_to_register_again() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[])),
            cached_run(2, "00000000000000ff", vec![]),
        ],
    );
    assert_eq!(sent[1].id, Some(CorrelationId(2)));
    assert_eq!(code_of(&sent[1]), "protocol.unknown_script_handle");
    assert!(message_of(&sent[1]).contains("00000000000000ff"));
    assert!(message_of(&sent[1]).contains("CodeRegister"));
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn a_handle_another_session_registered_is_unknown_here() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (registered_by_a, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, "return 1;")],
    );
    let theirs = handle_of(&registered_by_a[1]).to_string();
    let (mut by_b, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[])),
            cached_run(2, &theirs, vec![]),
            cached_run(3, "00000000000000ff", vec![]),
        ],
    );
    by_b.sort_by_key(|reply| reply.id.map(|id| id.0));
    assert_eq!(by_b[1].id, Some(CorrelationId(2)));
    assert_eq!(code_of(&by_b[1]), "protocol.unknown_script_handle");
    assert_eq!(code_of(&by_b[2]), "protocol.unknown_script_handle");
    assert_eq!(
        message_of(&by_b[1]).replace(&theirs, "HANDLE"),
        message_of(&by_b[2]).replace("00000000000000ff", "HANDLE"),
    );
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_malformed_cached_run_is_answered_as_invalid_payload_and_the_connection_keeps_serving() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let source = Received::Message(Envelope::new(
        CorrelationId(2),
        MessageType::CachedExecutionStart,
        Value::Map(vec![
            (string("handle"), string("0000000000000001")),
            (string("variables"), Value::Map(vec![])),
            (string("source"), string("1;")),
        ]),
    ));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[])),
            source,
            execution(3, "return 1;"),
        ],
    );
    let refused = sent
        .iter()
        .find(|reply| reply.id == Some(CorrelationId(2)))
        .expect("answered");
    assert_eq!(code_of(refused), "protocol.invalid_payload");
    assert!(message_of(refused).contains("`source`"));
    let ran = sent
        .iter()
        .find(|reply| reply.id == Some(CorrelationId(3)))
        .expect("answered");
    assert_eq!(value_of(ran), Value::from(1));
}

#[test]
fn a_fast_cached_run_is_answered_before_a_slow_one_submitted_earlier() {
    let slow_source = format!("let i = 0; while (i < {SLOW_TURNS}) {{ i = i + 1; }}; return i;");
    hosted(&wired_runtime(), &[], false, |mut backend| async move {
        backend.send(register(2, &slow_source));
        let slow = handle_of(&backend.next().await).to_string();
        backend.send(register(3, "return 3;"));
        let fast = handle_of(&backend.next().await).to_string();
        backend.send(cached_run(10, &slow, vec![]));
        backend.send(cached_run(11, &fast, vec![]));
        let first = backend.next().await;
        assert_eq!(first.id, Some(CorrelationId(11)), "completion order");
        assert_eq!(value_of(&first), Value::from(3));
        let second = backend.next().await;
        assert_eq!(second.id, Some(CorrelationId(10)));
        assert_eq!(value_of(&second), Value::from(SLOW_TURNS));
        backend.close();
    });
}

#[test]
fn a_cached_run_under_a_config_update_sees_the_live_budget_and_toggles() {
    hosted(
        &wired_runtime(),
        &[("getOrder", true)],
        false,
        |mut backend| async move {
            backend.send(register(2, "return getOrder(1);"));
            let calls = handle_of(&backend.next().await).to_string();
            backend.send(config_update(3, budget_of("rpc_calls", 0)));
            assert_updated(&backend.next().await, 3);
            backend.send(cached_run(4, &calls, vec![]));
            assert_eq!(code_of(&backend.next().await), "budget.rpc_calls_exceeded");
            backend.close();
        },
    );
    hosted(&wired_runtime(), &[], false, |mut backend| async move {
        backend.send(register(2, COUNTED_LOOP_SOURCE));
        let looping = handle_of(&backend.next().await).to_string();
        backend.send(config_update(3, feature_of("loops", false)));
        assert_updated(&backend.next().await, 3);
        backend.send(cached_run(4, &looping, vec![]));
        assert_eq!(code_of(&backend.next().await), "policy.construct_disabled");
        // An override lifts the toggle for this run alone.
        backend.send(cached_run_with(
            5,
            &looping,
            vec![],
            Some(feature_of("loops", true)),
        ));
        assert_eq!(value_of(&backend.next().await), Value::from(2));
        backend.send(cached_run(6, &looping, vec![]));
        assert_eq!(code_of(&backend.next().await), "policy.construct_disabled");
        backend.close();
    });
}

/// The counted loop of [`a_cached_run_under_a_config_update_sees_the_live_budget_and_toggles`].
const COUNTED_LOOP_SOURCE: &str = "let n = 0; while (n < 2) { n = n + 1; }; return n;";

#[test]
fn a_cached_run_is_checked_by_registration_only_and_an_override_check_is_refused() {
    hosted(&wired_runtime(), &[], false, |mut backend| async move {
        // Registered with the check off: the error finding sits on a path that never runs.
        backend.send(register(2, "if (false) { return z; }; return 1;"));
        let handle = handle_of(&backend.next().await).to_string();
        backend.send(config_update(3, check_of(string("error"))));
        assert_updated(&backend.next().await, 3);
        backend.send(cached_run(4, &handle, vec![]));
        assert_eq!(value_of(&backend.next().await), Value::from(1));
        backend.send(cached_run_with(
            5,
            &handle,
            vec![],
            Some(check_of(string("warn"))),
        ));
        let refused = backend.next().await;
        assert_eq!(code_of(&refused), "protocol.invalid_payload");
        assert!(message_of(&refused).contains("`overrides.check`"));
        assert!(message_of(&refused).contains("CodeRegister"));
        backend.close();
    });
}

#[test]
fn a_cached_run_makes_its_call_with_its_own_request_id_as_the_execution() {
    hosted(
        &wired_runtime(),
        &[("echo", true)],
        false,
        |mut backend| async move {
            backend.send(register(2, "return echo(1);"));
            let handle = handle_of(&backend.next().await).to_string();
            backend.send(cached_run(10, &handle, vec![]));
            let (call, name, _, from) = backend.request_from(MessageType::Call).await;
            assert_eq!((name.as_str(), from), ("echo", 10));
            backend.reply(
                call,
                MessageType::Result,
                Value::Map(vec![(string("value"), Value::from(5))]),
            );
            assert_eq!(value_of(&backend.next().await), Value::from(5));
            backend.close();
        },
    );
}

#[test]
fn a_failed_cached_run_is_logged_at_debug_under_its_request() {
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
            let (mut backend, serving) = connect(&sessions, init_payload(&[]), false).await;
            backend.send(cached_run(7, "00000000000000ff", vec![]));
            let reply = backend.next().await;
            assert_eq!(code_of(&reply), "protocol.unknown_script_handle");
            backend.close();
            finished(serving).await;
        });
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["fields"]["message"] == "a Cached Execution failed")
        .collect();
    assert_eq!(events.len(), 1, "{text}");
    let event = &events[0];
    assert_eq!(event["level"], "DEBUG", "{event}");
    assert_eq!(
        event["fields"]["code"], "protocol.unknown_script_handle",
        "{event}"
    );
    assert_eq!(event["fields"]["handle"], "00000000000000ff", "{event}");
    assert_eq!(event["span"]["name"], "request", "{event}");
    assert_eq!(event["span"]["id"], "7", "{event}");
}

// --- Story 4.3: bound the cache and clean it up with the Session ---

/// A `CodeUnregister` request removing `handle`.
fn unregister_request(id: u64, handle: &str) -> Received {
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::CodeUnregister,
        Value::Map(vec![(string("handle"), string(handle))]),
    ))
}

#[test]
fn code_unregister_before_init_is_init_not_completed() {
    let (sent, _) = serve(vec![message(1, MessageType::CodeUnregister)], None);
    assert_eq!(code_of(&sent[0]), "protocol.init_not_completed");
}

#[test]
fn a_code_unregister_is_answered_with_an_empty_result_and_its_entry_is_gone_before_the_reply() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let probe = Arc::clone(&cache);
    hosted_on(
        &wired_runtime(),
        &Arc::new(Sessions::new()),
        &cache,
        &[],
        false,
        |mut backend| async move {
            backend.send(register(2, "return 1;"));
            let handle = handle_of(&backend.next().await).to_string();
            backend.send(unregister_request(3, &handle));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(3)));
            assert_eq!(
                reply.message_type,
                MessageType::Result,
                "{:?}",
                reply.payload
            );
            assert_eq!(reply.payload, Value::Map(vec![]));
            // The reply is read only after the removal, so the entry is already gone.
            assert_eq!(probe.entry_count(), 0);
            backend.close();
        },
    );
}

#[test]
fn a_malformed_code_unregister_is_refused_by_key_and_the_entry_survives() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let probe = Arc::clone(&cache);
    hosted_on(
        &wired_runtime(),
        &Arc::new(Sessions::new()),
        &cache,
        &[],
        false,
        |mut backend| async move {
            backend.send(register(2, "return 1;"));
            let handle = handle_of(&backend.next().await).to_string();
            backend.send(Received::Message(Envelope::new(
                CorrelationId(3),
                MessageType::CodeUnregister,
                Value::Map(vec![(string("handle"), Value::from(1))]),
            )));
            let refused = backend.next().await;
            assert_eq!(code_of(&refused), "protocol.invalid_payload");
            assert!(
                message_of(&refused).contains("`handle`"),
                "{}",
                message_of(&refused)
            );
            assert_eq!(probe.entry_count(), 1, "nothing was removed");
            // The connection keeps serving, and the valid removal then works.
            backend.send(unregister_request(4, &handle));
            assert_eq!(backend.next().await.message_type, MessageType::Result);
            assert_eq!(probe.entry_count(), 0);
            backend.close();
        },
    );
}

#[test]
fn a_code_unregister_of_a_foreign_or_removed_handle_is_unknown_and_leaves_the_owner_alone() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let probe = Arc::clone(&cache);
    let sessions = Arc::new(Sessions::new());
    // Session A registers handle 1; it stays after A's connection ends, as no teardown is wired.
    hosted_on(
        &wired_runtime(),
        &sessions,
        &cache,
        &[],
        false,
        |mut backend| async move {
            backend.send(register(2, "return 1;"));
            assert_eq!(
                handle_of(&backend.next().await).to_string(),
                "0000000000000001"
            );
            backend.close();
        },
    );
    hosted_on(
        &wired_runtime(),
        &sessions,
        &cache,
        &[],
        false,
        |mut backend| async move {
            backend.send(register(2, "return 2;"));
            assert_eq!(
                handle_of(&backend.next().await).to_string(),
                "0000000000000002"
            );
            // Session B cannot remove A's handle.
            backend.send(unregister_request(3, "0000000000000001"));
            let foreign = backend.next().await;
            assert_eq!(code_of(&foreign), "protocol.unknown_script_handle");
            assert_eq!(probe.entry_count(), 2, "A's entry is untouched");
            // B removes its own once; a second removal is unknown.
            backend.send(unregister_request(4, "0000000000000002"));
            assert_eq!(backend.next().await.message_type, MessageType::Result);
            backend.send(unregister_request(5, "0000000000000002"));
            assert_eq!(
                code_of(&backend.next().await),
                "protocol.unknown_script_handle"
            );
            backend.close();
        },
    );
}

#[test]
fn the_run_that_fetched_a_handle_survives_its_removal_and_the_next_run_is_unknown() {
    // Story 4.2's deferred test: the run holds its program, so removing the handle under it
    // changes nothing for that run.
    let source = format!(
        "let r = echo(1); let i = 0; while (i < {SLOW_TURNS}) {{ i = i + 1; }}; return r + i;"
    );
    hosted(
        &wired_runtime(),
        &[("echo", true)],
        false,
        |mut backend| async move {
            backend.send(register(2, &source));
            let handle = handle_of(&backend.next().await).to_string();
            backend.send(cached_run(10, &handle, vec![]));
            // The run has started: its first host call has reached the Backend.
            let (call, _, _, from) = backend.request_from(MessageType::Call).await;
            assert_eq!(from, 10);
            backend.send(unregister_request(11, &handle));
            let removed = backend.next().await;
            assert_eq!(removed.id, Some(CorrelationId(11)));
            assert_eq!(removed.message_type, MessageType::Result);
            backend.reply(
                call,
                MessageType::Result,
                Value::Map(vec![(string("value"), Value::from(5))]),
            );
            let finished_run = backend.next().await;
            assert_eq!(finished_run.id, Some(CorrelationId(10)));
            assert_eq!(value_of(&finished_run), Value::from(5 + SLOW_TURNS));
            backend.send(cached_run(12, &handle, vec![]));
            assert_eq!(
                code_of(&backend.next().await),
                "protocol.unknown_script_handle"
            );
            backend.close();
        },
    );
}

#[test]
fn a_capacity_eviction_is_answered_unknown_over_the_wire_and_the_kept_handle_runs() {
    let cache = Arc::new(AstCache::with_capacity(1));
    let sessions = Arc::new(Sessions::new());
    let runtime = wired_runtime();
    tracing::dispatcher::with_default(&discarding_dispatch(), || {
        runtime.block_on(async {
            let (mut backend, serving, client) =
                connect_on(&sessions, &cache, init_payload(&[]), false).await;
            backend.send(register(2, "return 1;"));
            let first = handle_of(&backend.next().await).to_string();
            backend.send(register(3, "return 2;"));
            let second = handle_of(&backend.next().await).to_string();
            let kept = cache.handles(client);
            assert_eq!(kept.len(), 1, "{kept:?}");
            let kept = kept[0].to_string();
            let evicted = if kept == first {
                second.clone()
            } else {
                first.clone()
            };
            backend.send(cached_run(4, &evicted, vec![]));
            assert_eq!(
                code_of(&backend.next().await),
                "protocol.unknown_script_handle"
            );
            backend.send(cached_run(5, &kept, vec![]));
            let expected = if kept == first { 1 } else { 2 };
            assert_eq!(value_of(&backend.next().await), Value::from(expected));
            backend.close();
            finished(serving).await;
        });
    });
}

#[test]
fn a_teardown_releases_its_session_and_leaves_another_sessions_entries() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let sessions = torn_down_by(&cache);
    let runtime = wired_runtime();
    tracing::dispatcher::with_default(&discarding_dispatch(), || {
        runtime.block_on(async {
            let (mut a, serving_a, client_a) =
                connect_on(&sessions, &cache, init_payload(&[]), false).await;
            let (mut b, serving_b, client_b) =
                connect_on(&sessions, &cache, init_payload(&[]), false).await;
            a.send(register(2, "return 1;"));
            handle_of(&a.next().await);
            b.send(register(2, "return 2;"));
            let theirs = handle_of(&b.next().await);
            // A's last connection closes: its Session is torn down, B's entries stay.
            a.close();
            finished(serving_a).await;
            assert!(cache.handles(client_a).is_empty());
            assert_eq!(cache.handles(client_b), vec![theirs]);
            assert_eq!(cache.entry_count(), 1);
            b.close();
            finished(serving_b).await;
            assert!(cache.handles(client_b).is_empty());
            assert_eq!(cache.entry_count(), 0);
        });
    });
}

#[test]
fn a_session_whose_only_connection_closes_cleanly_leaves_no_entry() {
    // The serial variant: a Session's entries are gone once its only connection has ended.
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let sessions = torn_down_by(&cache);
    let (first, _) = serve_cached(
        &sessions,
        &cache,
        vec![init(1, init_payload(&[])), register(2, "return 1;")],
    );
    // The registration did store an entry, so its absence below is the teardown's doing.
    assert_eq!(first[1].message_type, MessageType::Result, "{:?}", first[1]);
    handle_of(&first[1]);
    let gone = client_id_of(&first[0]);
    assert!(cache.handles(gone).is_empty());
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn an_eviction_is_logged_at_debug_and_each_one_is_counted() {
    let log = Captured::default();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Mutex::new(log.clone()))
            .finish(),
    );
    let worker = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .on_thread_start(move || {
            // Blocking threads too: the insert, and so the eviction, runs there.
            std::mem::forget(tracing::dispatcher::set_default(&worker));
        })
        .build()
        .unwrap();
    let cache = Arc::new(AstCache::with_capacity(1));
    let sessions = Arc::new(Sessions::new());
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let (mut backend, serving, _) =
                connect_on(&sessions, &cache, init_payload(&[]), false).await;
            backend.send(register(2, "return 1;"));
            handle_of(&backend.next().await);
            backend.send(register(3, "return 2;"));
            handle_of(&backend.next().await);
            backend.close();
            finished(serving).await;
        });
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| {
            event["fields"]["message"] == "evicted a cached Script over the cache's capacity"
        })
        .collect();
    assert!(
        cache.evictions() >= 1,
        "the second registration displaced or was refused"
    );
    assert_eq!(events.len() as u64, cache.evictions(), "{text}");
    assert_eq!(events[0]["level"], "DEBUG", "{}", events[0]);
    assert!(events[0]["fields"]["handle"].is_string(), "{}", events[0]);
    // It names the evicted entry's own Session, and claims no request's span: maintenance runs
    // on whichever request triggers it.
    assert!(
        events[0]["fields"]["client_id"].is_string(),
        "{}",
        events[0]
    );
    assert!(events[0].get("span").is_none(), "{}", events[0]);
}

#[test]
fn a_cached_run_dispatched_before_an_unregister_of_its_handle_still_runs() {
    // Requests take effect in the order sent: the run fetched its program as it was dispatched,
    // so the unregister served right after it cannot take the program away.
    hosted(&wired_runtime(), &[], false, |mut backend| async move {
        backend.send(register(2, "return 7;"));
        let handle = handle_of(&backend.next().await).to_string();
        backend.send(cached_run(3, &handle, vec![]));
        backend.send(unregister_request(4, &handle));
        let mut replies = [backend.next().await, backend.next().await];
        replies.sort_by_key(|reply| reply.id.map(|id| id.0));
        assert_eq!(replies[0].id, Some(CorrelationId(3)));
        assert_eq!(value_of(&replies[0]), Value::from(7));
        assert_eq!(replies[1].id, Some(CorrelationId(4)));
        assert_eq!(replies[1].message_type, MessageType::Result);
        backend.send(cached_run(5, &handle, vec![]));
        assert_eq!(
            code_of(&backend.next().await),
            "protocol.unknown_script_handle"
        );
        backend.close();
    });
}

// --- Story 5.5: the reconnect credential issued at init ---

#[test]
fn a_valid_init_answers_client_id_then_reconnect_secret_in_that_order() {
    let sessions = Arc::new(Sessions::new());
    let verified = Arc::new(Mutex::new(None));
    let sent = {
        let (sessions, verified) = (Arc::clone(&sessions), Arc::clone(&verified));
        serve_probed(
            &Arc::clone(&sessions),
            vec![init(1, init_payload(&[("getUser", true)]))],
            // While the connection is still attached: the secret on the wire is the very one
            // the Session's stored hash verifies, and nothing else is.
            move |sent| {
                let (client_id, secret) = init_reply_of(&sent[0]);
                let parsed = ReconnectSecret::from_hex(&secret).expect("64 lowercase hex");
                let mut flipped = secret.clone().into_bytes();
                flipped[0] = if flipped[0] == b'0' { b'1' } else { b'0' };
                let flipped = ReconnectSecret::from_hex(std::str::from_utf8(&flipped).unwrap())
                    .expect("still 64 lowercase hex");
                *verified.lock().unwrap() = Some((
                    sessions.verify_reconnect(client_id, &parsed),
                    sessions.verify_reconnect(client_id, &flipped),
                ));
            },
        )
    };
    assert_eq!(sent.len(), 1);
    // `init_reply_of` asserts the two keys in order and the 64 lowercase hex secret.
    let (_, secret) = init_reply_of(&sent[0]);
    assert_eq!(secret.len(), 64);
    assert_eq!(
        verified.lock().unwrap().take().expect("the probe ran"),
        (true, false)
    );
}

#[test]
fn two_connections_initing_get_different_client_ids_and_different_secrets() {
    let sessions = Arc::new(Sessions::new());
    let (first, _) = serve_with(&sessions, vec![init(1, init_payload(&[]))], None);
    let (second, _) = serve_with(&sessions, vec![init(1, init_payload(&[]))], None);
    let (first_id, first_secret) = init_reply_of(&first[0]);
    let (second_id, second_secret) = init_reply_of(&second[0]);
    assert_ne!(first_id, second_id);
    assert_ne!(first_secret, second_secret);
}

#[test]
fn no_reply_after_init_on_that_connection_carries_the_reconnect_secret() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let (sent, _) = serve_cached(
        &sessions,
        &cache,
        vec![
            init(1, init_payload(&[("getUser", true)])),
            execution(2, "return 1;"),
            execution(3, "return 1 / 0;"),
            config_update(4, Value::Map(vec![])),
            register(5, "return 1;"),
            init(6, init_payload(&[])),
        ],
    );
    let (_, secret) = init_reply_of(&sent[0]);
    // Six replies in all: the init, two executions (one failing), the ConfigUpdate, the
    // CodeRegister and the refused second Init.
    assert_eq!(sent.len(), 6, "{sent:?}");
    // Executions answer in completion order, so the later replies are matched by content.
    let mut outcomes: Vec<String> = sent[1..].iter().map(outcome).collect();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [
            "Result",
            "Result",
            "Result",
            "arithmetic.division_by_zero",
            "protocol.already_initialized",
        ],
        "{sent:?}"
    );
    assert!(
        sent.iter().any(|reply| keys_of(reply) == ["handle"]),
        "the CodeRegister was answered: {sent:?}"
    );
    for reply in &sent[1..] {
        let shown = format!("{reply:?}");
        assert!(
            !shown.contains(&secret),
            "a later reply carries the reconnect secret: {shown}"
        );
    }
}

#[test]
fn an_invalid_init_answers_no_reconnect_secret_and_creates_no_session() {
    let sessions = Arc::new(Sessions::new());
    let (sent, _) = serve_with(
        &sessions,
        vec![init(
            1,
            Value::Map(vec![(string("registrations"), Value::Array(vec![]))]),
        )],
        None,
    );
    assert_eq!(code_of(&sent[0]), "protocol.invalid_payload");
    let Value::Map(fields) = &sent[0].payload else {
        panic!("an error payload is a map: {:?}", sent[0].payload);
    };
    assert!(
        fields
            .iter()
            .all(|(key, _)| key.as_str() != Some("reconnect_secret")),
        "{fields:?}"
    );
    assert!(sessions.is_empty());
}

// --- Story 5.6: reconnect without resending configuration ---

/// A `Reconnect` request as `id`, carrying exactly `client_id` and `reconnect_secret`.
fn reconnect(id: u64, client_id: &str, secret: &str) -> Received {
    reconnect_raw(
        id,
        Value::Map(vec![
            (string("client_id"), string(client_id)),
            (string("reconnect_secret"), string(secret)),
        ]),
    )
}

/// A `Reconnect` request as `id` with an extra `key` beside its credential.
fn reconnect_carrying(
    id: u64,
    client_id: &str,
    secret: &str,
    (key, value): (&str, Value),
) -> Received {
    reconnect_raw(
        id,
        Value::Map(vec![
            (string("client_id"), string(client_id)),
            (string("reconnect_secret"), string(secret)),
            (string(key), value),
        ]),
    )
}

/// A `Reconnect` request as `id` with exactly `payload`.
fn reconnect_raw(id: u64, payload: Value) -> Received {
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::Reconnect,
        payload,
    ))
}

/// `secret` with its last character changed: still 64 lowercase hex characters, never the secret.
fn altered_secret(secret: &str) -> String {
    let mut text = secret.to_owned();
    let last = text.pop().expect("a secret");
    text.push(if last == '0' { '1' } else { '0' });
    text
}

/// The Client ID no Session has: 32 lowercase hex characters, all zero.
const UNKNOWN_CLIENT_ID: &str = "00000000000000000000000000000000";

/// The refusal every denied reconnect carries, fixed (Story 5.6).
const RECONNECT_DENIED: &str = "no Session matches this Client ID and reconnect secret";

/// A Connection opened on `sessions` and `cache` with nothing sent: the Backend's side of it, and
/// its running `serve`.
fn open_bare(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
) -> (Backend, tokio::task::JoinHandle<()>) {
    open_bare_with_plugins(sessions, cache, &Arc::new(Plugins::new()))
}

/// [`open_bare`] over a given Plugin registry, so several connections can share one.
fn open_bare_with_plugins(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    plugins: &Arc<Plugins>,
) -> (Backend, tokio::task::JoinHandle<()>) {
    let (to_daemon, from_test) = tokio::sync::mpsc::unbounded_channel();
    let (to_test, from_daemon) = tokio::sync::mpsc::unbounded_channel();
    let port = Wired {
        from_test,
        to_test,
        refuse_calls: false,
    };
    let serving = tokio::spawn(hexput_connection::serve(
        port,
        Arc::clone(sessions),
        Arc::clone(cache),
        Arc::clone(plugins),
    ));
    (
        Backend {
            to_daemon: Some(to_daemon),
            from_daemon,
        },
        serving,
    )
}

/// [`open_bare`] and an `Init` with `payload`, its reply read: the Backend, its `serve`, and the
/// Client ID and reconnect secret the init issued.
async fn issued(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    payload: Value,
) -> (Backend, tokio::task::JoinHandle<()>, ClientId, String) {
    let (mut backend, serving) = open_bare(sessions, cache);
    backend.reply(CorrelationId(1), MessageType::Init, payload);
    let (client_id, secret) = init_reply_of(&backend.next().await);
    (backend, serving, client_id, secret)
}

/// Runs `body` on a fresh runtime, with the discarding subscriber every wired test runs under.
fn on_runtime(body: impl std::future::Future<Output = ()>) {
    let runtime = wired_runtime();
    let dispatch = discarding_dispatch();
    tracing::dispatcher::with_default(&dispatch, || runtime.block_on(body));
}

/// The next reply, which must be `Error` `code` to `id`; returns it.
async fn refused_with(backend: &mut Backend, id: u64, code: &str) -> Envelope<Value> {
    let reply = backend.next().await;
    assert_eq!(reply.id, Some(CorrelationId(id)), "{:?}", reply.payload);
    assert_eq!(code_of(&reply), code, "{:?}", reply.payload);
    reply
}

/// The next reply, which must be a `Result` to `id` whose payload is exactly `client_id`.
async fn resumed(backend: &mut Backend, id: u64, client_id: ClientId) -> Envelope<Value> {
    let reply = backend.next().await;
    assert_eq!(reply.id, Some(CorrelationId(id)), "{:?}", reply.payload);
    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "{:?}",
        reply.payload
    );
    assert_eq!(keys_of(&reply), ["client_id"], "{:?}", reply.payload);
    assert_eq!(
        field_of(&reply, "client_id"),
        Some(string(&client_id.to_string()))
    );
    reply
}

/// The registered function names of a Session, in order.
fn names_of(sessions: &Sessions, client_id: ClientId) -> Vec<String> {
    sessions
        .registrations(client_id)
        .expect("the Session is live")
        .iter()
        .map(|registered| registered.name().to_owned())
        .collect()
}

#[test]
fn a_reconnect_is_answered_with_its_client_id_alone_and_attaches_the_connection() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        let reply = resumed(&mut b, 2, client_id).await;
        assert!(!format!("{reply:?}").contains(&secret), "{reply:?}");
        assert_eq!(registry.attached(client_id), Some(2));

        // The reconnecting Connection leaves: the first still holds the Session.
        b.close();
        finished(serving_b).await;
        assert!(registry.contains(client_id));
        assert_eq!(registry.attached(client_id), Some(1));

        // The last one leaves: the Session is torn down.
        a.close();
        finished(serving_a).await;
        assert!(!registry.contains(client_id));
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnect_resumes_the_registrations_so_a_blanket_call_goes_to_the_reconnecting_connection() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[("getOrder", true)])).await;
        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        resumed(&mut b, 2, client_id).await;

        // Nothing was resent, yet the stored registration is in force on this Connection.
        b.send(execution(3, "return getOrder(1);"));
        let (call, name, arguments) = b.call().await;
        assert_eq!(name, "getOrder");
        assert_eq!(arguments, vec![Value::from(1u64)]);
        b.reply(
            call,
            MessageType::Result,
            Value::Map(vec![(string("value"), Value::from(7u64))]),
        );
        let reply = b.next().await;
        assert_eq!(reply.id, Some(CorrelationId(3)));
        assert_eq!(value_of(&reply), Value::from(7u64));

        // The Call went to the reconnecting Connection alone.
        assert!(
            a.from_daemon.try_recv().is_err(),
            "no Call reached the first Connection"
        );
        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnect_resumes_the_config_so_its_limits_apply_to_the_reconnecting_connection() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(budget_of("rpc_calls", 0), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        resumed(&mut b, 2, client_id).await;

        // A zero RPC budget in the stored Config refuses the call before anything is sent.
        b.send(execution(3, "return getOrder(1);"));
        let reply = refused_with(&mut b, 3, "budget.rpc_calls_exceeded").await;
        assert_eq!(reply.message_type, MessageType::Error);

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnect_resumes_the_cached_handles_of_the_session() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[])).await;
        a.send(register_with(2, "return n * 2;", Some(&["n"])));
        let handle = handle_of(&a.next().await).to_string();

        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        resumed(&mut b, 2, client_id).await;
        b.send(cached_run(3, &handle, vec![("n", Value::from(21u64))]));
        let reply = b.next().await;
        assert_eq!(reply.id, Some(CorrelationId(3)));
        assert_eq!(value_of(&reply), Value::from(42u64));

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_wrong_secret_and_an_unknown_id_are_denied_with_identical_payloads_and_the_connection_stays_open()
 {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[("getOrder", true)])).await;
        let wrong = altered_secret(&secret);
        let (mut b, serving_b) = open_bare(&registry, &cache);

        b.send(reconnect(2, &client_id.to_string(), &wrong));
        let first = refused_with(&mut b, 2, "protocol.reconnect_denied").await;
        b.send(reconnect(3, UNKNOWN_CLIENT_ID, &secret));
        let second = refused_with(&mut b, 3, "protocol.reconnect_denied").await;

        assert_eq!(message_of(&first), RECONNECT_DENIED);
        assert_eq!(message_of(&second), RECONNECT_DENIED);
        assert_eq!(
            first.payload, second.payload,
            "the denials differ in nothing but the echoed id"
        );
        for reply in [&first, &second] {
            let shown = format!("{reply:?}");
            assert!(!shown.contains(&wrong), "{shown}");
            assert!(!shown.contains(&secret), "{shown}");
            assert!(!shown.contains(&client_id.to_string()), "{shown}");
        }
        // The registry is unchanged, and the refused Connection is unattached and open: it
        // may retry with the right credential.
        assert_eq!(registry.attached(client_id), Some(1));
        assert_eq!(names_of(&registry, client_id), ["getOrder"]);
        b.send(reconnect(4, &client_id.to_string(), &secret));
        resumed(&mut b, 4, client_id).await;

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnect_after_the_session_is_torn_down_is_denied_exactly_like_an_unknown_id() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[])).await;
        a.close();
        finished(serving_a).await;
        assert!(!registry.contains(client_id));

        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        let torn = refused_with(&mut b, 2, "protocol.reconnect_denied").await;
        b.send(reconnect(3, UNKNOWN_CLIENT_ID, &secret));
        let unknown = refused_with(&mut b, 3, "protocol.reconnect_denied").await;
        assert_eq!(torn.payload, unknown.payload);

        // Still unattached after its denials, the connection may `Init` instead: a fresh Session.
        b.reply(CorrelationId(4), MessageType::Init, init_payload(&[]));
        let (fresh, _) = init_reply_of(&b.next().await);
        assert_ne!(fresh, client_id);
        assert_eq!(registry.attached(fresh), Some(1));
        assert!(!registry.contains(client_id));

        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnect_carrying_config_or_registrations_is_refused_before_any_lookup_and_changes_nothing() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(budget_of("rpc_calls", 2), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = open_bare(&registry, &cache);
        let id = client_id.to_string();

        // A valid credential with a Config, a registration list, and an unknown Client ID with
        // a Config: all refused as invalid payload, naming the key.
        b.send(reconnect_carrying(
            2,
            &id,
            &secret,
            ("config", budget_of("rpc_calls", 0)),
        ));
        let config = refused_with(&mut b, 2, "protocol.invalid_payload").await;
        assert!(message_of(&config).contains("config"), "{config:?}");
        b.send(reconnect_carrying(
            3,
            &id,
            &secret,
            ("registrations", Value::Array(vec![])),
        ));
        let registrations = refused_with(&mut b, 3, "protocol.invalid_payload").await;
        assert!(
            message_of(&registrations).contains("registrations"),
            "{registrations:?}"
        );
        b.send(reconnect_carrying(
            4,
            UNKNOWN_CLIENT_ID,
            &secret,
            ("config", budget_of("rpc_calls", 0)),
        ));
        let unknown = refused_with(&mut b, 4, "protocol.invalid_payload").await;
        assert_eq!(
            message_of(&config).replace("config", "KEY"),
            message_of(&unknown).replace("config", "KEY"),
            "the refusal reveals nothing about which Client IDs exist"
        );

        // Nothing was attached: the Connection is still unattached, so an execution is refused.
        assert_eq!(registry.attached(client_id), Some(1));
        b.send(execution(5, "return 1;"));
        refused_with(&mut b, 5, "protocol.init_not_completed").await;

        // The stored Config stays authoritative: its limit of 2 applies, not the refused 0.
        b.send(reconnect(6, &id, &secret));
        resumed(&mut b, 6, client_id).await;
        b.send(execution(7, &calls(3)));
        answer_calls(&mut b, 2).await;
        refused_with(&mut b, 7, "budget.rpc_calls_exceeded").await;

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_malformed_reconnect_is_invalid_payload_and_no_presented_value_is_echoed() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[])).await;
        let id = client_id.to_string();
        let upper_id = id.to_uppercase();
        let upper_secret = secret.to_uppercase();
        let (mut b, serving_b) = open_bare(&registry, &cache);

        let malformed = [
            (Value::Nil, String::new()),
            (Value::from(3u64), String::new()),
            (Value::Map(vec![]), String::new()),
            (
                Value::Map(vec![(string("client_id"), string(&id))]),
                String::new(),
            ),
            (
                Value::Map(vec![
                    (string("client_id"), string(&upper_id)),
                    (string("reconnect_secret"), string(&secret)),
                ]),
                upper_id.clone(),
            ),
            (
                Value::Map(vec![
                    (string("client_id"), string(&id)),
                    (string("reconnect_secret"), string(&upper_secret)),
                ]),
                upper_secret.clone(),
            ),
            (
                Value::Map(vec![
                    (string("client_id"), string(&id)),
                    (string("reconnect_secret"), string(&secret[..63])),
                ]),
                secret[..63].to_owned(),
            ),
            (
                Value::Map(vec![
                    (string("client_id"), string(&id)),
                    (string("reconnect_secret"), Value::from(1u64)),
                ]),
                String::new(),
            ),
            (
                Value::Map(vec![
                    (string("client_id"), string(&id)),
                    (string("client_id"), string(&id)),
                    (string("reconnect_secret"), string(&secret)),
                ]),
                String::new(),
            ),
        ];
        for (offset, (payload, presented)) in malformed.into_iter().enumerate() {
            let request = 10 + offset as u64;
            b.send(reconnect_raw(request, payload));
            let reply = refused_with(&mut b, request, "protocol.invalid_payload").await;
            let shown = format!("{reply:?}");
            assert!(!shown.contains(&secret) && !shown.contains(&id), "{shown}");
            if !presented.is_empty() {
                assert!(!shown.contains(&presented), "{shown}");
            }
        }

        // Still open and unattached after all of them.
        b.send(reconnect(30, &id, &secret));
        resumed(&mut b, 30, client_id).await;

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_second_init_or_reconnect_on_an_attached_connection_is_already_initialized_and_changes_nothing()
{
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[("getOrder", true)])).await;
        let id = client_id.to_string();
        let wrong = altered_secret(&secret);
        let (mut b, serving_b) = open_bare(&registry, &cache);

        // Init after Reconnect, and Reconnect after Reconnect, are both refused.
        b.send(reconnect(2, &id, &secret));
        resumed(&mut b, 2, client_id).await;
        b.send(init(3, init_payload(&[("other", true)])));
        refused_with(&mut b, 3, "protocol.already_initialized").await;
        b.send(reconnect(4, &id, &secret));
        refused_with(&mut b, 4, "protocol.already_initialized").await;

        // Reconnect after Init is refused too, and the credential is not even looked at: a wrong
        // one, or an unknown ID, gets the same refusal rather than a denial.
        a.send(reconnect(2, &id, &wrong));
        refused_with(&mut a, 2, "protocol.already_initialized").await;
        a.send(reconnect(3, UNKNOWN_CLIENT_ID, &secret));
        refused_with(&mut a, 3, "protocol.already_initialized").await;
        a.send(init(4, init_payload(&[("other", true)])));
        refused_with(&mut a, 4, "protocol.already_initialized").await;

        // Neither refusal touched the Session.
        assert_eq!(registry.attached(client_id), Some(2));
        assert_eq!(names_of(&registry, client_id), ["getOrder"]);

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnect_bypasses_the_init_gate_and_is_answered_as_a_denial_not_as_uninitialized() {
    let (sent, left) = serve(vec![reconnect(1, UNKNOWN_CLIENT_ID, &"0".repeat(64))], None);
    assert_eq!(left, 0);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].id, Some(CorrelationId(1)));
    assert_eq!(code_of(&sent[0]), "protocol.reconnect_denied");
}

// --- Story 5.7: several Connections attached to one Session ---

/// A Connection reconnected to the Session `client_id` with `secret`, its `Reconnect` answered
/// with the Client ID alone: the Backend, and its running `serve`.
async fn attach_again(
    registry: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    client_id: ClientId,
    secret: &str,
) -> (Backend, tokio::task::JoinHandle<()>) {
    let (mut backend, serving) = open_bare(registry, cache);
    backend.send(reconnect(1, &client_id.to_string(), secret));
    resumed(&mut backend, 1, client_id).await;
    (backend, serving)
}

/// The id of the sentinel request [`assert_silent`] sends. No Story 5.7 test uses it otherwise.
const SILENT_SENTINEL: u64 = 1_000;

/// Asserts nothing has been written to `backend` that the test has not read (a stray envelope on
/// another Connection is a failure of the per-Connection rule): a sentinel execution's `Result` must
/// be the very next envelope, so a write that arrives late still lands ahead of it and fails here,
/// as does a Connection that has died.
async fn assert_silent(backend: &mut Backend, who: &str) {
    backend.send(execution(SILENT_SENTINEL, "return 0;"));
    let next = backend.next().await;
    assert_eq!(
        (next.id, next.message_type),
        (Some(CorrelationId(SILENT_SENTINEL)), MessageType::Result),
        "{who} received a stray envelope before its sentinel's reply: {next:?}"
    );
    assert_eq!(value_of(&next), Value::from(0));
}

/// `{value: n}` as a `Result` payload, for answering a host call.
fn answer(n: i64) -> Value {
    Value::Map(vec![(string("value"), Value::from(n))])
}

#[test]
fn two_connections_on_one_session_are_both_attached_and_serve_at_once() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_configured(Value::Map(vec![]), &[])).await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;
        assert_eq!(registry.attached(client_id), Some(2));

        // Both executions are in flight before either Connection is read.
        a.send(execution(2, "return 1 + 1;"));
        b.send(execution(2, "return 3 + 3;"));
        let from_a = a.next().await;
        let from_b = b.next().await;
        assert_eq!(from_a.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&from_a), Value::from(2));
        assert_eq!(from_b.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&from_b), Value::from(6));
        assert_silent(&mut a, "A").await;
        assert_silent(&mut b, "B").await;

        a.close();
        finished(serving_a).await;
        assert_eq!(registry.attached(client_id), Some(1));
        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn the_same_request_id_on_two_connections_is_two_requests_each_answered_on_its_own() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("double", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        // Both executions use request id 2 and are pending at once.
        a.send(execution(2, "return double(3);"));
        b.send(execution(2, "return double(5);"));
        let (a_call, a_name, a_arguments) = a.call().await;
        let (b_call, b_name, b_arguments) = b.call().await;
        assert_eq!(a_name, "double");
        assert_eq!(b_name, "double");
        assert_eq!(
            a_call, b_call,
            "each Connection has its own call id space, so the ids collide"
        );
        assert_eq!(a_arguments, [Value::from(3)], "A's own argument");
        assert_eq!(b_arguments, [Value::from(5)], "B's own argument");

        // B answers first; A's execution is still waiting on its own Call.
        b.reply(b_call, MessageType::Result, answer(10));
        let reply_b = b.next().await;
        assert_eq!(reply_b.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply_b), Value::from(10));
        assert_silent(&mut a, "A").await;

        a.reply(a_call, MessageType::Result, answer(6));
        let reply_a = a.next().await;
        assert_eq!(reply_a.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply_a), Value::from(6));
        assert_silent(&mut a, "A").await;
        assert_silent(&mut b, "B").await;

        a.close();
        finished(serving_a).await;
        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_reply_naming_another_connections_pending_call_is_unexpected_and_leaves_that_call_pending() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("double", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        a.send(execution(2, "return double(3);"));
        let (a_call, _, _) = a.call().await;

        // B has no call of its own pending under that id: it answers A's call id anyway.
        b.reply(a_call, MessageType::Result, answer(99));
        let stray = b.next().await;
        assert_eq!(
            stray.id, None,
            "refused with a nil id, as any stray reply is"
        );
        assert_eq!(code_of(&stray), "protocol.unexpected_message");
        assert_silent(&mut a, "A").await;

        // Not fatal: B keeps serving its own execution.
        b.send(execution(3, "return 4;"));
        let own = b.next().await;
        assert_eq!(own.id, Some(CorrelationId(3)));
        assert_eq!(value_of(&own), Value::from(4));

        // A's call was never completed by B's stray reply: A's own answer completes it.
        a.reply(a_call, MessageType::Result, answer(6));
        let reply_a = a.next().await;
        assert_eq!(reply_a.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply_a), Value::from(6));
        assert_silent(&mut b, "B").await;

        a.close();
        finished(serving_a).await;
        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_per_call_question_reaches_only_the_connection_that_issued_the_execution() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("ask", false)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        a.send(execution(2, "return ask(4);"));
        let (question, name, arguments) = a.question().await;
        assert_eq!(name, "ask");
        assert_eq!(arguments, [Value::from(4)]);
        assert_silent(&mut b, "B").await;

        a.allow(question, Value::Boolean(true));
        let (call, name, _) = a.call().await;
        assert_eq!(name, "ask");
        assert_silent(&mut b, "B").await;
        a.reply(call, MessageType::Result, answer(9));
        assert_eq!(value_of(&a.next().await), Value::from(9));
        assert_silent(&mut b, "B").await;

        a.close();
        finished(serving_a).await;
        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_config_update_on_one_connection_is_in_force_on_the_others_next_request_with_no_message_on_it()
{
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        b.send(config_update(2, budget_of("rpc_calls", 0)));
        assert_updated(&b.next().await, 2);
        assert_silent(&mut a, "A").await;

        // A's next host call is refused by the budget B set: nothing is sent to A's Backend.
        a.send(execution(2, "return getOrder(1);"));
        refused_with(&mut a, 2, "budget.rpc_calls_exceeded").await;
        assert_silent(&mut a, "A").await;
        assert_silent(&mut b, "B").await;

        a.close();
        finished(serving_a).await;
        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_script_registered_on_one_connection_runs_on_another_until_unregistered_there() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_configured(Value::Map(vec![]), &[])).await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        a.send(register(2, "return 7;"));
        let handle = handle_of(&a.next().await).to_string();

        b.send(cached_run(2, &handle, vec![]));
        assert_eq!(value_of(&b.next().await), Value::from(7));

        b.send(unregister_request(3, &handle));
        let removed = b.next().await;
        assert_eq!(removed.id, Some(CorrelationId(3)));
        assert_eq!(removed.message_type, MessageType::Result);
        assert_eq!(removed.payload, Value::Map(vec![]));
        assert_silent(&mut a, "A").await;

        a.send(cached_run(3, &handle, vec![]));
        refused_with(&mut a, 3, "protocol.unknown_script_handle").await;
        assert_silent(&mut b, "B").await;

        a.close();
        finished(serving_a).await;
        b.close();
        finished(serving_b).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_connection_closing_mid_flight_leaves_the_others_execution_to_complete() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        a.send(execution(2, "return getOrder(1) + 1;"));
        let (call, _, _) = a.call().await;

        // B leaves while A's execution waits on its Call.
        b.close();
        finished(serving_b).await;
        assert!(registry.contains(client_id), "A keeps the Session live");
        assert_eq!(registry.attached(client_id), Some(1));

        a.reply(call, MessageType::Result, answer(1));
        let reply = a.next().await;
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply), Value::from(2));

        // A's own close is the last detach, and it tears the Session down.
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_connection_sending_a_fatal_frame_mid_flight_leaves_the_others_execution_to_complete() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        a.send(execution(2, "return getOrder(1) + 1;"));
        let (call, _, _) = a.call().await;

        // B's fatal frame is answered on B alone, and B detaches.
        b.send(malformed(None, ProtocolCode::FrameTooLarge));
        let fatal = b.next().await;
        assert_eq!(code_of(&fatal), "protocol.frame_too_large");
        finished(serving_b).await;
        assert_eq!(registry.attached(client_id), Some(1));
        assert_silent(&mut a, "A").await;

        a.reply(call, MessageType::Result, answer(1));
        let reply = a.next().await;
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply), Value::from(2));

        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn three_connections_on_one_session_each_get_only_their_own_replies_and_closing_two_keeps_it() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("double", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;
        let (mut c, serving_c) = attach_again(&registry, &cache, client_id, &secret).await;
        assert_eq!(registry.attached(client_id), Some(3));

        // The same request id 2 on all three, each calling with its own argument.
        a.send(execution(2, "return double(1);"));
        b.send(execution(2, "return double(2);"));
        c.send(execution(2, "return double(3);"));
        let (a_call, _, a_args) = a.call().await;
        let (b_call, _, b_args) = b.call().await;
        let (c_call, _, c_args) = c.call().await;
        assert_eq!(
            [a_call, b_call],
            [c_call, c_call],
            "per-Connection call ids collide"
        );
        assert_eq!(
            [a_args, b_args, c_args],
            [[Value::from(1)], [Value::from(2)], [Value::from(3)]]
        );
        c.reply(c_call, MessageType::Result, answer(6));
        b.reply(b_call, MessageType::Result, answer(4));
        a.reply(a_call, MessageType::Result, answer(2));
        assert_eq!(value_of(&a.next().await), Value::from(2));
        assert_eq!(value_of(&b.next().await), Value::from(4));
        assert_eq!(value_of(&c.next().await), Value::from(6));
        assert_silent(&mut a, "A").await;
        assert_silent(&mut b, "B").await;
        assert_silent(&mut c, "C").await;

        // Closing all but one keeps the Session serving on the one left.
        b.close();
        finished(serving_b).await;
        c.close();
        finished(serving_c).await;
        assert_eq!(registry.attached(client_id), Some(1));
        a.send(execution(3, "return 4;"));
        assert_eq!(value_of(&a.next().await), Value::from(4));

        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_connection_closing_with_its_own_call_pending_fails_only_that_call() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        // Both executions wait on a Call under the same call id, one on each Connection.
        a.send(execution(2, "return getOrder(1) + 1;"));
        b.send(execution(2, "return getOrder(2) + 1;"));
        let (a_call, _, _) = a.call().await;
        let (b_call, _, _) = b.call().await;
        assert_eq!(a_call, b_call, "per-Connection call ids collide");

        // B leaves: its own pending call fails with `host.no_reply`, and only its own.
        b.close();
        refused_with(&mut b, 2, "host.no_reply").await;
        finished(serving_b).await;
        assert_eq!(registry.attached(client_id), Some(1));

        a.reply(a_call, MessageType::Result, answer(1));
        let reply = a.next().await;
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply), Value::from(2));
        assert_silent(&mut a, "A").await;

        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_connection_whose_stream_is_lost_mid_flight_abandons_only_its_own_execution() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = Arc::clone(&sessions);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) = issued(
            &registry,
            &cache,
            init_configured(Value::Map(vec![]), &[("getOrder", true)]),
        )
        .await;
        let (mut b, serving_b) = attach_again(&registry, &cache, client_id, &secret).await;

        a.send(execution(2, "return getOrder(1) + 1;"));
        b.send(execution(2, "return getOrder(2) + 1;"));
        let (a_call, _, _) = a.call().await;
        let (b_call, _, _) = b.call().await;
        assert_eq!(a_call, b_call, "per-Connection call ids collide");

        // B's stream is lost: its execution is abandoned, nothing more is written to it.
        b.send(Received::Closed(Some(io::Error::from(
            io::ErrorKind::ConnectionReset,
        ))));
        finished(serving_b).await;
        assert!(
            b.from_daemon.try_recv().is_err(),
            "nothing is written to a lost stream"
        );
        assert_eq!(registry.attached(client_id), Some(1));

        a.reply(a_call, MessageType::Result, answer(1));
        let reply = a.next().await;
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(value_of(&reply), Value::from(2));
        assert_silent(&mut a, "A").await;

        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

// --- Story 5.8: a Session outlives its last Connection by the TTL ---

/// The TTL a wired test configures when nothing must happen *inside* it: short, so a test can wait
/// out several of them.
const SHORT_TTL: Duration = Duration::from_millis(200);

/// The TTL a wired test configures when it must act or assert inside the countdown (reconnect,
/// "still held"): long enough that a loaded runner's scheduling delay never outlasts it.
const RESUMABLE_TTL: Duration = Duration::from_secs(1);

/// A registry with `ttl` whose teardown releases the Session's entries in `cache`, wired the way
/// the Daemon wires it.
fn torn_down_after(cache: &Arc<AstCache>, ttl: Duration) -> Arc<Sessions> {
    let cache = Arc::clone(cache);
    Arc::new(
        Sessions::new()
            .with_ttl(ttl)
            .with_teardown(move |client_id| cache.close(client_id)),
    )
}

/// Waits until `client_id` is no longer in `registry`, failing the test after `limit`.
async fn until_gone(registry: &Sessions, client_id: ClientId, limit: Duration) {
    let started = Instant::now();
    while registry.contains(client_id) {
        assert!(started.elapsed() < limit, "the Session never expired");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn a_reconnect_within_the_ttl_resumes_the_session_and_its_cached_handles_and_the_countdown_never_fires()
 {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = torn_down_after(&cache, RESUMABLE_TTL);
    let outside = Arc::clone(&cache);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[("getUser", true)])).await;
        a.send(register_with(2, "return n * 2;", Some(&["n"])));
        let handle = handle_of(&a.next().await);
        a.close();
        finished(serving_a).await;
        assert!(registry.contains(client_id), "the countdown has started");

        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        resumed(&mut b, 2, client_id).await;
        // Well past the TTL the first countdown would have expired at: the reconnect stopped it.
        tokio::time::sleep(RESUMABLE_TTL * 3 / 2).await;
        assert!(
            registry.contains(client_id),
            "the earlier countdown never tears it down"
        );
        assert_eq!(cache.handles(client_id), vec![handle]);
        b.send(cached_run(
            3,
            &handle.to_string(),
            vec![("n", Value::from(21u64))],
        ));
        let reply = b.next().await;
        assert_eq!(reply.id, Some(CorrelationId(3)));
        assert_eq!(value_of(&reply), Value::from(42u64));
        // The registrations survived too: a host call to the blanket-granted function goes out.
        b.send(execution(4, "return getUser(5) + 1;"));
        let (call, name, arguments) = b.call().await;
        assert_eq!(name, "getUser");
        assert_eq!(arguments, vec![Value::from(5u64)]);
        b.reply(call, MessageType::Result, answer(10));
        let reply = b.next().await;
        assert_eq!(reply.id, Some(CorrelationId(4)));
        assert_eq!(value_of(&reply), Value::from(11u64));

        b.close();
        finished(serving_b).await;
        until_gone(&registry, client_id, RESUMABLE_TTL * 10).await;
        assert!(cache.handles(client_id).is_empty());
    });
    assert_eq!(outside.entry_count(), 0);
}

#[test]
fn two_attached_connections_keep_their_session_live_past_several_ttls() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = torn_down_after(&cache, SHORT_TTL);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[("getUser", true)])).await;
        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        resumed(&mut b, 2, client_id).await;

        // A leaves while B is attached: no countdown starts.
        a.close();
        finished(serving_a).await;
        tokio::time::sleep(SHORT_TTL * 3).await;
        assert!(registry.contains(client_id));
        assert_eq!(registry.attached(client_id), Some(1));
        assert_eq!(registry.expiring(client_id), Some(false));

        b.send(execution(3, "return 1 + 1;"));
        assert_eq!(value_of(&b.next().await), Value::from(2u64));
        b.close();
        finished(serving_b).await;
        until_gone(&registry, client_id, SHORT_TTL * 50).await;
    });
}

#[test]
fn the_last_close_with_no_reconnect_expires_the_session_and_its_handles_then_a_reconnect_is_denied()
{
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = torn_down_after(&cache, RESUMABLE_TTL);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[])).await;
        a.send(register(2, "return 1;"));
        let handle = handle_of(&a.next().await);
        a.close();
        finished(serving_a).await;
        // Still held during the countdown.
        assert!(registry.contains(client_id));
        assert_eq!(cache.handles(client_id), vec![handle]);

        until_gone(&registry, client_id, RESUMABLE_TTL * 10).await;
        assert!(
            cache.handles(client_id).is_empty(),
            "its cache handles are released"
        );
        assert_eq!(cache.entry_count(), 0);

        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        let torn = refused_with(&mut b, 2, "protocol.reconnect_denied").await;
        b.send(reconnect(3, UNKNOWN_CLIENT_ID, &secret));
        let unknown = refused_with(&mut b, 3, "protocol.reconnect_denied").await;
        assert_eq!(
            torn.payload, unknown.payload,
            "identical to an unknown Client ID"
        );

        // Not fatal: the same connection may now `Init` a fresh Session.
        b.reply(CorrelationId(4), MessageType::Init, init_payload(&[]));
        let (fresh, _) = init_reply_of(&b.next().await);
        assert_ne!(fresh, client_id);
        assert_eq!(registry.attached(fresh), Some(1));

        b.close();
        finished(serving_b).await;
    });
}

#[test]
fn a_wrong_secret_during_the_countdown_is_denied_and_the_session_still_expires() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = torn_down_after(&cache, RESUMABLE_TTL);
    on_runtime(async move {
        let (mut a, serving_a, client_id, secret) =
            issued(&registry, &cache, init_payload(&[])).await;
        a.close();
        finished(serving_a).await;

        let (mut b, serving_b) = open_bare(&registry, &cache);
        b.send(reconnect(
            2,
            &client_id.to_string(),
            &altered_secret(&secret),
        ));
        refused_with(&mut b, 2, "protocol.reconnect_denied").await;
        assert!(registry.contains(client_id), "a denial changes nothing");
        b.close();
        finished(serving_b).await;

        until_gone(&registry, client_id, RESUMABLE_TTL * 10).await;
    });
}

#[test]
fn the_countdown_and_the_expiry_are_logged_at_debug_with_the_ttl_and_the_client_id() {
    let log = Captured::default();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Mutex::new(log.clone()))
            .finish(),
    );
    let worker = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .on_thread_start(move || {
            std::mem::forget(tracing::dispatcher::set_default(&worker));
        })
        .build()
        .unwrap();
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let registry = torn_down_after(&cache, SHORT_TTL);
    let issued_id = tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let (mut a, serving_a, client_id, _) =
                issued(&registry, &cache, init_payload(&[])).await;
            a.close();
            finished(serving_a).await;
            until_gone(&registry, client_id, SHORT_TTL * 50).await;
            client_id
        })
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect();
    let named = |message: &str| -> Vec<&serde_json::Value> {
        events
            .iter()
            .filter(|event| event["fields"]["message"] == message)
            .collect()
    };
    let client_id = issued_id.to_string();

    let started = named(
        "last connection detached; the Session expires after its TTL unless a Connection reconnects",
    );
    assert_eq!(started.len(), 1, "{text}");
    assert_eq!(started[0]["level"], "DEBUG", "{}", started[0]);
    assert_eq!(
        started[0]["fields"]["ttl_secs"],
        SHORT_TTL.as_secs_f64(),
        "{}",
        started[0]
    );
    assert_eq!(started[0]["span"]["client_id"], client_id, "{}", started[0]);

    let expired = named("Session expired after its TTL; torn down");
    assert_eq!(expired.len(), 1, "{text}");
    assert_eq!(expired[0]["level"], "DEBUG", "{}", expired[0]);
    assert_eq!(expired[0]["span"]["client_id"], client_id, "{}", expired[0]);
}

// --- Story 6.2: a Plugin is its own kind of registration ---

/// [`serve`]'s run against `plugins` as well as `sessions` and `cache`, which the test keeps.
fn serve_plugins(
    sessions: &Arc<Sessions>,
    cache: &Arc<AstCache>,
    plugins: &Arc<Plugins>,
    script: Vec<Received>,
) -> (Vec<Envelope<Value>>, usize) {
    let (written, left, _) = run_timed_with_plugins(sessions, cache, plugins, script, None, None);
    (written, left)
}

/// A `PluginRegister` request for `source`.
fn plugin_register(id: u64, source: &str) -> Received {
    plugin_register_payload(id, Value::Map(vec![(string("source"), string(source))]))
}

/// A `PluginRegister` request carrying `payload`, whatever it is.
fn plugin_register_payload(id: u64, payload: Value) -> Received {
    Received::Message(Envelope::new(
        CorrelationId(id),
        MessageType::PluginRegister,
        payload,
    ))
}

/// A valid Plugin source named `name`, with one `@Event` handler.
fn plugin_source(name: &str) -> String {
    format!("plugin {{ name = \"{name}\" }}\n@Event(Ping) fn h(p) {{ return 1; }}")
}

/// The `name` of a successful `PluginRegister` reply: exactly `name`.
fn name_of(response: &Envelope<Value>) -> String {
    assert_eq!(
        response.message_type,
        MessageType::Result,
        "{:?}",
        response.payload
    );
    let Value::Map(fields) = &response.payload else {
        panic!("a Result payload is a map: {:?}", response.payload);
    };
    assert_eq!(fields.len(), 1, "exactly `name`: {fields:?}");
    assert_eq!(fields[0].0.as_str(), Some("name"));
    fields[0].1.as_str().expect("a string name").to_owned()
}

/// Whether an `Error` reply carries a `span` map (its diagnostic's location).
fn has_span(response: &Envelope<Value>) -> bool {
    let Value::Map(fields) = &response.payload else {
        return false;
    };
    fields
        .iter()
        .any(|(key, value)| key.as_str() == Some("span") && matches!(value, Value::Map(_)))
}

/// The id `sessions` lists `name` under for `client_id`, if it lists it.
fn plugin_id_of(sessions: &Sessions, client_id: ClientId, name: &str) -> Option<PluginId> {
    sessions
        .plugins(client_id)?
        .into_iter()
        .find(|(listed, _)| &**listed == name)
        .map(|(_, id)| id)
}

/// The payloads a `PluginRegister` must refuse, each with the key its message names (none when
/// the payload is not a map, which names no key).
fn bad_plugin_payloads() -> Vec<(Value, Option<&'static str>)> {
    let source = plugin_source("x");
    vec![
        // nil reads as missing, so the message names `source`.
        (Value::Nil, Some("source")),
        (Value::Map(vec![]), Some("source")),
        (
            Value::Map(vec![(string("source"), Value::from(1))]),
            Some("source"),
        ),
        (
            Value::Map(vec![
                (string("source"), string(&source)),
                (string("events"), Value::Array(vec![])),
            ]),
            Some("events"),
        ),
        (
            Value::Map(vec![
                (string("source"), string(&source)),
                (string("source"), string(&source)),
            ]),
            Some("source"),
        ),
        (Value::from(1), None),
        (Value::Map(vec![(Value::from(1), string(&source))]), None),
    ]
}

#[test]
fn a_plugin_registration_is_answered_with_its_name_and_held_by_its_session() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut backend, serving, client_id) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        backend.send(plugin_register(2, &plugin_source("x")));
        let reply = backend.next().await;
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(name_of(&reply), "x");
        let id = plugin_id_of(&sessions, client_id, "x").expect("the Session holds x");
        let plugin = plugins.get(client_id, id).expect("the Plugin is held");
        assert_eq!(plugin.name(), "x");
        assert_eq!(plugin.program().handlers.len(), 1);
        backend.close();
        finished(serving).await;
    });
}

#[test]
fn a_bad_plugin_payload_is_refused_naming_its_key_and_nothing_is_held() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut backend, serving, client_id) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        for (n, (payload, key)) in bad_plugin_payloads().into_iter().enumerate() {
            let id = 2 + n as u64;
            backend.send(plugin_register_payload(id, payload.clone()));
            let reply = backend.next().await;
            assert_eq!(reply.id, Some(CorrelationId(id)), "{payload:?}");
            assert_eq!(code_of(&reply), "protocol.invalid_payload", "{payload:?}");
            if let Some(key) = key {
                assert!(
                    message_of(&reply).contains(key),
                    "{payload:?}: {}",
                    message_of(&reply)
                );
            }
        }
        assert_eq!(sessions.plugins(client_id), Some(vec![]));
        backend.close();
        finished(serving).await;
    });
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_plugin_that_does_not_parse_is_its_diagnostic_with_a_span_and_registers_nothing() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut backend, serving, client_id) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        backend.send(plugin_register(2, "let x = 1;"));
        let reply = backend.next().await;
        assert_eq!(reply.id, Some(CorrelationId(2)));
        assert_eq!(code_of(&reply), "syntax.plugin_block_required");
        assert!(has_span(&reply), "{:?}", reply.payload);
        assert_eq!(sessions.plugins(client_id), Some(vec![]));
        backend.close();
        finished(serving).await;
    });
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_duplicate_plugin_name_in_one_session_is_refused_naming_it_and_the_first_is_unchanged() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut backend, serving, client_id) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        backend.send(plugin_register(2, &plugin_source("x")));
        assert_eq!(name_of(&backend.next().await), "x");
        let first = plugin_id_of(&sessions, client_id, "x").expect("x is listed");
        backend.send(plugin_register(3, &plugin_source("x")));
        let refusal = backend.next().await;
        assert_eq!(refusal.id, Some(CorrelationId(3)));
        assert_eq!(code_of(&refusal), "protocol.duplicate_plugin_name");
        assert!(
            message_of(&refusal).contains("`x`"),
            "{}",
            message_of(&refusal)
        );
        assert_eq!(
            sessions.plugins(client_id),
            Some(vec![(Arc::<str>::from("x"), first)]),
            "the first registration is untouched"
        );
        assert_eq!(plugins.ids(client_id), vec![first]);
        backend.close();
        finished(serving).await;
    });
}

#[test]
fn the_same_name_on_another_connection_is_a_different_session_and_a_different_plugin() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut first, serving_first, first_client) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        let (mut second, serving_second, second_client) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        first.send(plugin_register(2, &plugin_source("x")));
        assert_eq!(name_of(&first.next().await), "x");
        second.send(plugin_register(2, &plugin_source("x")));
        assert_eq!(name_of(&second.next().await), "x");
        let on_first = plugin_id_of(&sessions, first_client, "x").expect("first holds x");
        let on_second = plugin_id_of(&sessions, second_client, "x").expect("second holds x");
        assert_ne!(on_first, on_second);
        first.close();
        second.close();
        finished(serving_first).await;
        finished(serving_second).await;
    });
}

#[test]
fn several_plugins_on_one_session_are_each_listed_and_retrievable_as_distinct_plugins() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut backend, serving, client_id) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        for (n, name) in ["a", "b", "c"].into_iter().enumerate() {
            backend.send(plugin_register(2 + n as u64, &plugin_source(name)));
            assert_eq!(name_of(&backend.next().await), name);
        }
        let listed = sessions.plugins(client_id).expect("the Session is live");
        let names: Vec<&str> = listed.iter().map(|(name, _)| &**name).collect();
        assert_eq!(names, ["a", "b", "c"]);
        let ids: Vec<PluginId> = listed.iter().map(|(_, id)| *id).collect();
        assert!(ids[0] < ids[1] && ids[1] < ids[2], "listed by id");
        let held: Vec<_> = ids
            .iter()
            .map(|id| plugins.get(client_id, *id).expect("each is held"))
            .collect();
        assert_eq!(held[0].name(), "a");
        assert_ne!(held[0].program(), held[1].program());
        assert_ne!(held[1].program(), held[2].program());
        backend.close();
        finished(serving).await;
    });
}

#[test]
fn a_plugin_registration_before_init_is_init_not_completed_and_holds_nothing() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    let (sent, left) = serve_plugins(
        &sessions,
        &cache,
        &plugins,
        vec![plugin_register(1, &plugin_source("x"))],
    );
    assert_eq!(left, 0);
    assert_eq!(code_of(&sent[0]), "protocol.init_not_completed");
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_plugin_held_by_a_session_is_released_when_its_session_ends_through_the_teardown_hook() {
    let plugins = Arc::new(Plugins::new());
    let hook = Arc::clone(&plugins);
    let sessions = Arc::new(Sessions::new().with_teardown(move |client_id| hook.close(client_id)));
    let cache = Arc::new(AstCache::with_capacity(10_000));
    on_runtime(async {
        let (mut backend, serving, client_id) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        backend.send(plugin_register(2, &plugin_source("x")));
        assert_eq!(name_of(&backend.next().await), "x");
        assert_eq!(plugins.ids(client_id).len(), 1);
        backend.close();
        finished(serving).await;
        assert!(plugins.ids(client_id).is_empty());
        assert!(sessions.plugins(client_id).is_none());
    });
    assert_eq!(plugins.len(), 0);
}

#[test]
fn plugin_code_cached_and_direct_requests_on_one_connection_are_each_answered() {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    on_runtime(async {
        let (mut backend, serving, _) =
            connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
        backend.send(plugin_register(2, &plugin_source("mixed")));
        assert_eq!(name_of(&backend.next().await), "mixed");
        backend.send(register(3, "return 1 + 1;"));
        let handle = handle_of(&backend.next().await).to_string();
        backend.send(cached_run(4, &handle, vec![]));
        assert_eq!(value_of(&backend.next().await), Value::from(2));
        backend.send(execution(5, "return 3;"));
        assert_eq!(value_of(&backend.next().await), Value::from(3));
        backend.close();
        finished(serving).await;
    });
}

#[test]
fn a_reconnected_connection_registers_plugins_in_its_session_and_names_stay_unique_across_its_connections()
 {
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    let registry = Arc::clone(&sessions);
    let held = Arc::clone(&plugins);
    on_runtime(async move {
        let (mut a, serving_a) = open_bare_with_plugins(&registry, &cache, &held);
        a.reply(CorrelationId(1), MessageType::Init, init_payload(&[]));
        let (client_id, secret) = init_reply_of(&a.next().await);
        a.send(plugin_register(2, &plugin_source("x")));
        assert_eq!(name_of(&a.next().await), "x");

        // A Connection attached by `Reconnect` registers into the same Session: a name the
        // Session holds through another Connection is taken, a new one is held beside it.
        let (mut b, serving_b) = open_bare_with_plugins(&registry, &cache, &held);
        b.send(reconnect(2, &client_id.to_string(), &secret));
        resumed(&mut b, 2, client_id).await;
        b.send(plugin_register(3, &plugin_source("x")));
        refused_with(&mut b, 3, "protocol.duplicate_plugin_name").await;
        b.send(plugin_register(4, &plugin_source("y")));
        assert_eq!(name_of(&b.next().await), "y");

        let listed: Vec<String> = registry
            .plugins(client_id)
            .expect("the Session is live")
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        assert_eq!(listed, ["x", "y"]);
        assert_eq!(held.ids(client_id).len(), 2);

        b.close();
        finished(serving_b).await;
        a.close();
        finished(serving_a).await;
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_large_plugin_does_not_hold_back_a_fast_execution_on_its_connection() {
    let mut source = String::from("plugin { name = \"big\" }\n");
    source.extend((0..LARGE_REGISTRATION_STATEMENTS).map(|n| format!("let v{n} = {n};\n")));
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    let (sent, left) = serve_plugins(
        &sessions,
        &cache,
        &plugins,
        vec![
            init(1, init_payload(&[])),
            plugin_register(2, &source),
            execution(3, "return 3;"),
        ],
    );
    assert_eq!(left, 0);
    assert_eq!(ids(&sent), [1, 3, 2], "the Plugin completes last");
    assert_eq!(
        sent[1].payload,
        Value::Map(vec![(string("value"), Value::from(3))])
    );
    assert_eq!(name_of(&sent[2]), "big");
    assert_eq!(plugins.len(), 1);
}

#[test]
fn a_plugin_registration_is_logged_at_debug_under_its_request_and_a_failure_names_its_code() {
    let log = Captured::default();
    let writer = log.clone();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Mutex::new(writer))
            .finish(),
    );
    let sessions = Arc::new(Sessions::new());
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let plugins = Arc::new(Plugins::new());
    let runtime = wired_runtime();
    let mut registered = None;
    tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(async {
            let (mut backend, serving, client_id) =
                connect_with_plugins(&sessions, &cache, &plugins, init_payload(&[]), false).await;
            backend.send(plugin_register(7, &plugin_source("x")));
            assert_eq!(name_of(&backend.next().await), "x");
            registered = plugin_id_of(&sessions, client_id, "x");
            backend.send(plugin_register(8, "let x = 1;"));
            assert_eq!(
                code_of(&backend.next().await),
                "syntax.plugin_block_required"
            );
            backend.close();
            finished(serving).await;
        });
    });
    let id = registered.expect("the registration was answered");
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect();
    let success: Vec<_> = events
        .iter()
        .filter(|event| event["fields"]["message"] == "plugin registered")
        .collect();
    assert_eq!(success.len(), 1, "{text}");
    let event = success[0];
    assert_eq!(event["level"], "DEBUG", "{event}");
    assert_eq!(event["fields"]["plugin"], "x", "{event}");
    assert_eq!(event["fields"]["plugin_id"], id.to_string(), "{event}");
    assert_eq!(event["span"]["name"], "request", "{event}");
    assert_eq!(event["span"]["id"], "7", "{event}");
    let failure: Vec<_> = events
        .iter()
        .filter(|event| event["fields"]["message"] == "a plugin registration failed")
        .collect();
    assert_eq!(failure.len(), 1, "{text}");
    assert_eq!(
        failure[0]["fields"]["code"], "syntax.plugin_block_required",
        "{}",
        failure[0]
    );
    assert_eq!(failure[0]["span"]["id"], "8", "{}", failure[0]);
}
