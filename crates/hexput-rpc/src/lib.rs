//! Host-function registry, capability grants, and outbound RPC dispatch to the Backend.
//! Depends on hexput-port only.
//!
//! Note the direction: this crate does not reach enforcement at all. hexput-exec depends on
//! hexput-rpc (not the reverse) and wraps every dispatch through here with the Capability and
//! Resource Budget checks from hexput-enforce, which hexput-exec alone may depend on. An
//! `rpc -> exec` edge would invert that and create a dependency cycle.
//!
//! # Host-call correlation (Story 3.1)
//!
//! A host call is a request the Daemon originates: one generic `Call` envelope, payload
//! `{name, arguments}`, written on the connection that submitted the execution and answered by
//! the Backend with `Result {value}` or `Error` under the same id. This crate owns everything
//! about that exchange except writing and reading the bytes:
//!
//! * [`Calls`] belongs to one connection's loop — its single writer. It hands out the Daemon's
//!   call ids (a per-connection counter, independent of the Backend's own request ids: a
//!   Backend's `Result`/`Error` is always a reply to a Daemon `Call`, so the two id spaces never
//!   meet), keeps the pending-call table, builds each `Call` envelope, and routes a Backend reply
//!   to the call it names.
//! * [`Caller`] is the handle an execution holds. [`Caller::dispatch_authorized`] queues a call for
//!   the loop to write and waits — a plain `.await`, no thread and no lock held — for its outcome.
//!   It checks nothing: its caller must already hold `hexput-enforce`'s permission for the call,
//!   which only `hexput-exec` can obtain (AD-3). The name is deliberately unmistakable, and
//!   `scripts/check-crate-graph.py` fails CI when it appears in any production crate but
//!   `hexput-exec` and this one — so `hexput-connection`, which creates the `Caller`, and
//!   `hexput-script`, which passes it on, can hold it but never use it.
//!
//! When the connection can no longer deliver a reply — its peer stopped sending, or the stream is
//! gone — [`Calls::close`] (or dropping the [`Calls`]) fails every pending call and every later
//! one with [`CallFailure::NoReply`] at once, so an execution the connection is still waiting for
//! can never wait on a reply that cannot come.
//!
//! Registration grants are decided in `hexput-enforce`, never here; the Session's registrations
//! reach it through `hexput-exec`.
//!
//! Binds: AD-3.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

use hexput_port::{CorrelationId, Envelope, MessageType};
/// The wire vocabulary a call is expressed in: its arguments and the Backend's reply value are
/// MessagePack values, and a `Call` travels in one frame. Re-exported because they are this
/// crate's API — the Executor converts to and from exactly these.
pub use hexput_port::{MAX_FRAME_LEN, MAX_NESTING_DEPTH, Value};

const NAME: &str = "name";
const ARGUMENTS: &str = "arguments";
const VALUE: &str = "value";
const MESSAGE: &str = "message";

/// Why a host call produced no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallFailure {
    /// The Backend answered with `Error`; its `message`, when the payload carried a string one.
    Failed(Option<String>),
    /// The Backend answered with a `Result` that is not a valid `{value}`: what was wrong.
    Malformed(String),
    /// The connection ended — or stopped reading — before the Backend answered.
    NoReply,
    /// The call could not be written (its frame would be too large): why. Nothing was sent.
    Unsendable(String),
}

/// One call on its way from an execution to the connection's writer.
#[derive(Debug)]
pub struct Call {
    name: String,
    arguments: Vec<Value>,
    reply: oneshot::Sender<Result<Value, CallFailure>>,
}

/// An execution's handle for making host calls on the connection that submitted it. Cheap to
/// clone; every clone reaches the same connection.
#[derive(Debug, Clone)]
pub struct Caller {
    calls: mpsc::UnboundedSender<Call>,
}

impl Caller {
    /// Call the Registered Function `name` with `arguments`, and wait for the Backend's answer:
    /// the reply's `value`, or why there is none.
    ///
    /// Only for a call `hexput-enforce` has already allowed: this sends whatever it is given.
    /// `hexput-exec` is its one caller (AD-3), pinned by `scripts/check-crate-graph.py`.
    ///
    /// Holds nothing while it waits. Every call ends: the connection answers it, or fails it
    /// with [`CallFailure::NoReply`] once no answer can arrive.
    ///
    /// # Errors
    /// The [`CallFailure`] describing why the call produced no value.
    pub async fn dispatch_authorized(
        &self,
        name: impl Into<String>,
        arguments: Vec<Value>,
    ) -> Result<Value, CallFailure> {
        let (reply, answer) = oneshot::channel();
        let call = Call {
            name: name.into(),
            arguments,
            reply,
        };
        if self.calls.send(call).is_err() {
            return Err(CallFailure::NoReply);
        }
        // A dropped sender is a connection that gave up on the call.
        answer.await.unwrap_or(Err(CallFailure::NoReply))
    }
}

/// One connection's host calls: the queue executions submit to, the Daemon's call-id counter, and
/// the calls written and not yet answered. Owned by the connection's loop and never shared, so it
/// needs no lock.
#[derive(Debug)]
pub struct Calls {
    queue: mpsc::UnboundedReceiver<Call>,
    pending: HashMap<CorrelationId, oneshot::Sender<Result<Value, CallFailure>>>,
    next: u64,
}

impl Calls {
    /// A connection's call table, and the [`Caller`] its executions submit through.
    #[must_use]
    pub fn new() -> (Self, Caller) {
        let (sender, queue) = mpsc::unbounded_channel();
        (
            Self {
                queue,
                pending: HashMap::new(),
                next: 0,
            },
            Caller { calls: sender },
        )
    }

    /// The next call an execution submitted, to be [issued](Self::issue) and written. `None`
    /// once the table is closed and drained, or no [`Caller`] is left.
    ///
    /// Cancel-safe: a call is never lost when this future is dropped before it completes.
    pub async fn submitted(&mut self) -> Option<Call> {
        self.queue.recv().await
    }

    /// Give `call` the next Daemon-issued id, record it as pending, and return the `Call`
    /// envelope to write: `{name, arguments}`.
    pub fn issue(&mut self, call: Call) -> Envelope<Value> {
        let id = CorrelationId(self.next);
        self.next = self.next.wrapping_add(1);
        self.pending.insert(id, call.reply);
        Envelope::new(
            id,
            MessageType::Call,
            Value::Map(vec![
                (Value::from(NAME), Value::from(call.name)),
                (Value::from(ARGUMENTS), Value::Array(call.arguments)),
            ]),
        )
    }

    /// Route a Backend message to the pending call it answers: a `Result` or `Error` whose id
    /// names a call written and not yet answered. Anything else — another message type, no id, an
    /// id no call is pending on — is handed back untouched for the caller to answer.
    ///
    /// # Errors
    /// The message itself, when it answers no pending call.
    pub fn complete(&mut self, message: Envelope<Value>) -> Result<(), Envelope<Value>> {
        if !matches!(
            message.message_type,
            MessageType::Result | MessageType::Error
        ) {
            return Err(message);
        }
        let Some(reply) = message.id.and_then(|id| self.pending.remove(&id)) else {
            return Err(message);
        };
        let outcome = match message.message_type {
            MessageType::Result => value_of(message.payload),
            _ => Err(CallFailure::Failed(message_of(&message.payload))),
        };
        // The execution may be gone already (its connection abandoned it); nothing to tell.
        let _ = reply.send(outcome);
        Ok(())
    }

    /// Fail the call written as `id` without sending it: its envelope could not be framed.
    pub fn unsendable(&mut self, id: CorrelationId, reason: impl Into<String>) {
        if let Some(reply) = self.pending.remove(&id) {
            let _ = reply.send(Err(CallFailure::Unsendable(reason.into())));
        }
    }

    /// No reply can arrive any more: fail every pending call and every submitted one with
    /// [`CallFailure::NoReply`], and every later [`Caller::dispatch_authorized`] at once.
    pub fn close(&mut self) {
        self.queue.close();
        // Dropping a call's sender is its `NoReply`.
        while self.queue.try_recv().is_ok() {}
        self.pending.clear();
    }

    /// How many calls are written and waiting for the Backend.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// A `Result` reply's `value`: the payload must be a map with exactly the key `value` (nil is the
/// value `null`; absent is malformed).
fn value_of(payload: Value) -> Result<Value, CallFailure> {
    let Value::Map(fields) = payload else {
        return Err(CallFailure::Malformed(
            "the reply is not a map with `value`".to_owned(),
        ));
    };
    let mut value = None;
    for (key, field) in fields {
        match key.as_str() {
            Some(VALUE) if value.is_none() => value = Some(field),
            Some(VALUE) => {
                return Err(CallFailure::Malformed(
                    "the reply repeats the key `value`".to_owned(),
                ));
            }
            Some(other) => {
                return Err(CallFailure::Malformed(format!(
                    "the reply has an unknown key {:?}",
                    bounded(other)
                )));
            }
            None => {
                return Err(CallFailure::Malformed(
                    "the reply has a key that is not a string".to_owned(),
                ));
            }
        }
    }
    value.ok_or_else(|| CallFailure::Malformed("the reply has no `value`".to_owned()))
}

/// An `Error` reply's `message`, when its payload is a map carrying a string one.
fn message_of(payload: &Value) -> Option<String> {
    let Value::Map(fields) = payload else {
        return None;
    };
    fields
        .iter()
        .find(|(key, _)| key.as_str() == Some(MESSAGE))
        .and_then(|(_, message)| message.as_str())
        .map(bounded)
}

/// The most characters of Backend text carried into a Script error.
const TEXT_LIMIT: usize = 512;

/// `text` cut to [`TEXT_LIMIT`] characters, with an ellipsis when anything was cut.
fn bounded(text: &str) -> String {
    match text.char_indices().nth(TEXT_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
