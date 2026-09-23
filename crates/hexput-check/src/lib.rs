//! Static check (FR-26). Exposes a single pure entry point taking a parsed AST plus what the
//! environment provides — the callable-name set and the Script's starting-variable names — and
//! the active policy, and returning findings — it never executes a script, never reaches
//! hexput-rpc or hexput-enforce, and holds no state between calls.
//! Depends on hexput-ast only — never hexput-interpreter, hexput-rpc, or hexput-enforce, by
//! construction.
//!
//! Binds: AD-8.
//!
//! # What it reports
//!
//! Everything LANGUAGE-REFERENCE §10 lists that is decidable from an AST: undeclared identifier
//! reads and assignments, wrong argument counts against functions the Script itself declares,
//! type errors between literal operands, unreachable code, unused locals, calls to names the
//! caller did not say are callable, and uses of a construct the policy disables.
//!
//! # What it deliberately does not report
//!
//! * **Duplicate `let` and `break`/`continue` outside a loop.** `hexput-parser` rejects both, so
//!   neither can appear in a parsed AST. §10 lists them because a caller sees them reported with
//!   the same category, stable code, message and span either way — not because this pass has to
//!   produce them. Carrying unreachable code for them would be a lie about where the guarantee
//!   lives.
//! * **Anything needing a value.** This is not a type system: it never infers a binding's type
//!   from its initializer, never folds a constant, and never judges what a value turns out to be.
//!
//! The consequence is the property the rest of the system leans on: **a finding is never a false
//! positive.** If the pass reports something, that code really does fail when it is reached. The
//! converse does not hold and is not meant to — silence means "nothing decidable was wrong".
//!
//! # Advisory by construction
//!
//! A Script passing the check is not thereby authorized for anything: capability grants and
//! budget charges remain `hexput-enforce`'s alone, reached only through the `Executor` (AD-3),
//! and no crate on that path is even compiled into this one.

mod operands;
mod pass;

use hexput_ast::Program;

/// The diagnostics shape and its rendering, re-exported so a consumer of the check pass can
/// report findings without a `hexput-shared` edge the Spine's crate graph does not list.
pub use hexput_ast::{
    Category, Code, Diagnostic, RenderOptions, Severity, Span, render_diagnostic,
};

/// The closed set of language constructs a Backend may switch off (FR-3, OQ-3), each
/// independently, all enabled by default.
///
/// The set is closed on purpose: variable declaration and assignment, scalar literals,
/// operators, property and index access, and `return` are always on, because disabling any of
/// them would leave the language unable to express or report anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Policy {
    /// `while` and `for … in`.
    pub loops: bool,
    /// `if` / `else if` / `else`.
    pub conditionals: bool,
    /// Defining a function, named or anonymous. With this off no local function can exist, so
    /// nothing local can be invoked either.
    pub callbacks: bool,
    /// Object literals.
    pub object_literals: bool,
    /// Array literals.
    pub array_literals: bool,
    /// Invoking a Registered Function — a blanket switch, independent of any Capability grant.
    pub rpc_calls: bool,
}

impl Policy {
    /// Every construct enabled, which is what a Backend that sets nothing gets.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            loops: true,
            conditionals: true,
            callbacks: true,
            object_literals: true,
            array_literals: true,
            rpc_calls: true,
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::new()
    }
}

/// What the Script's environment provides: the names bound before it runs, and — optionally —
/// the names it is allowed to call.
///
/// The optionality of the callable list is the contract, not an implementation detail. A
/// **missing** list suppresses [`Code::UNKNOWN_FUNCTION`] entirely, because a caller that never
/// said what is callable cannot have every host call held against it. An **empty** list does not:
/// it means "nothing but this Script's own functions is callable". `hexput check` supplies no
/// list unless `--callable` is given; the daemon supplies its Session's Registered Functions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    variables: Vec<String>,
    callables: Option<Vec<String>>,
}

impl Environment {
    /// Nothing bound, and no claim about what is callable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare the Script's **starting variables** by name — the inputs a caller binds from
    /// outside the source, which `hexput_interpreter::evaluate_with_variables` would bind into
    /// the root scope before the top-level named functions hoist.
    ///
    /// Only the names are taken: a static check needs to know a name exists, never what it holds.
    /// A name given twice binds once, as `hexput_interpreter::evaluate_with_variables` documents
    /// for the same input — rejecting a repeat belongs to the layer that knows where the two came
    /// from.
    ///
    /// **Replaces** any names set before, rather than adding to them.
    #[must_use]
    pub fn with_variables<I, N>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = N>,
        N: Into<String>,
    {
        self.variables = names.into_iter().map(Into::into).collect();
        self
    }

    /// Supply the callable-name list. Calling this with an empty iterator supplies an *empty*
    /// list, which is not the same as never calling it — see the type's documentation.
    ///
    /// **Replaces** any list set before, rather than adding to it; calling it twice keeps the
    /// second list alone.
    #[must_use]
    pub fn with_callables<I, N>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = N>,
        N: Into<String>,
    {
        self.callables = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// The starting-variable names, in the order supplied.
    #[must_use]
    pub fn variables(&self) -> &[String] {
        &self.variables
    }

    /// The callable-name list, or `None` when the caller supplied none.
    #[must_use]
    pub fn callables(&self) -> Option<&[String]> {
        self.callables.as_deref()
    }
}

/// What a completed check says about a Script, beyond the findings themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// No findings at all.
    Clean,
    /// Findings, none of them errors. The Script is not rejected: a warning can never do that.
    Warnings,
    /// At least one error-severity finding.
    Errors,
}

/// The result of one check pass: every finding, in source order, and what they add up to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    diagnostics: Vec<Diagnostic>,
}

impl Findings {
    /// Every finding, ordered by where it starts in the source.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether the Script is clean, carries warnings only, or must be rejected.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        if self.diagnostics.is_empty() {
            Outcome::Clean
        } else if self.has_errors() {
            Outcome::Errors
        } else {
            Outcome::Warnings
        }
    }

    /// Whether any finding is an error — the one question a caller rejecting a Script asks.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|finding| finding.severity == Severity::Error)
    }

    /// How many findings there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// Whether there are no findings at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// How many findings are errors.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .count()
    }
}

/// Check a parsed Script against what its environment provides and the policy in force.
///
/// Pure: the same `(program, environment, policy)` yields the same findings every time, and
/// nothing is retained between calls. No source the parser accepts — malformed, adversarially
/// nested, or adversarially large — panics or grows the host stack, because the walk runs on an
/// explicit work stack rather than the host's.
///
/// # Panics
/// `program`'s `ExprId`s and `BlockId`s must index its own arenas, which is `hexput-ast`'s
/// standing rule for every consumer and is automatic for anything `hexput_parser::parse`
/// produced. A hand-built `Program` carrying an out-of-range id panics in `Program::expression`
/// or `Program::block`, and one whose ids form a cycle does not terminate.
#[must_use]
pub fn check(program: &Program, environment: &Environment, policy: &Policy) -> Findings {
    Findings {
        diagnostics: pass::run(program, environment, policy),
    }
}
