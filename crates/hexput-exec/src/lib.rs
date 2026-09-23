#![forbid(clippy::undocumented_unsafe_blocks)]

//! The one shared Executor that Direct Execution, Cached Execution, and every Plugin Event
//! handler funnel through. No execution path reaches a Registered Function or consumes
//! Resource Budget outside this crate. For any execution that continues past its dispatching
//! call's return (every async = true Plugin handler), the spawned task is handed a live
//! budget-accounting handle, not a closed one.
//! The only crate that depends on hexput-enforce.
//!
//! [`execute`] is the entry point. Today it is a thin pass-through to the interpreter: no
//! Capability, Resource Budget or Registered Function exists until Epic 3, which wraps this same
//! function with enforcement rather than adding a second one. Every evaluation the Daemon
//! performs enters here — `hexput-script` never calls the interpreter's `evaluate*` itself —
//! so that wrapping reaches every execution mode at once.
//!
//! Binds: AD-3, AD-6.

use std::sync::Arc;

pub use hexput_interpreter::{Diagnostic, Program, Value};

/// Run a parsed Script whose root scope binds `variables` — its starting variables — and return
/// its result.
///
/// The one Executor entry point (AD-3). The caller has already validated each name as a §2
/// identifier, rejected a repeated name, and supplied only finite numbers — the obligations
/// [`hexput_interpreter::evaluate_with_variables`] documents and cannot check itself.
///
/// # Errors
///
/// The Script's first failure as a [`Diagnostic`]: a starting variable the Script's own top level
/// also declares (`syntax.duplicate_declaration`), or any runtime failure with its §7 code.
pub fn execute(program: &Program, variables: Vec<(Arc<str>, Value)>) -> Result<Value, Diagnostic> {
    hexput_interpreter::evaluate_with_variables(program, variables)
}
