//! Transient per-connection actor, attached to a Session. Holds no state that outlives the
//! Session it is attached to; a Session may have zero or more Connections attached at once.
//!
//! [`serve`] is the core's side of one connection. It is generic over [`Port`] and names no
//! transport: whatever adapter accepted the connection, the core reads envelopes, answers each
//! on the same connection, and stops when the peer leaves (AD-1).
//!
//! # What a connection is answered today
//!
//! Nothing can complete init yet (Story 2.4), so every well-formed request is refused with a
//! stable code, echoing its correlation id:
//!
//! * `ExecutionStart` — `protocol.init_not_completed`: nothing runs before init (FR-1).
//! * `Init` — `protocol.not_implemented`, until Story 2.4 serves it.
//! * `Result` or `Error` from the Backend — `protocol.unexpected_message`: the Daemon asked
//!   nothing it could answer.
//!
//! A frame the codec rejects gets its `protocol.*` error response; the connection keeps
//! reading unless the error is fatal (an oversized frame), after which it closes.
//!
//! Binds: AD-1, AD-2.

use std::io;

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, Inbound, MessageType, Outbound, Port, ProtocolCode,
    ProtocolError, Received, Value, error_response,
};

/// Serve one connection until the peer leaves, a write fails, or a fatal protocol error closes
/// it. Never panics on anything the peer sends; failures are logged at `debug`, since a peer
/// leaving is routine.
pub async fn serve<P: Port>(port: P) {
    let (mut inbound, mut outbound) = port.split();
    loop {
        let (reply, close) = match inbound.recv().await {
            Received::Message(request) => (answer(&request), false),
            Received::Malformed(failure) => {
                tracing::debug!(error = %failure, "rejected a malformed frame");
                (failure.to_response(), failure.error.is_fatal())
            }
            Received::Closed(None) => {
                tracing::debug!("connection closed by the peer");
                return;
            }
            Received::Closed(Some(error)) => {
                tracing::debug!(%error, "connection lost");
                return;
            }
        };
        match outbound.send(reply).await {
            Ok(()) => {}
            // Nothing was written: the reply could not be framed, and the connection is intact.
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                tracing::warn!(%error, "a reply could not be sent; the connection stays open");
            }
            Err(error) => {
                tracing::debug!(%error, "cannot write to the connection; closing it");
                return;
            }
        }
        if close {
            tracing::debug!("closing the connection after a fatal protocol error");
            return;
        }
    }
}

/// The response to one well-formed message from a connection that has not completed init.
fn answer(request: &Envelope<Value>) -> Envelope<Value> {
    let (code, message) = match request.message_type {
        MessageType::ExecutionStart => (
            ProtocolCode::InitNotCompleted,
            "`ExecutionStart` needs a completed init; send `Init` first".to_owned(),
        ),
        MessageType::Init => (
            ProtocolCode::NotImplemented,
            "`Init` is not served yet".to_owned(),
        ),
        other => (
            ProtocolCode::UnexpectedMessage,
            format!("a Backend does not send `{other}` unless the Daemon asked for it"),
        ),
    };
    refuse(request.id, code, message)
}

fn refuse(id: Option<CorrelationId>, code: ProtocolCode, message: String) -> Envelope<Value> {
    error_response(id, &ErrorBody::from(&ProtocolError::new(code, message)))
}
