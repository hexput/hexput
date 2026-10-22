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
//!   with this connection attached, opens it in the [`AstCache`] so its registrations are
//!   accepted (Story 4.3), and is answered `Result { client_id }`; an invalid one is
//!   `protocol.invalid_payload` naming what was wrong, and no Session is created.
//! * `Init` on an attached connection — `protocol.already_initialized`; its Session is untouched.
//! * `ExecutionStart` — `protocol.init_not_completed` before init: nothing runs (FR-1). After
//!   init, a Direct Execution (FR-4): [`hexput_script::direct_execution`] decodes the payload,
//!   runs the Script through the one Executor, and the connection answers `Result { value }` or
//!   the `Error` it returns — a parse or runtime diagnostic, or a `protocol.*` refusal. A failing
//!   Script ends nothing but itself.
//! * `ConfigUpdate` — `protocol.init_not_completed` before init. After init, the payload's
//!   `config` — a complete Config, decoded like `Init.config` — replaces the Session's Config
//!   (Story 3.8) and is answered `Result {}`; an invalid one is `protocol.invalid_payload` and
//!   changes nothing. Handled inline, so it is in force for every execution this connection sends
//!   after it, and for every later execution on any Connection attached to the Session. An
//!   execution already dispatched keeps the settings it started with.
//! * `CodeRegister` — `protocol.init_not_completed` before init. After init, a registration
//!   (FR-5, Story 4.1): [`hexput_script::code_register`] parses the source once, runs the static
//!   check when the Session's check mode asks for it, and stores the AST in the Daemon's one
//!   [`AstCache`] under this Session's Client ID, and the connection answers `Result { handle }`
//!   (with `findings` when the check found anything) or the `Error` it returns, with no entry
//!   created. Dispatched as its own task, like an execution, so a large source never stalls the
//!   loop. Logged at `debug`: "script registered" with its `handle`, or the failure.
//! * `CachedExecutionStart` — `protocol.init_not_completed` before init. After init, a Cached
//!   Execution (FR-5, Story 4.2): [`hexput_script::cached_execution`] looks the payload's handle up
//!   in the [`AstCache`] under this Session's Client ID and runs the registered Script with the
//!   payload's fresh variables through the one Executor, with nothing parsed and no static check,
//!   and the connection answers exactly as for a Direct Execution — `Result { value,
//!   modifications? }` or the `Error` — or `protocol.unknown_script_handle` for a handle this
//!   Session has no entry under. A failure is logged at `debug` as "a Cached Execution failed",
//!   with its `code` and, when the payload names a well-formed one, its `handle`.
//! * `CodeUnregister` — `protocol.init_not_completed` before init. After init (Story 4.3),
//!   [`hexput_script::code_unregister`] removes the entry the payload's handle names under this
//!   Session's Client ID from the [`AstCache`] at once, and the connection answers `Result {}`, or
//!   `protocol.unknown_script_handle` for a handle this Session has no entry under, or
//!   `protocol.invalid_payload` for a malformed payload, with nothing removed. Handled inline, so
//!   the entry is gone before the reply is written. A Cached Execution dispatched before it —
//!   sent earlier on this connection — fetched its program at dispatch and finishes. A failure
//!   is logged at `debug` as "an unregister failed", with its `code` and, when the payload names
//!   a well-formed one, its `handle`.
//! * `Result` or `Error` from the Backend — the reply to a host call the Daemon made on this
//!   connection, when its id names one still pending (see "Host calls"); otherwise
//!   `protocol.unexpected_message` with a **nil** id: the Daemon asked nothing it could answer,
//!   and the stray id is in the Daemon's call-id space, so echoing it would read, in the
//!   Backend's own id space, as the failure of an unrelated request of its own.
//! * `Call` or `Authorize` from the Backend — `protocol.unexpected_message`: only the Daemon calls,
//!   and only the Daemon asks a per-call handler.
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
//! Every initialized `ExecutionStart`, `CachedExecutionStart` and `CodeRegister` is dispatched as
//! its own task, owned by the connection; a registration's parse and check run on the blocking
//! pool too (NFR5). An execution's task runs [`hexput_script::direct_execution`] or
//! [`hexput_script::cached_execution`], which run every piece of Script work on the
//! shared runtime's blocking pool: runtime workers never run Script code, so a slow Script delays
//! neither this connection's reads nor any other connection, even when slow Scripts outnumber the
//! cores (AD-6, FR-16). A connection may have any number of executions in flight, and there is no
//! per-connection serial queue: up to the size of Tokio's blocking pool (512 threads by default),
//! none waits on another. Past it, executions queue Daemon-wide for a free thread; a cap on
//! in-flight executions is deferred to Epic 3. Everything else — `Init`, `ConfigUpdate`,
//! `CodeUnregister`, the gate's refusals, malformed-frame answers, routing a host call's reply —
//! is handled inline, at once.
//!
//! The loop races reading the next message, the next host call an execution submits, and the next
//! finished execution, and is the only writer of the `Outbound` half, so no lock guards it. Each
//! execution's reply is written, with its request id, as soon as it finishes — in completion
//! order, not submission order. A write is always driven to completion (`Outbound::send` is not
//! cancel-safe); only `Inbound::recv`, the call queue and `JoinSet::join_next`, all cancel-safe,
//! are raced. While one envelope is being written the loop neither reads nor collects anything,
//! and `send` has no timeout: a peer that stops reading stalls this connection's reads and
//! replies until it reads again — this connection alone, never another. A write timeout is
//! deferred (Epic 3).
//!
//! When the peer stops sending — a clean close, possibly of its write side only, or a fatal
//! frame — the connection stops reading, waits for its in-flight executions, writes each reply,
//! and only then closes. A lost stream or a failed write abandons them at once: nothing more can
//! be written. An abandoned execution already running finishes its blocking segment and its
//! result is discarded; nothing can cancel a running Script until Story 3.5's Resource Budget.
//!
//! # Host calls
//!
//! A Script calls its Session's Registered Functions through this connection (Story 3.1). The
//! connection holds one [`hexput_rpc::Calls`] table and hands each execution a
//! [`hexput_rpc::Caller`], together with the Session's registrations and their grants and its
//! Config's execution limits (Story 3.7), read together once when the execution is dispatched;
//! the Executor alone decides which calls may go ahead (AD-3).
//! This crate must neither dispatch through the `Caller` nor build a `Call` envelope itself; a
//! source-text guard in `scripts/check-crate-graph.py` enforces that, and a sealed token the
//! compiler enforces is still open. When an execution makes a call, the loop writes the `Call`
//! envelope `hexput-rpc` built under a Daemon-issued id — a per-connection counter, independent of
//! the Backend's ids — and routes the Backend's `Result` or `Error` naming that id back to the
//! waiting execution, which
//! holds no thread while it waits. A `Call` the adapter cannot frame fails only that call. A
//! Backend's `Error` is the Script's failure, never the Daemon's, and is logged at `debug`.
//!
//! A function registered without a blanket grant is decided by the Backend's per-call handler
//! (Story 3.3). The Executor's question travels exactly like a call: `hexput-rpc` builds an
//! `Authorize` envelope under an id from the same counter, the loop writes it, routes the
//! Backend's `Result` or `Error` back through the same table, and closing the table fails a
//! pending question like a pending call. An answer that arrives after the Executor stopped
//! waiting is taken off the table and dropped, never answered as a stray reply.
//!
//! When the connection stops reading, or is lost, every pending call fails with `host.no_reply` at
//! once, as does every call made after: no execution the connection is still waiting for can wait
//! on a reply that cannot arrive.
//!
//! However the connection ends — the peer closing, a lost stream, a failed write, a fatal frame —
//! [`serve`] detaches it from its Session, and detaching the last Connection tears the Session
//! down. The detach is an explicit call on the one exit path rather than a `Drop` guard, so
//! teardown is never implied by `Drop` (AD-4); a panic inside `serve` skips it.
//!
//! # Logging
//!
//! Everything [`serve`] logs is attributed through `tracing` span fields, never message text
//! (FR-12). A connection runs inside a `connection` span carrying `connection` — its
//! [`ConnectionId`], issued by [`Sessions::connect`] when it opens and also its Session
//! attachment — and `client_id`, which is `"none"` until init completes: the field is marked,
//! never omitted. Each well-formed request (and each malformed frame whose id is readable) is
//! handled inside a child `request` span carrying its correlation `id` as a string (`"none"` when
//! a request has none). That span instruments the task that runs a Direct Execution, and is
//! entered again while the execution's reply is written, so every event about a request —
//! wherever it is emitted — names both the Client ID and the request.
//!
//! A successful init *replaces* the connection span with one carrying the issued Client ID,
//! rather than recording into the old one: an unset field would be omitted from output, and a
//! re-recorded one is written twice in text output. "init completed" is the first event of the
//! new span. Both spans are created at `ERROR` level, so no configured log level disables them
//! and strips their fields from the events that do pass. This crate only creates spans; the
//! Daemon chooses how they are written.
//!
//! Binds: AD-1, AD-2, AD-6, FR-12.

use std::io;
use std::sync::Arc;

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, Inbound, MessageType, Outbound, Port, ProtocolCode,
    ProtocolError, Received, ScriptHandle, Value, error_response,
};
use hexput_rpc::{Call, Caller, Calls};
use hexput_script::{AstCache, Registration};
use hexput_session::{ClientId, Config, ConnectionId, InitRequest, Sessions, Settings};
use tokio::task::{JoinError, JoinSet};
use tracing::{Instrument, Span};

/// The one connection's identity and state, as the loop sees it.
struct Connection<'s> {
    sessions: &'s Sessions,
    /// The Daemon's one AST Cache, which a registration stores its Script in (Story 4.1).
    cache: &'s Arc<AstCache>,
    /// Issued when the connection opened; also its attachment once init completes.
    id: ConnectionId,
    /// The Client ID of the Session this connection is attached to, once init completes. Never a
    /// copy of the Session's Config — that lives only in [`Sessions`] (AD-5).
    attached: Option<ClientId>,
    /// The span everything about this connection is logged in; replaced once, on init.
    span: Span,
}

/// Serve one connection until the peer leaves, a write fails, or a fatal protocol error closes
/// it, then detach it from its Session if it completed init. Never panics on anything the peer
/// sends; failures are logged at `debug`, since a peer leaving is routine.
///
/// `sessions` and `cache` are the Daemon run's one Session registry and one AST Cache, shared by
/// every connection.
pub async fn serve<P: Port>(port: P, sessions: Arc<Sessions>, cache: Arc<AstCache>) {
    let id = sessions.connect();
    let mut connection = Connection {
        sessions: &sessions,
        cache: &cache,
        id,
        attached: None,
        span: connection_span(id, None),
    };
    connection
        .span
        .in_scope(|| tracing::debug!("connection opened"));
    exchange(port, &mut connection).await;
    // The one exit path: however `exchange` ended — its executions finished or abandoned — a
    // completed init is undone here.
    if let Some(client_id) = connection.attached {
        sessions.detach(client_id, id);
        connection
            .span
            .in_scope(|| tracing::debug!("connection detached from its Session"));
    }
}

/// The span a connection is served in. `ERROR` level, so no log level can disable it.
fn connection_span(id: ConnectionId, client_id: Option<ClientId>) -> Span {
    match client_id {
        Some(client_id) => tracing::error_span!(
            "connection",
            connection = id.get(),
            client_id = %client_id,
        ),
        // `%`, like the Client ID: plain text shows `client_id=none`, not `client_id="none"`.
        None => tracing::error_span!("connection", connection = id.get(), client_id = %"none"),
    }
}

/// The span one request is handled in, a child of its connection's. `ERROR` level, like it.
fn request_span(connection: &Span, id: Option<CorrelationId>) -> Span {
    match id {
        // Always a string, `"none"` included, so the field has one type in every JSON line.
        Some(id) => tracing::error_span!(parent: connection, "request", id = %id.get()),
        None => tracing::error_span!(parent: connection, "request", id = %"none"),
    }
}

/// What the loop woke up for.
enum Event {
    /// The next item from the peer.
    Received(Received),
    /// An execution task ended, with its reply or its failure.
    Finished(Result<Envelope<Value>, JoinError>),
    /// An execution made a host call, or asks whether it may, to be written.
    Call(Call),
}

/// Read, answer and write until the connection ends. Returning drops `running`, which abandons
/// every execution still in flight.
///
/// No span guard is ever held across an `.await`: synchronous work runs in `Span::in_scope`,
/// and each awaited future is `instrument`ed with the span it belongs to.
async fn exchange<P: Port>(port: P, connection: &mut Connection<'_>) {
    let (mut inbound, mut outbound) = port.split();
    let mut running: JoinSet<Envelope<Value>> = JoinSet::new();
    // This connection's host calls. The loop keeps a `Caller` itself, so the queue stays open
    // while it reads, and clones it for each execution.
    let (mut calls, caller) = Calls::new();
    // Whether the peer may still send; once it cannot, only in-flight executions are awaited.
    let mut reading = true;
    loop {
        let event = if reading {
            // All three futures are cancel-safe: a branch that loses loses nothing.
            tokio::select! {
                received = inbound.recv().instrument(connection.span.clone()) => {
                    Event::Received(received)
                }
                Some(call) = calls.submitted() => Event::Call(call),
                Some(finished) = running.join_next() => Event::Finished(finished),
            }
        } else {
            match running
                .join_next()
                .instrument(connection.span.clone())
                .await
            {
                Some(finished) => Event::Finished(finished),
                None => return,
            }
        };
        // The reply, and the request span it is written in (the connection's, for a malformed
        // frame whose id is unreadable).
        let (reply, span) = match event {
            Event::Received(Received::Message(request)) => {
                // A Backend `Result`/`Error` is a reply to one of this connection's host calls
                // when its id names one still pending; anything else is answered below.
                let request = match route_reply(&mut calls, request, &connection.span) {
                    Some(request) => request,
                    None => continue,
                };
                let span = request_span(&connection.span, request.id);
                match span.in_scope(|| answer(request, connection)) {
                    Answer::Reply(reply) => (reply, span),
                    Answer::Initialized(client_id, reply) => {
                        // The Client ID becomes part of the connection span, so the span is
                        // replaced (see "Logging"); "init completed" is its first event.
                        connection.attached = Some(client_id);
                        connection.span = connection_span(connection.id, Some(client_id));
                        let span = request_span(&connection.span, reply.id);
                        span.in_scope(|| tracing::debug!("init completed; Session created"));
                        (reply, span)
                    }
                    Answer::Execute(id, payload) => {
                        let (registrations, settings) = session_now(connection);
                        // The task carries the request's span, so everything the execution logs
                        // names its connection, its Client ID and its request.
                        running.spawn(
                            execute(
                                id,
                                payload,
                                registrations,
                                settings,
                                caller.for_execution(id),
                            )
                            .instrument(span),
                        );
                        continue;
                    }
                    Answer::CachedExecute(id, payload, client_id) => {
                        let (registrations, settings) = session_now(connection);
                        // Read before the payload moves, so an operator can tell which handle
                        // failed.
                        let handle = handle_of(&payload);
                        // Called here, as the request is dispatched: the handle's program is
                        // fetched now, so a `CodeUnregister` this loop serves next cannot take it
                        // from this run (Story 4.3).
                        let run = span.in_scope(|| {
                            hexput_script::cached_execution(
                                payload,
                                client_id,
                                connection.cache,
                                registrations,
                                settings,
                                caller.for_execution(id),
                            )
                        });
                        // Exactly like a Direct Execution: its own task, carrying the request's
                        // span, its calls naming this request as their execution.
                        running.spawn(cached_execute(id, handle, run).instrument(span));
                        continue;
                    }
                    Answer::Register(id, payload, client_id) => {
                        let (registrations, settings) = session_now(connection);
                        // Like an execution: its own task, carrying the request's span.
                        running.spawn(
                            register(
                                id,
                                payload,
                                client_id,
                                Arc::clone(connection.cache),
                                registrations,
                                settings,
                            )
                            .instrument(span),
                        );
                        continue;
                    }
                }
            }
            Event::Received(Received::Malformed(failure)) => {
                let span = match failure.id {
                    Some(id) => request_span(&connection.span, Some(id)),
                    None => connection.span.clone(),
                };
                span.in_scope(|| {
                    tracing::debug!(error = %failure, "rejected a malformed frame");
                    if failure.error.is_fatal() {
                        tracing::debug!(
                            "closing the connection after a fatal protocol error, once its executions finish"
                        );
                        reading = false;
                    }
                });
                if !reading {
                    // Nothing more is read, so no host call can be answered.
                    calls.close();
                }
                (failure.to_response(), span)
            }
            Event::Received(Received::Closed(None)) => {
                connection
                    .span
                    .in_scope(|| tracing::debug!("connection closed by the peer"));
                reading = false;
                // Nothing more is read, so no host call can be answered.
                calls.close();
                continue;
            }
            Event::Received(Received::Closed(Some(error))) => {
                // The stream is broken: nothing more can be written, so nothing is waited for.
                connection
                    .span
                    .in_scope(|| tracing::debug!(%error, "connection lost"));
                return;
            }
            Event::Finished(Ok(reply)) => {
                // Executions start only after init, so the connection span already names the
                // Client ID the request was made under.
                let span = request_span(&connection.span, reply.id);
                (reply, span)
            }
            Event::Call(call) => {
                // Nothing to write when its execution already stopped waiting (a question that
                // timed out while queued).
                let Some(call) = connection.span.in_scope(|| calls.issue(call)) else {
                    continue;
                };
                if !send_call(&mut outbound, &mut calls, call)
                    .instrument(connection.span.clone())
                    .await
                {
                    return;
                }
                continue;
            }
            Event::Finished(Err(error)) => {
                // A task that panicked has no id left to answer with. It ends only itself.
                connection.span.in_scope(|| {
                    tracing::error!(%error, "an execution or registration task failed; its request goes unanswered");
                });
                continue;
            }
        };
        // Every registry lock `answer` took is released by now: nothing is held across this
        // `.await`, and it is driven to completion, never raced.
        if !deliver(&mut outbound, reply).instrument(span).await {
            return;
        }
    }
}

/// Hand a Backend `Result`/`Error` that answers a pending host call or authorization question to
/// the execution waiting on it. Returns the message when it answers none, for the caller to answer.
fn route_reply(
    calls: &mut Calls,
    message: Envelope<Value>,
    span: &Span,
) -> Option<Envelope<Value>> {
    let (id, failed) = (message.id, message.message_type == MessageType::Error);
    match calls.complete(message) {
        Ok(()) => {
            if failed {
                // The Script's failure, not the Daemon's: the execution reports it.
                span.in_scope(|| {
                    tracing::debug!(
                        call = id.map(CorrelationId::get),
                        "the Backend answered a host call or authorization question with Error"
                    );
                });
            }
            None
        }
        Err(message) => Some(message),
    }
}

/// Write one host call. A call that cannot be framed fails alone, with nothing written; any other
/// failure means the connection is unusable. Returns whether it is still usable.
async fn send_call<O: Outbound>(
    outbound: &mut O,
    calls: &mut Calls,
    call: Envelope<Value>,
) -> bool {
    let Some(id) = call.id else {
        return true;
    };
    match outbound.send(call).await {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            tracing::debug!(%error, call = id.get(), "a host call was too large to send");
            calls.unsendable(id, format!("its frame would be too large: {error}"));
            true
        }
        Err(error) => {
            tracing::debug!(%error, "cannot write to the connection; closing it");
            false
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
    /// At once, with this reply: init completed and a Session with this Client ID now has the
    /// connection attached.
    Initialized(ClientId, Envelope<Value>),
    /// By a Direct Execution of this payload, dispatched as its own task, replying to this id.
    Execute(Option<CorrelationId>, Value),
    /// By a registration of this payload for the Session with this Client ID, dispatched as its
    /// own task, replying to this id (Story 4.1).
    Register(Option<CorrelationId>, Value, ClientId),
    /// By a Cached Execution of this payload for the Session with this Client ID, dispatched as
    /// its own task, replying to this id (Story 4.2).
    CachedExecute(Option<CorrelationId>, Value, ClientId),
}

/// Answer one well-formed message. Takes it by value so an execution's payload — up to a whole
/// frame — moves to its task instead of being copied.
fn answer(request: Envelope<Value>, connection: &Connection<'_>) -> Answer {
    // Messages that need no Session.
    match request.message_type {
        MessageType::Init => return init(&request, connection),
        MessageType::ExecutionStart
        | MessageType::ConfigUpdate
        | MessageType::CodeRegister
        | MessageType::CachedExecutionStart
        | MessageType::CodeUnregister => {}
        other => {
            // A stray reply's id is the Daemon's call id, not a Backend request id: echoing it
            // would fail whatever Backend request shares the number. Nil is allowed on `Error`.
            let id = match other {
                MessageType::Result | MessageType::Error => None,
                _ => request.id,
            };
            return Answer::Reply(refuse(
                id,
                ProtocolCode::UnexpectedMessage,
                format!("a Backend does not send `{other}` unless the Daemon asked for it"),
            ));
        }
    }

    // The init gate: the one check between a connection and everything that needs a Session.
    // Health/metrics (Epic 7) join the arm above to bypass it.
    let Some(client_id) = connection.attached else {
        return Answer::Reply(refuse(
            request.id,
            ProtocolCode::InitNotCompleted,
            format!(
                "`{}` needs a completed init; send `Init` first",
                request.message_type
            ),
        ));
    };

    // Past the gate: a `ConfigUpdate` or a `CodeUnregister`, answered inline, a registration, a
    // Cached Execution or a Direct Execution.
    match request.message_type {
        MessageType::ConfigUpdate => Answer::Reply(config_update(&request, connection)),
        MessageType::CodeUnregister => {
            Answer::Reply(code_unregister(&request, client_id, connection))
        }
        MessageType::CodeRegister => Answer::Register(request.id, request.payload, client_id),
        MessageType::CachedExecutionStart => {
            Answer::CachedExecute(request.id, request.payload, client_id)
        }
        _ => Answer::Execute(request.id, request.payload),
    }
}

/// The Session's registrations, grants and Config settings as they are now, read together once
/// per execution or registration and never cached (AD-5).
fn session_now(connection: &Connection<'_>) -> (Vec<Registration>, Settings) {
    let (registrations, settings) = connection
        .attached
        .and_then(|client_id| connection.sessions.for_execution(client_id))
        .unwrap_or_default();
    let registrations = registrations
        .into_iter()
        .map(|r| Registration {
            name: r.name().to_owned(),
            key: r.key().map(str::to_owned),
            blanket: r.blanket(),
        })
        .collect();
    (registrations, settings)
}

/// Serve `ConfigUpdate` (Story 3.8): decode the complete new Config and replace the Session's
/// single live copy with it, answering `Result {}`. Inline and synchronous — the registry's shard
/// lock is taken and released here — so an `ExecutionStart` this connection sends after it is
/// always dispatched under the new Config. A refused update changes nothing.
fn config_update(request: &Envelope<Value>, connection: &Connection<'_>) -> Envelope<Value> {
    let config = match Config::from_update_payload(&request.payload) {
        Ok(config) => config,
        Err(error) => {
            tracing::debug!(%error, "refused an invalid config update");
            return refuse(request.id, ProtocolCode::InvalidPayload, error.to_string());
        }
    };
    let updated = connection
        .attached
        .is_some_and(|client_id| connection.sessions.update_config(client_id, config));
    if !updated {
        // Unreachable while the connection is attached: its Session outlives it. No better code
        // exists than `init_not_completed`; the message says what actually happened.
        tracing::error!("an attached connection's Session no longer exists; config not updated");
        return refuse(
            request.id,
            ProtocolCode::InitNotCompleted,
            "this connection's Session no longer exists; the Config was not updated".to_owned(),
        );
    }
    tracing::debug!("config updated");
    Envelope {
        id: request.id,
        message_type: MessageType::Result,
        payload: Value::Map(Vec::new()),
    }
}

/// Serve `CodeUnregister` (Story 4.3): remove this Session's AST Cache entry at once, answering
/// `Result {}`. Inline and synchronous — no parse and no Script, only the cache's own short work —
/// so the entry is gone before the reply is written, and a `CachedExecutionStart` this connection
/// sends after it finds the handle unknown. A run that already fetched the program finishes.
fn code_unregister(
    request: &Envelope<Value>,
    client_id: ClientId,
    connection: &Connection<'_>,
) -> Envelope<Value> {
    match hexput_script::code_unregister(&request.payload, client_id, connection.cache) {
        Ok(payload) => Envelope {
            id: request.id,
            message_type: MessageType::Result,
            payload,
        },
        Err(body) => {
            // Like a Cached Execution's failure: the handle when the payload names a well-formed
            // one, so an operator can tell which entry the Backend expected.
            match handle_of(&request.payload) {
                Some(handle) => {
                    tracing::debug!(code = %body.code, handle = %handle, "an unregister failed");
                }
                None => tracing::debug!(code = %body.code, "an unregister failed"),
            }
            error_response(request.id, &body)
        }
    }
}

/// Run one Direct Execution to its reply. The Script's own work runs on the blocking pool; this
/// task only waits, including for its host calls' replies.
async fn execute(
    id: Option<CorrelationId>,
    payload: Value,
    registrations: Vec<Registration>,
    settings: Settings,
    caller: Caller,
) -> Envelope<Value> {
    match hexput_script::direct_execution(payload, registrations, settings, caller).await {
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

/// Run one Cached Execution to its reply (Story 4.2). Like [`execute`], this task only waits: the
/// Script's own work runs on the blocking pool. `run` is [`hexput_script::cached_execution`]'s
/// future, made as the request was dispatched; `handle` is the well-formed handle the payload
/// names, if any — only that is logged; a malformed one is the `invalid_payload` the reply names.
async fn cached_execute(
    id: Option<CorrelationId>,
    handle: Option<ScriptHandle>,
    run: impl Future<Output = Result<Value, Box<ErrorBody>>>,
) -> Envelope<Value> {
    match run.await {
        Ok(payload) => Envelope {
            id,
            message_type: MessageType::Result,
            payload,
        },
        Err(body) => {
            match handle {
                Some(handle) => tracing::debug!(
                    code = %body.code,
                    handle = %handle,
                    "a Cached Execution failed"
                ),
                None => tracing::debug!(code = %body.code, "a Cached Execution failed"),
            }
            error_response(id, &body)
        }
    }
}

/// The well-formed `handle` a `CachedExecutionStart` payload names, if it names one.
fn handle_of(payload: &Value) -> Option<ScriptHandle> {
    payload
        .as_map()?
        .iter()
        .find(|(key, _)| key.as_str() == Some("handle"))?
        .1
        .as_str()?
        .parse()
        .ok()
}

/// Run one registration to its reply (Story 4.1). Parsing and checking run on the blocking pool;
/// this task only waits.
async fn register(
    id: Option<CorrelationId>,
    payload: Value,
    client_id: ClientId,
    cache: Arc<AstCache>,
    registrations: Vec<Registration>,
    settings: Settings,
) -> Envelope<Value> {
    match hexput_script::code_register(payload, client_id, cache, registrations, settings).await {
        Ok(payload) => {
            if let Some(handle) = payload
                .as_map()
                .and_then(|fields| {
                    fields
                        .iter()
                        .find(|(key, _)| key.as_str() == Some("handle"))
                })
                .and_then(|(_, handle)| handle.as_str())
            {
                tracing::debug!(handle, "script registered");
            }
            Envelope {
                id,
                message_type: MessageType::Result,
                payload,
            }
        }
        Err(body) => {
            tracing::debug!(code = %body.code, "a registration failed");
            error_response(id, &body)
        }
    }
}

/// Serve `Init`: decode the payload, create the Session with this connection attached, and
/// answer its Client ID. The caller records the attachment and logs the completed init in the
/// new connection span.
fn init(request: &Envelope<Value>, connection: &Connection<'_>) -> Answer {
    if connection.attached.is_some() {
        return Answer::Reply(refuse(
            request.id,
            ProtocolCode::AlreadyInitialized,
            "this connection already completed init; its Session is unchanged".to_owned(),
        ));
    }
    let init = match InitRequest::from_value(&request.payload) {
        Ok(init) => init,
        Err(error) => {
            tracing::debug!(%error, "refused an invalid init");
            return Answer::Reply(refuse(
                request.id,
                ProtocolCode::InvalidPayload,
                error.to_string(),
            ));
        }
    };
    let client_id = connection.sessions.create(init, connection.id);
    // The cache accepts this Session's registrations from now until its teardown closes it.
    connection.cache.open(client_id);
    Answer::Initialized(
        client_id,
        Envelope {
            id: request.id,
            message_type: MessageType::Result,
            payload: Value::Map(vec![(
                Value::from("client_id"),
                Value::from(client_id.to_string()),
            )]),
        },
    )
}

/// A `protocol.*` refusal of request `id`, logged at `debug` in whatever span is current.
fn refuse(id: Option<CorrelationId>, code: ProtocolCode, message: String) -> Envelope<Value> {
    tracing::debug!(code = %code, "refused a request");
    error_response(id, &ErrorBody::from(&ProtocolError::new(code, message)))
}
