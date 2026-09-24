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
//! # Host calls
//!
//! A call whose callee is a bare name that no scope declares is a **host call** (§8, Story 3.1):
//! a call to a Registered Function. A local binding of the name shadows it, and naming one
//! without calling it stays `reference.undeclared_identifier` — a host function is not a value.
//! This crate knows nothing about the host: an [`Execution`] stops at a host call with a
//! [`HostCall`] holding the name and the detached arguments, and whoever drives it — the one
//! Executor, `hexput-exec` — decides whether the call is allowed, makes it, and either resumes the
//! Script with the returned value or ends it. A function or a cyclic value cannot be an argument
//! (`type.function_argument`, `type.cyclic_argument`, spanned on that argument), for the same
//! reason neither can be a Script result.
//!
//! [`evaluate`] and [`evaluate_with_variables`] run with no host at all, so every host call is
//! refused there as `capability.unknown_function` — which is what `hexput eval` reports.
//!
//! # Metering (Story 3.5)
//!
//! An [`Execution`] can be handed a [`Meter`]: a slice size, after which [`Execution::run`]
//! stops with [`Outcome::Paused`] so its driver can see how long the slice took and carry on or
//! not, and a memory ceiling, past which it stops with [`Outcome::OutOfMemory`]. The interpreter
//! only meters: the limits, and what crossing one means, belong to the Executor and
//! `hexput-enforce`. [`evaluate`] and [`evaluate_with_variables`] run unmetered.
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

use std::sync::Arc;

use hexput_ast::StatementKind;

pub use machine::Argument;
pub use value::{Array, Object, Value};

/// The diagnostics shape and its rendering, re-exported so a consumer of the evaluator — the CLI
/// in particular — can report what [`evaluate`] returns without a `hexput-shared` or `hexput-ast`
/// edge the Spine's crate graph does not list. `hexput-ast` re-exports these for the same reason.
pub use hexput_ast::{
    Category, Code, Diagnostic, RenderOptions, Severity, Span, render_diagnostic,
};

/// The parsed Script [`evaluate`] takes, re-exported so `hexput-exec` — the one Executor, which
/// the Spine gives no `hexput-ast` edge — can name what it runs.
pub use hexput_ast::Program;

/// How an [`Execution`] is metered: how much work it does before pausing, and how much memory
/// its values may hold. Both `None` in [`Meter::UNMETERED`], the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Meter {
    /// Pause after this many units of work: one per evaluation step, plus one per 256 bytes a
    /// string operation reads or writes, so a slice takes about as long whatever the Script
    /// does. `None` never pauses.
    pub slice: Option<core::num::NonZeroU64>,
    /// Stop once the execution's values hold more than this many bytes, by the interpreter's
    /// approximate count — checked after every step, and before a string concatenation whose
    /// result alone would cross it, so one step overshoots by at most its own allocation.
    /// `None` never stops.
    pub memory_ceiling: Option<usize>,
}

impl Meter {
    /// No slices and no ceiling: the Script runs until it ends or calls the host.
    pub const UNMETERED: Self = Self {
        slice: None,
        memory_ceiling: None,
    };
}

/// How many calls may be active at once before a call raises `depth.call_depth_exceeded`.
///
/// A documented constant, deliberately not a parameter: the interpreter's job is only to make
/// unbounded recursion terminate with a defined error instead of overflowing the host stack.
/// What recursion within the limit may cost is the Resource Budget's (CPU time and memory, Story
/// 3.5), which the Executor enforces through a [`Meter`].
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
    evaluate_with_variables(program, Vec::<(&str, Value)>::new())
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
/// Otherwise the same failures as [`evaluate`]. There is no host, so a host call is a
/// `capability` error (`capability.unknown_function`) spanned on the call.
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
    check_starting_variables(program, &variables)?;
    let mut machine = machine::Machine::with_variables(program, variables);
    // Consuming the machine on every path is the memory contract: the heap is dropped here and
    // only the detached result escapes. The machine is unmetered, so it never pauses and never
    // stops at a ceiling; the arms below only keep that true should it ever be metered.
    let stop = loop {
        match machine.execute()? {
            machine::Stop::Paused(_) => {}
            stop => break stop,
        }
    };
    match stop {
        machine::Stop::Finished(result) => Ok(result),
        // Unreachable: an unmetered machine has no ceiling to stop at, and pauses are looped
        // over above. Limits and their errors are the Executor's, so this is no budget error.
        machine::Stop::Paused(span) | machine::Stop::OutOfMemory(span) => Err(Diagnostic::new(
            Category::Syntax,
            Code::EXPECTED_SYNTAX,
            "internal error: an unmetered evaluation stopped at a meter",
            span,
        )),
        machine::Stop::HostCall(call) => Err(Diagnostic::new(
            Category::Capability,
            Code::UNKNOWN_FUNCTION,
            format!(
                "`{}` is not declared, and there is no host here to call it on: a Script run on \
                 its own can call only the functions it declares",
                call.name
            ),
            call.span,
        )),
    }
}

/// Reject a starting variable whose name the Script's own top level also declares.
fn check_starting_variables<N: AsRef<str>>(
    program: &Program,
    variables: &[(N, Value)],
) -> Result<(), Diagnostic> {
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
    Ok(())
}

/// A resumable run of one Script: what the one Executor drives, a segment at a time, so the
/// Script can wait for a host call's value without holding a thread (Story 3.1).
///
/// An `Execution` owns its Program (shared through [`Arc`]) and its heap, and is `Send + 'static`,
/// so it can move between threads while suspended. [`Execution::run`] consumes it: the Script
/// either finishes, fails — and the heap is dropped with it — or stops at a host call, which hands
/// the `Execution` back inside the [`HostCall`].
pub struct Execution {
    machine: Box<machine::Machine<Arc<Program>>>,
}

impl Execution {
    /// An execution of `program` whose root scope binds `variables`, exactly as
    /// [`evaluate_with_variables`] binds them — with the same caller obligations. Nothing runs
    /// until [`Execution::run`].
    ///
    /// # Errors
    /// A supplied name that the Script's own top level also declares, as
    /// `syntax.duplicate_declaration` (see [`evaluate_with_variables`]).
    pub fn with_variables<N: AsRef<str>>(
        program: Arc<Program>,
        variables: impl IntoIterator<Item = (N, Value)>,
    ) -> Result<Self, Diagnostic> {
        let variables: Vec<(N, Value)> = variables.into_iter().collect();
        check_starting_variables(&program, &variables)?;
        Ok(Self {
            machine: Box::new(machine::Machine::with_variables(program, variables)),
        })
    }

    /// The names bound in this execution's root scope, sorted. Before it runs: its starting
    /// variables and its top-level named functions (hoisted, §6) — and, by construction, no
    /// builtin but [`BUILTINS`]; once it has run up to a host call, also the top-level `let`s
    /// that ran. Always the root scope, whatever scope the Script stopped in. For enumerating
    /// everything a Script can reach (Story 3.4).
    #[must_use]
    pub fn root_names(&self) -> Vec<String> {
        self.machine.root_names()
    }

    /// Meter the rest of this execution against `meter` — until the next call, if any. A new
    /// execution is [`Meter::UNMETERED`].
    #[must_use]
    pub fn metered(mut self, meter: Meter) -> Self {
        self.machine.set_meter(meter);
        self
    }

    /// Approximately how many bytes this execution's values hold right now, by the count its
    /// memory ceiling is checked against.
    #[must_use]
    pub fn memory_used(&self) -> usize {
        self.machine.memory_used()
    }

    /// Run until the Script ends, calls the host, or its [`Meter`] stops it.
    ///
    /// # Errors
    /// The Script's first runtime failure, as [`evaluate`] reports it — including a host call's
    /// argument that is, or contains, a function or a cycle.
    pub fn run(self) -> Result<Outcome, Diagnostic> {
        let mut machine = self.machine;
        match machine.execute()? {
            machine::Stop::Finished(result) => Ok(Outcome::Finished(result)),
            machine::Stop::HostCall(call) => Ok(Outcome::HostCall(HostCall {
                name: call.name,
                arguments: call.arguments,
                span: call.span,
                execution: Self { machine },
            })),
            machine::Stop::Paused(span) => Ok(Outcome::Paused(Paused {
                span,
                execution: Self { machine },
            })),
            machine::Stop::OutOfMemory(span) => {
                let used = machine.memory_used();
                // The heap goes here, before the driver hears of it.
                drop(machine);
                Ok(Outcome::OutOfMemory(OutOfMemory { span, used }))
            }
        }
    }
}

/// Where an [`Execution::run`] stopped.
pub enum Outcome {
    /// The Script ended with this result, detached from the execution.
    Finished(Value),
    /// The Script called the host and waits for the value.
    HostCall(HostCall),
    /// The slice its [`Meter`] allows is done; [`Paused::resume`] hands the execution back.
    Paused(Paused),
    /// Its values came to hold more than its [`Meter`]'s memory ceiling. The execution is gone,
    /// its heap already freed.
    OutOfMemory(OutOfMemory),
}

/// An [`Execution`] paused at the end of a slice of work.
pub struct Paused {
    span: Span,
    execution: Execution,
}

impl Paused {
    /// The construct that was running when the slice ended.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The execution, ready to [`run`](Execution::run) on from where it paused.
    #[must_use]
    pub fn resume(self) -> Execution {
        self.execution
    }
}

/// An execution stopped at its memory ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfMemory {
    span: Span,
    used: usize,
}

impl OutOfMemory {
    /// The construct whose allocation crossed the ceiling — or, for a concatenation stopped
    /// before it allocated, its operator.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Approximately how many bytes the execution's values held when it stopped. At or below
    /// the ceiling when a concatenation was stopped before it allocated.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }
}

/// A host call an [`Execution`] is suspended on: a call to a bare name no scope declares (§8).
///
/// Dropping it ends the execution. [`HostCall::resume`] hands the execution back with the call's
/// value in place.
pub struct HostCall {
    name: String,
    arguments: Vec<Argument>,
    span: Span,
    execution: Execution,
}

impl HostCall {
    /// The name the Script called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The arguments, in order, each detached and with the span of its expression.
    #[must_use]
    pub fn arguments(&self) -> &[Argument] {
        &self.arguments
    }

    /// The call, from the callee's name through its closing parenthesis — where an error about
    /// the call as a whole points.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Give the call `value` as its result and hand back the execution, ready to
    /// [`run`](Execution::run) on from the call. The value is copied into the execution, like a
    /// starting variable, and carries on exactly as an ordinary call's returned value would.
    ///
    /// The caller obligation of [`evaluate_with_variables`] applies: every number must be finite.
    #[must_use]
    pub fn resume(self, value: &Value) -> Execution {
        let mut execution = self.execution;
        execution.machine.resume(value);
        execution
    }
}

/// Every name a Script can reach that it neither declared nor was given as a starting variable:
/// the language's builtins. **Empty in v2** — there is no standard library at all, not even
/// `len()` (LANGUAGE-REFERENCE §11) — so the only other names a Script can reach are its
/// Session's Registered Functions, and only by calling them (§8, FR-7).
///
/// This is the one enumeration Story 3.4 asserts: a fresh [`Execution`]'s root scope binds its
/// starting variables, its top-level named functions and these names, and nothing else
/// ([`Execution::root_names`]). Adding a name here widens the trust boundary (NFR1) and needs a
/// spec of its own.
pub const BUILTINS: &[&str] = &[];

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
