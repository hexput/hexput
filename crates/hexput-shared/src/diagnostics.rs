//! Category, Code, Diagnostic, Span — the one error/finding shape shared by the language
//! (LANGUAGE-REFERENCE §7) and by `hexput-port`'s error responses.
//!
//! Story 1.2 lands the shape itself; Story 1.8 owns *rendering* it (source line, caret) and must
//! be able to do so without re-scanning the source, which is why [`Span`] carries a line and a
//! column alongside the byte offset.

use core::fmt;

/// A half-open region of source text, recorded three ways.
///
/// * `offset`/`len` are **bytes**, so `&source[span.range()]` slices the original text.
/// * `line` is 1-based.
/// * `column` is 1-based and counted in **Unicode scalar values, not bytes** — a column has to
///   mean what a human sees, and while identifiers are ASCII, strings and comments are full UTF-8.
///
/// Deliberately `Copy` and free of any lexer-specific field: `hexput-ast` reuses it verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// 0-based byte offset of the first byte of the span.
    pub offset: usize,
    /// Length of the span in bytes. Zero is legal (an empty span points between two characters).
    pub len: usize,
    /// 1-based line number of the first character.
    pub line: usize,
    /// 1-based column, counted in Unicode scalar values.
    pub column: usize,
}

impl Span {
    /// Construct a span from its byte offset, byte length, 1-based line and 1-based column.
    #[must_use]
    pub const fn new(offset: usize, len: usize, line: usize, column: usize) -> Self {
        Self {
            offset,
            len,
            line,
            column,
        }
    }

    /// Byte offset one past the end of the span.
    #[must_use]
    pub const fn end(&self) -> usize {
        self.offset + self.len
    }

    /// The span as a byte range, suitable for slicing the original source.
    #[must_use]
    pub const fn range(&self) -> core::ops::Range<usize> {
        self.offset..self.end()
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Failure categories from LANGUAGE-REFERENCE §7. Every diagnostic the workspace produces —
/// lexical, syntactic, runtime, or static-check finding — carries exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Unterminated string, unknown character. Detected at lex time.
    Lexical,
    /// Malformed construct, `break` outside a loop, duplicate `let`. Detected at parse time.
    Syntax,
    /// A conversion §4.2 does not perform, an index of the wrong type for its receiver (or any
    /// index on a non-collection), property access on a value that is not an object, or a
    /// Script result that contains a value referring back to itself. Detected at runtime.
    Type,
    /// Undeclared identifier, property access on `null`, an array write outside the one
    /// appendable position. Detected at runtime. Reading outside an array's range is not an
    /// error — it yields `null` (§7).
    Reference,
    /// Wrong argument count. Detected at runtime.
    Arity,
    /// Division by zero, non-finite result. Detected at runtime.
    Arithmetic,
    /// Call-depth limit exceeded. Detected at runtime.
    Depth,
    /// Call to an unregistered or denied Registered Function. Detected at runtime.
    Capability,
    /// A Resource Budget dimension exceeded. Detected at runtime.
    Budget,
    /// A disabled language construct was used. Detected at parse time or runtime.
    Policy,
}

impl Category {
    /// The category's wire spelling — the lowercase name used in §7 and in error responses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Syntax => "syntax",
            Self::Type => "type",
            Self::Reference => "reference",
            Self::Arity => "arity",
            Self::Arithmetic => "arithmetic",
            Self::Depth => "depth",
            Self::Capability => "capability",
            Self::Budget => "budget",
            Self::Policy => "policy",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stable, machine-readable identifier for one specific failure.
///
/// Stable in the sense that Backends may match on it: the string of an existing code never
/// changes. New codes are added as associated constants next to the ones below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code(&'static str);

impl Code {
    /// Define a code. Kept `const` so codes are associated constants, not runtime strings.
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        Self(code)
    }

    /// The code's stable string form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    // --- `syntax` codes (Story 1.3) ---

    /// A token or end of input does not match the expected grammar construct.
    pub const EXPECTED_SYNTAX: Self = Self::new("syntax.expected_syntax");
    /// Assignment requires a name or an ordinary property/index target.
    pub const INVALID_ASSIGNMENT_TARGET: Self = Self::new("syntax.invalid_assignment_target");
    /// A name was declared more than once in the same scope.
    pub const DUPLICATE_DECLARATION: Self = Self::new("syntax.duplicate_declaration");

    /// Loop control appeared outside a lexically enclosing loop.
    pub const LOOP_CONTROL_OUTSIDE_LOOP: Self = Self::new("syntax.loop_control_outside_loop");

    /// Two entries in an object literal have the same decoded key.
    pub const DUPLICATE_OBJECT_KEY: Self = Self::new("syntax.duplicate_object_key");

    // --- `lexical` codes (Story 1.2) ---

    /// A string literal ran to a raw newline or to end of input without its closing quote.
    pub const UNTERMINATED_STRING: Self = Self::new("lex.unterminated_string");
    /// A `/*` block comment ran to end of input without its closing `*/`.
    pub const UNTERMINATED_COMMENT: Self = Self::new("lex.unterminated_comment");
    /// A character that begins no token appeared outside a string or comment.
    pub const UNKNOWN_CHARACTER: Self = Self::new("lex.unknown_character");
    /// A non-ASCII letter or digit appeared where an identifier was being read (§2).
    pub const NON_ASCII_IDENTIFIER: Self = Self::new("lex.non_ascii_identifier");
    /// A `\` escape in a string literal names no escape the language defines (§3).
    pub const INVALID_ESCAPE: Self = Self::new("lex.invalid_escape");
    /// A `\u{...}` escape is malformed or names no Unicode scalar value.
    pub const INVALID_UNICODE_ESCAPE: Self = Self::new("lex.invalid_unicode_escape");
    /// A numeric literal is not representable as a finite f64 (§3 — infinity is never a value).
    pub const INVALID_NUMBER: Self = Self::new("lex.invalid_number");

    // --- runtime codes (Story 1.6) ---

    /// An operator's operand has no conversion §4.2/§4.3 performs — a non-numeric string or a
    /// collection in arithmetic, a collection concatenated to a string. Also the operand of
    /// `for … in` when it is neither an array nor an object. Category `type`.
    pub const OPERAND_MISMATCH: Self = Self::new("type.operand_mismatch");
    /// An index of the wrong type for its receiver: a non-number on an array, a non-string on
    /// an object, or any index on a value that is not a collection (including a function).
    /// Category `type`.
    pub const INVALID_INDEX: Self = Self::new("type.invalid_index");
    /// `.name` on a value that is not an object (number, bool, string, array, function).
    /// Category `type`.
    pub const INVALID_PROPERTY_ACCESS: Self = Self::new("type.invalid_property_access");
    /// A Script returned a value that is, or contains, a value referring back to itself
    /// (`a[0] = a; return a;` or `return [1, a];`): its reachable graph has a cycle and so no
    /// finite detached form. Cycles built but not returned are fine. Category
    /// `type`.
    pub const CYCLIC_RESULT: Self = Self::new("type.cyclic_result");
    /// A read of a name no enclosing scope declares. Category `reference`.
    pub const UNDECLARED_IDENTIFIER: Self = Self::new("reference.undeclared_identifier");
    /// An assignment to a name no enclosing scope declares — there are no implicit globals.
    /// Category `reference`.
    pub const UNDECLARED_ASSIGNMENT: Self = Self::new("reference.undeclared_assignment");
    /// Property or index access on `null` without `?.`. Category `reference`.
    pub const NULL_ACCESS: Self = Self::new("reference.null_access");
    /// An array write at a negative, fractional, or out-of-range index other than the length
    /// (which appends). Reads outside the range yield `null` instead. Category `reference`.
    pub const INDEX_OUT_OF_RANGE: Self = Self::new("reference.index_out_of_range");
    /// `/` or `%` with a zero divisor. Category `arithmetic`.
    pub const DIVISION_BY_ZERO: Self = Self::new("arithmetic.division_by_zero");
    /// An operation whose result would be infinite or `NaN`. Category `arithmetic`.
    pub const NON_FINITE: Self = Self::new("arithmetic.non_finite");

    // --- runtime codes (Story 1.7) ---

    /// A call whose callee is not a function — `let x = 1; x();`, or `o.missing()` where the
    /// property reads as `null`. There is no optional call form to suppress it. Category `type`.
    pub const NOT_CALLABLE: Self = Self::new("type.not_callable");
    /// A Script returned a function, or a value containing one (`return fn(x) { … };`,
    /// `return [f];`). A Script result must be data the Backend can receive, and a function has
    /// no wire representation. Category `type`.
    pub const FUNCTION_RESULT: Self = Self::new("type.function_result");
    /// A call passed more or fewer arguments than the function declares parameters: parameters
    /// are positional, with no `null` padding and no variadic collection (§6). Category `arity`.
    pub const ARGUMENT_COUNT: Self = Self::new("arity.argument_count");
    /// Nested calls exceeded the interpreter's call-depth limit — unbounded recursion ends here
    /// rather than in a host stack overflow (§6). Category `depth`.
    pub const CALL_DEPTH_EXCEEDED: Self = Self::new("depth.call_depth_exceeded");
    /// The collection a `for … in` loop is iterating was mutated by its own body (§5). Mutating
    /// a collection *nested* inside it is fine. Category `reference`.
    pub const COLLECTION_MUTATED: Self = Self::new("reference.collection_mutated");
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// One failure or static-check finding: what kind, which specific one, what to tell a human, and
/// where in the source it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The §7 category.
    pub category: Category,
    /// The stable code within that category.
    pub code: Code,
    /// Human-readable message. Story 1.8 renders it with the source line the span points at.
    pub message: String,
    /// Where the failure is, including its extent — Story 1.8 marks the span, not just its start.
    pub span: Span,
}

impl Diagnostic {
    /// Build a diagnostic.
    #[must_use]
    pub fn new(category: Category, code: Code, message: impl Into<String>, span: Span) -> Self {
        Self {
            category,
            code,
            message: message.into(),
            span,
        }
    }

    /// Build a [`Category::Lexical`] diagnostic — the only category Story 1.2 can produce.
    #[must_use]
    pub fn lexical(code: Code, message: impl Into<String>, span: Span) -> Self {
        Self::new(Category::Lexical, code, message, span)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} error [{}] at {}: {}",
            self.category, self.code, self.span, self.message
        )
    }
}

impl core::error::Error for Diagnostic {}
