//! Capability checks and Resource Budget enforcement — the single implementation, reachable
//! only via hexput-exec. Depends on hexput-shared only; hexput-exec is its sole dependent,
//! which is what makes a second path into capability/budget enforcement a compile error rather
//! than a review finding.
//!
//! # What exists today (Story 3.1)
//!
//! [`Capabilities`] is what an execution may call: the names of its Session's Registered
//! Functions, read once per execution. [`Capabilities::check_call`] is the one decision on
//! whether a host call may go ahead. An unregistered name is a `capability` error
//! (`capability.unknown_function`) — never `reference`, since the name is a host call's, not a
//! variable's. Grants (`context.allow()`, per-call handlers — Stories 3.2 and 3.3) will refine
//! the same decision, and a denied call will raise exactly the same error as an unregistered one,
//! so a Script can never tell the two apart.
//!
//! Binds: AD-3.

use std::collections::HashSet;

use hexput_shared::diagnostics::{Category, Code, Diagnostic, Span};

/// The host functions one execution may call.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    registered: HashSet<String>,
}

impl Capabilities {
    /// Nothing is callable: an execution with no host, or a Session that registered nothing.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The Registered Functions of a Session, by name.
    #[must_use]
    pub fn registered<N: Into<String>>(names: impl IntoIterator<Item = N>) -> Self {
        Self {
            registered: names.into_iter().map(Into::into).collect(),
        }
    }

    /// Decide whether the Script may call the host function `name`; `span` is the call's.
    ///
    /// # Errors
    /// `capability.unknown_function`, spanned on the call, when `name` is not a Registered
    /// Function of this execution's Session. The message is the same for every refusal.
    pub fn check_call(&self, name: &str, span: Span) -> Result<(), Diagnostic> {
        if self.registered.contains(name) {
            return Ok(());
        }
        Err(Diagnostic::new(
            Category::Capability,
            Code::UNKNOWN_FUNCTION,
            format!("`{name}` is not declared, and it is not a function this Script may call"),
            span,
        ))
    }
}
