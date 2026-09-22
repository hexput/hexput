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
