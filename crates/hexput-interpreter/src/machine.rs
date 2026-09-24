//! The evaluator: one loop over an explicit stack of continuation frames plus a value stack,
//! mirroring the parser's driver. Nothing here recurses on input nesting, and every frame boundary
//! is a point where the evaluation can stop and later carry on.
//!
//! The `Machine` owns the execution's heap. Every collection and scope the Script creates lives
//! there, so dropping the `Machine` — after a result or an error — frees all of it, cycles
//! included. Only the detached result outlives it.
//!
//! # Suspension (Story 3.1)
//!
//! A call whose callee is a bare name no scope declares is a host call (LANGUAGE-REFERENCE §8).
//! The machine evaluates its arguments, detaches them, and stops with a [`Stop::HostCall`]; the
//! caller answers it with [`Machine::resume`] and runs the machine again, which carries on as if an
//! ordinary call had returned that value. Nothing about the host is known here: which names are
//! registered, how the call travels and what it may cost are the Executor's.
//!
//! A suspended machine moves between threads, so it must be `Send` and must not borrow the
//! Program. Frames therefore address the syntax tree by index (an [`ExprId`], a [`BlockId`], a
//! statement's position), never by reference, and the machine holds the Program through `P` —
//! `Arc<Program>` for a resumable execution, `&Program` for a run-to-completion one. A test
//! asserts `Machine<Arc<Program>>` is `Send + 'static`.
//!
//! # Metering (Story 3.5)
//!
//! A machine can be handed a [`Meter`]. With a slice size it stops with [`Stop::Paused`] once it
//! has done that much work — one unit per frame, plus one per [`BYTES_PER_UNIT`] bytes a string
//! operation reads or writes, so a slice of long-string steps is no longer than a slice of cheap
//! ones — and carries on from the same frame when run again. With a memory ceiling it stops with
//! [`Stop::OutOfMemory`] after the first frame that leaves the heap holding more than the
//! ceiling, and before a string concatenation whose result alone would cross it. Either stop
//! carries the span of the construct that was running. What the limits are, and what exceeding
//! them means, is the Executor's: the machine only meters.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use hexput_ast::{
    AccessKind, AccessLink, BinaryOperator, BlockId, Category, Code, Diagnostic, ExprId,
    ExpressionKind, Function, Identifier, Literal, Program, Span, Spanned, Statement,
    StatementKind, UnaryOperator,
};
use indexmap::IndexMap;

use crate::convert::{number_to_string, to_number, to_string};
use crate::heap::{DetachFailure, Heap, RtValue, SlotId, TEXT_OVERHEAD};
use crate::{CALL_DEPTH_LIMIT, Meter, Value};

/// How many bytes a string operation reads or writes per unit of slice work.
const BYTES_PER_UNIT: usize = 256;

/// Where a statement sits: at the Program's top level (`block: None`) or in a block, by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StmtAt {
    block: Option<BlockId>,
    index: usize,
}

/// Where a `Function` sits: a named declaration statement, or a function literal expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FnAt {
    Declared(StmtAt),
    Literal(ExprId),
}

/// One access chain, or its first `end` links: an assignment target's receiver chain is every
/// link but the assigned one.
#[derive(Debug, Clone, Copy)]
struct Chain {
    /// The `Access` expression the chain belongs to.
    access: ExprId,
    /// How many of its links the chain covers.
    end: usize,
}

/// A pending unit of work. Frames that consume values document the value-stack shape they
/// expect on entry, top last.
enum Frame {
    Statement(StmtAt),
    /// Reclaim the block's scope and restore the one that was current before it was entered.
    ExitScope(SlotId),
    /// Evaluate an expression and push its value.
    Eval(ExprId),
    /// `[value]` → `[]`.
    Discard,
    /// `[value]` → `[]`, binding the name of the `let` statement at this position.
    Declare(StmtAt),
    /// `[value]` → the Script result, detached from the heap. The id is the returned
    /// expression, which a cyclic result's diagnostic points at.
    Return(ExprId),
    /// `[operand]` → `[result]`.
    Unary {
        operator: Spanned<UnaryOperator>,
        operand: ExprId,
    },
    /// `[left]` → decides short-circuiting, then schedules the right operand.
    BinaryRight {
        left: ExprId,
        operator: Spanned<BinaryOperator>,
        right: ExprId,
    },
    /// `[left, right]` → `[result]`.
    Binary {
        left: ExprId,
        operator: Spanned<BinaryOperator>,
        right: ExprId,
    },
    /// `[e0 … eN-1]` → `[array]`, for the array literal with this id and element count.
    BuildArray {
        array: ExprId,
        count: usize,
    },
    /// `[v0 … vN-1]` → `[object]`, keyed by the entries of the object literal with this id.
    BuildObject(ExprId),
    /// `[receiver]` → applies the chain's `links[index]` onward. `in_target` marks an assignment
    /// target's receiver chain, where `?.` is not allowed.
    Link {
        chain: Chain,
        index: usize,
        in_target: bool,
    },
    /// `[receiver, key]` → `[element]`, then continues with `links[index + 1]`.
    IndexRead {
        chain: Chain,
        index: usize,
        in_target: bool,
    },
    /// `[value]` → `[]`, assigning the identifier expression with this id.
    AssignName(ExprId),
    /// `[receiver, value]` or `[receiver, key, value]` → `[]`, for the assignment whose target is
    /// the access expression with this id. Its last link is the one assigned; the links before it
    /// name a `null` receiver.
    AssignMember(ExprId),
    /// `[condition]` → runs the `if` statement's `branches[index]` body, tests the next branch,
    /// or takes the `else`. Only the taken branch's body is ever scheduled.
    Branch {
        at: StmtAt,
        index: usize,
    },
    /// A loop's control boundary: it sits directly below the running body for the whole loop, so
    /// `break` and `continue` can unwind to it. Popping it advances the loop by one iteration.
    Loop(Box<LoopFrame>),
    /// `[condition]` → runs one `while` iteration or ends the loop.
    LoopTest(Box<LoopFrame>),
    /// `[iterable]` → opens the `for` loop at this position over it.
    ForStart(StmtAt),
    /// `[callee, a0 … aN-1]` → binds a fresh call scope and runs the body.
    Invoke {
        chain: Chain,
        index: usize,
        in_target: bool,
        arguments: usize,
    },
    /// A call's boundary: `return` unwinds to it, and popping it is a body that ran off its end,
    /// which yields `null` (§6). Either way it restores what the call replaced and resumes the
    /// access chain the call sits in.
    CallEnd {
        outer_scope: SlotId,
        values: usize,
        chain: Chain,
        index: usize,
        in_target: bool,
    },
    /// `[a0 … aN-1]` → stops the machine with a host call: the chain's base is a bare name no
    /// scope declares and its first link is the call (§8). [`Machine::resume`] continues the
    /// chain from its second link.
    HostCall {
        chain: Chain,
        in_target: bool,
    },
}

/// What a loop needs to run and to be unwound to. Boxed inside [`Frame`] so one loop's state
/// does not widen every frame.
struct LoopFrame {
    /// The scope in effect outside the loop; each iteration's scope hangs off it.
    outer_scope: SlotId,
    /// The value-stack depth outside the loop, restored by `break` and `continue`.
    values: usize,
    kind: LoopKind,
}

enum LoopKind {
    While {
        keyword: Span,
        condition: ExprId,
        body: BlockId,
    },
    /// The collection is snapshotted by handle at the loop's start, together with its mutation
    /// counter: each iteration re-checks the counter, so mutating the iterated collection is an
    /// error rather than undefined behaviour (§5). `keys` is `None` for an array. `at` is the
    /// `for` statement, which names the binding.
    For {
        keyword: Span,
        at: StmtAt,
        body: BlockId,
        collection: SlotId,
        version: u64,
        index: usize,
        keys: Option<Vec<Arc<str>>>,
    },
}

impl LoopKind {
    const fn body(&self) -> BlockId {
        match self {
            Self::While { body, .. } | Self::For { body, .. } => *body,
        }
    }
}

/// One argument of a host call: its detached value and the span of the expression that
/// produced it, which an error about that argument points at.
#[derive(Debug, Clone)]
pub struct Argument {
    /// The argument, detached from the execution like a Script result.
    pub value: Value,
    /// The argument expression's span.
    pub span: Span,
}

/// Why the machine stopped.
pub(crate) enum Stop {
    /// The Script ended with this result.
    Finished(Value),
    /// The Script called a host function and waits for its value.
    HostCall(PendingCall),
    /// The slice of work the [`Meter`] allows is done; running the machine again carries on.
    /// The span is the construct that was running.
    Paused(Span),
    /// The heap holds, or a concatenation would make it hold, more than the [`Meter`]'s memory
    /// ceiling. The span is the construct whose allocation crossed it. The machine must be
    /// dropped.
    OutOfMemory(Span),
}

/// What frame is running, cheaply: resolved to a span only when a metering stop needs one.
#[derive(Clone, Copy)]
enum Site {
    Statement(StmtAt),
    Expression(ExprId),
    Span(Span),
    Link(Chain, usize),
    /// A frame that allocates nothing and ends nothing a user wrote (scope exit, discard).
    Unknown,
}

/// A host call the machine is suspended on.
pub(crate) struct PendingCall {
    pub(crate) name: String,
    pub(crate) arguments: Vec<Argument>,
    /// From the callee's name through the call's closing parenthesis.
    pub(crate) span: Span,
}

pub(crate) struct Machine<P> {
    program: P,
    frames: Vec<Frame>,
    values: Vec<RtValue>,
    heap: Heap,
    scope: SlotId,
    /// The root scope: the starting variables and the Script's top level.
    root: SlotId,
    /// Every `Function` a function value points at, addressed by index so the heap never carries
    /// the program. `definitions` maps a `Function`'s position back to its index, so a closure
    /// created in a loop reuses one entry instead of adding one per iteration.
    functions: Vec<FnAt>,
    definitions: HashMap<FnAt, usize>,
    /// Number of calls currently on the frame stack; bounded by [`CALL_DEPTH_LIMIT`].
    depth: usize,
    /// The chain a host call suspended, to carry on with once its value arrives.
    suspended: Option<(Chain, bool)>,
    /// The limits the machine meters against.
    meter: Meter,
    /// Work done in the current slice, in units (see the module documentation).
    work: u64,
    /// The last construct the machine ran, for a metering stop's span.
    site: Site,
}

impl<P: Deref<Target = Program> + Clone> Machine<P> {
    #[cfg(test)]
    pub(crate) fn new(program: P) -> Self {
        Self::with_variables(program, Vec::<(&str, Value)>::new())
    }

    /// A machine whose root scope already binds `variables`, each attached into the fresh heap.
    ///
    /// The bindings land **between** allocating the root scope and [`Machine::open`]'s hoisting of
    /// the top-level named functions, because `let`, named functions and parameters share one
    /// block namespace (§5): binding afterwards would let a starting variable silently shadow a
    /// top-level `fn`, and binding before means the `fn` wins, exactly as a redeclaration in
    /// source would be rejected outright.
    pub(crate) fn with_variables<N: AsRef<str>>(
        program: P,
        variables: impl IntoIterator<Item = (N, Value)>,
    ) -> Self {
        let mut heap = Heap::default();
        let scope = heap.push_scope(None);
        let tree = program.clone();
        let mut machine = Self {
            program,
            frames: Vec::new(),
            values: Vec::new(),
            heap,
            scope,
            root: scope,
            functions: Vec::new(),
            definitions: HashMap::new(),
            depth: 0,
            suspended: None,
            meter: Meter::UNMETERED,
            work: 0,
            site: Site::Unknown,
        };
        for (name, value) in variables {
            let attached = machine.heap.attach(&value);
            machine.heap.declare(scope, name.as_ref(), attached);
        }
        machine.open(&tree, None);
        machine
    }

    /// Meter the rest of the run against `meter`.
    pub(crate) fn set_meter(&mut self, meter: Meter) {
        self.meter = meter;
    }

    /// Approximately how many bytes the execution's values hold (see `heap.rs`).
    pub(crate) fn memory_used(&self) -> usize {
        self.heap.used()
    }

    /// Run until the Script ends, calls the host, or the [`Meter`] stops it. After an error,
    /// [`Stop::Finished`] or [`Stop::OutOfMemory`], the machine must be dropped; after
    /// [`Stop::HostCall`], it continues only through [`Machine::resume`]; after
    /// [`Stop::Paused`], it continues by running it again.
    pub(crate) fn execute(&mut self) -> Result<Stop, Diagnostic> {
        // One handle for the whole run, so every step borrows the tree apart from `self`.
        let program = self.program.clone();
        let tree: &Program = &program;
        // A ceiling the starting variables or a host call's value already crossed stops the
        // machine before it runs a step.
        if self.over_ceiling() {
            return Ok(Stop::OutOfMemory(site_span(tree, self.site)));
        }
        while let Some(frame) = self.frames.pop() {
            // A frame with no place of its own (a scope exit) keeps the last one.
            if let site @ (Site::Statement(_)
            | Site::Expression(_)
            | Site::Span(_)
            | Site::Link(..)) = self.site(&frame)
            {
                self.site = site;
            }
            let site = self.site;
            let stop = self.step(tree, frame)?;
            if self.over_ceiling() {
                return Ok(Stop::OutOfMemory(site_span(tree, site)));
            }
            if let Some(stop) = stop {
                return Ok(stop);
            }
            self.work = self.work.saturating_add(1);
            if let Some(slice) = self.meter.slice
                && self.work >= slice.get()
                && !self.frames.is_empty()
            {
                self.work = 0;
                return Ok(Stop::Paused(site_span(tree, site)));
            }
        }
        Ok(Stop::Finished(Value::Null))
    }

    /// Whether the heap holds more than the memory ceiling.
    fn over_ceiling(&self) -> bool {
        self.meter
            .memory_ceiling
            .is_some_and(|ceiling| self.heap.used() > ceiling)
    }

    /// Whether `extra` more bytes would take the heap past the memory ceiling.
    fn would_cross_ceiling(&self, extra: usize) -> bool {
        self.meter
            .memory_ceiling
            .is_some_and(|ceiling| self.heap.used().saturating_add(extra) > ceiling)
    }

    /// Count the work of reading or writing `bytes` of string towards the slice.
    fn burn(&mut self, bytes: usize) {
        self.work = self.work.saturating_add((bytes / BYTES_PER_UNIT) as u64);
    }

    /// Count the work of reading `value` towards the slice, when it is a string.
    fn burn_text(&mut self, value: &RtValue) {
        if let RtValue::String(text) = value {
            self.burn(text.len());
        }
    }

    /// Where `frame` is in the Script.
    fn site(&self, frame: &Frame) -> Site {
        match frame {
            Frame::Statement(at)
            | Frame::Declare(at)
            | Frame::Branch { at, .. }
            | Frame::ForStart(at) => Site::Statement(*at),
            Frame::Eval(id)
            | Frame::Return(id)
            | Frame::BuildArray { array: id, .. }
            | Frame::BuildObject(id)
            | Frame::AssignName(id)
            | Frame::AssignMember(id) => Site::Expression(*id),
            Frame::Unary { operator, .. } => Site::Span(operator.span),
            Frame::BinaryRight { operator, .. } | Frame::Binary { operator, .. } => {
                Site::Span(operator.span)
            }
            Frame::Link { chain, index, .. }
            | Frame::IndexRead { chain, index, .. }
            | Frame::Invoke { chain, index, .. }
            | Frame::CallEnd { chain, index, .. } => Site::Link(*chain, *index),
            Frame::HostCall { chain, .. } => Site::Link(*chain, 0),
            Frame::Loop(state) | Frame::LoopTest(state) => match &state.kind {
                LoopKind::While { keyword, .. } | LoopKind::For { keyword, .. } => {
                    Site::Span(*keyword)
                }
            },
            Frame::ExitScope(_) | Frame::Discard => Site::Unknown,
        }
    }

    /// Answer the host call the machine stopped on with `value`, which becomes the call's value
    /// exactly as if an ordinary call had returned it. Without a pending host call, does
    /// nothing.
    pub(crate) fn resume(&mut self, value: &Value) {
        if let Some((chain, in_target)) = self.suspended.take() {
            let value = self.heap.attach(value);
            self.values.push(value);
            self.frames.push(Frame::Link {
                chain,
                index: 1,
                in_target,
            });
        }
    }

    /// The names bound in the root scope: the starting variables, the top-level named functions,
    /// and whatever the Script's own top level has declared by now.
    pub(crate) fn root_names(&self) -> Vec<String> {
        self.heap.names(self.root)
    }

    /// Schedule the statements of `block` (the top level for `None`) in the current scope, after
    /// hoisting the named functions they declare (decision 3: every `fn name` in a block is bound
    /// before the block runs, so mutual recursion works in any declaration order).
    fn open(&mut self, tree: &Program, block: Option<BlockId>) {
        let statements = statements(tree, block);
        for (index, statement) in statements.iter().enumerate() {
            if let StatementKind::Function { name, .. } = &statement.kind {
                let value = self.make_function(FnAt::Declared(StmtAt { block, index }));
                self.heap.declare(self.scope, &name.name, value);
            }
        }
        self.frames.extend(
            (0..statements.len())
                .rev()
                .map(|index| Frame::Statement(StmtAt { block, index })),
        );
    }

    /// Enter `block` in a fresh scope nested in the current one, arranging for that scope to be
    /// reclaimed when the block's statements are done.
    fn enter(&mut self, tree: &Program, block: BlockId) {
        let inner = self.heap.push_scope(Some(self.scope));
        let outer = core::mem::replace(&mut self.scope, inner);
        self.frames.push(Frame::ExitScope(outer));
        self.open(tree, Some(block));
    }

    /// Reclaim the current scope (unless a closure captured it) and restore `outer`.
    fn exit(&mut self, outer: SlotId) {
        self.heap.release_scope(self.scope);
        self.scope = outer;
    }

    /// A function value closing over the current scope, which is marked captured so the scope —
    /// and its ancestors, which lookups walk — outlive the block that created it.
    fn make_function(&mut self, at: FnAt) -> RtValue {
        let definition = match self.definitions.get(&at) {
            Some(index) => *index,
            None => {
                let index = self.functions.len();
                self.functions.push(at);
                self.definitions.insert(at, index);
                index
            }
        };
        self.heap.mark_captured(self.scope);
        self.heap.new_function(definition, self.scope)
    }

    /// Execute one frame. `Some` means the machine stops: the Script result from a `return`, or
    /// a host call.
    fn step(&mut self, tree: &Program, frame: Frame) -> Result<Option<Stop>, Diagnostic> {
        match frame {
            Frame::Statement(at) => {
                return Ok(self.statement(tree, at)?.map(Stop::Finished));
            }
            // A scope a closure captured is skipped here and lives until the execution ends,
            // like any other heap garbage; see `environment.rs`.
            Frame::ExitScope(outer) => self.exit(outer),
            Frame::Eval(id) => self.expression(tree, id)?,
            Frame::Discard => {
                self.pop();
            }
            Frame::Declare(at) => {
                let value = self.pop();
                if let StatementKind::Let { name, .. } = &statement(tree, at).kind {
                    self.heap.declare(self.scope, &name.name, value);
                }
            }
            Frame::Return(expression) => {
                let value = self.pop();
                if self.depth > 0 {
                    // Inside a call: this returns from the innermost function, not the Script.
                    self.return_from_call(value);
                    return Ok(None);
                }
                return self
                    .script_result(tree, &value, expression)
                    .map(|result| Some(Stop::Finished(result)));
            }
            Frame::Unary { operator, operand } => {
                let value = self.pop();
                self.burn_text(&value);
                let result = self.unary(tree, operator, operand, &value)?;
                self.values.push(result);
            }
            Frame::BinaryRight {
                left,
                operator,
                right,
            } => self.binary_right(left, operator, right),
            Frame::Binary {
                left,
                operator,
                right,
            } => {
                let right_value = self.pop();
                let left_value = self.pop();
                self.burn_text(&left_value);
                self.burn_text(&right_value);
                if operator.kind == BinaryOperator::Add
                    && let Some(joined) = concatenated_len(&left_value, &right_value)
                    && self.would_cross_ceiling(joined)
                {
                    return Ok(Some(Stop::OutOfMemory(operator.span)));
                }
                let result = self.binary(tree, left, operator, right, &left_value, &right_value)?;
                self.values.push(result);
            }
            Frame::BuildArray { count, .. } => {
                let items = self.pop_many(count);
                let array = self.heap.new_array(items);
                self.values.push(array);
            }
            Frame::BuildObject(id) => {
                let expression = tree.expression(id);
                let ExpressionKind::Object { entries, .. } = &expression.kind else {
                    return Err(internal(expression.span));
                };
                let values = self.pop_many(entries.len());
                let map: IndexMap<Arc<str>, RtValue> = entries
                    .iter()
                    .zip(values)
                    .map(|(entry, value)| (Arc::from(entry.key.name.as_str()), value))
                    .collect();
                let object = self.heap.new_object(map);
                self.values.push(object);
            }
            Frame::Link {
                chain,
                index,
                in_target,
            } => self.link(tree, chain, index, in_target)?,
            Frame::IndexRead {
                chain,
                index,
                in_target,
            } => {
                let key = self.pop();
                let receiver = self.pop();
                self.burn_text(&key);
                let (_, links) = chain_links(tree, chain);
                let Some(link) = links.get(index) else {
                    return Err(internal(tree.expression(chain.access).span));
                };
                let AccessKind::Index { expression, .. } = &link.kind else {
                    return Err(internal(link.span));
                };
                let element = self.read_index(tree, &receiver, &key, link, *expression)?;
                self.values.push(element);
                self.frames.push(Frame::Link {
                    chain,
                    index: index + 1,
                    in_target,
                });
            }
            Frame::AssignName(target) => {
                let value = self.pop();
                let expression = tree.expression(target);
                let ExpressionKind::Identifier(name) = &expression.kind else {
                    return Err(internal(expression.span));
                };
                if self.heap.assign(self.scope, &name.name, value).is_err() {
                    return Err(Diagnostic::new(
                        Category::Reference,
                        Code::UNDECLARED_ASSIGNMENT,
                        format!(
                            "cannot assign to `{}`: it is not declared; declare it first with `let {} = …`",
                            name.name, name.name
                        ),
                        name.span,
                    ));
                }
            }
            Frame::AssignMember(target) => self.assign_member(tree, target)?,
            Frame::Branch { at, index } => {
                let taken = self.pop();
                if self.heap.is_truthy(&taken) {
                    if let StatementKind::If { branches, .. } = &statement(tree, at).kind
                        && let Some(branch) = branches.get(index)
                    {
                        self.enter(tree, branch.body);
                    }
                } else {
                    self.next_branch(tree, at, index + 1);
                }
            }
            Frame::Loop(state) => self.advance(tree, state)?,
            Frame::LoopTest(state) => {
                let condition = self.pop();
                if self.heap.is_truthy(&condition) {
                    let body = state.kind.body();
                    self.frames.push(Frame::Loop(state));
                    self.enter(tree, body);
                }
            }
            Frame::ForStart(at) => self.for_start(tree, at)?,
            Frame::Invoke {
                chain,
                index,
                in_target,
                arguments,
            } => self.invoke(tree, chain, index, in_target, arguments)?,
            Frame::CallEnd {
                outer_scope,
                values,
                chain,
                index,
                in_target,
            } => {
                // The body ran off its end without `return`, so the call yields null (§6).
                self.values.truncate(values);
                self.exit(outer_scope);
                self.depth -= 1;
                self.values.push(RtValue::Null);
                self.frames.push(Frame::Link {
                    chain,
                    index: index + 1,
                    in_target,
                });
            }
            Frame::HostCall { chain, in_target } => {
                return self.host_call(tree, chain, in_target).map(Some);
            }
        }
        Ok(None)
    }

    /// Detach the top-level `return`'s value into the Script result.
    fn script_result(
        &self,
        tree: &Program,
        value: &RtValue,
        expression: ExprId,
    ) -> Result<Value, Diagnostic> {
        let span = tree.expression(expression).span;
        match self.heap.detach(value) {
            Ok(result) => Ok(result),
            Err(DetachFailure::Cycle) => Err(Diagnostic::new(
                Category::Type,
                Code::CYCLIC_RESULT,
                format!(
                    "cannot return this {}: it contains a value that refers back to itself, and \
                     a Script result must be a finite tree of values",
                    value.type_name()
                ),
                span,
            )),
            // Decision 1: a function has no wire representation, so it cannot leave the
            // execution. Widening this into a callable handle later is not a breaking change.
            Err(DetachFailure::Function) => Err(Diagnostic::new(
                Category::Type,
                Code::FUNCTION_RESULT,
                if matches!(value, RtValue::Function(_)) {
                    "cannot return a function: a Script result must be data the Backend can \
                     receive"
                        .to_owned()
                } else {
                    format!(
                        "cannot return this {}: it contains a function, and a Script result must \
                         be data the Backend can receive",
                        value.type_name()
                    )
                },
                span,
            )),
        }
    }

    /// Suspend on the host call whose arguments are on the value stack: detach each one, as a
    /// Script result is detached, and remember where to carry on.
    fn host_call(
        &mut self,
        tree: &Program,
        chain: Chain,
        in_target: bool,
    ) -> Result<Stop, Diagnostic> {
        let (base, links) = chain_links(tree, chain);
        let base = tree.expression(base);
        let (Some(link), ExpressionKind::Identifier(name)) = (links.first(), &base.kind) else {
            return Err(internal(base.span));
        };
        let AccessKind::Call { arguments, .. } = &link.kind else {
            return Err(internal(link.span));
        };
        let values = self.pop_many(arguments.len());
        let mut detached = Vec::with_capacity(values.len());
        for (value, argument) in values.iter().zip(arguments) {
            let span = tree.expression(*argument).span;
            let (code, reason) = match self.heap.detach(value) {
                Ok(value) => {
                    detached.push(Argument { value, span });
                    continue;
                }
                Err(DetachFailure::Cycle) => (
                    Code::CYCLIC_ARGUMENT,
                    "it contains a value that refers back to itself",
                ),
                Err(DetachFailure::Function) if matches!(value, RtValue::Function(_)) => (
                    Code::FUNCTION_ARGUMENT,
                    "a function has no wire representation",
                ),
                Err(DetachFailure::Function) => (
                    Code::FUNCTION_ARGUMENT,
                    "it contains a function, which has no wire representation",
                ),
            };
            return Err(Diagnostic::new(
                Category::Type,
                code,
                format!(
                    "cannot pass this {} to `{}`: {reason}, and a host call's arguments must be \
                     data the Backend can receive",
                    value.type_name(),
                    name.name
                ),
                span,
            ));
        }
        self.suspended = Some((chain, in_target));
        let span = through(name.span, link.span);
        // Until the next frame runs after it resumes, the host call is the running construct:
        // a value it brings back that crosses the memory ceiling is spanned on it.
        self.site = Site::Span(span);
        Ok(Stop::HostCall(PendingCall {
            name: name.name.clone(),
            arguments: detached,
            span: through(name.span, link.span),
        }))
    }

    /// Test the `if` statement's `branches[index]`, or fall through to its `else` body when there
    /// is none left.
    fn next_branch(&mut self, tree: &Program, at: StmtAt, index: usize) {
        let StatementKind::If {
            branches,
            else_branch,
        } = &statement(tree, at).kind
        else {
            return;
        };
        if let Some(branch) = branches.get(index) {
            self.frames.push(Frame::Branch { at, index });
            self.frames.push(Frame::Eval(branch.condition.expression));
        } else if let Some(otherwise) = else_branch {
            self.enter(tree, otherwise.body);
        }
    }

    /// Open the `for` loop at `at` over the iterable on top of the value stack.
    fn for_start(&mut self, tree: &Program, at: StmtAt) -> Result<(), Diagnostic> {
        let value = self.pop();
        let statement = statement(tree, at);
        let StatementKind::For {
            keyword,
            iterable,
            body,
            ..
        } = &statement.kind
        else {
            return Err(internal(statement.span));
        };
        let (collection, keys) = match value {
            RtValue::Array(id) => (id, None),
            RtValue::Object(id) => {
                let keys = self.heap.object_keys(id);
                self.work = self.work.saturating_add(keys.len() as u64);
                (id, Some(keys))
            }
            other => {
                return Err(Diagnostic::new(
                    Category::Type,
                    Code::OPERAND_MISMATCH,
                    format!(
                        "cannot iterate {}: `for … in` needs an array or an object",
                        article(&other)
                    ),
                    tree.expression(*iterable).span,
                ));
            }
        };
        self.frames.push(Frame::Loop(Box::new(LoopFrame {
            outer_scope: self.scope,
            values: self.values.len(),
            kind: LoopKind::For {
                keyword: *keyword,
                at,
                body: *body,
                collection,
                version: self.heap.version(collection),
                index: 0,
                keys,
            },
        })));
        Ok(())
    }

    /// Advance a loop by one iteration, or let it end by not re-scheduling itself.
    fn advance(&mut self, tree: &Program, mut state: Box<LoopFrame>) -> Result<(), Diagnostic> {
        match &mut state.kind {
            LoopKind::While { condition, .. } => {
                let condition = *condition;
                self.frames.push(Frame::LoopTest(state));
                self.frames.push(Frame::Eval(condition));
            }
            LoopKind::For {
                keyword,
                at,
                body,
                collection,
                version,
                index,
                keys,
            } => {
                if self.heap.version(*collection) != *version {
                    return Err(Diagnostic::new(
                        Category::Reference,
                        Code::COLLECTION_MUTATED,
                        "the collection this `for` loop is iterating was modified while it ran"
                            .to_owned(),
                        *keyword,
                    ));
                }
                let item = match keys {
                    // Charged in full like any new string: the value can outlive the object's key.
                    Some(keys) => keys.get(*index).map(|key| self.heap.text(Arc::clone(key))),
                    None => self.heap.array_get(*collection, *index),
                };
                let Some(item) = item else {
                    return Ok(()); // exhausted: the loop frame is not re-scheduled
                };
                *index += 1;
                let (at, body) = (*at, *body);
                // Decision 4: the binding and the body share one scope, fresh per iteration, so
                // a closure created in iteration i captures that iteration's value.
                let inner = self.heap.push_scope(Some(state.outer_scope));
                let outer = core::mem::replace(&mut self.scope, inner);
                if let StatementKind::For { binding, .. } = &statement(tree, at).kind {
                    self.heap.declare(inner, &binding.name, item);
                }
                self.frames.push(Frame::Loop(state));
                self.frames.push(Frame::ExitScope(outer));
                self.open(tree, Some(body));
            }
        }
        Ok(())
    }

    /// Unwind to the innermost loop boundary. `break` consumes it; `continue` leaves it in
    /// place, so the next step re-tests the condition or takes the next element.
    fn unwind_to_loop(&mut self, consume: bool) {
        while let Some(frame) = self.frames.pop() {
            match frame {
                Frame::ExitScope(outer) => self.exit(outer),
                Frame::Loop(state) => {
                    self.values.truncate(state.values);
                    self.scope = state.outer_scope;
                    if !consume {
                        self.frames.push(Frame::Loop(state));
                    }
                    return;
                }
                // The parser rejects loop control that is not inside a loop of its own function
                // body, so a call boundary can only be reached here from a hand-built tree. The
                // frames above it are already gone, so the call cannot be resumed correctly;
                // put the boundary back and stop, which ends the call with `null` instead of
                // unwinding past it into the caller's loop.
                Frame::CallEnd { .. } => {
                    self.frames.push(frame);
                    return;
                }
                _ => {}
            }
        }
    }

    /// Return `value` from the innermost call, resuming the access chain the call sits in.
    fn return_from_call(&mut self, value: RtValue) {
        while let Some(frame) = self.frames.pop() {
            match frame {
                Frame::ExitScope(outer) => self.exit(outer),
                Frame::CallEnd {
                    outer_scope,
                    values,
                    chain,
                    index,
                    in_target,
                } => {
                    self.values.truncate(values);
                    self.exit(outer_scope);
                    self.depth -= 1;
                    self.values.push(value);
                    self.frames.push(Frame::Link {
                        chain,
                        index: index + 1,
                        in_target,
                    });
                    return;
                }
                _ => {}
            }
        }
    }

    /// Call the function under its arguments on the value stack.
    fn invoke(
        &mut self,
        tree: &Program,
        chain: Chain,
        index: usize,
        in_target: bool,
        arguments: usize,
    ) -> Result<(), Diagnostic> {
        let (_, links) = chain_links(tree, chain);
        let Some(link) = links.get(index) else {
            return Err(internal(tree.expression(chain.access).span));
        };
        let values = self.pop_many(arguments);
        let callee = self.pop();
        let RtValue::Function(slot) = callee else {
            return Err(Diagnostic::new(
                Category::Type,
                Code::NOT_CALLABLE,
                format!(
                    "cannot call {}: only functions can be called",
                    article(&callee)
                ),
                link.span,
            ));
        };
        let Some((definition, captured)) = self.heap.function(slot) else {
            return Err(internal(link.span));
        };
        let Some(function) = self
            .functions
            .get(definition)
            .and_then(|at| function(tree, *at))
        else {
            return Err(internal(link.span));
        };
        if values.len() != function.parameters.len() {
            return Err(Diagnostic::new(
                Category::Arity,
                Code::ARGUMENT_COUNT,
                format!(
                    "this call passes {} argument{}, but the function takes {}",
                    values.len(),
                    if values.len() == 1 { "" } else { "s" },
                    function.parameters.len()
                ),
                link.span,
            ));
        }
        if self.depth >= CALL_DEPTH_LIMIT {
            return Err(Diagnostic::new(
                Category::Depth,
                Code::CALL_DEPTH_EXCEEDED,
                format!("too many nested calls: the limit is {CALL_DEPTH_LIMIT}"),
                link.span,
            ));
        }
        self.depth += 1;
        // The call scope's parent is the callee's captured scope, never the caller's (§6).
        let inner = self.heap.push_scope(Some(captured));
        let outer = core::mem::replace(&mut self.scope, inner);
        self.frames.push(Frame::CallEnd {
            outer_scope: outer,
            values: self.values.len(),
            chain,
            index,
            in_target,
        });
        for (parameter, value) in function.parameters.iter().zip(values) {
            self.heap.declare(inner, &parameter.name, value);
        }
        self.open(tree, Some(function.body));
        Ok(())
    }

    fn statement(&mut self, tree: &Program, at: StmtAt) -> Result<Option<Value>, Diagnostic> {
        match &statement(tree, at).kind {
            StatementKind::Let { initializer, .. } => {
                self.frames.push(Frame::Declare(at));
                self.frames.push(Frame::Eval(*initializer));
            }
            StatementKind::Expression(id) => {
                self.frames.push(Frame::Discard);
                self.frames.push(Frame::Eval(*id));
            }
            StatementKind::Return { value, .. } => match value {
                Some(id) => {
                    self.frames.push(Frame::Return(*id));
                    self.frames.push(Frame::Eval(*id));
                }
                // A bare `return` yields null (§5) — from the function when inside a call, and
                // otherwise as the Script result.
                None if self.depth > 0 => self.return_from_call(RtValue::Null),
                None => return Ok(Some(Value::Null)),
            },
            StatementKind::Block(id) => self.enter(tree, *id),
            StatementKind::Assignment { target, value, .. } => {
                self.assignment(tree, *target, *value)?;
            }
            // Already bound by `open` before this block's statements ran (decision 3).
            StatementKind::Function { .. } => {}
            StatementKind::If { .. } => self.next_branch(tree, at, 0),
            StatementKind::While {
                keyword,
                condition,
                body,
            } => {
                self.frames.push(Frame::Loop(Box::new(LoopFrame {
                    outer_scope: self.scope,
                    values: self.values.len(),
                    kind: LoopKind::While {
                        keyword: *keyword,
                        condition: condition.expression,
                        body: *body,
                    },
                })));
            }
            StatementKind::For { iterable, .. } => {
                self.frames.push(Frame::ForStart(at));
                self.frames.push(Frame::Eval(*iterable));
            }
            StatementKind::Break { .. } => self.unwind_to_loop(true),
            StatementKind::Continue { .. } => self.unwind_to_loop(false),
        }
        Ok(None)
    }

    fn assignment(
        &mut self,
        tree: &Program,
        target: ExprId,
        value: ExprId,
    ) -> Result<(), Diagnostic> {
        let target_expression = tree.expression(target);
        match &target_expression.kind {
            ExpressionKind::Identifier(_) => {
                self.frames.push(Frame::AssignName(target));
                self.frames.push(Frame::Eval(value));
            }
            ExpressionKind::Access { links, .. } => {
                let Some((link, prefix)) = links.split_last() else {
                    return Err(invalid_target(target_expression.span));
                };
                if prefix.iter().any(|l| l.optional) || link.optional {
                    return Err(invalid_target(target_expression.span));
                }
                // Order: receiver, then index key, then the assigned value.
                self.frames.push(Frame::AssignMember(target));
                self.frames.push(Frame::Eval(value));
                match &link.kind {
                    AccessKind::Property(_) => {}
                    AccessKind::Index { expression, .. } => {
                        self.frames.push(Frame::Eval(*expression));
                    }
                    AccessKind::Call { .. } => return Err(invalid_target(target_expression.span)),
                }
                self.open_chain(tree, target, prefix.len(), true)?;
            }
            _ => return Err(invalid_target(target_expression.span)),
        }
        Ok(())
    }

    fn expression(&mut self, tree: &Program, id: ExprId) -> Result<(), Diagnostic> {
        let expression = tree.expression(id);
        match &expression.kind {
            ExpressionKind::Literal(literal) => self.values.push(match literal {
                Literal::Null => RtValue::Null,
                Literal::Bool(b) => RtValue::Bool(*b),
                Literal::Number(n) => RtValue::Number(*n),
                Literal::String(s) => self.heap.text(s.as_str()),
            }),
            ExpressionKind::Identifier(name) => {
                // A host function is not a value (§8): only calling a bare undeclared name
                // reaches the host, so naming one without calling it is still undeclared.
                let Some(value) = self.heap.lookup(self.scope, &name.name) else {
                    return Err(Diagnostic::new(
                        Category::Reference,
                        Code::UNDECLARED_IDENTIFIER,
                        format!("`{}` is not declared", name.name),
                        name.span,
                    ));
                };
                self.values.push(value);
            }
            ExpressionKind::Group { expression, .. } => self.frames.push(Frame::Eval(*expression)),
            ExpressionKind::Unary { operator, operand } => {
                self.frames.push(Frame::Unary {
                    operator: *operator,
                    operand: *operand,
                });
                self.frames.push(Frame::Eval(*operand));
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                self.frames.push(Frame::BinaryRight {
                    left: *left,
                    operator: *operator,
                    right: *right,
                });
                self.frames.push(Frame::Eval(*left));
            }
            ExpressionKind::Array { elements, .. } => {
                self.frames.push(Frame::BuildArray {
                    array: id,
                    count: elements.len(),
                });
                self.frames
                    .extend(elements.iter().rev().map(|e| Frame::Eval(*e)));
            }
            ExpressionKind::Object { entries, .. } => {
                self.frames.push(Frame::BuildObject(id));
                self.frames
                    .extend(entries.iter().rev().map(|e| Frame::Eval(e.value)));
            }
            ExpressionKind::Access { links, .. } => {
                self.open_chain(tree, id, links.len(), false)?;
            }
            ExpressionKind::Function(_) => {
                let value = self.make_function(FnAt::Literal(id));
                self.values.push(value);
            }
        }
        Ok(())
    }

    /// Schedule the first `end` links of the access chain `access`, starting with its base.
    ///
    /// When the base is a bare name that no scope declares and the first link calls it, the chain
    /// starts with a host call instead (§8): its arguments are evaluated and the machine stops.
    /// A local binding of the name — `let`, parameter, named `fn`, starting variable — makes it an
    /// ordinary call.
    fn open_chain(
        &mut self,
        tree: &Program,
        access: ExprId,
        end: usize,
        in_target: bool,
    ) -> Result<(), Diagnostic> {
        let expression = tree.expression(access);
        let ExpressionKind::Access { base, links } = &expression.kind else {
            return Err(internal(expression.span));
        };
        let chain = Chain { access, end };
        if let Some(AccessLink {
            kind: AccessKind::Call { arguments, .. },
            ..
        }) = links.get(..end).and_then(<[AccessLink]>::first)
            && let ExpressionKind::Identifier(name) = &tree.expression(*base).kind
            && self.heap.lookup(self.scope, &name.name).is_none()
        {
            self.frames.push(Frame::HostCall { chain, in_target });
            self.frames
                .extend(arguments.iter().rev().map(|a| Frame::Eval(*a)));
            return Ok(());
        }
        self.frames.push(Frame::Link {
            chain,
            index: 0,
            in_target,
        });
        self.frames.push(Frame::Eval(*base));
        Ok(())
    }

    /// Apply the chain's `links[index]` to the receiver on top of the value stack.
    fn link(
        &mut self,
        tree: &Program,
        chain: Chain,
        index: usize,
        in_target: bool,
    ) -> Result<(), Diagnostic> {
        let (base, links) = chain_links(tree, chain);
        let Some(link) = links.get(index) else {
            return Ok(()); // chain complete; its value is on the stack
        };
        let receiver_is_null = self.values.last().is_none_or(RtValue::is_null);
        if receiver_is_null && link.optional {
            // §4.4: `?.` on null short-circuits the whole remaining chain; `null` stays as the
            // chain's value and no later link (or index expression) is evaluated.
            return Ok(());
        }
        if let AccessKind::Call { arguments, .. } = &link.kind {
            // Calls are handled before the null check: calling `null` is `type.not_callable`,
            // not a null access, and there is no optional call form to suppress it (§4.4).
            self.frames.push(Frame::Invoke {
                chain,
                index,
                in_target,
                arguments: arguments.len(),
            });
            self.frames
                .extend(arguments.iter().rev().map(|a| Frame::Eval(*a)));
            return Ok(());
        }
        if receiver_is_null {
            // `?.` is not allowed in an assignment target, so only suggest it for plain reads.
            return Err(null_access(
                tree,
                base,
                &links[..index],
                link,
                "read",
                !in_target,
            ));
        }
        match &link.kind {
            AccessKind::Property(name) => {
                let receiver = self.pop();
                let value = self.read_property(&receiver, name, link)?;
                self.values.push(value);
                self.frames.push(Frame::Link {
                    chain,
                    index: index + 1,
                    in_target,
                });
            }
            AccessKind::Index { expression, .. } => {
                self.frames.push(Frame::IndexRead {
                    chain,
                    index,
                    in_target,
                });
                self.frames.push(Frame::Eval(*expression));
            }
            // Scheduled above, before the null check.
            AccessKind::Call { .. } => {}
        }
        Ok(())
    }

    fn read_index(
        &self,
        tree: &Program,
        receiver: &RtValue,
        key: &RtValue,
        link: &AccessLink,
        key_expression: ExprId,
    ) -> Result<RtValue, Diagnostic> {
        let key_span = tree.expression(key_expression).span;
        match (receiver, key) {
            (RtValue::Array(array), RtValue::Number(n)) => {
                // §7: anything that is not an in-range whole index reads as absent.
                Ok(array_slot(*n)
                    .and_then(|i| self.heap.array_get(*array, i))
                    .unwrap_or(RtValue::Null))
            }
            (RtValue::Object(object), RtValue::String(k)) => {
                Ok(self.heap.object_get(*object, k).unwrap_or(RtValue::Null))
            }
            (RtValue::Array(_) | RtValue::Object(_), _) => Err(wrong_key(receiver, key, key_span)),
            _ => Err(not_indexable(receiver, link.span)),
        }
    }

    /// Store the assigned value through the last link of the access expression `target`.
    fn assign_member(&mut self, tree: &Program, target: ExprId) -> Result<(), Diagnostic> {
        let value = self.pop();
        let expression = tree.expression(target);
        let ExpressionKind::Access { base, links } = &expression.kind else {
            return Err(internal(expression.span));
        };
        let Some((link, prefix)) = links.split_last() else {
            return Err(internal(expression.span));
        };
        match &link.kind {
            AccessKind::Property(name) => {
                let receiver = self.pop();
                match &receiver {
                    RtValue::Object(object) => {
                        self.heap.object_store(*object, &name.name, value);
                        Ok(())
                    }
                    RtValue::Null => Err(null_access(tree, *base, prefix, link, "assign", false)),
                    other => Err(no_properties(other, name, link.span)),
                }
            }
            AccessKind::Index { expression, .. } => {
                let key = self.pop();
                let receiver = self.pop();
                self.burn_text(&key);
                let key_span = tree.expression(*expression).span;
                match (&receiver, &key) {
                    (RtValue::Null, _) => {
                        Err(null_access(tree, *base, prefix, link, "assign", false))
                    }
                    (RtValue::Array(array), RtValue::Number(n)) => {
                        if array_slot(*n).is_some_and(|i| self.heap.array_store(*array, i, value)) {
                            Ok(())
                        } else {
                            Err(Diagnostic::new(
                                Category::Reference,
                                Code::INDEX_OUT_OF_RANGE,
                                format!(
                                    "cannot assign at index {} of an array of length {}: only an \
                                     existing index or the length (to append) can be written",
                                    number_to_string(*n),
                                    self.heap.array_len(*array)
                                ),
                                key_span,
                            ))
                        }
                    }
                    (RtValue::Object(object), RtValue::String(k)) => {
                        self.heap.object_store(*object, k, value);
                        Ok(())
                    }
                    (RtValue::Array(_) | RtValue::Object(_), _) => {
                        Err(wrong_key(&receiver, &key, key_span))
                    }
                    _ => Err(not_indexable(&receiver, link.span)),
                }
            }
            AccessKind::Call { .. } => Err(internal(link.span)),
        }
    }

    fn binary_right(&mut self, left: ExprId, operator: Spanned<BinaryOperator>, right: ExprId) {
        match operator.kind {
            // §4.1: `||` keeps a truthy left, `&&` keeps a falsy left; otherwise the result is
            // the right operand. The decided case never evaluates the right side.
            BinaryOperator::Or | BinaryOperator::And => {
                let left_truthy = self.values.last().is_some_and(|v| self.heap.is_truthy(v));
                if left_truthy == (operator.kind == BinaryOperator::Or) {
                    return;
                }
                self.pop();
                self.frames.push(Frame::Eval(right));
            }
            _ => {
                self.frames.push(Frame::Binary {
                    left,
                    operator,
                    right,
                });
                self.frames.push(Frame::Eval(right));
            }
        }
    }

    fn unary(
        &self,
        tree: &Program,
        operator: Spanned<UnaryOperator>,
        operand: ExprId,
        value: &RtValue,
    ) -> Result<RtValue, Diagnostic> {
        match operator.kind {
            UnaryOperator::Not => Ok(RtValue::Bool(!self.heap.is_truthy(value))),
            UnaryOperator::Negate => match to_number(value) {
                Some(n) => Ok(RtValue::Number(-n)),
                None => Err(Diagnostic::new(
                    Category::Type,
                    Code::OPERAND_MISMATCH,
                    format!(
                        "cannot apply unary `-` to {}: {}",
                        article(value),
                        not_a_number_reason(value)
                    ),
                    tree.expression(operand).span,
                )),
            },
        }
    }

    fn binary(
        &self,
        tree: &Program,
        left: ExprId,
        operator: Spanned<BinaryOperator>,
        right: ExprId,
        l: &RtValue,
        r: &RtValue,
    ) -> Result<RtValue, Diagnostic> {
        use BinaryOperator as Op;
        let left_span = tree.expression(left).span;
        let right_span = tree.expression(right).span;
        let symbol = binary_symbol(operator.kind);
        let mismatch = |span: Span, reason: String| {
            Diagnostic::new(
                Category::Type,
                Code::OPERAND_MISMATCH,
                format!(
                    "cannot apply `{symbol}` to {} and {}: {reason}",
                    l.type_name(),
                    r.type_name()
                ),
                span,
            )
        };
        let number = |value: &RtValue, span: Span| {
            to_number(value).ok_or_else(|| mismatch(span, not_a_number_reason(value).to_owned()))
        };
        let finite = |n: f64| {
            if n.is_finite() {
                Ok(RtValue::Number(n))
            } else {
                Err(Diagnostic::new(
                    Category::Arithmetic,
                    Code::NON_FINITE,
                    format!("the result of `{symbol}` is not a finite number"),
                    operator.span,
                ))
            }
        };
        match operator.kind {
            Op::Add => {
                if matches!(l, RtValue::String(_)) || matches!(r, RtValue::String(_)) {
                    let not_text = |value: &RtValue, span: Span| {
                        mismatch(
                            span,
                            format!("{} cannot be converted to a string", article(value)),
                        )
                    };
                    let a = to_string(l).ok_or_else(|| not_text(l, left_span))?;
                    let b = to_string(r).ok_or_else(|| not_text(r, right_span))?;
                    let mut joined = String::with_capacity(a.len() + b.len());
                    joined.push_str(&a);
                    joined.push_str(&b);
                    Ok(self.heap.text(joined))
                } else {
                    finite(number(l, left_span)? + number(r, right_span)?)
                }
            }
            Op::Subtract => finite(number(l, left_span)? - number(r, right_span)?),
            Op::Multiply => finite(number(l, left_span)? * number(r, right_span)?),
            Op::Divide | Op::Remainder => {
                let a = number(l, left_span)?;
                let b = number(r, right_span)?;
                if b == 0.0 {
                    return Err(Diagnostic::new(
                        Category::Arithmetic,
                        Code::DIVISION_BY_ZERO,
                        format!("`{symbol}` by zero"),
                        right_span,
                    ));
                }
                finite(if operator.kind == Op::Divide {
                    a / b
                } else {
                    a % b
                })
            }
            Op::Less | Op::LessEqual | Op::Greater | Op::GreaterEqual => {
                let ordering = if let (RtValue::String(a), RtValue::String(b)) = (l, r) {
                    // UTF-8 byte order is Unicode code point order.
                    a.as_str().partial_cmp(b.as_str())
                } else {
                    number(l, left_span)?.partial_cmp(&number(r, right_span)?)
                };
                let result = ordering.is_some_and(|o| match operator.kind {
                    Op::Less => o.is_lt(),
                    Op::LessEqual => o.is_le(),
                    Op::Greater => o.is_gt(),
                    _ => o.is_ge(),
                });
                Ok(RtValue::Bool(result))
            }
            Op::Equal => Ok(RtValue::Bool(l.equals(r))),
            Op::NotEqual => Ok(RtValue::Bool(!l.equals(r))),
            // Resolved in `binary_right`; reaching here means both operands were evaluated.
            Op::And | Op::Or => Ok(r.clone()),
        }
    }

    fn pop(&mut self) -> RtValue {
        debug_assert!(!self.values.is_empty(), "value stack underflow");
        self.values.pop().unwrap_or(RtValue::Null)
    }

    fn pop_many(&mut self, count: usize) -> Vec<RtValue> {
        debug_assert!(self.values.len() >= count, "value stack underflow");
        let at = self.values.len().saturating_sub(count);
        self.values.split_off(at)
    }

    fn read_property(
        &self,
        receiver: &RtValue,
        name: &Identifier,
        link: &AccessLink,
    ) -> Result<RtValue, Diagnostic> {
        match receiver {
            RtValue::Object(object) => Ok(self
                .heap
                .object_get(*object, &name.name)
                .unwrap_or(RtValue::Null)),
            other => Err(no_properties(other, name, link.span)),
        }
    }
}

/// What `left + right` would allocate as a string concatenation, when it is one whose operands
/// both convert (§4.3): the joined text and that string's fixed overhead — what the new value
/// will hold. The transient copy made turning the built `String` into the shared string is not
/// counted: it lives only within this one step, the overshoot the ceiling allows. An upper bound, since a number spells in
/// at most 32 bytes. `None` when neither side is a string, or when either side cannot convert —
/// that is a `type` error, which the ceiling must not mask.
fn concatenated_len(left: &RtValue, right: &RtValue) -> Option<usize> {
    if !matches!(left, RtValue::String(_)) && !matches!(right, RtValue::String(_)) {
        return None;
    }
    let len = |value: &RtValue| match value {
        RtValue::String(text) => Some(text.len()),
        RtValue::Null | RtValue::Bool(_) | RtValue::Number(_) => Some(32),
        RtValue::Array(_) | RtValue::Object(_) | RtValue::Function(_) => None,
    };
    let joined = len(left)?.saturating_add(len(right)?);
    Some(joined.saturating_add(TEXT_OVERHEAD))
}

/// The span of `site`; for a frame with none of its own, the whole Program.
fn site_span(tree: &Program, site: Site) -> Span {
    match site {
        Site::Statement(at) => statement(tree, at).span,
        Site::Expression(id) => tree.expression(id).span,
        Site::Span(span) => span,
        Site::Link(chain, index) => {
            let (_, links) = chain_links(tree, chain);
            links
                .get(index)
                .map_or_else(|| tree.expression(chain.access).span, |link| link.span)
        }
        Site::Unknown => tree.span,
    }
}

/// A `reference` error for applying `link` to `null`, naming what produced the null: the last of
/// `before` (the chain's earlier links), or the chain's base.
fn null_access(
    tree: &Program,
    base: ExprId,
    before: &[AccessLink],
    link: &AccessLink,
    verb: &str,
    suggest_optional: bool,
) -> Diagnostic {
    let subject = match before.last() {
        None => match &tree.expression(base).kind {
            ExpressionKind::Identifier(name) => format!("`{}`", name.name),
            ExpressionKind::Literal(Literal::Null) => "`null`".to_owned(),
            _ => "the value".to_owned(),
        },
        Some(previous) => match &previous.kind {
            AccessKind::Property(name) => format!("`{}`", name.name),
            AccessKind::Index { .. } => "the indexed element".to_owned(),
            AccessKind::Call { .. } => "the call result".to_owned(),
        },
    };
    let what = match &link.kind {
        AccessKind::Property(name) => format!("property `{}`", name.name),
        _ => "an index".to_owned(),
    };
    let hint = if suggest_optional {
        "; use `?.` to read it as null instead"
    } else {
        ""
    };
    Diagnostic::new(
        Category::Reference,
        Code::NULL_ACCESS,
        format!("cannot {verb} {what}: {subject} is null{hint}"),
        link.span,
    )
}

/// The statements of `block`, or the Program's top level for `None`.
fn statements(tree: &Program, block: Option<BlockId>) -> &[Statement] {
    match block {
        None => &tree.statements,
        Some(id) => &tree.block(id).statements,
    }
}

/// The statement at `at`. Positions are only ever made from the statements they name.
fn statement(tree: &Program, at: StmtAt) -> &Statement {
    &statements(tree, at.block)[at.index]
}

/// The `Function` at `at`; `None` only if `at` names something else, which the machine never
/// records.
fn function(tree: &Program, at: FnAt) -> Option<&Function> {
    match at {
        FnAt::Declared(at) => match &statement(tree, at).kind {
            StatementKind::Function { function, .. } => Some(function),
            _ => None,
        },
        FnAt::Literal(id) => match &tree.expression(id).kind {
            ExpressionKind::Function(function) => Some(function),
            _ => None,
        },
    }
}

/// The chain's base and its links. A chain is only ever made from an `Access` expression; were
/// it not one, the chain would have no links and end at once.
fn chain_links(tree: &Program, chain: Chain) -> (ExprId, &[AccessLink]) {
    match &tree.expression(chain.access).kind {
        ExpressionKind::Access { base, links } => (*base, links.get(..chain.end).unwrap_or(links)),
        _ => (chain.access, &[]),
    }
}

/// The span from the start of `first` through the end of `last`, both on the same source.
const fn through(first: Span, last: Span) -> Span {
    Span::new(
        first.offset,
        last.end().saturating_sub(first.offset),
        first.line,
        first.column,
    )
}

/// A whole, non-negative number as an array position.
fn array_slot(n: f64) -> Option<usize> {
    if n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0 {
        // In range and whole, so the conversion is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(n as usize)
    } else {
        None
    }
}

fn no_properties(receiver: &RtValue, name: &Identifier, span: Span) -> Diagnostic {
    Diagnostic::new(
        Category::Type,
        Code::INVALID_PROPERTY_ACCESS,
        format!(
            "cannot access property `{}` on {}: only objects have properties",
            name.name,
            article(receiver)
        ),
        span,
    )
}

fn wrong_key(receiver: &RtValue, key: &RtValue, span: Span) -> Diagnostic {
    let expected = if matches!(receiver, RtValue::Array(_)) {
        "number"
    } else {
        "string"
    };
    Diagnostic::new(
        Category::Type,
        Code::INVALID_INDEX,
        format!(
            "cannot index {} with {}: {} indices must be {}s",
            article(receiver),
            article(key),
            receiver.type_name(),
            expected
        ),
        span,
    )
}

fn not_indexable(receiver: &RtValue, span: Span) -> Diagnostic {
    Diagnostic::new(
        Category::Type,
        Code::INVALID_INDEX,
        format!(
            "cannot index {}: only arrays and objects can be indexed",
            article(receiver)
        ),
        span,
    )
}

fn not_a_number_reason(value: &RtValue) -> &'static str {
    match value {
        RtValue::String(_) => "the string does not look like a number",
        RtValue::Array(_) => "an array cannot be converted to a number",
        RtValue::Object(_) => "an object cannot be converted to a number",
        _ => "the value cannot be converted to a number",
    }
}

fn article(value: &RtValue) -> &'static str {
    match value {
        RtValue::Null => "null",
        RtValue::Bool(_) => "a bool",
        RtValue::Number(_) => "a number",
        RtValue::String(_) => "a string",
        RtValue::Array(_) => "an array",
        RtValue::Object(_) => "an object",
        RtValue::Function(_) => "a function",
    }
}

const fn binary_symbol(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Remainder => "%",
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Less => "<",
        BinaryOperator::LessEqual => "<=",
        BinaryOperator::Greater => ">",
        BinaryOperator::GreaterEqual => ">=",
        BinaryOperator::Equal => "==",
        BinaryOperator::NotEqual => "!=",
        BinaryOperator::And => "&&",
        BinaryOperator::Or => "||",
    }
}

/// The parser never produces these shapes; a hand-built AST gets a diagnostic, not a panic.
fn invalid_target(span: Span) -> Diagnostic {
    Diagnostic::new(
        Category::Syntax,
        Code::INVALID_ASSIGNMENT_TARGET,
        "expected a name or ordinary property/index assignment target",
        span,
    )
}

pub(crate) fn internal(span: Span) -> Diagnostic {
    Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "malformed syntax tree",
        span,
    )
}

// These tests live here, not in `hexput-tests`, because they must observe the heap itself: that
// a cycle really is in it, that its contents are released when the `Machine` drops, and that
// block scopes are reclaimed. None of that is visible through the public API, which only ever
// sees detached results. Programs are built by hand because this crate depends on `hexput-ast`
// only — no parser.
#[cfg(test)]
mod tests {
    use hexput_ast::{Block, Expression};

    use super::*;

    const fn span() -> Span {
        Span::new(0, 0, 1, 1)
    }

    fn identifier(name: &str) -> Identifier {
        Identifier {
            name: name.to_owned(),
            span: span(),
        }
    }

    fn statement(kind: StatementKind) -> Statement {
        Statement {
            span: span(),
            terminator: None,
            kind,
        }
    }

    struct Build(Program);

    impl Build {
        fn new() -> Self {
            Self(Program {
                span: span(),
                statements: Vec::new(),
                blocks: Vec::new(),
                expressions: Vec::new(),
            })
        }

        fn expression(&mut self, kind: ExpressionKind) -> ExprId {
            self.0.expressions.push(Expression { span: span(), kind });
            ExprId(self.0.expressions.len() - 1)
        }

        fn name(&mut self, name: &str) -> ExprId {
            self.expression(ExpressionKind::Identifier(identifier(name)))
        }

        fn literal(&mut self, literal: Literal) -> ExprId {
            self.expression(ExpressionKind::Literal(literal))
        }

        fn array(&mut self, elements: Vec<ExprId>) -> ExprId {
            self.expression(ExpressionKind::Array {
                open: span(),
                elements,
                close: span(),
            })
        }

        fn index(&mut self, base: ExprId, index: ExprId) -> ExprId {
            self.expression(ExpressionKind::Access {
                base,
                links: vec![AccessLink {
                    span: span(),
                    operator: span(),
                    optional: false,
                    kind: AccessKind::Index {
                        open: span(),
                        expression: index,
                        close: span(),
                    },
                }],
            })
        }

        fn declare(&mut self, name: &str, initializer: ExprId) -> Statement {
            statement(StatementKind::Let {
                keyword: span(),
                name: identifier(name),
                equals: span(),
                initializer,
            })
        }

        fn assign(&mut self, target: ExprId, value: ExprId) -> Statement {
            statement(StatementKind::Assignment {
                target,
                equals: span(),
                value,
            })
        }

        fn body(&mut self, statements: Vec<Statement>) -> BlockId {
            self.0.blocks.push(Block {
                span: span(),
                open: span(),
                close: span(),
                statements,
            });
            BlockId(self.0.blocks.len() - 1)
        }

        fn block(&mut self, statements: Vec<Statement>) -> Statement {
            let id = self.body(statements);
            statement(StatementKind::Block(id))
        }

        fn function(&mut self, parameters: &[&str], statements: Vec<Statement>) -> Function {
            let body = self.body(statements);
            Function {
                keyword: span(),
                open: span(),
                parameters: parameters.iter().map(|p| identifier(p)).collect(),
                close: span(),
                body,
            }
        }

        fn call(&mut self, callee: ExprId, arguments: Vec<ExprId>) -> ExprId {
            self.expression(ExpressionKind::Access {
                base: callee,
                links: vec![AccessLink {
                    span: span(),
                    operator: span(),
                    optional: false,
                    kind: AccessKind::Call {
                        open: span(),
                        arguments,
                        close: span(),
                    },
                }],
            })
        }

        fn ret(&mut self, value: Option<ExprId>) -> Statement {
            statement(StatementKind::Return {
                keyword: span(),
                value,
            })
        }

        /// `let a = ["marker"]; a[1] = a;` — a self-containing array holding a string.
        fn cycle(&mut self) -> Vec<Statement> {
            let marker = self.literal(Literal::String("marker".to_owned()));
            let array = self.array(vec![marker]);
            let declare = self.declare("a", array);
            let receiver = self.name("a");
            let one = self.literal(Literal::Number(1.0));
            let target = self.index(receiver, one);
            let value = self.name("a");
            vec![declare, self.assign(target, value)]
        }
    }

    /// Run `machine` to its result; these trees make no host call.
    fn finish(machine: &mut Machine<&Program>) -> Result<Value, Diagnostic> {
        match machine.execute()? {
            Stop::Finished(value) => Ok(value),
            Stop::HostCall(call) => panic!("unexpected host call to `{}`", call.name),
            Stop::Paused(_) | Stop::OutOfMemory(_) => panic!("these machines are unmetered"),
        }
    }

    fn marker(machine: &Machine<&Program>) -> Arc<str> {
        let strings = machine.heap.strings();
        let found = strings.iter().find(|s| s.as_ref() == "marker");
        Arc::clone(found.expect("the marker string is in the heap"))
    }

    #[test]
    fn machine_is_send() {
        fn assert_send<T: Send>() {}
        fn assert_static<T: 'static>() {}
        // A suspended execution moves between blocking-pool threads (Story 3.1).
        assert_send::<Machine<Arc<Program>>>();
        assert_static::<Machine<Arc<Program>>>();
    }

    #[test]
    fn a_cycle_is_freed_when_the_execution_returns() {
        let mut build = Build::new();
        let mut statements = build.cycle();
        let one = build.literal(Literal::Number(1.0));
        statements.push(statement(StatementKind::Return {
            keyword: span(),
            value: Some(one),
        }));
        build.0.statements = statements;
        let program = build.0;

        let mut machine = Machine::new(&program);
        let result = finish(&mut machine).expect("evaluates");
        assert_eq!(result.as_number(), Some(1.0));
        assert!(machine.heap.has_self_containing_array());
        let marker = marker(&machine);
        assert!(Arc::strong_count(&marker) > 1);
        drop(machine);
        assert_eq!(Arc::strong_count(&marker), 1, "the heap released the cycle");
    }

    #[test]
    fn a_cycle_is_freed_when_the_execution_fails() {
        let mut build = Build::new();
        let mut statements = build.cycle();
        let nope = build.name("nope");
        statements.push(statement(StatementKind::Expression(nope)));
        build.0.statements = statements;
        let program = build.0;

        let mut machine = Machine::new(&program);
        let error = finish(&mut machine).expect_err("fails");
        assert_eq!(error.code, Code::UNDECLARED_IDENTIFIER);
        assert!(machine.heap.has_self_containing_array());
        let marker = marker(&machine);
        drop(machine);
        assert_eq!(Arc::strong_count(&marker), 1, "the heap released the cycle");
    }

    #[test]
    fn only_the_detached_result_outlives_the_execution() {
        // `let a = ["marker"]; a[1] = a; return [a[0]];`
        let mut build = Build::new();
        let mut statements = build.cycle();
        let receiver = build.name("a");
        let zero = build.literal(Literal::Number(0.0));
        let element = build.index(receiver, zero);
        let returned = build.array(vec![element]);
        statements.push(statement(StatementKind::Return {
            keyword: span(),
            value: Some(returned),
        }));
        build.0.statements = statements;
        let program = build.0;

        let mut machine = Machine::new(&program);
        let result = finish(&mut machine).expect("evaluates");
        let marker = marker(&machine);
        drop(machine);
        assert_eq!(Arc::strong_count(&marker), 2, "held by the result alone");
        drop(result);
        assert_eq!(Arc::strong_count(&marker), 1);
    }

    /// `{ let hidden = 7; f = fn() { return hidden; }; }` — the block's scope is captured, so
    /// the reclaim skips it and the closure still reads `hidden` after the block exits.
    #[test]
    fn a_captured_scope_survives_its_block_and_keeps_its_bindings() {
        let mut build = Build::new();
        let null = build.literal(Literal::Null);
        let declare_f = build.declare("f", null);
        let seven = build.literal(Literal::Number(7.0));
        let declare_hidden = build.declare("hidden", seven);
        let read_hidden = build.name("hidden");
        let inner_return = build.ret(Some(read_hidden));
        let closure = build.function(&[], vec![inner_return]);
        let closure_expression = build.expression(ExpressionKind::Function(closure));
        let target = build.name("f");
        let assign = build.assign(target, closure_expression);
        let block = build.block(vec![declare_hidden, assign]);
        let callee = build.name("f");
        let call = build.call(callee, Vec::new());
        let result = build.ret(Some(call));
        build.0.statements = vec![declare_f, block, result];
        let program = build.0;

        let mut machine = Machine::new(&program);
        let value = finish(&mut machine).expect("evaluates");
        assert_eq!(value.as_number(), Some(7.0), "the capture is intact");
        // Root scope, the retained (captured) block scope, and the function value itself. The
        // call's own scope captured nothing, so it was reclaimed.
        assert_eq!(machine.heap.live(), 3);
    }

    /// The same shape without a closure: the block's scope is reclaimed as before.
    #[test]
    fn a_block_that_creates_no_function_is_still_reclaimed() {
        let mut build = Build::new();
        let seven = build.literal(Literal::Number(7.0));
        let declare_hidden = build.declare("hidden", seven);
        let block = build.block(vec![declare_hidden]);
        build.0.statements = vec![block];
        let program = build.0;

        let mut machine = Machine::new(&program);
        finish(&mut machine).expect("evaluates");
        assert_eq!(machine.heap.live(), 1, "only the root scope is live");
    }

    /// A call whose body creates no function value releases its scope on return, so repeated
    /// calls reuse one slot instead of accumulating.
    #[test]
    fn call_scopes_that_capture_nothing_are_reclaimed() {
        let n = 5_000;
        let mut build = Build::new();
        let a = build.name("a");
        let body_return = build.ret(Some(a));
        let identity = build.function(&["a"], vec![body_return]);
        let declaration = statement(StatementKind::Function {
            name: identifier("id"),
            function: identity,
        });
        let mut statements = vec![declaration];
        for _ in 0..n {
            let callee = build.name("id");
            let one = build.literal(Literal::Number(1.0));
            let call = build.call(callee, vec![one]);
            statements.push(statement(StatementKind::Expression(call)));
        }
        build.0.statements = statements;
        let program = build.0;

        let mut machine = Machine::new(&program);
        finish(&mut machine).expect("evaluates");
        // Root scope plus the one function value; the single call scope is on the free list.
        assert_eq!(machine.heap.live(), 2);
        assert_eq!(
            machine.heap.capacity(),
            3,
            "every call reused the same reclaimed scope slot"
        );
    }

    /// Recursion allocates one call scope per active call and gives every one of them back.
    #[test]
    fn recursion_unwinds_its_call_scopes() {
        // `fn down(n) { if (n == 0) { return 0; }; return down(n - 1); }; return down(500);`
        let depth = 500.0;
        let mut build = Build::new();
        let zero_literal = build.literal(Literal::Number(0.0));
        let base_return = build.ret(Some(zero_literal));
        let base_body = build.body(vec![base_return]);
        let n_read = build.name("n");
        let zero = build.literal(Literal::Number(0.0));
        let test = build.expression(ExpressionKind::Binary {
            left: n_read,
            operator: Spanned {
                kind: BinaryOperator::Equal,
                span: span(),
            },
            right: zero,
        });
        let guard = statement(StatementKind::If {
            branches: vec![hexput_ast::ConditionalBranch {
                else_keyword: None,
                keyword: span(),
                condition: hexput_ast::Condition {
                    open: span(),
                    expression: test,
                    close: span(),
                },
                body: base_body,
            }],
            else_branch: None,
        });
        let n_again = build.name("n");
        let one = build.literal(Literal::Number(1.0));
        let next = build.expression(ExpressionKind::Binary {
            left: n_again,
            operator: Spanned {
                kind: BinaryOperator::Subtract,
                span: span(),
            },
            right: one,
        });
        let callee = build.name("down");
        let recurse = build.call(callee, vec![next]);
        let tail = build.ret(Some(recurse));
        let function = build.function(&["n"], vec![guard, tail]);
        let declaration = statement(StatementKind::Function {
            name: identifier("down"),
            function,
        });
        let entry = build.name("down");
        let argument = build.literal(Literal::Number(depth));
        let call = build.call(entry, vec![argument]);
        let result = build.ret(Some(call));
        build.0.statements = vec![declaration, result];
        let program = build.0;

        let mut machine = Machine::new(&program);
        let value = finish(&mut machine).expect("evaluates");
        assert_eq!(value.as_number(), Some(0.0));
        assert_eq!(machine.heap.live(), 2, "every call scope was reclaimed");
    }

    #[test]
    fn exited_block_scopes_are_reclaimed() {
        let n = 12_000;
        let mut build = Build::new();
        // Siblings: `{ let y = 1; }` n times — one scope slot, reused.
        let mut statements = Vec::new();
        for _ in 0..n {
            let one = build.literal(Literal::Number(1.0));
            let declare = build.declare("y", one);
            statements.push(build.block(vec![declare]));
        }
        // Nested: `{ { … { let y = 1; } … } }` n deep.
        let one = build.literal(Literal::Number(1.0));
        let mut nested = build.declare("y", one);
        for _ in 0..n {
            nested = build.block(vec![nested]);
        }
        statements.push(nested);
        // Siblings again, after the nested run: they reuse the reclaimed slots.
        for _ in 0..n {
            let one = build.literal(Literal::Number(1.0));
            let declare = build.declare("y", one);
            statements.push(build.block(vec![declare]));
        }
        build.0.statements = statements;
        let program = build.0;

        let mut machine = Machine::new(&program);
        finish(&mut machine).expect("evaluates");
        assert_eq!(machine.heap.live(), 1, "only the root scope is live");
        assert_eq!(
            machine.heap.capacity(),
            n + 1,
            "the nested run peaks at n + 1 scopes; siblings add none"
        );
        // Story 3.5: every reclaimed scope, and every string it held, gave its bytes back.
        let empty = Build::new().0;
        assert_eq!(machine.memory_used(), Machine::new(&empty).memory_used());
    }

    /// Story 3.5: a string's bytes count while any value holds it, once however many do, and
    /// stop counting when the last one lets go.
    #[test]
    fn strings_meter_themselves_once_and_give_their_bytes_back() {
        let heap = Heap::default();
        let before = heap.used();
        let text = heap.text("y".repeat(10_000));
        let held = heap.used() - before;
        assert!(held >= 10_000, "{held}");
        let copies: Vec<RtValue> = (0..100).map(|_| text.clone()).collect();
        assert_eq!(heap.used() - before, held, "a copy is the same string");
        drop(copies);
        drop(text);
        assert_eq!(heap.used(), before);
    }
}
