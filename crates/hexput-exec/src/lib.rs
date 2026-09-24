#![forbid(clippy::undocumented_unsafe_blocks)]

//! The one shared Executor that Direct Execution, Cached Execution, and every Plugin Event
//! handler funnel through. No execution path reaches a Registered Function or consumes
//! Resource Budget outside this crate. For any execution that continues past its dispatching
//! call's return (every async = true Plugin handler), the spawned task is handed a live
//! budget-accounting handle, not a closed one.
//! The only crate that depends on hexput-enforce.
//!
//! [`execute`] is the entry point. Every evaluation the Daemon performs enters here —
//! `hexput-script` never calls the interpreter itself — so what this function enforces reaches
//! every execution mode at once.
//!
//! # Host calls (Story 3.1)
//!
//! [`execute`] drives a resumable [`Execution`](hexput_interpreter::Execution) a segment at a
//! time. Every segment — running the Script, and converting the values that cross the boundary —
//! runs on the blocking pool (`spawn_blocking`), never on a runtime worker. When the Script calls
//! a bare name no scope declares, the segment ends at that host call, and the Executor:
//!
//! 1. measures each argument: one nested deeper than [`ARGUMENT_DEPTH_LIMIT`] is
//!    `depth.argument_too_deep`, spanned on that argument; arguments that together are certain to
//!    exceed a frame cannot be sent, `host.function_failed` on the call. (A function or cyclic
//!    argument was already refused by the interpreter, as a `type` error on that argument.)
//! 2. asks `hexput-enforce` whether the Session registered the name (AD-3): if not, the call is a
//!    `capability` error and nothing is sent.
//! 3. hands the call to the connection through its [`Caller`] and awaits the reply — a plain
//!    `.await`, holding no thread and no lock.
//! 4. converts the reply's value back, in a new blocking segment, and resumes the Script with it.
//!    An `Error` reply or a malformed one ends the Script with `host.function_failed`, and a
//!    connection that ends first with `host.no_reply`, both spanned on the call.
//!
//! Every refusal happens before anything is sent.
//!
//! Binds: AD-3, AD-6.

pub mod wire;

use std::sync::Arc;

use hexput_enforce::Capabilities;
use hexput_interpreter::{Category, Code, Execution, HostCall, Outcome};
use hexput_rpc::CallFailure;

pub use hexput_interpreter::{Diagnostic, Program, Value};
/// The handle a connection gives an execution for its host calls. Re-exported so
/// `hexput-script`, which takes one and passes it on, can name it; the connection, which creates
/// it, depends on `hexput-rpc` itself.
pub use hexput_rpc::Caller;

/// How many arrays or objects deep a host call's argument may nest: 12. An argument nested
/// deeper is `depth.argument_too_deep`, and nothing is sent.
///
/// A documented constant today; Story 3.7 makes it the default of a Backend-configurable limit.
pub const ARGUMENT_DEPTH_LIMIT: usize = 12;

/// What an execution may reach outside itself: which host functions it may call, and the
/// connection that carries the calls.
#[derive(Debug, Default)]
pub struct Host {
    capabilities: Capabilities,
    caller: Option<Caller>,
}

impl Host {
    /// No host at all: every host call is a `capability` error.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A Session's host: its Registered Functions, by name, reached through `caller`.
    #[must_use]
    pub fn new<N: Into<String>>(
        registrations: impl IntoIterator<Item = N>,
        caller: Caller,
    ) -> Self {
        Self {
            capabilities: Capabilities::registered(registrations),
            caller: Some(caller),
        }
    }
}

/// Where one segment of an execution left it.
enum Step {
    /// The Script ended with this result.
    Finished(Value),
    /// The Script waits on this host call, whose arguments are ready to send.
    Call(HostCall, Vec<hexput_rpc::Value>),
}

/// Run a parsed Script whose root scope binds `variables` — its starting variables — reaching the
/// host through `host`, and return its result.
///
/// The one Executor entry point (AD-3). The caller has already validated each name as a §2
/// identifier, rejected a repeated name, and supplied only finite numbers — the obligations
/// [`hexput_interpreter::evaluate_with_variables`] documents and cannot check itself.
///
/// Must run inside a Tokio runtime: every segment of the Script runs on its blocking pool.
///
/// # Errors
///
/// The Script's first failure as a [`Diagnostic`]: a starting variable the Script's own top level
/// also declares (`syntax.duplicate_declaration`), any runtime failure with its §7 code, or a host
/// call that is refused or fails (see the crate documentation).
///
/// # Panics
///
/// If a segment panics, the panic continues here.
pub async fn execute(
    program: Arc<Program>,
    variables: Vec<(Arc<str>, Value)>,
    host: Host,
) -> Result<Value, Diagnostic> {
    let Host {
        capabilities,
        caller,
    } = host;
    let capabilities = Arc::new(capabilities);
    let mut step = {
        let capabilities = Arc::clone(&capabilities);
        blocking(move || {
            let execution = Execution::with_variables(program, variables)?;
            advance(execution.run(), &capabilities)
        })
        .await?
    };
    loop {
        let (call, arguments) = match step {
            Step::Finished(result) => return Ok(result),
            Step::Call(call, arguments) => (call, arguments),
        };
        let reply = match &caller {
            Some(caller) => caller.call(call.name(), arguments).await,
            // Unreachable in practice: without a caller nothing is registered, so the capability
            // check refused the call already.
            None => Err(CallFailure::NoReply),
        };
        let capabilities = Arc::clone(&capabilities);
        step = blocking(move || {
            let value = received(&call, reply)?;
            advance(call.resume(&value).run(), &capabilities)
        })
        .await?;
    }
}

/// Run `work` on the blocking pool and wait for it.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done,
        Err(error) => match error.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            // Only a runtime shutting down cancels a blocking task, and it is dropping this
            // future too.
            Err(error) => panic!("a Script segment was cancelled: {error}"),
        },
    }
}

/// Where a segment stopped: the result, or a host call that may go ahead, with its arguments
/// ready to send.
fn advance(
    run: Result<Outcome, Diagnostic>,
    capabilities: &Capabilities,
) -> Result<Step, Diagnostic> {
    let call = match run? {
        Outcome::Finished(result) => return Ok(Step::Finished(result)),
        Outcome::HostCall(call) => call,
    };
    let arguments = arguments(&call)?;
    capabilities.check_call(call.name(), call.span())?;
    Ok(Step::Call(call, arguments))
}

/// A host call's arguments as wire values, each measured first.
fn arguments(call: &HostCall) -> Result<Vec<hexput_rpc::Value>, Diagnostic> {
    // One frame carries every argument; the envelope and payload maps' own bytes are small
    // enough that a lower bound over the arguments alone is the useful check.
    let mut budget = hexput_rpc::MAX_FRAME_LEN;
    for argument in call.arguments() {
        match wire::measure(&argument.value, ARGUMENT_DEPTH_LIMIT, &mut budget) {
            Ok(()) => {}
            Err(wire::Unsendable::TooDeep) => {
                return Err(Diagnostic::new(
                    Category::Depth,
                    Code::ARGUMENT_TOO_DEEP,
                    format!(
                        "cannot pass this {} to `{}`: it nests more than {ARGUMENT_DEPTH_LIMIT} \
                         arrays or objects deep",
                        argument.value.type_name(),
                        call.name()
                    ),
                    argument.span,
                ));
            }
            Err(wire::Unsendable::TooLarge | wire::Unsendable::Unrepresentable) => {
                return Err(host_error(
                    Code::FUNCTION_FAILED,
                    format!(
                        "the call to `{}` could not be sent: its arguments encode to more than \
                         the maximum frame of {} bytes",
                        call.name(),
                        hexput_rpc::MAX_FRAME_LEN
                    ),
                    call,
                ));
            }
        }
    }
    Ok(call
        .arguments()
        .iter()
        .map(|argument| wire::to_wire(&argument.value))
        .collect())
}

/// The value a host call resumes the Script with, or the `host` error that ends it.
fn received(
    call: &HostCall,
    reply: Result<hexput_rpc::Value, CallFailure>,
) -> Result<Value, Diagnostic> {
    let name = call.name();
    let (code, message) = match reply {
        Ok(value) => match wire::to_hexput(&value, &mut wire::Path::reply()) {
            Ok(value) => return Ok(value),
            Err(reason) => (
                Code::FUNCTION_FAILED,
                format!("the Backend's reply to `{name}` is malformed: {reason}"),
            ),
        },
        Err(CallFailure::Failed(Some(message))) => (
            Code::FUNCTION_FAILED,
            format!("`{name}` failed on the Backend: {message}"),
        ),
        Err(CallFailure::Failed(None)) => (
            Code::FUNCTION_FAILED,
            format!("`{name}` failed on the Backend"),
        ),
        Err(CallFailure::Malformed(reason)) => (
            Code::FUNCTION_FAILED,
            format!("the Backend's reply to `{name}` is malformed: {reason}"),
        ),
        Err(CallFailure::Unsendable(reason)) => (
            Code::FUNCTION_FAILED,
            format!("the call to `{name}` could not be sent: {reason}"),
        ),
        Err(CallFailure::NoReply) => (
            Code::NO_REPLY,
            format!("the connection ended before the Backend answered the call to `{name}`"),
        ),
    };
    Err(host_error(code, message, call))
}

/// A `host` error, spanned on the call.
fn host_error(code: Code, message: String, call: &HostCall) -> Diagnostic {
    Diagnostic::new(Category::Host, code, message, call.span())
}
