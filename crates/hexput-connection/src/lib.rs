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
//!   Script ends nothing but itself.
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
//! # Concurrency
//!
//! Every initialized `ExecutionStart` is dispatched as its own task, owned by the connection, on
//! the shared runtime's blocking pool (`spawn_blocking`): runtime workers never run Script code, so
//! a slow Script delays neither this connection's reads nor any other connection, even when slow
//! Scripts outnumber the cores (AD-6, FR-16). A connection may have any number of executions in
//! flight, and there is no per-connection serial queue: up to the size of Tokio's blocking pool
//! (512 threads by default), none waits on another. Past it, executions queue Daemon-wide for a
//! free thread; a cap on in-flight executions is deferred to Epic 3. Everything else — `Init`, the
//! gate's refusals, malformed-frame answers — is answered inline, at once.
//!
//! The loop races reading the next message against the next finished execution and is the only
//! writer of the `Outbound` half, so no lock guards it. Each execution's reply is written, with
//! its request id, as soon as it finishes — in completion order, not submission order. A write is
//! always driven to completion (`Outbound::send` is not cancel-safe); only `Inbound::recv` and
//! `JoinSet::join_next`, both cancel-safe, are raced. While one reply is being written the loop
//! neither reads nor collects finished executions, and `send` has no timeout: a peer that stops
//! reading stalls this connection's reads and replies until it reads again — this connection
//! alone, never another. A write timeout is deferred (Epic 3).
//!
//! When the peer stops sending — a clean close, possibly of its write side only, or a fatal
//! frame — the connection stops reading, waits for its in-flight executions, writes each reply,
//! and only then closes. A lost stream or a failed write abandons them at once: nothing more can
//! be written. An abandoned execution already running finishes on its blocking thread and its
//! result is discarded; nothing can cancel a running Script until Story 3.5's Resource Budget.
//!
//! However the connection ends — the peer closing, a lost stream, a failed write, a fatal frame —
//! [`serve`] detaches it from its Session, and detaching the last Connection tears the Session
//! down. The detach is an explicit call on the one exit path rather than a `Drop` guard, so
//! teardown is never implied by `Drop` (AD-4); a panic inside `serve` skips it.
//!
//! Binds: AD-1, AD-2, AD-6.

use std::io;
use std::sync::Arc;

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, Inbound, MessageType, Outbound, Port, ProtocolCode,
    ProtocolError, Received, Value, error_response,
};
use hexput_session::{ClientId, ConnectionId, InitRequest, Sessions};
use tokio::task::{JoinError, JoinSet};

/// A connection's attachment: the Session's Client ID and this connection's own attachment id.
/// Never a copy of the Session's Config — that lives only in [`Sessions`] (AD-5).
type Attachment = (ClientId, ConnectionId);

/// Serve one connection until the peer leaves, a write fails, or a fatal protocol error closes
/// it, then detach it from its Session if it completed init. Never panics on anything the peer
/// sends; failures are logged at `debug`, since a peer leaving is routine.
pub async fn serve<P: Port>(port: P, sessions: Arc<Sessions>) {
    let mut attached: Option<Attachment> = None;
    exchange(port, &sessions, &mut attached).await;
    // The one exit path: however `exchange` ended — its executions finished or abandoned — a
    // completed init is undone here.
    if let Some((client_id, connection)) = attached {
        sessions.detach(client_id, connection);
        tracing::debug!("connection detached from its Session");
    }
}

/// What the loop woke up for.
enum Event {
    /// The next item from the peer.
    Received(Received),
    /// An execution task ended, with its reply or its failure.
    Finished(Result<Envelope<Value>, JoinError>),
}

/// Read, answer and write until the connection ends. Returning drops `running`, which abandons
/// every execution still in flight.
async fn exchange<P: Port>(port: P, sessions: &Sessions, attached: &mut Option<Attachment>) {
    let (mut inbound, mut outbound) = port.split();
    let mut running: JoinSet<Envelope<Value>> = JoinSet::new();
    // Whether the peer may still send; once it cannot, only in-flight executions are awaited.
    let mut reading = true;
    loop {
        let event = if reading {
            // Both futures are cancel-safe: the branch that loses loses nothing.
            tokio::select! {
                received = inbound.recv() => Event::Received(received),
                Some(finished) = running.join_next() => Event::Finished(finished),
            }
        } else {
            match running.join_next().await {
                Some(finished) => Event::Finished(finished),
                None => return,
            }
        };
        let reply = match event {
            Event::Received(Received::Message(request)) => {
                match answer(request, sessions, attached) {
                    Answer::Reply(reply) => reply,
                    Answer::Execute(id, payload) => {
                        // The blocking thread does not inherit the caller's span; carry it, so
                        // everything the execution logs stays in the connection's span.
                        let span = tracing::Span::current();
                        running.spawn_blocking(move || span.in_scope(|| execute(id, payload)));
                        continue;
                    }
                }
            }
            Event::Received(Received::Malformed(failure)) => {
                tracing::debug!(error = %failure, "rejected a malformed frame");
                if failure.error.is_fatal() {
                    tracing::debug!(
                        "closing the connection after a fatal protocol error, once its executions finish"
                    );
                    reading = false;
                }
                failure.to_response()
            }
            Event::Received(Received::Closed(None)) => {
                tracing::debug!("connection closed by the peer");
                reading = false;
                continue;
            }
            Event::Received(Received::Closed(Some(error))) => {
                // The stream is broken: nothing more can be written, so nothing is waited for.
                tracing::debug!(%error, "connection lost");
                return;
            }
            Event::Finished(Ok(reply)) => reply,
            Event::Finished(Err(error)) => {
                // A task that panicked has no id left to answer with. It ends only itself.
                tracing::error!(%error, "a Direct Execution task failed; its request goes unanswered");
                continue;
            }
        };
        // Every registry lock `answer` took is released by now: nothing is held across this
        // `.await`, and it is driven to completion, never raced.
        if !deliver(&mut outbound, reply).await {
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

/// How one well-formed message is answered.
enum Answer {
    /// At once, with this reply.
    Reply(Envelope<Value>),
    /// By a Direct Execution of this payload, dispatched as its own task, replying to this id.
    Execute(Option<CorrelationId>, Value),
}

/// Answer one well-formed message. Takes it by value so an execution's payload — up to a whole
/// frame — moves to its task instead of being copied.
fn answer(
    request: Envelope<Value>,
    sessions: &Sessions,
    attached: &mut Option<Attachment>,
) -> Answer {
    // Messages that need no Session.
    match request.message_type {
        MessageType::Init => return Answer::Reply(init(&request, sessions, attached)),
        MessageType::ExecutionStart => {}
        other => {
            return Answer::Reply(refuse(
                request.id,
                ProtocolCode::UnexpectedMessage,
                format!("a Backend does not send `{other}` unless the Daemon asked for it"),
            ));
        }
    }

    // The init gate: the one check between a connection and everything that needs a Session.
    // Health/metrics (Epic 7) join the arm above to bypass it.
    if attached.is_none() {
        return Answer::Reply(refuse(
            request.id,
            ProtocolCode::InitNotCompleted,
            format!(
                "`{}` needs a completed init; send `Init` first",
                request.message_type
            ),
        ));
    }

    // Past the gate, `ExecutionStart` is the only message left: a Direct Execution.
    Answer::Execute(request.id, request.payload)
}

/// Run one Direct Execution to its reply. Runs on a blocking-pool thread, never a runtime worker.
fn execute(id: Option<CorrelationId>, payload: Value) -> Envelope<Value> {
    match hexput_script::direct_execution(&payload) {
        Ok(payload) => Envelope {
            id,
            message_type: MessageType::Result,
            payload,
        },
        Err(body) => {
            tracing::debug!(code = %body.code, "a Direct Execution failed");
            error_response(id, &body)
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
