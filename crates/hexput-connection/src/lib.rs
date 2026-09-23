//! Transient per-connection actor, attached to a Session. Holds no state that outlives the
//! Session it is attached to; a Session may have zero or more Connections attached at once.
//!
//! [`serve`] is the core's side of one connection. It is generic over [`Port`] and names no
//! transport: whatever adapter accepted the connection, the core reads envelopes, answers each
//! on the same connection, and stops when the peer leaves (AD-1).
//!
//! # What a connection is answered today
//!
//! A connection starts unattached. Every well-formed request gets exactly one reply, on this
//! connection alone, echoing its correlation id:
//!
//! * `Init` on an unattached connection — the payload is decoded; a valid one creates a Session
//!   with this connection attached and is answered `Result { client_id }`, an invalid one is
//!   `protocol.invalid_payload` naming what was wrong, and no Session is created.
//! * `Init` on an attached connection — `protocol.already_initialized`; its Session is untouched.
//! * `ExecutionStart` — `protocol.init_not_completed` before init: nothing runs (FR-1). After
//!   init, a Direct Execution (FR-4): [`hexput_script::direct_execution`] decodes the payload,
//!   runs the Script through the one Executor, and the connection answers `Result { value }` or
//!   the `Error` it returns — a parse or runtime diagnostic, or a `protocol.*` refusal. A failing
//!   Script ends nothing but itself. For now it runs inline in this loop, so this connection reads
//!   nothing else until it finishes; Story 2.7 moves it to an independent task.
//! * `Result` or `Error` from the Backend — `protocol.unexpected_message`: the Daemon asked
//!   nothing it could answer.
//!
//! A frame the codec rejects gets its `protocol.*` error response; the connection keeps
//! reading unless the error is fatal (an oversized frame), after which it closes.
//!
//! A reply the adapter cannot frame (too large) is replaced by `protocol.response_too_large`
//! carrying the same id, so a request is never left unanswered for its reply's size; the
//! connection stays open.
//!
//! However the connection ends — the peer closing, a lost stream, a failed write, a fatal frame —
//! [`serve`] detaches it from its Session, and detaching the last Connection tears the Session
//! down. The detach is an explicit call on the one exit path rather than a `Drop` guard, so
//! teardown is never implied by `Drop` (AD-4); a panic inside `serve` skips it.
//!
//! Binds: AD-1, AD-2.

use std::io;
use std::sync::Arc;

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, Inbound, MessageType, Outbound, Port, ProtocolCode,
    ProtocolError, Received, Value, error_response,
};
use hexput_session::{ClientId, ConnectionId, InitRequest, Sessions};

/// A connection's attachment: the Session's Client ID and this connection's own attachment id.
/// Never a copy of the Session's Config — that lives only in [`Sessions`] (AD-5).
type Attachment = (ClientId, ConnectionId);

/// Serve one connection until the peer leaves, a write fails, or a fatal protocol error closes
/// it, then detach it from its Session if it completed init. Never panics on anything the peer
/// sends; failures are logged at `debug`, since a peer leaving is routine.
pub async fn serve<P: Port>(port: P, sessions: Arc<Sessions>) {
    let mut attached: Option<Attachment> = None;
    exchange(port, &sessions, &mut attached).await;
    // The one exit path: however `exchange` ended, a completed init is undone here.
    if let Some((client_id, connection)) = attached {
        sessions.detach(client_id, connection);
        tracing::debug!("connection detached from its Session");
    }
}

/// Read, answer and write until the connection ends.
async fn exchange<P: Port>(port: P, sessions: &Sessions, attached: &mut Option<Attachment>) {
    let (mut inbound, mut outbound) = port.split();
    loop {
        let (reply, close) = match inbound.recv().await {
            Received::Message(request) => (answer(&request, sessions, attached), false),
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
        // Every registry lock `answer` took is released by now: nothing is held across this
        // `.await`.
        if !deliver(&mut outbound, reply).await {
            return;
        }
        if close {
            tracing::debug!("closing the connection after a fatal protocol error");
            return;
        }
    }
}

/// Write `reply`, or — when it cannot be framed — `protocol.response_too_large` with its id in
/// its place. Returns whether the connection is still usable.
async fn deliver<O: Outbound>(outbound: &mut O, reply: Envelope<Value>) -> bool {
    let id = reply.id;
    let error = match outbound.send(reply).await {
        Ok(()) => return true,
        Err(error) => error,
    };
    if error.kind() != io::ErrorKind::InvalidInput {
        tracing::debug!(%error, "cannot write to the connection; closing it");
        return false;
    }
    // Nothing was written: the reply could not be framed, and the connection is intact.
    tracing::debug!(%error, "a reply was too large to send; answering that instead");
    let refusal = refuse(
        id,
        ProtocolCode::ResponseTooLarge,
        format!("the response could not be sent: {error}"),
    );
    match outbound.send(refusal).await {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            // A refusal is a few hundred bytes; only a broken adapter lands here.
            tracing::warn!(%error, "not even the refusal could be sent; the connection stays open");
            true
        }
        Err(error) => {
            tracing::debug!(%error, "cannot write to the connection; closing it");
            false
        }
    }
}

/// The response to one well-formed message.
fn answer(
    request: &Envelope<Value>,
    sessions: &Sessions,
    attached: &mut Option<Attachment>,
) -> Envelope<Value> {
    // Messages that need no Session.
    match request.message_type {
        MessageType::Init => return init(request, sessions, attached),
        MessageType::ExecutionStart => {}
        other => {
            return refuse(
                request.id,
                ProtocolCode::UnexpectedMessage,
                format!("a Backend does not send `{other}` unless the Daemon asked for it"),
            );
        }
    }

    // The init gate: the one check between a connection and everything that needs a Session.
    // Health/metrics (Epic 7) join the arm above to bypass it.
    if attached.is_none() {
        return refuse(
            request.id,
            ProtocolCode::InitNotCompleted,
            format!(
                "`{}` needs a completed init; send `Init` first",
                request.message_type
            ),
        );
    }

    // Past the gate, `ExecutionStart` is the only message left: a Direct Execution.
    match hexput_script::direct_execution(&request.payload) {
        Ok(payload) => Envelope {
            id: request.id,
            message_type: MessageType::Result,
            payload,
        },
        Err(body) => {
            tracing::debug!(code = %body.code, "a Direct Execution failed");
            error_response(request.id, &body)
        }
    }
}

/// Serve `Init`: decode the payload, create the Session with this connection attached, and
/// answer its Client ID.
fn init(
    request: &Envelope<Value>,
    sessions: &Sessions,
    attached: &mut Option<Attachment>,
) -> Envelope<Value> {
    if attached.is_some() {
        return refuse(
            request.id,
            ProtocolCode::AlreadyInitialized,
            "this connection already completed init; its Session is unchanged".to_owned(),
        );
    }
    let init = match InitRequest::from_value(&request.payload) {
        Ok(init) => init,
        Err(error) => {
            tracing::debug!(%error, "refused an invalid init");
            return refuse(request.id, ProtocolCode::InvalidPayload, error.to_string());
        }
    };
    let (client_id, connection) = sessions.create(init);
    *attached = Some((client_id, connection));
    tracing::debug!("init completed; Session created");
    Envelope {
        id: request.id,
        message_type: MessageType::Result,
        payload: Value::Map(vec![(
            Value::from("client_id"),
            Value::from(client_id.to_string()),
        )]),
    }
}

fn refuse(id: Option<CorrelationId>, code: ProtocolCode, message: String) -> Envelope<Value> {
    error_response(id, &ErrorBody::from(&ProtocolError::new(code, message)))
}
