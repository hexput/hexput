//! Capability checks and Resource Budget enforcement — the single implementation, reachable
//! only via hexput-exec. Depends on hexput-shared only; hexput-exec is its sole dependent,
//! which is what makes a second path into capability/budget enforcement a compile error rather
//! than a review finding.
//!
//! # What exists today (Stories 3.1 and 3.2)
//!
//! [`Capabilities`] is what an execution may call: its Session's Registered Functions with their
//! grants, as they were when the execution was dispatched. [`Capabilities::check_call`] is the one
//! decision on whether a host call may go ahead:
//!
//! * a name registered with a blanket grant proceeds, with no authorization round trip;
//! * a name registered without one is refused — fail closed until Story 3.3 turns "no grant" into
//!   "ask the Backend's per-call handler";
//! * an unregistered name is refused.
//!
//! Every refusal is the same `capability.unknown_function` error — same code, same message, same
//! span — never `reference`, since the name is a host call's, not a variable's. A Script can
//! therefore never tell an unregistered function from one it may not call. Only the
//! [`Refusal::reason`] tells them apart, for the Daemon's own log.
//!
//! Binds: AD-3.

use std::collections::HashMap;

use hexput_shared::diagnostics::{Category, Code, Diagnostic, Span};

/// The host functions one execution may call.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// Registered name -> whether it holds a blanket grant.
    registered: HashMap<String, bool>,
}

/// Why a host call was refused. For the Daemon's log only: the Script sees the same error for
/// every reason. Non-exhaustive: Story 3.3's per-call handler adds reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// No Registered Function of the Session has the name.
    Unregistered,
    /// The name is registered, but holds no grant that lets this call go ahead. The interim
    /// fail-closed reason: until Story 3.3's per-call handler, a function registered without a
    /// blanket grant is never callable.
    NotGranted,
}

impl Reason {
    /// The reason as a log field value: `unregistered` or `not_granted`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::NotGranted => "not_granted",
        }
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
    /// fail-closed: it holds the blanket grant only if every listing grants it.
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
    /// # Errors
    /// A [`Refusal`] carrying `capability.unknown_function`, spanned on the call, when `name` is
    /// not a Registered Function of this execution's Session or holds no grant. The error is the
    /// same for every refusal; only [`Refusal::reason`] differs.
    pub fn check_call(&self, name: &str, span: Span) -> Result<(), Refusal> {
        let reason = match self.registered.get(name) {
            Some(true) => return Ok(()),
            Some(false) => Reason::NotGranted,
            None => Reason::Unregistered,
        };
        Err(Refusal {
            reason,
            diagnostic: Diagnostic::new(
                Category::Capability,
                Code::UNKNOWN_FUNCTION,
                format!("`{name}` is not declared, and it is not a function this Script may call"),
                span,
            ),
        })
    }
}
