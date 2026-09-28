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
//! 1. measures each argument: one nested deeper than the argument depth limit
//!    ([`Limits::argument_depth`], by default [`ARGUMENT_DEPTH_LIMIT`]) is
//!    `depth.argument_too_deep`, spanned on that argument; arguments that together are certain to
//!    exceed a frame cannot be sent, `host.function_failed` on the call. (A function or cyclic
//!    argument was already refused by the interpreter, as a `type` error on that argument.)
//! 2. asks `hexput-enforce` whether the call may go ahead (AD-3), from the Session's registrations
//!    and their grants as they were when the execution was dispatched. A name registered with a
//!    blanket grant proceeds at once (Story 3.2). A name registered without one is decided by the
//!    Backend's per-call handler (Story 3.3): the Executor asks it through the [`Caller`] — an
//!    `Authorize` question carrying the call's name and arguments, before any `Call` — and waits
//!    at most [`Limits::authorization_timeout`] (by default [`AUTHORIZATION_TIMEOUT`]), holding no
//!    thread and no lock. `hexput-enforce` turns
//!    what came back into the decision: only an explicit `true` lets the call proceed. Nothing is
//!    cached; every call asks anew. An unregistered name, a refusal (`false`), an answer that is
//!    not a boolean, an `Error`, no answer in time, or a connection that ends first are all the
//!    same `capability.unknown_function` on the call, and nothing more is sent. The Daemon logs
//!    each refusal at `debug` with the `function` and a `reason` — `unregistered`, `refused`,
//!    `handler_invalid`, `handler_failed`, `handler_timeout` or `handler_no_reply` — which the
//!    Script never sees. A refusal ends the Script like every error: nothing can catch it.
//! 3. hands the call to the connection through its [`Caller`] and awaits the reply — a plain
//!    `.await`, holding no thread and no lock. [`Caller::dispatch_authorized`] and
//!    [`Caller::ask_authorization`] are called from here and nowhere else;
//!    `scripts/check-crate-graph.py` fails CI when either name appears in any other production
//!    crate but `hexput-rpc`, which defines them.
//! 4. converts the reply's value back, in a new blocking segment, and resumes the Script with it.
//!    An `Error` reply or a malformed one ends the Script with `host.function_failed`, and a
//!    connection that ends first with `host.no_reply`, both spanned on the call.
//!
//! Every refusal happens before anything is sent.
//!
//! # Resource Budget (Story 3.5)
//!
//! Every execution gets one `hexput-enforce` [`Budget`](hexput_enforce::Budget), kept for its
//! whole run across every host call. The Executor runs the Script metered: in slices of
//! [`SLICE`] units of work, and with the budget's memory ceiling. Everything it does for the
//! Script on its blocking thread is timed with a monotonic clock and charged to the budget:
//! binding the starting variables, each slice, measuring and converting a host call's arguments,
//! and converting and binding its reply. A slice is charged when it pauses, when it stops at a
//! host call — before anything about the call is sent — and when it finishes the Script, which
//! fails too if that last charge crosses the limit, so the limit is a hard bound. The time spent
//! waiting for a reply or a per-call handler's answer is never measured, let alone charged.
//! What is measured is wall time on that thread, so an oversubscribed host inflates it. A charge
//! that takes the execution past its CPU time ends it with
//! `budget.cpu_time_exceeded`; values that come to hold more than the ceiling end it with
//! `budget.memory_exceeded`. Both are spanned on the construct that was running, and both free
//! the blocking thread at once — the execution's heap is dropped with it. Host calls it already
//! made stand. The Daemon logs each at `debug` with the `dimension`.
//!
//! # The four counted dimensions (Story 3.6)
//!
//! The same budget bounds four counts, each on its own and each decided by `hexput-enforce`:
//!
//! * **allocations** — the Script runs with the budget's allocation ceiling in its meter; the
//!   interpreter counts and stops, and the Executor turns the stop into
//!   `budget.allocations_exceeded`, spanned on the constructing site.
//! * **RPC calls** and **side effects** — every time the Script stops at a host call whose
//!   arguments can be sent, after the slice is charged and the arguments are measured and
//!   converted, and before the capability is decided or any `Authorize` question asked, the call
//!   is charged as one of each. A call that would cross either limit is refused before anything
//!   is sent (`budget.rpc_calls_exceeded` or `budget.side_effects_exceeded`, spanned on the call);
//!   a call later refused, denied or failed has already counted. A call whose arguments cannot be
//!   sent — a function or cyclic value, one nested too deep, or too large for a frame — ends the
//!   Script with that argument's error and is not counted.
//! * **output size** — when the Script finishes, the exact MessagePack length of its `{value}`
//!   result payload ([`wire::payload_size`]) is charged, and one past the limit is
//!   `budget.output_size_exceeded`, spanned on the whole Script.
//!
//! # Tunable limits (Story 3.7)
//!
//! [`execute_with_limits`] runs under the [`Limits`] its caller computed from the Session's Config
//! and the execution's overrides ([`Limits::from_settings`]); [`execute`] under the defaults.
//! Besides the six budget limits, the limits carry the argument depth limit and the per-call
//! handler's timeout, and the Executor applies both from there — an error naming the depth limit
//! names the one in force.
//!
//! # Value Secrets (Story 3.11)
//!
//! Values cross the boundary with their Value Secrets ([`wire`] has the holder shape). A
//! starting variable or a host call's value arrives [`Held`]; a reply holding a malformed holder
//! is a malformed reply, `host.function_failed`. Every value a `Call` or `Authorize` sends is a
//! holder: the interpreter gives each value without a secret a Reference ID, `hx:<nonce>:<n>` —
//! the nonce 16 lowercase hex digits drawn once per execution from `std`'s `RandomState`, the
//! counter from 1, so each is unique within the execution and collides with another execution's
//! only with negligible probability — and a Backend's own IDs travel as given. The argument depth limit counts the
//! Script's nesting, not the holders; an argument whose holders take it past what a frame may
//! nest ([`wire::MAX_ARGUMENT_WIRE_DEPTH`]) cannot be sent, `host.function_failed`. The result
//! carries only the secrets the Backend supplied — a generated one never leaves in it — and the
//! output size budget counts its holders.
//!
//! # Registered Methods (Story 3.12)
//!
//! A [`Registration`] may name an object key, which makes it a Registered Method under that key.
//! The Executor hands the interpreter the method names per key — data only — so `value.name(args)`
//! on a keyed value stops at a host call carrying the receiver and its key
//! ([`HostCall::receiver`], [`HostCall::key`]). From there a method call is a function call in
//! every respect: the receiver is measured with the arguments (it counts toward the argument
//! depth limit, spanned on the receiver expression), the call is charged as one RPC call and one
//! side effect, `hexput-enforce` decides it by `(key, name)` — blanket, per-call `Authorize`, or
//! refused as the same `capability.unknown_function` — and the `Call` and `Authorize` payloads
//! carry `receiver`, a holder with the receiver's secret unchanged. A refusal is logged with the
//! method's `key` beside `function` and `reason`.
//!
//! # Modifications (Story 3.13)
//!
//! Changes to referenced values flow both ways by Reference ID, as whole values. A `Call` reply
//! may carry `modifications` — an array of `{ref, value}` maps ([`wire::modifications_to_hexput`])
//! — which are validated whole, then applied in order before the Script resumes
//! ([`HostCall::resume_with`]); anything malformed, or a modification giving an array or object a
//! value of another kind, fails the call with `host.function_failed`, spanned on it. Decoding and
//! applying them is charged to CPU time like the reply's value, and their values to memory; they
//! count no allocations. An `Authorize` answer carrying `modifications` is not a boolean answer
//! (`reason = handler_invalid`). The other way, a finished Script's [`Finished::modifications`]
//! list every referenced place and collection it wrote, once each with its final value, and the
//! output size budget charges the whole `{value, modifications}` payload
//! ([`wire::result_payload_size`]). A failed execution reports none.
//!
//! Binds: AD-3, AD-6.

pub mod wire;

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hexput_enforce::{Budget, Capabilities, Decision, Exceeded, HandlerAnswer, Question, Refusal};

/// The limits of an execution's Resource Budget, re-exported so a caller of
/// [`execute_with_limits`] can state them. Only `hexput-enforce` decides anything from them.
pub use hexput_enforce::Limits;
use hexput_interpreter::{Category, Code, Execution, HostCall, Meter, Outcome, Span};
use hexput_rpc::{CallFailure, Reply};

pub use hexput_interpreter::{Diagnostic, Finished, Held, Modification, Program, Secret, Value};
/// The handle a connection gives an execution for its host calls. Re-exported so
/// `hexput-script`, which takes one and passes it on, can name it; the connection, which creates
/// it, depends on `hexput-rpc` itself.
pub use hexput_rpc::Caller;

/// How many arrays or objects deep a host call's argument may nest by default: 12. An argument
/// nested deeper than the limit in force ([`Limits::argument_depth`]) is
/// `depth.argument_too_deep`, and nothing is sent.
///
/// The default of the `argument_depth` setting (Story 3.7), which Config or an override may set
/// from 1 to 64; an alias of `hexput-enforce`'s `DEFAULT_ARGUMENT_DEPTH`.
pub const ARGUMENT_DEPTH_LIMIT: usize = hexput_enforce::DEFAULT_ARGUMENT_DEPTH;

/// How long the Executor waits for the Backend's per-call handler to answer an `Authorize`
/// question by default: 5 seconds. No answer within the timeout in force
/// ([`Limits::authorization_timeout`]) denies the call (`reason = handler_timeout`), and an answer
/// arriving later is dropped.
///
/// The default of the `authorization_timeout_ms` setting (Story 3.7), which Config or an override
/// may set from 1 ms to 60 s; an alias of `hexput-enforce`'s `DEFAULT_AUTHORIZATION_TIMEOUT`.
pub const AUTHORIZATION_TIMEOUT: Duration = hexput_enforce::DEFAULT_AUTHORIZATION_TIMEOUT;

/// How much work a Script does between two CPU time charges: 10 000 units — one per evaluation
/// step, plus one per 256 bytes a string operation reads or writes. A slice takes around a
/// millisecond on a release build, so a runaway Script is stopped within a small fraction of its
/// CPU time past the limit.
pub const SLICE: NonZeroU64 = NonZeroU64::new(10_000).expect("non-zero");

/// One of a Session's registrations, as the Executor takes it: a Registered Function (no `key`)
/// or a Registered Method under an object key (Story 3.12), and whether the Backend granted it
/// blanket at registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The name a Script calls it by.
    pub name: String,
    /// The object key a method is bound under; `None` for a function.
    pub key: Option<String>,
    /// Whether it holds a blanket grant.
    pub blanket: bool,
}

impl Registration {
    /// A Registered Function.
    #[must_use]
    pub fn function(name: impl Into<String>, blanket: bool) -> Self {
        Self {
            name: name.into(),
            key: None,
            blanket,
        }
    }

    /// A Registered Method `name` under the object key `key`.
    #[must_use]
    pub fn method(key: impl Into<String>, name: impl Into<String>, blanket: bool) -> Self {
        Self {
            name: name.into(),
            key: Some(key.into()),
            blanket,
        }
    }
}

/// A `(name, blanket)` pair is a Registered Function.
impl<N: Into<String>> From<(N, bool)> for Registration {
    fn from((name, blanket): (N, bool)) -> Self {
        Self::function(name, blanket)
    }
}

/// What an execution may reach outside itself: which host functions and methods it may call,
/// and the connection that carries the calls.
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

    /// A Session's host: its registrations — Registered Functions, which a `(name, blanket)`
    /// pair converts into, and Registered Methods ([`Registration::method`]) — each with whether
    /// the Backend granted it blanket at registration, reached through `caller`.
    #[must_use]
    pub fn new<R: Into<Registration>>(
        registrations: impl IntoIterator<Item = R>,
        caller: Caller,
    ) -> Self {
        Self {
            capabilities: Capabilities::none().with_methods(registrations.into_iter().map(|r| {
                let Registration { name, key, blanket } = r.into();
                (key, name, blanket)
            })),
            caller: Some(caller),
        }
    }
}

/// Why a segment ended the execution.
enum Halt {
    /// The Script failed.
    Failed(Diagnostic),
    /// The Script exceeded a Resource Budget dimension.
    Exceeded(Exceeded),
}

impl From<Diagnostic> for Halt {
    fn from(diagnostic: Diagnostic) -> Self {
        Self::Failed(diagnostic)
    }
}

impl Halt {
    /// The Script's error. A budget error, and a construct the feature toggles disable (Story
    /// 3.9), are logged here, in the execution's own task, so the event carries the request's span.
    fn into_diagnostic(self) -> Diagnostic {
        match self {
            Self::Failed(diagnostic) => {
                if diagnostic.code == Code::CONSTRUCT_DISABLED {
                    tracing::debug!(
                        code = diagnostic.code.as_str(),
                        "refused a disabled construct"
                    );
                }
                diagnostic
            }
            Self::Exceeded(exceeded) => {
                tracing::debug!(
                    dimension = exceeded.dimension().as_str(),
                    "stopped an execution over its budget"
                );
                exceeded.into_diagnostic()
            }
        }
    }
}

/// Where one segment of an execution left it.
enum Step {
    /// The Script ended with this result and these modifications.
    Finished(Finished),
    /// The Script waits on this host call, whose arguments (and receiver) are ready to send; when
    /// the call is granted per call, the question for the Backend's handler comes first.
    Call(Box<HostCall>, Sent, Option<Question>),
    /// The Script made a host call `hexput-enforce` refused: the function's or method's name, a
    /// method's key, and why.
    Refused(String, Option<String>, Refusal),
}

/// What a host call sends, as wire values: its arguments, and a method call's receiver.
struct Sent {
    arguments: Vec<hexput_rpc::Value>,
    receiver: Option<hexput_rpc::Value>,
}

/// Run a parsed Script whose root scope binds `variables` — its starting variables — reaching the
/// host through `host`, and return its result.
///
/// The one Executor entry point (AD-3). The caller has already validated each name as a §2
/// identifier, rejected a repeated name, and supplied only finite numbers — the obligations
/// [`hexput_interpreter::evaluate_with_variables`] documents and cannot check itself.
///
/// Must run inside a Tokio runtime: every segment of the Script runs on its blocking pool. The
/// runtime must have timers enabled when a Script may call a function granted per call, since the
/// handler's answer is awaited under a timeout ([`Limits::authorization_timeout`]).
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
    execute_with_limits(program, variables, host, Limits::default()).await
}

/// [`execute`], under a Resource Budget with `limits` rather than the documented defaults.
///
/// Every limit is still enforced by `hexput-enforce`, exactly as under [`execute`]; only the
/// numbers differ. Direct Execution computes them from the Session's Config overlaid with the
/// execution's overrides (Story 3.7), through [`Limits::from_settings`].
///
/// # Errors
/// As [`execute`].
///
/// # Panics
/// As [`execute`].
pub async fn execute_with_limits(
    program: Arc<Program>,
    variables: Vec<(Arc<str>, Value)>,
    host: Host,
    limits: Limits,
) -> Result<Value, Diagnostic> {
    let variables = variables
        .into_iter()
        .map(|(name, value)| (name, Held::plain(value)))
        .collect();
    execute_held(program, variables, host, limits)
        .await
        .map(|finished| finished.result.value)
}

/// [`execute_with_limits`] with Value Secrets (Story 3.11): each starting variable may arrive
/// [`Held`], with the secret of the variable it lands in, and the result comes back `Held`, with
/// the secret of the place the Script returned it from when the `return` names one (an array's or
/// object's own secrets are inside the value either way) — beside the referenced places and
/// collections the Script wrote (Story 3.13). What Direct Execution calls.
///
/// # Errors
/// As [`execute`].
///
/// # Panics
/// As [`execute`].
pub async fn execute_held(
    program: Arc<Program>,
    variables: Vec<(Arc<str>, Held)>,
    host: Host,
    limits: Limits,
) -> Result<Finished, Diagnostic> {
    let Host {
        capabilities,
        caller,
    } = host;
    let capabilities = Arc::new(capabilities);
    let budget = Budget::with_limits(limits);
    let authorization_timeout = limits.authorization_timeout();
    let meter = Meter {
        slice: Some(SLICE),
        memory_ceiling: Some(budget.memory_ceiling()),
        allocation_ceiling: Some(budget.allocation_ceiling()),
    };
    // Where a charge with no construct of its own points: the whole Script.
    let whole = program.span;
    let (mut step, mut budget) = {
        let capabilities = Arc::clone(&capabilities);
        blocking(move || {
            let mut budget = budget;
            // Binding the starting variables is charged with the first slice.
            let started = Instant::now();
            // The method names only tell a method call from a property call and refuse an
            // override; every call is still decided by `check_call` below.
            let methods: Vec<(Arc<str>, Arc<str>)> = capabilities
                .methods()
                .map(|(key, name)| (Arc::from(key), Arc::from(name)))
                .collect();
            let execution = Execution::with_variables(program, variables)?
                .metered(meter)
                .with_features(limits.features())
                .with_methods(methods)
                .with_reference_prefix(reference_prefix());
            let step = segment(execution, &mut budget, &capabilities, whole, started)?;
            Ok((step, budget))
        })
        .await
        .map_err(Halt::into_diagnostic)?
    };
    loop {
        let (
            call,
            Sent {
                arguments,
                receiver,
            },
            question,
        ) = match step {
            Step::Finished(result) => return Ok(result),
            Step::Call(call, sent, question) => (call, sent, question),
            Step::Refused(function, key, refusal) => {
                return Err(refused(&function, key.as_deref(), refusal));
            }
        };
        if let Some(question) = question {
            let answer = ask(
                caller.as_ref(),
                &question,
                arguments.clone(),
                receiver.clone(),
                authorization_timeout,
            )
            .await;
            if let Err(refusal) = question.decide(answer) {
                return Err(refused(call.name(), call.key(), refusal));
            }
        }
        let reply = match &caller {
            // Authorized: `advance` returned this call only after `check_call` allowed it, and a
            // call granted per call reaches here only once its handler answered `true`.
            Some(caller) => {
                caller
                    .dispatch_authorized(call.name(), arguments, receiver)
                    .await
            }
            // Unreachable in practice: without a caller nothing is registered, so the capability
            // check refused the call already.
            None => Err(CallFailure::NoReply),
        };
        let capabilities = Arc::clone(&capabilities);
        (step, budget) = blocking(move || {
            // Converting and binding the reply is charged with the next slice, so a limit it
            // helps cross is reported before the next call is sent.
            let started = Instant::now();
            let (value, modifications) = received(&call, reply)?;
            let execution = resume(*call, &value, &modifications)?;
            let step = segment(execution, &mut budget, &capabilities, whole, started)?;
            Ok((step, budget))
        })
        .await
        .map_err(Halt::into_diagnostic)?;
    }
}

/// Run `execution` slice by slice, charging each slice's time to `budget`, until the Script ends
/// or calls the host — or a limit ends it. `started` is when the Executor began working for the
/// Script on this thread, so what it did before the first slice is charged with it. `whole` is
/// the Program's span, for the finishing charge. Runs on the blocking pool; nothing here waits.
fn segment(
    mut execution: Execution,
    budget: &mut Budget,
    capabilities: &Capabilities,
    whole: Span,
    mut started: Instant,
) -> Result<Step, Halt> {
    loop {
        match execution.run()? {
            Outcome::Paused(paused) => {
                budget
                    .charge_cpu(started.elapsed(), paused.span())
                    .map_err(Halt::Exceeded)?;
                started = Instant::now();
                execution = paused.resume();
            }
            Outcome::OutOfMemory(stopped) => {
                return Err(Halt::Exceeded(budget.memory_exceeded(stopped.span())));
            }
            Outcome::AllocationsExceeded(stopped) => {
                return Err(Halt::Exceeded(budget.allocations_exceeded(stopped.span())));
            }
            // The last slice is charged like any other: crossing the limit in it fails the
            // Script even though it finished. Measuring the result is charged with it: the whole
            // payload, its modifications included (Story 3.13).
            Outcome::Finished(finished) => {
                budget
                    .charge_cpu(started.elapsed(), whole)
                    .map_err(Halt::Exceeded)?;
                let limit = budget.limits().output_size();
                let size =
                    wire::result_payload_size(&finished.result, &finished.modifications, limit);
                budget.charge_output(size, whole).map_err(Halt::Exceeded)?;
                return Ok(Step::Finished(finished));
            }
            Outcome::HostCall(call) => {
                // The slice, then the argument work and the call itself — one RPC call and one
                // side effect, whatever becomes of it — all before anything about the call is
                // sent.
                let span = call.span();
                budget
                    .charge_cpu(started.elapsed(), span)
                    .map_err(Halt::Exceeded)?;
                let started = Instant::now();
                let step = advance(call, capabilities, budget)?;
                budget
                    .charge_cpu(started.elapsed(), span)
                    .map_err(Halt::Exceeded)?;
                return Ok(step);
            }
        }
    }
}

/// Log a refused host call and return the Script's error. Logged here, in the execution's own
/// task, so the event carries the request's span; the reason is for the Daemon's log only, never
/// the Script's error.
fn refused(function: &str, key: Option<&str>, refusal: Refusal) -> Diagnostic {
    // `key` is recorded only for a method: a function's refusal logs no such field.
    tracing::debug!(
        function,
        key,
        reason = refusal.reason().as_str(),
        "refused a host call"
    );
    refusal.into_diagnostic()
}

/// Put `question` to the Backend's per-call handler, waiting at most `timeout`, and report what
/// came back. Holds no thread and no lock while it waits.
async fn ask(
    caller: Option<&Caller>,
    question: &Question,
    arguments: Vec<hexput_rpc::Value>,
    receiver: Option<hexput_rpc::Value>,
    timeout: Duration,
) -> HandlerAnswer {
    // Unreachable in practice, like a dispatch without a caller: nothing is registered.
    let Some(caller) = caller else {
        return HandlerAnswer::NoReply;
    };
    let answer = caller.ask_authorization(question.name(), arguments, receiver);
    match tokio::time::timeout(timeout, answer).await {
        Err(_elapsed) => HandlerAnswer::TimedOut,
        // An answer is `{value}` alone: one carrying `modifications` is no answer (Story 3.13).
        Ok(Ok(Reply {
            value: hexput_rpc::Value::Boolean(allowed),
            modifications: None,
        })) => HandlerAnswer::Boolean(allowed),
        // Any other value, or a `Result` that is not a well-formed `{value}`.
        Ok(Ok(_) | Err(CallFailure::Malformed(_))) => HandlerAnswer::NotBoolean,
        // An `Error`, or a question that could not be written: the handler gave no answer.
        Ok(Err(CallFailure::Failed(_) | CallFailure::Unsendable(_))) => HandlerAnswer::Failed,
        Ok(Err(CallFailure::NoReply)) => HandlerAnswer::NoReply,
    }
}

/// This execution's Reference ID prefix: `hx:` and a nonce of 16 lowercase hex digits, drawn from
/// `std`'s randomly keyed hasher. The counter after it makes each generated ID unique within the
/// execution; two executions' IDs collide only if their 64-bit nonces do, a negligible chance —
/// not an impossibility.
fn reference_prefix() -> String {
    use std::hash::{BuildHasher, RandomState};
    static DRAWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let draw = DRAWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nonce = RandomState::new().hash_one((draw, Instant::now()));
    format!("hx:{nonce:016x}")
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

/// Where a host call leaves the execution: a call that may go ahead, with its arguments ready to
/// send, or one `hexput-enforce` refused.
///
/// A call whose arguments can be sent is charged to `budget` as one RPC call and one side effect,
/// before the capability decision, so a refused or denied call counts; a call whose arguments
/// cannot be sent never became a host call and is not charged.
fn advance(call: HostCall, capabilities: &Capabilities, budget: &mut Budget) -> Result<Step, Halt> {
    let sent = arguments(&call, budget.limits().argument_depth())?;
    budget
        .charge_rpc_call(call.span())
        .map_err(Halt::Exceeded)?;
    // A method is looked up under its receiver's key (Story 3.12), a function under none.
    match capabilities.check_call(call.key(), call.name(), call.span()) {
        Ok(Decision::Allowed) => Ok(Step::Call(Box::new(call), sent, None)),
        Ok(Decision::AskHandler(question)) => Ok(Step::Call(Box::new(call), sent, Some(question))),
        Err(refusal) => Ok(Step::Refused(
            call.name().to_owned(),
            call.key().map(str::to_owned),
            refusal,
        )),
    }
}

/// A host call's arguments — and a method call's receiver, first — as wire values, each
/// measured first against the argument depth limit `depth`.
fn arguments(call: &HostCall, depth: usize) -> Result<Sent, Diagnostic> {
    // One frame carries every argument; the envelope and payload maps' own bytes are small
    // enough that a lower bound over the arguments alone is the useful check.
    let mut budget = hexput_rpc::MAX_FRAME_LEN;
    // The receiver, when there is one, comes first, so an error on it names it alone.
    let receiver = call.receiver().map(|receiver| (true, receiver));
    let sent = receiver
        .into_iter()
        .chain(call.arguments().iter().map(|argument| (false, argument)));
    for (is_receiver, argument) in sent.clone() {
        let what = if is_receiver {
            "its receiver"
        } else {
            "an argument"
        };
        match wire::measure_held(
            &argument.value,
            argument.secret.as_ref(),
            depth,
            &mut budget,
        ) {
            Ok(()) => {}
            Err(wire::Unsendable::TooDeep) => {
                return Err(Diagnostic::new(
                    Category::Depth,
                    Code::ARGUMENT_TOO_DEEP,
                    format!(
                        "cannot pass this {} to `{}`: it nests more than {depth} \
                         arrays or objects deep",
                        argument.value.type_name(),
                        call.name()
                    ),
                    argument.span,
                ));
            }
            Err(wire::Unsendable::Unrepresentable) => {
                return Err(host_error(
                    Code::FUNCTION_FAILED,
                    format!(
                        "the call to `{}` could not be sent: {what} holds a value with no wire \
                         representation",
                        call.name()
                    ),
                    call,
                ));
            }
            Err(wire::Unsendable::TooLarge) => {
                return Err(host_error(
                    Code::FUNCTION_FAILED,
                    format!(
                        "the call to `{}` could not be sent: {} to more than the maximum frame \
                         of {} bytes",
                        call.name(),
                        if is_receiver {
                            "its receiver encodes"
                        } else {
                            "its arguments encode"
                        },
                        hexput_rpc::MAX_FRAME_LEN
                    ),
                    call,
                ));
            }
        }
    }
    let mut values = Vec::with_capacity(call.arguments().len() + 1);
    for (is_receiver, argument) in sent {
        let what = if is_receiver {
            "its receiver"
        } else {
            "an argument"
        };
        let value = wire::to_wire_held(&argument.value, argument.secret.as_ref());
        // The Script's nesting is within the limit; its holders may still take it past what a
        // frame may nest.
        if wire::wire_depth(&value) > wire::MAX_ARGUMENT_WIRE_DEPTH {
            return Err(host_error(
                Code::FUNCTION_FAILED,
                format!(
                    "the call to `{}` could not be sent: with its Value Secrets {what} nests more \
                     than {} levels deep, past what a frame may carry",
                    call.name(),
                    wire::MAX_ARGUMENT_WIRE_DEPTH
                ),
                call,
            ));
        }
        values.push(value);
    }
    let receiver = call.receiver().is_some().then(|| values.remove(0));
    Ok(Sent {
        arguments: values,
        receiver,
    })
}

/// Hand `call` its value back after applying its `modifications`, or end the Script with
/// `host.function_failed` when one gives an array or object a value of another kind (Story 3.13).
fn resume(
    call: HostCall,
    value: &Held,
    modifications: &[Modification],
) -> Result<Execution, Diagnostic> {
    let name = call.name().to_owned();
    let span = call.span();
    call.resume_with(value, modifications).map_err(|mismatch| {
        Diagnostic::new(
            Category::Host,
            Code::FUNCTION_FAILED,
            format!(
                "the Backend's reply to `{name}` is malformed: `{}[{}].value` is {} {}, but its \
                 `ref` names {} {}",
                wire::MODIFICATIONS_KEY,
                mismatch.index(),
                article(mismatch.found()),
                mismatch.found(),
                article(mismatch.expected()),
                mismatch.expected(),
            ),
            span,
        )
    })
}

/// `a` or `an`, for a type name.
fn article(type_name: &str) -> &'static str {
    if type_name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

/// The value a host call resumes the Script with and the modifications to apply first, or the
/// `host` error that ends it. Both are converted before anything is applied, so a malformed
/// modification changes nothing (Story 3.13).
fn received(
    call: &HostCall,
    reply: Result<Reply, CallFailure>,
) -> Result<(Held, Vec<Modification>), Diagnostic> {
    let name = call.name();
    let (code, message) = match reply {
        Ok(Reply {
            value,
            modifications,
        }) => {
            let converted = wire::to_hexput(&value, &mut wire::Path::reply()).and_then(|value| {
                let modifications = match &modifications {
                    Some(modifications) => wire::modifications_to_hexput(modifications)?,
                    None => Vec::new(),
                };
                Ok((value, modifications))
            });
            match converted {
                Ok(converted) => return Ok(converted),
                Err(reason) => (
                    Code::FUNCTION_FAILED,
                    format!("the Backend's reply to `{name}` is malformed: {reason}"),
                ),
            }
        }
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
