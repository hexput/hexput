//! Owned, source-spanned syntax data; no parsing, evaluation, or host access.
//!
//! Expressions and blocks live in flat arenas, linked by [`ExprId`] and [`BlockId`]. This keeps dropping,
//! cloning and inspecting deeply nested source safe without recursive ownership. IDs belong to
//! their enclosing [`Program`]; consumers must not mix IDs from different programs.

/// The diagnostics shape and its rendering, re-exported because `hexput-parser` and
/// `hexput-interpreter` depend on this crate alone — a direct `hexput-shared` edge from either
/// is a dependency the crate-graph check rejects.
pub use hexput_shared::diagnostics::{
    Category, Code, Diagnostic, RenderOptions, Severity, Span, render_diagnostic,
};

/// The language feature toggles (Story 3.9), re-exported for the same reason: the interpreter
/// refuses a disabled construct and reaches the toggles through this crate alone.
pub use hexput_shared::policy::{Feature, Features};

/// An index into [`Program::expressions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(pub usize);

/// An index into the owning [`Program::blocks`]; never mix IDs between programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub usize);

/// A brace-delimited lexical scope. Children use IDs, keeping all ownership flat.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub span: Span,
    pub open: Span,
    pub close: Span,
    pub statements: Vec<Statement>,
}

/// A parenthesized condition, including both delimiter locations.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub open: Span,
    pub expression: ExprId,
    pub close: Span,
}

/// An ordered conditional branch; `else_keyword` is absent on the initial branch.
#[derive(Debug, Clone, PartialEq)]
pub struct ConditionalBranch {
    pub else_keyword: Option<Span>,
    pub keyword: Span,
    pub condition: Condition,
    pub body: BlockId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElseBranch {
    pub keyword: Span,
    pub body: BlockId,
}

/// A complete file. Its span includes leading and trailing trivia (and an initial BOM).
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub span: Span,
    pub statements: Vec<Statement>,
    /// Blocks are allocated when their closing brace is parsed, so nested bodies precede
    /// their enclosing block. Follow [`BlockId`] links from statements for source traversal;
    /// arena order is close order, not statement order.
    pub blocks: Vec<Block>,
    /// Most children precede parents, but index expressions may be forward references:
    /// in `a.b[c]`, the chain node exists before `c` and gains its index link afterward.
    /// Traverse by following [`ExprId`] links, never by assuming arena order is tree order.
    pub expressions: Vec<Expression>,
}

impl Program {
    /// Look up a block belonging to this program.
    ///
    /// # Panics
    /// Panics if the ID is outside [`Self::blocks`].
    #[must_use]
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0]
    }

    /// Look up an expression belonging to this program.
    ///
    /// # Panics
    /// Panics if `id.0` is outside [`Self::expressions`].
    #[must_use]
    pub fn expression(&self, id: ExprId) -> &Expression {
        &self.expressions[id.0]
    }
}

/// A statement span includes its semicolon when present.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub span: Span,
    pub terminator: Option<Span>,
    pub kind: StatementKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementKind {
    /// Named declaration; anonymous functions occur only in expressions.
    Function {
        name: Identifier,
        function: Function,
    },
    Return {
        keyword: Span,
        value: Option<ExprId>,
    },
    Block(BlockId),
    If {
        branches: Vec<ConditionalBranch>,
        else_branch: Option<ElseBranch>,
    },
    While {
        keyword: Span,
        condition: Condition,
        body: BlockId,
    },
    For {
        keyword: Span,
        open: Span,
        binding: Identifier,
        in_keyword: Span,
        iterable: ExprId,
        close: Span,
        body: BlockId,
    },
    Break {
        keyword: Span,
    },
    Continue {
        keyword: Span,
    },
    Let {
        keyword: Span,
        name: Identifier,
        equals: Span,
        initializer: ExprId,
    },
    Assignment {
        target: ExprId,
        equals: Span,
        value: ExprId,
    },
    Expression(ExprId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub span: Span,
    pub kind: ExpressionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionKind {
    Function(Function),
    Array {
        open: Span,
        elements: Vec<ExprId>,
        close: Span,
    },
    Object {
        open: Span,
        entries: Vec<ObjectEntry>,
        close: Span,
    },
    Literal(Literal),
    Identifier(Identifier),
    Group {
        open: Span,
        expression: ExprId,
        close: Span,
    },
    Unary {
        operator: Spanned<UnaryOperator>,
        operand: ExprId,
    },
    Binary {
        left: ExprId,
        operator: Spanned<BinaryOperator>,
        right: ExprId,
    },
    /// One uninterrupted chain. A group in `base` is an explicit chain boundary.
    Access {
        base: ExprId,
        links: Vec<AccessLink>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spanned<T> {
    pub kind: T,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Negate,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Multiply,
    Divide,
    Remainder,
    Add,
    Subtract,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    And,
    Or,
}

/// A property, index, or ordinary call operation, in source order within its chain.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessLink {
    /// From `.` / `?.` / `[` / `(` through the property or closing delimiter.
    pub span: Span,
    /// `.` or `?.` for properties; `[` or `?.` for indices; `(` for calls.
    pub operator: Span,
    pub optional: bool,
    pub kind: AccessKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AccessKind {
    /// Ordinary call in the same uninterrupted optional-access chain.
    Call {
        open: Span,
        arguments: Vec<ExprId>,
        close: Span,
    },
    Property(Identifier),
    Index {
        open: Span,
        expression: ExprId,
        close: Span,
    },
}

/// Function syntax owns ordered parameter names and links to a flat body arena.
/// The declaration owns a name; expression functions are always anonymous.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub keyword: Span,
    pub open: Span,
    pub parameters: Vec<Identifier>,
    pub close: Span,
    pub body: BlockId,
}

/// An insertion-ordered entry. Keys are decoded, while their spans retain source spelling.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectEntry {
    pub key: Identifier,
    pub colon: Span,
    pub value: ExprId,
    pub span: Span,
}

// --- Plugin source (Story 6.1, LANGUAGE-REFERENCE §9) ---

/// A parsed Plugin: an ordinary [`Program`] holding only the top-level `let`s (Global Variables)
/// and `fn`s (Event handlers and helpers), plus side tables describing them. The side tables
/// index into [`Program::statements`]; the `plugin { }` block and the annotations are not
/// statements.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginProgram {
    pub program: Program,
    pub metadata: PluginMetadata,
    /// Every top-level `let`, in source order.
    pub global_variables: Vec<GlobalVariableDeclaration>,
    /// Every `@Event`-annotated top-level `fn`, in source order. An unannotated top-level `fn`
    /// is a helper and appears only in [`Self::program`].
    pub handlers: Vec<EventHandler>,
}

/// The `plugin { }` block: `plugin` through its closing `}`.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginMetadata {
    pub span: Span,
    /// In source order; each key appears once and `name` is always present.
    pub entries: Vec<MetadataEntry>,
}

impl PluginMetadata {
    /// The Plugin's `name`: a non-empty string whenever this value came from the parser, which
    /// refuses a block without one. The fields are public, so a value built by hand without a
    /// string `name` entry yields `""` — build one through the parser where the name matters.
    #[must_use]
    pub fn name(&self) -> &str {
        self.entries
            .iter()
            .find_map(|entry| match &entry.value.kind {
                Literal::String(name) if entry.key.name == "name" => Some(name.as_str()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// One `key = value` entry of the `plugin { }` block. The value is a scalar literal; a negative
/// number's span includes its `-`.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataEntry {
    pub key: Identifier,
    pub equals: Span,
    pub value: Spanned<Literal>,
}

/// A top-level `let`: a Global Variable.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalVariableDeclaration {
    /// Index of its `let` in [`PluginProgram::program`]'s statements.
    pub statement: usize,
    pub name: Identifier,
    /// The `@Global(...)` annotation, `None` when the `let` has none (`@Global()` is `Some`).
    pub annotation: Option<GlobalAnnotation>,
}

/// An `@Global(...)` annotation's recorded values; an absent argument is `None` (the Daemon's
/// default applies). Whether the Backend permits them is decided at registration, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalAnnotation {
    /// `@Global` through `)`.
    pub span: Span,
    /// Spanned on the `behavior` value.
    pub behavior: Option<Spanned<GlobalVariableBehavior>>,
    /// Spanned on the `locking` value.
    pub locking: Option<Spanned<GlobalVariableLocking>>,
}

/// A Global Variable Behavior (lifetime strategy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalVariableBehavior {
    Forever,
    /// `ttl` converted to milliseconds.
    Ttl {
        millis: u64,
    },
    SeparateEachTrigger,
    Keyed,
}

/// A Global Variable Locking strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalVariableLocking {
    Safe,
    Unsafe,
}

/// An `@Event(...)`-annotated top-level `fn`.
#[derive(Debug, Clone, PartialEq)]
pub struct EventHandler {
    /// Index of its `fn` in [`PluginProgram::program`]'s statements.
    pub statement: usize,
    /// The function's name.
    pub function: Identifier,
    /// The Event it handles, as written.
    pub event: Identifier,
    /// `@Event` through `)`.
    pub annotation: Span,
    /// `priority`, spanned on its value; `None` when absent.
    pub priority: Option<Spanned<i64>>,
    /// `async`, `false` when absent.
    pub is_async: bool,
    /// The `async` value's span, when present.
    pub async_span: Option<Span>,
}

/// The reserved Event every Plugin's initialization handlers bind (LANGUAGE-REFERENCE §9): a
/// Backend can neither declare nor fire it.
pub const BACKEND_REGISTERED_INIT: &str = "BackendRegisteredInit";

// --- Event return shapes (Story 6.3, LANGUAGE-REFERENCE §9) ---

/// An index into [`Shape::nodes`]; never mix ids between shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShapeId(pub usize);

impl ShapeId {
    /// The index into [`Shape::nodes`].
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// An Event's declared return shape: a Hexput literal used as a template, held as a flat arena
/// so dropping, cloning and walking an adversarially nested one never recurses.
///
/// A shape `hexput_parser::parse_shape` returns is well formed: every id is an index of
/// `nodes`, `root` is `ShapeId(0)`, each node is the child of exactly one parent, and a child's
/// id is always greater than its parent's. The fields are public data; a shape built by hand
/// that breaks those rules makes [`Shape::node`] panic and [`Shape::depth`] meaningless (it
/// reads an out-of-range child as depth `0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// Every node; `nodes[root]` is the whole shape.
    pub nodes: Vec<ShapeNode>,
    /// The outermost node.
    pub root: ShapeId,
}

/// One node of a [`Shape`]. What a result must be to conform is Story 6.6's to enforce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeNode {
    /// A string literal: any string.
    String,
    /// A number literal, negated or not: any number.
    Number,
    /// `true` or `false`: any boolean.
    Boolean,
    /// `null`: any value, `null` included.
    Any,
    /// `[]`: any array (`None`); `[t]`: an array whose every element conforms to `t`.
    Array(Option<ShapeId>),
    /// An object literal: exactly these keys, in source order, each conforming; no other key.
    Object(Vec<(String, ShapeId)>),
}

impl Shape {
    /// The node `id` names.
    ///
    /// # Panics
    /// When `id` is not an index of this shape's arena.
    #[must_use]
    pub fn node(&self, id: ShapeId) -> &ShapeNode {
        &self.nodes[id.0]
    }

    /// How many arrays and objects nest along the deepest path: `0` for a scalar, `1` for `[]`
    /// or `{ a: 1 }`, `2` for `[[]]` — the same count as a result's nesting depth. Computed
    /// without recursion, children before parents.
    #[must_use]
    pub fn depth(&self) -> usize {
        let mut depths = vec![0_usize; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate().rev() {
            depths[index] = match node {
                ShapeNode::Array(element) => {
                    1 + element.map_or(0, |child| depths.get(child.0).copied().unwrap_or(0))
                }
                ShapeNode::Object(fields) => {
                    1 + fields
                        .iter()
                        .map(|(_, child)| depths.get(child.0).copied().unwrap_or(0))
                        .max()
                        .unwrap_or(0)
                }
                _ => 0,
            };
        }
        depths.get(self.root.0).copied().unwrap_or(0)
    }
}
