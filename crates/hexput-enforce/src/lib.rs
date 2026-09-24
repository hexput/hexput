//! Capability checks and Resource Budget enforcement — the single implementation, reachable
//! only via hexput-exec. Depends on hexput-shared only; hexput-exec is its sole dependent,
//! which is what makes a second path into capability/budget enforcement a compile error rather
//! than a review finding.
//!
//! # What exists today (Stories 3.1–3.3)
//!
//! [`Capabilities`] is what an execution may call: its Session's Registered Functions with their
//! grants, as they were when the execution was dispatched. [`Capabilities::check_call`] is the one
//! decision on whether a host call may go ahead, and it has three answers:
//!
//! * a name registered with a blanket grant is [`Decision::Allowed`] — it proceeds with no
//!   authorization round trip (Story 3.2);
//! * a name registered without one is [`Decision::AskHandler`] — the Backend's per-call handler
//!   decides this one call (Story 3.3). The returned [`Question`] is the only way to turn the
//!   handler's [`HandlerAnswer`] into a decision: [`Question::decide`] allows the call on an
//!   explicit `true` alone, and refuses it on anything else;
//! * an unregistered name is refused.
//!
//! Every refusal is the same `capability.unknown_function` error — same code, same message, same
//! span — never `reference`, since the name is a host call's, not a variable's. A Script can
//! therefore never tell an unregistered function from one its handler refused, or one whose
//! handler failed to answer. Only the [`Refusal::reason`] tells them apart, for the Daemon's own
//! log.
//!
//! This crate asks nothing itself: `hexput-exec` puts the question to the Backend and reports what
//! came back. Nothing here caches an answer — every call of a function granted per call is a new
//! [`Decision::AskHandler`].
//!
//! # Resource Budget (Story 3.5)
//!
//! [`Budget`] is one execution's Resource Budget: its [`Limits`] and what it has used so far.
//! Two of the six [`Dimension`]s are enforced today:
//!
//! * **CPU time** — [`Budget::charge_cpu`] adds each measured slice of Script code and refuses
//!   the one that takes the total past [`Limits::cpu_time`]. What is charged is the monotonic
//!   time the Executor measured around running Script code on its thread — never a wait on the
//!   Backend. It is wall time on that thread, not an OS thread-CPU counter, so a host with more
//!   runnable threads than cores inflates it.
//! * **memory** — the interpreter stops an execution whose values hold more than
//!   [`Budget::memory_ceiling`], by its own approximate count; [`Budget::memory_exceeded`] is
//!   the error that ends it.
//!
//! Either is a `budget` error naming its dimension (`budget.cpu_time_exceeded`,
//! `budget.memory_exceeded`), spanned on the construct that was running. The limits are the
//! documented defaults [`DEFAULT_CPU_TIME`] and [`DEFAULT_MEMORY`] until Story 3.7 makes them
//! Config values.
//!
//! Binds: AD-3.

use std::collections::HashMap;
use std::time::Duration;

pub use hexput_shared::budget::Dimension;
use hexput_shared::diagnostics::{Category, Code, Diagnostic, Span};

/// The CPU time an execution may spend running Script code by default: 1 second.
pub const DEFAULT_CPU_TIME: Duration = Duration::from_secs(1);

/// The memory an execution's values may hold by default: 64 MiB.
pub const DEFAULT_MEMORY: usize = 64 * 1024 * 1024;

/// The limits of one execution's Resource Budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    cpu_time: Duration,
    memory: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu_time: DEFAULT_CPU_TIME,
            memory: DEFAULT_MEMORY,
        }
    }
}

impl Limits {
    /// The CPU time limit.
    #[must_use]
    pub const fn cpu_time(&self) -> Duration {
        self.cpu_time
    }

    /// The memory limit, in bytes.
    #[must_use]
    pub const fn memory(&self) -> usize {
        self.memory
    }
}

/// One execution's Resource Budget: its limits, and what it has used of them. Created once per
/// execution and kept for all of it, across every host call.
#[derive(Debug, Clone, Default)]
pub struct Budget {
    limits: Limits,
    cpu_used: Duration,
}

/// A Resource Budget dimension the execution exceeded, and the error that ends it.
#[derive(Debug, Clone, PartialEq)]
pub struct Exceeded {
    dimension: Dimension,
    diagnostic: Diagnostic,
}

impl Exceeded {
    /// The dimension exceeded.
    #[must_use]
    pub fn dimension(&self) -> Dimension {
        self.dimension
    }

    /// The error the Script sees.
    #[must_use]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// The error the Script sees, by value.
    #[must_use]
    pub fn into_diagnostic(self) -> Diagnostic {
        self.diagnostic
    }
}

impl Budget {
    /// A budget with the default [`Limits`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// This budget's limits.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The CPU time charged so far.
    #[must_use]
    pub fn cpu_used(&self) -> Duration {
        self.cpu_used
    }

    /// Charge `elapsed` of running Script code; `span` is the construct that was running.
    ///
    /// # Errors
    /// `budget.cpu_time_exceeded`, spanned on `span`, when the total charged passes
    /// [`Limits::cpu_time`]. The execution must end there.
    pub fn charge_cpu(&mut self, elapsed: Duration, span: Span) -> Result<(), Exceeded> {
        self.cpu_used = self.cpu_used.saturating_add(elapsed);
        if self.cpu_used <= self.limits.cpu_time {
            return Ok(());
        }
        Err(Exceeded {
            dimension: Dimension::CpuTime,
            diagnostic: Diagnostic::new(
                Category::Budget,
                Code::CPU_TIME_EXCEEDED,
                format!(
                    "the Script ran for longer than its CPU time budget of {} ms",
                    self.limits.cpu_time.as_millis()
                ),
                span,
            ),
        })
    }

    /// How many bytes the execution's values may hold: the ceiling the interpreter is handed.
    #[must_use]
    pub fn memory_ceiling(&self) -> usize {
        self.limits.memory
    }

    /// The execution's values came to hold more than [`Budget::memory_ceiling`], at `span`: the
    /// error that ends it, `budget.memory_exceeded`.
    #[must_use]
    pub fn memory_exceeded(&self, span: Span) -> Exceeded {
        Exceeded {
            dimension: Dimension::Memory,
            diagnostic: Diagnostic::new(
                Category::Budget,
                Code::MEMORY_EXCEEDED,
                format!(
                    "the Script's values need more than its memory budget of {} bytes",
                    self.limits.memory
                ),
                span,
            ),
        }
    }
}

/// The host functions one execution may call.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// Registered name -> whether it holds a blanket grant.
    registered: HashMap<String, bool>,
}

/// Why a host call was refused. For the Daemon's log only: the Script sees the same error for
/// every reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// No Registered Function of the Session has the name.
    Unregistered,
    /// The Backend's per-call handler answered `false`.
    Refused,
    /// The Backend's per-call handler answered something other than a boolean.
    HandlerInvalid,
    /// The Backend's per-call handler answered with an error.
    HandlerFailed,
    /// The Backend's per-call handler did not answer in time.
    HandlerTimeout,
    /// The connection ended before the Backend's per-call handler answered.
    HandlerNoReply,
}

impl Reason {
    /// The reason as a log field value: `unregistered`, `refused`, `handler_invalid`,
    /// `handler_failed`, `handler_timeout` or `handler_no_reply`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::Refused => "refused",
            Self::HandlerInvalid => "handler_invalid",
            Self::HandlerFailed => "handler_failed",
            Self::HandlerTimeout => "handler_timeout",
            Self::HandlerNoReply => "handler_no_reply",
        }
    }
}

/// Whether a host call that was not refused outright may go ahead.
#[derive(Debug)]
#[must_use]
pub enum Decision {
    /// The call may go ahead at once: its function holds a blanket grant.
    Allowed,
    /// The Backend's per-call handler decides: ask it, then hand its answer to
    /// [`Question::decide`].
    AskHandler(Question),
}

/// A host call waiting for the Backend's per-call handler. Obtained only from
/// [`Capabilities::check_call`], and consumed by [`Question::decide`]: one question, one decision.
/// Neither `Clone` nor comparable, so one question can never be decided twice.
#[derive(Debug)]
#[must_use]
pub struct Question {
    name: String,
    span: Span,
}

/// What came back when the Backend's per-call handler was asked about one call, as the Executor
/// observed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerAnswer {
    /// The handler answered with this boolean.
    Boolean(bool),
    /// The handler answered, but not with a boolean (or not with a well-formed answer at all).
    NotBoolean,
    /// The handler answered with an error, or the question could not be asked.
    Failed,
    /// No answer arrived in time.
    TimedOut,
    /// The connection ended before an answer arrived.
    NoReply,
}

impl Question {
    /// The function the question is about.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Decide the call from the handler's `answer`: an explicit `true` alone lets it go ahead.
    ///
    /// # Errors
    /// A [`Refusal`] for every other answer, carrying the same `capability.unknown_function` an
    /// unregistered name gets; only its [`Refusal::reason`] says what the handler did.
    pub fn decide(self, answer: HandlerAnswer) -> Result<(), Refusal> {
        let reason = match answer {
            HandlerAnswer::Boolean(true) => return Ok(()),
            HandlerAnswer::Boolean(false) => Reason::Refused,
            HandlerAnswer::NotBoolean => Reason::HandlerInvalid,
            HandlerAnswer::Failed => Reason::HandlerFailed,
            HandlerAnswer::TimedOut => Reason::HandlerTimeout,
            HandlerAnswer::NoReply => Reason::HandlerNoReply,
        };
        Err(refusal(reason, &self.name, self.span))
    }
}

/// A refused host call: the error the Script sees, and why, which it never does.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    reason: Reason,
    diagnostic: Diagnostic,
}

impl Refusal {
    /// Why the call was refused — for logging, never for the Script.
    #[must_use]
    pub fn reason(&self) -> Reason {
        self.reason
    }

    /// The error the Script sees: identical whatever the reason.
    #[must_use]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// The error the Script sees, by value.
    #[must_use]
    pub fn into_diagnostic(self) -> Diagnostic {
        self.diagnostic
    }
}

impl Capabilities {
    /// Nothing is callable: an execution with no host, or a Session that registered nothing.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The Registered Functions of a Session, as `(name, blanket)` pairs: each name, and whether
    /// the Backend granted it blanket at registration. A name listed more than once folds
    /// fail-closed: it holds the blanket grant only if every listing grants it, and is otherwise
    /// decided per call.
    #[must_use]
    pub fn registered<N: Into<String>>(registrations: impl IntoIterator<Item = (N, bool)>) -> Self {
        let mut registered = HashMap::new();
        for (name, blanket) in registrations {
            registered
                .entry(name.into())
                .and_modify(|granted: &mut bool| *granted &= blanket)
                .or_insert(blanket);
        }
        Self { registered }
    }

    /// Decide whether the Script may call the host function `name`; `span` is the call's.
    ///
    /// [`Decision::Allowed`] for a blanket-granted name; [`Decision::AskHandler`] for a name
    /// registered without a blanket grant, asked anew on every call.
    ///
    /// # Errors
    /// A [`Refusal`] carrying `capability.unknown_function`, spanned on the call, when `name` is
    /// not a Registered Function of this execution's Session. It is the same error every refusal
    /// carries, [`Question::decide`]'s included; only [`Refusal::reason`] differs.
    pub fn check_call(&self, name: &str, span: Span) -> Result<Decision, Refusal> {
        match self.registered.get(name) {
            Some(true) => Ok(Decision::Allowed),
            Some(false) => Ok(Decision::AskHandler(Question {
                name: name.to_owned(),
                span,
            })),
            None => Err(refusal(Reason::Unregistered, name, span)),
        }
    }
}

/// A refusal of a call to `name` for `reason`: the one error every refusal carries.
fn refusal(reason: Reason, name: &str, span: Span) -> Refusal {
    Refusal {
        reason,
        diagnostic: Diagnostic::new(
            Category::Capability,
            Code::UNKNOWN_FUNCTION,
            format!("`{name}` is not declared, and it is not a function this Script may call"),
            span,
        ),
    }
}
