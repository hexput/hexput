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
//! # Starting variables
//!
//! [`evaluate`] runs a Script with nothing but what its own source declares. [`evaluate_with_variables`]
//! binds caller-supplied inputs into the root scope first, which is how the CLI's `--var` and, later,
//! a Backend's execution parameters reach a Script.
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

use hexput_ast::{Program, StatementKind};

pub use value::{Array, Object, Value};

/// The diagnostics shape and its rendering, re-exported so a consumer of the evaluator — the CLI
/// in particular — can report what [`evaluate`] returns without a `hexput-shared` or `hexput-ast`
/// edge the Spine's crate graph does not list. `hexput-ast` re-exports these for the same reason.
pub use hexput_ast::{
    Category, Code, Diagnostic, RenderOptions, Severity, Span, render_diagnostic,
};

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

/// Evaluate a parsed Script whose root scope already binds `variables` — the Script's **starting
/// variables**, the inputs a caller supplies from outside the source.
///
/// Each value is copied into the execution's fresh heap before a single statement runs, so the
/// Script reads a starting variable exactly as it reads a top-level `let`: assignable, shadowable
/// by an inner block, and captured by reference by a closure. Structure the caller shared between
/// two inputs (or within one) expands into separate collections inside the execution, which is
/// unobservable — a detached value carries no identity.
///
/// Names are bound in the order given, before the top-level named functions are hoisted. A name
/// repeated in `variables` keeps the last value; rejecting a repeat belongs to the caller, which
/// is the layer that knows where the two came from.
///
/// Passing no variables is exactly [`evaluate`].
///
/// # Errors
/// Before anything runs, a supplied name that the Script's own top level also declares — with
/// `let` or as a named `fn` — is a `syntax` error (`syntax.duplicate_declaration`) spanned on
/// that declaration. A starting variable is a top-level binding, and `let`, named functions and
/// parameters share one block namespace (§5), so the two really are a redeclaration in one block;
/// reporting it is what keeps a supplied value from being silently discarded by source the caller
/// may not have written.
///
/// Otherwise the same failures as [`evaluate`].
///
/// # Caller obligations
/// Neither is checked here, and neither can be detected later:
///
/// * A name that is not a §2 identifier (ASCII letters, digits and `_`, not starting with a
///   digit, not a reserved word) binds a variable no Hexput source can name, so the value is
///   simply unreachable. Validate names at the layer that receives them.
/// * A [`Value::Number`] must be finite. Every number inside an execution is (`Value::Number`'s
///   own documentation says so, and every operation that would produce a non-finite result raises
///   instead); a `NaN` or an infinity supplied here breaks that invariant for the rest of the
///   execution and can reach the Script result.
pub fn evaluate_with_variables<N: AsRef<str>>(
    program: &Program,
    variables: impl IntoIterator<Item = (N, Value)>,
) -> Result<Value, Diagnostic> {
    let variables: Vec<(N, Value)> = variables.into_iter().collect();
    for statement in &program.statements {
        let (StatementKind::Let { name, .. } | StatementKind::Function { name, .. }) =
            &statement.kind
        else {
            continue;
        };
        if variables
            .iter()
            .any(|(supplied, _)| supplied.as_ref() == name.name)
        {
            return Err(Diagnostic::new(
                Category::Syntax,
                Code::DUPLICATE_DECLARATION,
                format!(
                    "`{}` is already bound as a starting variable, and this declaration is in the \
                     same block; rename one of them",
                    name.name
                ),
                name.span,
            ));
        }
    }
    machine::Machine::with_variables(program, variables).run()
}

/// Format a number the way the language does (§4.3): shortest round-tripping digits, plain
/// notation for magnitudes in `[1e-6, 1e21)` and exponent notation outside it, whole values with
/// no decimal point, and `-0` as `0`.
///
/// Public so a caller that prints a Script result — the CLI — spells a number exactly as §4.3
/// stringification does, rather than growing a second spelling of the same value.
#[must_use]
pub fn number_to_string(number: f64) -> String {
    convert::number_to_string(number)
}
