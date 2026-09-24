//! The walk itself: one pass over the AST on an explicit work stack, carrying a stack of
//! lexical scopes that mirrors exactly what `hexput-interpreter` builds at run time.
//!
//! # Why the walk is deferred for function bodies
//!
//! A function body does not run where it is written. Closures capture by reference, so
//!
//! ```text
//! fn f() { return x; }
//! let x = 1;
//! return f();
//! ```
//!
//! is correct at run time: `f` is hoisted, `x` is declared, and only then is `f` called.
//! Walking the body where it appears would report `x` undeclared — a false positive, the one
//! thing this pass must never produce. So every function body is deferred to the end of the
//! block that defines it, when that block's scope holds everything it will ever hold. The cost
//! is a false *negative* for a body genuinely called before a `let` it reads, which the runtime
//! still catches; that trade only ever goes this direction.

use std::collections::{HashMap, HashSet};

use hexput_ast::{
    AccessKind, AccessLink, Block, BlockId, Category, Code, Diagnostic, ExprId, Expression,
    ExpressionKind, Function, Identifier, Program, Span, Statement, StatementKind,
};

use crate::operands;
use crate::{Environment, Policy};

/// One name bound in one scope.
struct Binding {
    /// Where the name is declared — what an unused or duplicate finding points at.
    span: Span,
    /// Parameter count, when the name certainly holds a function the Script declares here.
    /// `None` for everything else, including a function-valued name that is assigned somewhere.
    arity: Option<usize>,
    /// Whether "never read" is worth telling the author about. True for what the Script itself
    /// declares — a `let` or a named `fn` — and false for a parameter, a `for` binding or a
    /// starting variable, where not reading the name is ordinary rather than a mistake.
    report_unused: bool,
    read: bool,
}

struct Scope<'p> {
    bindings: HashMap<&'p str, Binding>,
    /// Declaration order, so unused findings come out the way the source reads.
    order: Vec<&'p str>,
    /// Function bodies to walk once this scope is complete.
    deferred: Vec<&'p Function>,
}

enum Job<'p> {
    /// Push a scope, bind `seed` — starting variables, parameters, or a `for` binding, none of
    /// which is ever reported unused — hoist the named functions, then run `statements` in it.
    Open {
        statements: &'p [Statement],
        seed: &'p [Identifier],
    },
    Statement(&'p Statement),
    Expression(ExprId),
    /// Bind a `let` name — after its initializer has been walked, as the runtime does.
    Declare {
        name: &'p Identifier,
        arity: Option<usize>,
    },
    /// Walk one function body deferred to the current scope, then come back for the next.
    Deferred,
    /// Report the current scope's unused locals and pop it.
    Close,
}

/// No seed, spelled once so `Open` can borrow it.
const NO_SEED: &[Identifier] = &[];

pub(crate) fn run(
    program: &Program,
    environment: &Environment,
    policy: &Policy,
) -> Vec<Diagnostic> {
    let mut walk = Walk {
        program,
        policy,
        callables: environment
            .callables()
            .map(|names| names.iter().map(String::as_str).collect()),
        reassigned: reassigned_names(program),
        scopes: Vec::new(),
        findings: Vec::new(),
    };

    // The starting variables are the root scope's seed: the interpreter binds them there before
    // the top-level named functions hoist, so a name the Script also declares is a redeclaration
    // in one block (§5) rather than a value silently dropped.
    let variables: Vec<Identifier> = environment
        .variables()
        .iter()
        .map(|name| Identifier {
            name: name.clone(),
            span: program.span,
        })
        .collect();

    let mut jobs = vec![Job::Open {
        statements: &program.statements,
        seed: &variables,
    }];
    while let Some(job) = jobs.pop() {
        walk.step(job, &mut jobs);
    }

    // Source order, not discovery order: unused locals surface when their scope closes and
    // deferred bodies are walked after the statements around them. A stable sort keeps two
    // findings at the same offset in the order they were produced.
    walk.findings.sort_by_key(|finding| finding.span.offset);
    walk.findings
}

/// Every name assigned to anywhere in the Script. A name that is assigned is no longer certain
/// to hold the function it was declared with, so no arity is checked against it — conservative
/// by name rather than by binding, because proving which binding an assignment reaches is the
/// kind of analysis this pass does not do.
fn reassigned_names(program: &Program) -> HashSet<&str> {
    let mut names = HashSet::new();
    // The block arena holds every block in the Script, so this needs no traversal of its own.
    let statements = program
        .statements
        .iter()
        .chain(program.blocks.iter().flat_map(|block| &block.statements));
    for statement in statements {
        if let StatementKind::Assignment { target, .. } = &statement.kind
            && let ExpressionKind::Identifier(name) = &program.expression(*target).kind
        {
            names.insert(name.name.as_str());
        }
    }
    names
}

struct Walk<'p> {
    program: &'p Program,
    policy: &'p Policy,
    /// `None` when the caller supplied no callable-name list, which suppresses the unknown-call
    /// finding entirely — as distinct from an empty list, which does not.
    callables: Option<HashSet<&'p str>>,
    reassigned: HashSet<&'p str>,
    scopes: Vec<Scope<'p>>,
    findings: Vec<Diagnostic>,
}

impl<'p> Walk<'p> {
    fn step(&mut self, job: Job<'p>, jobs: &mut Vec<Job<'p>>) {
        match job {
            Job::Open { statements, seed } => self.open(statements, seed, jobs),
            Job::Statement(statement) => self.statement(statement, jobs),
            Job::Expression(id) => self.expression(id, jobs),
            Job::Declare { name, arity } => self.declare(name, arity, true),
            Job::Deferred => self.deferred(jobs),
            Job::Close => self.close(),
        }
    }

    fn open(
        &mut self,
        statements: &'p [Statement],
        seed: &'p [Identifier],
        jobs: &mut Vec<Job<'p>>,
    ) {
        self.scopes.push(Scope {
            bindings: HashMap::new(),
            order: Vec::new(),
            deferred: Vec::new(),
        });
        // A name repeated in the seed keeps the last one, as `evaluate_with_variables`
        // documents: rejecting a repeat belongs to the layer that knows where the two came
        // from, and a seed carries no source span to point a diagnostic at.
        for name in seed {
            self.seed(name);
        }
        // Every `fn name` in a block is bound before any of that block's statements run (§6), so
        // mutual recursion works in any declaration order.
        for statement in statements {
            if let StatementKind::Function { name, function } = &statement.kind {
                self.declare(name, Some(function.parameters.len()), true);
            }
        }
        let reachable = self.unreachable(statements);
        jobs.push(Job::Close);
        jobs.push(Job::Deferred);
        // Statements past the terminator are not walked. Code that can never run can never fail,
        // so a finding there would say something certain about code that is certainly not
        // reached — and the warning above has already said the only true thing about it. Hoisted
        // declarations are the exception: `return g(); fn g() { … };` really does call `g`, so
        // its body is still walked.
        jobs.extend(
            statements
                .iter()
                .enumerate()
                .rev()
                .filter(|(at, statement)| {
                    *at < reachable || matches!(statement.kind, StatementKind::Function { .. })
                })
                .map(|(_, statement)| Job::Statement(statement)),
        );
    }

    /// One warning on the first statement that can never run. Only a `return`, `break` or
    /// `continue` in this very list makes the rest unreachable: an `if` whose branches all
    /// return does too, but proving that is control-flow analysis, and being wrong about it
    /// would cost a false positive.
    ///
    /// A named function declaration after the terminator is *not* dead code — it is hoisted, so
    /// it takes effect before the block's first statement runs (§6) and the very point of
    /// hoisting is that declaration order does not matter.
    ///
    /// Returns how many statements are reachable, so the caller can stop walking there.
    fn unreachable(&mut self, statements: &'p [Statement]) -> usize {
        let terminates = |statement: &Statement| {
            matches!(
                statement.kind,
                StatementKind::Return { .. }
                    | StatementKind::Break { .. }
                    | StatementKind::Continue { .. }
            )
        };
        if let Some(at) = statements.iter().position(terminates)
            && let Some(first) = statements[at + 1..]
                .iter()
                .find(|statement| !matches!(statement.kind, StatementKind::Function { .. }))
        {
            let keyword = match statements[at].kind {
                StatementKind::Return { .. } => "return",
                StatementKind::Break { .. } => "break",
                _ => "continue",
            };
            self.findings.push(Diagnostic::warning(
                Category::Syntax,
                Code::UNREACHABLE_CODE,
                format!("this code can never run: the block always `{keyword}`s before it"),
                first.span,
            ));
        }
        statements
            .iter()
            .position(terminates)
            .map_or(statements.len(), |at| at + 1)
    }

    fn deferred(&mut self, jobs: &mut Vec<Job<'p>>) {
        let Some(scope) = self.scopes.last_mut() else {
            return;
        };
        let Some(function) = scope.deferred.pop() else {
            return;
        };
        // Come back for the rest once this body — and everything nested in it — is done.
        jobs.push(Job::Deferred);
        // Parameters and the body's own statements share one scope, whose parent is the
        // function's *defining* scope, never its caller's (§6).
        jobs.push(Job::Open {
            statements: &self.program.block(function.body).statements,
            seed: &function.parameters,
        });
    }

    fn close(&mut self) {
        let Some(scope) = self.scopes.pop() else {
            return;
        };
        for name in scope.order {
            let Some(binding) = scope.bindings.get(name) else {
                continue;
            };
            if binding.report_unused && !binding.read {
                self.findings.push(Diagnostic::warning(
                    Category::Reference,
                    Code::UNUSED_VARIABLE,
                    format!("`{name}` is declared but never read"),
                    binding.span,
                ));
            }
        }
    }

    /// Bind a caller-supplied name — a starting variable, a parameter, or a `for` binding —
    /// without the redeclaration check. None of them can collide with each other within one
    /// scope through any source the parser accepts, and a starting variable's `Identifier` is
    /// synthesized rather than read from the file, so it has no span worth reporting.
    fn seed(&mut self, name: &'p Identifier) {
        let Some(scope) = self.scopes.last_mut() else {
            return;
        };
        if scope.bindings.contains_key(name.name.as_str()) {
            return;
        }
        scope.order.push(name.name.as_str());
        scope.bindings.insert(
            name.name.as_str(),
            Binding {
                span: name.span,
                arity: None,
                report_unused: false,
                read: false,
            },
        );
    }

    /// Bind `name` in the current scope. A name already bound here is a redeclaration in one
    /// block (§5) — which the parser already rejects within the source, so in practice this
    /// catches a starting variable the Script's own top level also declares.
    fn declare(&mut self, name: &'p Identifier, arity: Option<usize>, report_unused: bool) {
        let Some(scope) = self.scopes.last_mut() else {
            return;
        };
        if scope.bindings.contains_key(name.name.as_str()) {
            self.findings.push(Diagnostic::new(
                Category::Syntax,
                Code::DUPLICATE_DECLARATION,
                format!(
                    "expected a new binding name; `{}` is already declared in this scope",
                    name.name
                ),
                name.span,
            ));
            return;
        }
        scope.order.push(name.name.as_str());
        scope.bindings.insert(
            name.name.as_str(),
            Binding {
                span: name.span,
                arity: arity.filter(|_| !self.reassigned.contains(name.name.as_str())),
                report_unused,
                read: false,
            },
        );
    }

    /// The index of the innermost scope binding `name`.
    fn resolve(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rposition(|scope| scope.bindings.contains_key(name))
    }

    /// Resolve a read and mark the binding used. Returns the binding's arity when it has one.
    fn read(&mut self, name: &str) -> Option<Option<usize>> {
        let at = self.resolve(name)?;
        let binding = self.scopes[at].bindings.get_mut(name)?;
        binding.read = true;
        Some(binding.arity)
    }

    fn statement(&mut self, statement: &'p Statement, jobs: &mut Vec<Job<'p>>) {
        match &statement.kind {
            StatementKind::Let {
                name, initializer, ..
            } => {
                // The initializer is evaluated before the name exists, so `let x = x;` reads an
                // undeclared `x` exactly as it does at run time.
                jobs.push(Job::Declare {
                    name,
                    arity: self.function_arity(*initializer),
                });
                jobs.push(Job::Expression(*initializer));
            }
            StatementKind::Function { function, .. } => {
                // Already hoisted by `open`; the body waits for the rest of this block.
                self.defer(function);
            }
            StatementKind::Assignment {
                target,
                value,
                equals,
            } => {
                let _ = equals;
                jobs.push(Job::Expression(*value));
                self.assignment(*target, jobs);
            }
            StatementKind::Return { value, .. } => {
                if let Some(value) = value {
                    jobs.push(Job::Expression(*value));
                }
            }
            StatementKind::Block(body) => self.enter(*body, jobs),
            StatementKind::If {
                branches,
                else_branch,
            } => {
                if !self.policy.conditionals {
                    let span = branches
                        .first()
                        .map_or(statement.span, |first| first.keyword);
                    self.disabled("if", "conditionals", span);
                }
                if let Some(branch) = else_branch {
                    self.enter(branch.body, jobs);
                }
                for branch in branches.iter().rev() {
                    self.enter(branch.body, jobs);
                    jobs.push(Job::Expression(branch.condition.expression));
                }
            }
            StatementKind::While {
                keyword,
                condition,
                body,
            } => {
                if !self.policy.loops {
                    self.disabled("while", "loops", *keyword);
                }
                self.enter(*body, jobs);
                jobs.push(Job::Expression(condition.expression));
            }
            StatementKind::For {
                keyword,
                binding,
                iterable,
                body,
                ..
            } => {
                if !self.policy.loops {
                    self.disabled("for", "loops", *keyword);
                }
                // The binding and the body share one scope, fresh per iteration (§5); the
                // iterable is evaluated in the scope around the loop.
                jobs.push(Job::Open {
                    statements: &self.program.block(*body).statements,
                    seed: core::slice::from_ref(binding),
                });
                jobs.push(Job::Expression(*iterable));
            }
            StatementKind::Break { .. } | StatementKind::Continue { .. } => {}
            StatementKind::Expression(id) => jobs.push(Job::Expression(*id)),
        }
    }

    /// An assignment target: a bare name is resolved against the scopes (there is no implicit
    /// global creation, §5), while `o.k = v` and `a[0] = v` *read* their base and are walked as
    /// ordinary expressions.
    fn assignment(&mut self, target: ExprId, jobs: &mut Vec<Job<'p>>) {
        let ExpressionKind::Identifier(name) = &self.program.expression(target).kind else {
            jobs.push(Job::Expression(target));
            return;
        };
        if self.resolve(&name.name).is_none() {
            self.findings.push(Diagnostic::new(
                Category::Reference,
                Code::UNDECLARED_ASSIGNMENT,
                format!(
                    "cannot assign to `{}`: it is not declared; declare it first with `let {} = …`",
                    name.name, name.name
                ),
                name.span,
            ));
        }
        // Writing is not reading: a binding only ever assigned is still an unused local.
    }

    fn enter(&mut self, body: BlockId, jobs: &mut Vec<Job<'p>>) {
        let block: &'p Block = self.program.block(body);
        jobs.push(Job::Open {
            statements: &block.statements,
            seed: NO_SEED,
        });
    }

    fn expression(&mut self, id: ExprId, jobs: &mut Vec<Job<'p>>) {
        let expression: &'p Expression = self.program.expression(id);
        match &expression.kind {
            ExpressionKind::Literal(_) => {}
            ExpressionKind::Identifier(name) => {
                if self.read(&name.name).is_none() {
                    self.findings.push(Diagnostic::new(
                        Category::Reference,
                        Code::UNDECLARED_IDENTIFIER,
                        format!("`{}` is not declared", name.name),
                        name.span,
                    ));
                }
            }
            ExpressionKind::Group { expression, .. } => jobs.push(Job::Expression(*expression)),
            ExpressionKind::Unary { operator, operand } => {
                if let Some(finding) = operands::unary(self.program, operator.kind, *operand) {
                    self.findings.push(finding);
                }
                jobs.push(Job::Expression(*operand));
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                if let Some(finding) = operands::binary(self.program, *left, operator.kind, *right)
                {
                    self.findings.push(finding);
                }
                jobs.push(Job::Expression(*right));
                jobs.push(Job::Expression(*left));
            }
            ExpressionKind::Array { elements, .. } => {
                if !self.policy.array_literals {
                    self.disabled("array literal", "array_literals", expression.span);
                }
                jobs.extend(elements.iter().rev().map(|id| Job::Expression(*id)));
            }
            ExpressionKind::Object { entries, .. } => {
                if !self.policy.object_literals {
                    self.disabled("object literal", "object_literals", expression.span);
                }
                jobs.extend(
                    entries
                        .iter()
                        .rev()
                        .map(|entry| Job::Expression(entry.value)),
                );
            }
            ExpressionKind::Function(function) => self.defer(function),
            ExpressionKind::Access { base, links } => self.access(*base, links, jobs),
        }
    }

    /// An access chain. A chain whose base is a bare name and whose first link is a call is *the*
    /// call shape this pass reasons about: `f(1, 2)`. Anything else — `o.f()`, `f()()` — has a
    /// callee this pass cannot name, so it checks the parts and says nothing about the call.
    fn access(&mut self, base: ExprId, links: &'p [AccessLink], jobs: &mut Vec<Job<'p>>) {
        let called = match (&self.program.expression(base).kind, links.first()) {
            (ExpressionKind::Identifier(name), Some(link))
                if matches!(link.kind, AccessKind::Call { .. }) =>
            {
                Some((name, link))
            }
            _ => None,
        };
        match called {
            Some((name, link)) => self.call(name, link),
            None => jobs.push(Job::Expression(base)),
        }
        for link in links.iter().rev() {
            match &link.kind {
                AccessKind::Call { arguments, .. } => {
                    jobs.extend(arguments.iter().rev().map(|id| Job::Expression(*id)));
                }
                AccessKind::Index { expression, .. } => jobs.push(Job::Expression(*expression)),
                AccessKind::Property(_) => {}
            }
        }
    }

    /// A call of a bare name: either the Script's own function, or the host's.
    fn call(&mut self, name: &'p Identifier, link: &'p AccessLink) {
        let AccessKind::Call { arguments, .. } = &link.kind else {
            return;
        };
        if let Some(arity) = self.read(&name.name) {
            // A local binding. Its argument count is known only when the name certainly still
            // holds the function it was declared with.
            if let Some(arity) = arity
                && arguments.len() != arity
            {
                self.findings.push(Diagnostic::new(
                    Category::Arity,
                    Code::ARGUMENT_COUNT,
                    format!(
                        "this call passes {} argument{}, but the function takes {arity}",
                        arguments.len(),
                        if arguments.len() == 1 { "" } else { "s" }
                    ),
                    link.span,
                ));
            }
            return;
        }
        // No local binding, so this can only be a Registered Function — or a typo for one.
        if !self.policy.rpc_calls {
            self.disabled("call to a Registered Function", "rpc_calls", name.span);
            return;
        }
        if let Some(callables) = &self.callables
            && !callables.contains(name.name.as_str())
        {
            self.findings.push(Diagnostic::new(
                Category::Capability,
                Code::UNKNOWN_FUNCTION,
                format!(
                    "`{}` is neither declared in this script nor a function this caller says is callable",
                    name.name
                ),
                // The whole call, name through closing parenthesis — the span the runtime's
                // `capability.unknown_function` uses, so both underline the same range.
                Span::new(
                    name.span.offset,
                    link.span.end().saturating_sub(name.span.offset),
                    name.span.line,
                    name.span.column,
                ),
            ));
        }
    }

    /// Queue a function body to be walked once the scope defining it is complete.
    fn defer(&mut self, function: &'p Function) {
        if !self.policy.callbacks {
            self.disabled("function definition", "callbacks", function.keyword);
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.deferred.push(function);
        }
    }

    /// The parameter count of a `let` initializer that is written as a function literal, so
    /// `let f = fn(a) { … };` gets its calls checked exactly as a named declaration does.
    fn function_arity(&self, initializer: ExprId) -> Option<usize> {
        // Groups are transparent here exactly as they are to the literal-operand rules, and for
        // the same reason: `(fn(a) { … })` is the function it wraps. The bound belongs to a
        // hand-built AST whose groups form a cycle; source can only nest them finitely.
        let mut id = initializer;
        for _ in 0..self.program.expressions.len().saturating_add(1) {
            match &self.program.expression(id).kind {
                ExpressionKind::Group { expression, .. } => id = *expression,
                ExpressionKind::Function(function) => return Some(function.parameters.len()),
                _ => return None,
            }
        }
        None
    }

    fn disabled(&mut self, construct: &str, toggle: &str, span: Span) {
        self.findings.push(Diagnostic::new(
            Category::Policy,
            Code::CONSTRUCT_DISABLED,
            format!("`{construct}` is disabled by the `{toggle}` policy toggle"),
            span,
        ));
    }
}
