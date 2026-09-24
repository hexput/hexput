//! Category, Code, Diagnostic, Severity, Span — the one error/finding shape shared by the
//! language (LANGUAGE-REFERENCE §7) and by `hexput-port`'s error responses — plus
//! [`render_diagnostic`], the one terminal rendering the CLI, the check pass and the language
//! server all reuse.
//!
//! Story 1.2 lands the shape itself; Story 1.8 lands the rendering, which never re-scans the
//! source to find *where* a diagnostic is — that is why [`Span`] carries a line and a column
//! alongside the byte offset. Rendering is a pure function of `(&Diagnostic, &str, options)`:
//! no file reads, no stdout, no colour, no TTY detection. Colour belongs to the CLI, layered
//! over this plain text. The structured fields stay public and primary; this is one consumer of
//! them, never the only access path.

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
    /// Call-depth limit exceeded, or a host call's argument nested past the argument depth.
    /// Detected at runtime.
    Depth,
    /// Call to an unregistered or denied Registered Function. Detected at runtime.
    Capability,
    /// The Backend answered a host call with an error or a malformed reply, or the connection
    /// ended before it answered (§7, §8). Detected at runtime.
    Host,
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
            Self::Host => "host",
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

    // --- host calls (Story 3.1) ---

    /// A host call's argument is a function, or contains one (`getOrder(fn() {})`). Arguments
    /// travel to the Backend as data, and a function has no wire representation (§8). Spanned on
    /// that argument. Category `type`.
    pub const FUNCTION_ARGUMENT: Self = Self::new("type.function_argument");
    /// A host call's argument contains a value referring back to itself, so it has no finite
    /// form to send (§8). Spanned on that argument. Category `type`.
    pub const CYCLIC_ARGUMENT: Self = Self::new("type.cyclic_argument");
    /// A host call's argument nests deeper than the argument depth limit (§8; 12 by default).
    /// Spanned on that argument; nothing is sent. Category `depth`.
    pub const ARGUMENT_TOO_DEEP: Self = Self::new("depth.argument_too_deep");
    /// The Backend answered a host call with an `Error`, or with a reply that is not a valid
    /// `{value}` — or the call could not be sent at all. Spanned on the call. Category `host`.
    pub const FUNCTION_FAILED: Self = Self::new("host.function_failed");
    /// The connection ended before the Backend answered a host call. Spanned on the call.
    /// Category `host`.
    pub const NO_REPLY: Self = Self::new("host.no_reply");

    // --- Resource Budget (Story 3.5) ---

    /// The execution ran Script code for longer than its CPU time budget (1 second by default).
    /// CPU time is the time spent running Script code, measured on its thread: wall time,
    /// excluding every wait on the Backend, which an oversubscribed host inflates. Spanned on the
    /// construct running when the limit was crossed. Category `budget`.
    pub const CPU_TIME_EXCEEDED: Self = Self::new("budget.cpu_time_exceeded");
    /// The execution's values came to hold more memory than its memory budget (64 MiB by
    /// default). Spanned on the construct whose allocation crossed the limit. Category `budget`.
    pub const MEMORY_EXCEEDED: Self = Self::new("budget.memory_exceeded");

    // --- static-check findings (Story 1.10) ---

    /// Code after a `return`, `break` or `continue` in the same block can never run. A
    /// [`Severity::Warning`]: unreachable code cannot fail, so it must never reject a script.
    /// Category `syntax`.
    pub const UNREACHABLE_CODE: Self = Self::new("syntax.unreachable_code");
    /// A `let` binding no expression ever reads. A [`Severity::Warning`], for the same reason.
    /// Category `reference`.
    pub const UNUSED_VARIABLE: Self = Self::new("reference.unused_variable");
    /// A call to a name that is neither declared in the Script nor among the callable names the
    /// caller supplied — a typo'd host call, caught before it becomes a runtime `capability`
    /// failure. Raised only when a callable-name list was supplied at all. Category
    /// `capability`.
    ///
    /// Also the runtime failure itself (Story 3.1): a host call to a name the Session did not
    /// register, or any host call where there is no host (`hexput eval`).
    pub const UNKNOWN_FUNCTION: Self = Self::new("capability.unknown_function");
    /// A language construct the active policy disables (FR-3), named by its toggle. Category
    /// `policy`.
    pub const CONSTRUCT_DISABLED: Self = Self::new("policy.construct_disabled");

    /// Every code the workspace defines, so a test can assert that each one is covered rather
    /// than trusting a hand-maintained list to have kept up. Add a new code here in the same
    /// change that declares it.
    pub const ALL: &'static [Self] = &[
        Self::EXPECTED_SYNTAX,
        Self::INVALID_ASSIGNMENT_TARGET,
        Self::DUPLICATE_DECLARATION,
        Self::LOOP_CONTROL_OUTSIDE_LOOP,
        Self::DUPLICATE_OBJECT_KEY,
        Self::UNTERMINATED_STRING,
        Self::UNTERMINATED_COMMENT,
        Self::UNKNOWN_CHARACTER,
        Self::NON_ASCII_IDENTIFIER,
        Self::INVALID_ESCAPE,
        Self::INVALID_UNICODE_ESCAPE,
        Self::INVALID_NUMBER,
        Self::OPERAND_MISMATCH,
        Self::INVALID_INDEX,
        Self::INVALID_PROPERTY_ACCESS,
        Self::CYCLIC_RESULT,
        Self::UNDECLARED_IDENTIFIER,
        Self::UNDECLARED_ASSIGNMENT,
        Self::NULL_ACCESS,
        Self::INDEX_OUT_OF_RANGE,
        Self::DIVISION_BY_ZERO,
        Self::NON_FINITE,
        Self::NOT_CALLABLE,
        Self::FUNCTION_RESULT,
        Self::ARGUMENT_COUNT,
        Self::CALL_DEPTH_EXCEEDED,
        Self::COLLECTION_MUTATED,
        Self::FUNCTION_ARGUMENT,
        Self::CYCLIC_ARGUMENT,
        Self::ARGUMENT_TOO_DEEP,
        Self::FUNCTION_FAILED,
        Self::NO_REPLY,
        Self::CPU_TIME_EXCEEDED,
        Self::MEMORY_EXCEEDED,
        Self::UNREACHABLE_CODE,
        Self::UNUSED_VARIABLE,
        Self::UNKNOWN_FUNCTION,
        Self::CONSTRUCT_DISABLED,
    ];
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// How much a diagnostic matters: whether it stops the work or merely reports on it.
///
/// One shape carries both, so Story 1.10's warnings (an unused local can never reject a script)
/// and the language server's diagnostics reuse [`Diagnostic`] rather than wrapping it in a
/// second, almost-identical type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Severity {
    /// The work stopped here. Every lexical, syntax and runtime failure is an error.
    #[default]
    Error,
    /// Something worth telling the author, which does not stop anything.
    Warning,
}

impl Severity {
    /// The severity's wire spelling — the word `Display` and [`render_diagnostic`] print.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One failure or static-check finding: what kind, which specific one, what to tell a human, and
/// where in the source it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Whether this stops the work ([`Severity::Error`]) or only reports on it.
    pub severity: Severity,
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
    /// Build a [`Severity::Error`] diagnostic — every failure the language produces.
    #[must_use]
    pub fn new(category: Category, code: Code, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
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

    /// Build a [`Severity::Warning`] diagnostic — a finding that reports without rejecting.
    #[must_use]
    pub fn warning(category: Category, code: Code, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::new(category, code, message, span)
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} [{}] at {}: {}",
            self.category, self.severity, self.code, self.span, self.message
        )
    }
}

impl core::error::Error for Diagnostic {}

/// What the caller wants added to a rendered diagnostic beyond the diagnostic itself.
///
/// A struct rather than a bare `Option<&str>` so Story 1.9's colour and the language server's
/// needs can be added without breaking every call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct RenderOptions<'a> {
    /// A file or Script name for the location line. Absent, the line starts at `line:column`.
    pub origin: Option<&'a str>,
}

impl<'a> RenderOptions<'a> {
    /// Options with no origin label.
    #[must_use]
    pub const fn new() -> Self {
        Self { origin: None }
    }

    /// Label the location line with a file or Script name.
    #[must_use]
    pub const fn with_origin(self, origin: &'a str) -> Self {
        Self {
            origin: Some(origin),
        }
    }
}

/// The indent the source line and its marker share. Both carry it, so the marker still sits at
/// the span's true column relative to the printed line.
const SNIPPET_INDENT: &str = "    ";

/// Render `diagnostic` for a terminal against the source it came from.
///
/// Three lines — so a check pass emitting many findings at once stays scannable:
///
/// ```text
/// rules.hxp:3:9: error type[type.operand_mismatch]: cannot multiply a string by a number
///     let y = "abc" * 2;
///             ^^^^^
/// ```
///
/// The location is `line:column` for a span on one line and `line:column-line:column` for one
/// that crosses lines, so a multi-line span never loses where the construct ran to. The end of
/// that range is **one past the span's last character** — the exclusive convention
/// [`Span::end`] uses — so a consumer highlighting the range does not have to guess. The source
/// line is printed **in full** — never windowed or elided, because a windowed line is no longer
/// the literal source — and the terminal may wrap it.
///
/// Alignment is exact for tabs (copied through into the marker, before and inside the span) and
/// for a leading BOM (not printed, because the lexer does not count it as a column). It is
/// **not** exact for double-width or combining characters: a column is one Unicode scalar here,
/// as it is everywhere else in [`Span`], so an emoji or a CJK scalar shifts the marker one cell
/// where the terminal draws two. Measuring display width needs a Unicode width table, and
/// `hexput-shared` is dependency-free by design — an accepted limitation, not an oversight.
///
/// Total, for any `(diagnostic, source)` pair including a mismatched one: this returns a
/// `String`, never panics, never slices a non-boundary byte index, and never reads outside
/// `source`. A span that does not fit the source renders as the location line alone.
#[must_use]
pub fn render_diagnostic(
    diagnostic: &Diagnostic,
    source: &str,
    options: RenderOptions<'_>,
) -> String {
    let span = diagnostic.span;
    let location = match end_location(source, span) {
        Some((line, column)) if line != span.line => {
            format!("{}:{}-{line}:{column}", span.line, span.column)
        }
        _ => format!("{}:{}", span.line, span.column),
    };
    let origin = match options.origin {
        Some(origin) => format!("{origin}:"),
        None => String::new(),
    };
    let mut rendered = format!(
        "{origin}{location}: {} {}[{}]: {}",
        diagnostic.severity, diagnostic.category, diagnostic.code, diagnostic.message
    );
    if let Some((line, marker)) = snippet(source, span) {
        rendered.push('\n');
        rendered.push_str(SNIPPET_INDENT);
        rendered.push_str(line);
        rendered.push('\n');
        rendered.push_str(SNIPPET_INDENT);
        rendered.push_str(&marker);
    }
    rendered
}

/// `true` where the lexer would end a line: `\n`, `\r\n`, or a lone `\r` (§2). Agreeing with the
/// lexer matters — a lone `\r` really does start a new line in a recorded span.
fn ends_line(current: char, next: Option<char>) -> bool {
    current == '\n' || (current == '\r' && next != Some('\n'))
}

/// The smallest char-boundary index `>=` `index`, after clamping `index` to the end of `source`.
/// `index` may be neither a boundary nor within the source.
fn snap_forward(source: &str, mut index: usize) -> usize {
    index = index.min(source.len());
    while !source.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Whether the span describes a region of *this* source: it starts on a character of it and
/// does not run past its end. A caller that rendered against the wrong source fails here.
fn fits(source: &str, span: Span) -> bool {
    span.offset.saturating_add(span.len) <= source.len() && source.is_char_boundary(span.offset)
}

/// The line and column **one past the span's last character** — the same exclusive convention
/// `Span::end()` uses — walking the span's text with the lexer's line rules. `None` when the
/// span does not fit `source`.
fn end_location(source: &str, span: Span) -> Option<(usize, usize)> {
    if !fits(source, span) {
        return None;
    }
    let end = snap_forward(source, span.offset.saturating_add(span.len));
    let (mut line, mut column) = (span.line, span.column);
    // Iterate the rest of the source, not just the span's own text, and stop at the span's end:
    // a span ending exactly on the `\r` of a `\r\n` pair must still see that `\n` and count the
    // pair as one break, or it would be reported as crossing a line it does not.
    let mut chars = source[span.offset..].chars().peekable();
    let mut consumed = 0;
    while consumed < end - span.offset {
        let Some(c) = chars.next() else { break };
        consumed += c.len_utf8();
        if ends_line(c, chars.peek().copied()) {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    Some((line, column))
}

/// The byte range of the line containing `offset`, excluding its terminator.
///
/// Both line terminators are one byte, so `+ 1` lands on a boundary. Searching for either means
/// a lone `\r` opens a line here exactly as it does in the lexer, and the `\r` of a `\r\n` pair
/// ends the line *before* it — so an extracted line never carries a `\r`.
fn line_bounds(source: &str, offset: usize) -> (usize, usize) {
    let start = source[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let end = source[start..]
        .find(['\n', '\r'])
        .map_or(source.len(), |i| start + i);
    (start, end)
}

/// The text of the line the span opens on, and the marker that goes under it. `None` when the
/// span does not fit `source`, which is the mismatched-source case: the caller prints the
/// location line alone rather than guessing at a snippet.
fn snippet(source: &str, span: Span) -> Option<(&str, String)> {
    if !fits(source, span) {
        return None;
    }
    let (mut start, mut end) = line_bounds(source, span.offset);
    let mut line = &source[start..end];
    // `None` once the fallback below applies: the column then comes from the line's own length,
    // which can only be counted after a leading BOM is out of it.
    let mut column = Some(span.column);

    if start == end && start > 0 && span.len == 0 && span.offset == source.len() {
        // End of input on the empty line after a trailing newline — the common case, since
        // virtually every file ends with one. There is nothing to show on that line, so show
        // the line before it and mark one past its last scalar. The header keeps the span's
        // own `line:column`: the diagnostic really is at the start of the line after.
        let mut text_end = start - 1;
        if source[..text_end].ends_with('\r') {
            text_end -= 1;
        }
        (start, end) = line_bounds(source, text_end);
        line = &source[start..end];
        column = None;
    }
    if start == 0 {
        // The lexer does not count a leading BOM as a column, so the printed line must not
        // carry one either, or every marker on line 1 would sit one scalar too far right.
        line = line.strip_prefix('\u{feff}').unwrap_or(line);
    }
    let column = column.unwrap_or_else(|| line.chars().count() + 1);

    // The span may run past this line (a multi-line span); the marker stops at the line's end.
    // Under the fallback above the span starts past this line entirely, so nothing is marked
    // and the single caret below is what points at the end of input.
    let stop = snap_forward(source, span.offset.saturating_add(span.len).min(end));
    let marked = &source[span.offset..stop.max(span.offset)];

    let mut marker = String::new();
    let mut before = line.chars();
    for _ in 1..column {
        // Copy a tab through as a tab: the marker can only stay under the span if it is
        // indented by whatever the terminal indented the source line by.
        marker.push(if before.next() == Some('\t') {
            '\t'
        } else {
            ' '
        });
    }
    let mut marked_anything = false;
    for c in marked.chars() {
        // Same reason inside the span as before it: a caret under a tab would underrun the
        // construct by however wide the terminal draws that tab.
        marker.push(if c == '\t' { '\t' } else { '^' });
        marked_anything = true;
    }
    if !marked_anything {
        // A zero-length span (end of input) still has to point somewhere.
        marker.push('^');
    }
    Some((line, marker))
}
