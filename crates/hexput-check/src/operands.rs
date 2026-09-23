//! The §4.2/§4.3 conversion rules, decided over *literal* operands only.
//!
//! This is a second implementation of rules `hexput-interpreter` already implements — it has to
//! be, because AD-8 forbids the edge that would let the two share one. The duplication is held
//! honest from outside: `hexput-tests` asserts, case by case, that a finding here appears exactly
//! when evaluating the same source fails, with the same code, message and span.
//!
//! Both operands must be literals before anything is reported. That is stricter than it has to
//! be for some operators — an array operand of `*` fails whatever the other side holds — but it
//! is what lets a finding carry the runtime's message verbatim, and that message names *both*
//! operand types.

use hexput_ast::{
    BinaryOperator, Category, Code, Diagnostic, ExpressionKind, Literal, Program, Span,
    UnaryOperator,
};

/// A literal's type, as much of it as the source shows. `String` keeps its decoded text because
/// whether a string converts to a number depends on the text (§4.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Lit<'p> {
    Null,
    Bool,
    Number,
    String(&'p str),
    Array,
    Object,
    Function,
}

impl Lit<'_> {
    /// The type name the runtime prints in an operand-mismatch message.
    const fn type_name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "bool",
            Self::Number => "number",
            Self::String(_) => "string",
            Self::Array => "array",
            Self::Object => "object",
            Self::Function => "function",
        }
    }

    /// The same value with its article, as the runtime phrases it.
    const fn article(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "a bool",
            Self::Number => "a number",
            Self::String(_) => "a string",
            Self::Array => "an array",
            Self::Object => "an object",
            Self::Function => "a function",
        }
    }

    /// Whether §4.3's to-number rule accepts this literal.
    fn converts_to_number(self) -> bool {
        match self {
            Self::Null | Self::Bool | Self::Number => true,
            Self::String(text) => parses_as_number(text),
            Self::Array | Self::Object | Self::Function => false,
        }
    }

    /// Whether §4.3's to-string rule accepts this literal.
    const fn converts_to_string(self) -> bool {
        matches!(
            self,
            Self::Null | Self::Bool | Self::Number | Self::String(_)
        )
    }

    /// The runtime's explanation of why to-number refused this literal.
    const fn not_a_number_reason(self) -> &'static str {
        match self {
            Self::String(_) => "the string does not look like a number",
            Self::Array => "an array cannot be converted to a number",
            Self::Object => "an object cannot be converted to a number",
            _ => "the value cannot be converted to a number",
        }
    }
}

/// The literal an expression *is*, if it is one. Parenthesized groups are transparent, unwrapped
/// iteratively so a deeply parenthesized operand cannot grow the host stack.
pub(crate) fn literal<'p>(program: &'p Program, mut id: hexput_ast::ExprId) -> Option<Lit<'p>> {
    // A group only ever wraps one expression, so the chain is finite; the bound is belt and
    // braces for a hand-built AST whose groups form a cycle.
    for _ in 0..program.expressions.len().saturating_add(1) {
        match &program.expression(id).kind {
            ExpressionKind::Group { expression, .. } => id = *expression,
            ExpressionKind::Literal(Literal::Null) => return Some(Lit::Null),
            ExpressionKind::Literal(Literal::Bool(_)) => return Some(Lit::Bool),
            ExpressionKind::Literal(Literal::Number(_)) => return Some(Lit::Number),
            ExpressionKind::Literal(Literal::String(text)) => return Some(Lit::String(text)),
            ExpressionKind::Array { .. } => return Some(Lit::Array),
            ExpressionKind::Object { .. } => return Some(Lit::Object),
            ExpressionKind::Function(_) => return Some(Lit::Function),
            _ => return None,
        }
    }
    None
}

/// §4.3's to-number rule for a string: optional surrounding whitespace, one optional sign, then a
/// §3 number literal with a finite value. `""`, `"NaN"`, `"Infinity"`, `"0x10"` and `".5"` fail.
fn parses_as_number(text: &str) -> bool {
    let trimmed = text.trim();
    let unsigned = match trimmed.as_bytes().first() {
        Some(b'-' | b'+') => &trimmed[1..],
        _ => trimmed,
    };
    is_number_literal(unsigned.as_bytes()) && unsigned.parse::<f64>().is_ok_and(f64::is_finite)
}

fn is_number_literal(bytes: &[u8]) -> bool {
    let mut at = 0;
    let digits = |at: &mut usize| {
        let start = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        *at > start
    };
    if !digits(&mut at) {
        return false;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        if !digits(&mut at) {
            return false;
        }
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        if !digits(&mut at) {
            return false;
        }
    }
    at == bytes.len()
}

/// The finding a binary operator over two literal operands would raise at runtime, if any.
///
/// The offending operand is the one the runtime would reach first — it converts the left operand
/// before the right — and the span is that operand's, exactly as the interpreter spans it.
pub(crate) fn binary(
    program: &Program,
    left: hexput_ast::ExprId,
    operator: BinaryOperator,
    right: hexput_ast::ExprId,
) -> Option<Diagnostic> {
    use BinaryOperator as Op;
    let (l, r) = (literal(program, left)?, literal(program, right)?);
    let symbol = binary_symbol(operator);
    let mismatch = |span: Span, reason: String| {
        Some(Diagnostic::new(
            Category::Type,
            Code::OPERAND_MISMATCH,
            format!(
                "cannot apply `{symbol}` to {} and {}: {reason}",
                l.type_name(),
                r.type_name()
            ),
            span,
        ))
    };
    let (left_span, right_span) = (
        program.expression(left).span,
        program.expression(right).span,
    );
    let numeric = |side: Lit<'_>, span: Span| {
        (!side.converts_to_number())
            .then(|| mismatch(span, side.not_a_number_reason().to_owned()))
            .flatten()
    };

    match operator {
        // `+` concatenates when either side is a string, and adds otherwise (§4.2).
        Op::Add if matches!(l, Lit::String(_)) || matches!(r, Lit::String(_)) => {
            let text = |side: Lit<'_>, span: Span| {
                (!side.converts_to_string())
                    .then(|| {
                        mismatch(
                            span,
                            format!("{} cannot be converted to a string", side.article()),
                        )
                    })
                    .flatten()
            };
            text(l, left_span).or_else(|| text(r, right_span))
        }
        Op::Add | Op::Subtract | Op::Multiply | Op::Divide | Op::Remainder => {
            numeric(l, left_span).or_else(|| numeric(r, right_span))
        }
        // Ordering compares two strings lexicographically and converts to number otherwise.
        Op::Less | Op::LessEqual | Op::Greater | Op::GreaterEqual => {
            if matches!((l, r), (Lit::String(_), Lit::String(_))) {
                None
            } else {
                numeric(l, left_span).or_else(|| numeric(r, right_span))
            }
        }
        // Equality never raises (§4.2), and `&&`/`||` accept any value (§4.1).
        Op::Equal | Op::NotEqual | Op::And | Op::Or => None,
    }
}

/// The finding a unary operator over a literal operand would raise at runtime, if any. `!`
/// accepts any value (§4.1), so only numeric negation can fail.
pub(crate) fn unary(
    program: &Program,
    operator: UnaryOperator,
    operand: hexput_ast::ExprId,
) -> Option<Diagnostic> {
    if operator != UnaryOperator::Negate {
        return None;
    }
    let value = literal(program, operand)?;
    if value.converts_to_number() {
        return None;
    }
    Some(Diagnostic::new(
        Category::Type,
        Code::OPERAND_MISMATCH,
        format!(
            "cannot apply unary `-` to {}: {}",
            value.article(),
            value.not_a_number_reason()
        ),
        program.expression(operand).span,
    ))
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
