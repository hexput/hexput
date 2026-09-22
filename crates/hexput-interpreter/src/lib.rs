#![forbid(clippy::undocumented_unsafe_blocks)]

//! Tree-walking evaluator (Stories 1.6-1.7). Depends on hexput-ast only — no host reach,
//! no I/O, no execution-path capability or budget concerns of its own.
//! Binds no Architecture Decision directly; hexput-exec is what wraps it with AD-3's
//! Capability + Resource Budget enforcement.
//!
//! The evaluator covers the whole language LANGUAGE-REFERENCE defines: values, operators and
//! conversions (§3–§4), optional access (§4.4), absent-data and reference rules (§7), block
//! scoping, `let` and assignment, `if`/`else`, `while`, `for … in`, `break`/`continue`,
//! functions, closures and calls (§5–§6). Evaluation is driven by an explicit continuation
//! stack, so neither input nesting, nor iteration count, nor call depth grows the host stack.
//!
//! # Functions
//!
//! Functions are first-class values (§6): passable, returnable from a call, and storable in
//! bindings, arrays and objects. They capture their defining scope **by reference**, so a
//! callback sees later mutations of a captured binding, and each loop iteration gets a fresh
//! scope, so a closure made in iteration *i* captures that iteration's value. Named functions
//! are hoisted within their block — every `fn name` is bound before the block's statements run,
//! so mutual recursion works in any declaration order.
//!
//! A function cannot leave the execution: returning one, or a value containing one, is a `type`
//! error (`type.function_result`). A Script result must be data the Backend can receive, and a
//! function has no wire representation.
//!
//! Recursion is bounded by [`CALL_DEPTH_LIMIT`] rather than by the host stack; exceeding it is a
//! `depth` error (`depth.call_depth_exceeded`).
//!
//! # Memory
//!
//! Each call to [`evaluate`] owns one heap that holds every collection and scope the Script
//! creates; runtime values are handles into it. When evaluation ends, by result or by error,
//! the heap is dropped whole, so reference cycles (`let a = []; a[0] = a;`) cannot outlive the
//! execution. Nothing is collected while the Script runs; block scopes are the only thing
//! reclaimed early. The returned [`Value`] is detached: an owned, immutable copy of what the
//! Script returned, independent of any heap and `Send + Sync`.

mod convert;
mod environment;
mod heap;
mod machine;
mod value;

use hexput_ast::{Diagnostic, Program};

pub use value::{Array, Object, Value};

/// How many calls may be active at once before a call raises `depth.call_depth_exceeded`.
///
/// A documented constant, deliberately not a parameter: the interpreter's job is only to make
/// unbounded recursion terminate with a defined error instead of overflowing the host stack.
/// Story 3.5's Resource Budget owns a Backend-configurable limit.
pub const CALL_DEPTH_LIMIT: usize = 1024;

/// Evaluate a parsed Script and return its result: the value of the first `return` reached, or
/// `null` when execution runs off the end or hits a bare `return`.
///
/// The result is detached from the execution: a value the Script reached twice (`[x, x]`)
/// comes back as two equal copies, and no allocation the execution made outlives this call
/// except the result itself.
///
/// # Errors
/// The first runtime failure, as a [`Diagnostic`] with its §7 category, stable code, and the
/// span of the offending operator, operand, access link, or name. Evaluation stops there.
/// Returning a value that is, or contains, a value referring back to itself is a `type` error
/// (`type.cyclic_result`) spanned on the returned expression; cycles the Script builds but does
/// not return are fine. Returning a function, or a value containing one, is a `type` error
/// (`type.function_result`) spanned the same way.
pub fn evaluate(program: &Program) -> Result<Value, Diagnostic> {
    machine::Machine::new(program).run()
}
