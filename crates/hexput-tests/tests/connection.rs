//! Story 2.3: the core's side of a connection, driven through an in-memory `Port`.
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

/// Everything the core wrote.
#[derive(Clone, Default)]
struct Sent(Arc<Mutex<Vec<Envelope<Value>>>>);

/// A Port that yields a fixed script of `Received` items, then `Closed`.
struct Scripted {
    script: Arc<Mutex<VecDeque<Received>>>,
    sent: Sent,
    /// A write that fails: the attempt number (from 0) and how it fails.
    fail: Option<(usize, io::ErrorKind)>,
}

struct ScriptedIn(Arc<Mutex<VecDeque<Received>>>);
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
            ScriptedIn(self.script),
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
        next.unwrap_or(Received::Closed(None))
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
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let sent = Sent::default();
    let port = Scripted {
        script: Arc::clone(&script),
        sent: sent.clone(),
        fail,
    };
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(hexput_connection::serve(port));
    let written = sent.0.lock().unwrap().clone();
    let left = script.lock().unwrap().len();
    (written, left)
}

fn message(id: u64, message_type: MessageType) -> Received {
    Received::Message(Envelope::new(CorrelationId(id), message_type, Value::Nil))
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
    let (sent, _) = serve(
        vec![
            message(1, MessageType::ExecutionStart),
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
                "protocol.not_implemented".to_owned()
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
fn a_reply_too_large_to_frame_leaves_the_connection_open() {
    let (sent, left) = serve(
        vec![
            message(1, MessageType::ExecutionStart),
            message(2, MessageType::ExecutionStart),
        ],
        Some((0, io::ErrorKind::InvalidInput)),
    );
    let ids: Vec<_> = sent.iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        [Some(CorrelationId(2))],
        "the second request is still answered"
    );
    assert_eq!(left, 0);
}
