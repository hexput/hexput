//! Event return shapes (Story 6.3, LANGUAGE-REFERENCE §9): a Hexput literal used as a template.
//!
//! A shape is parsed by the language's own expression parser — there is no second schema
//! grammar — and then converted, with an explicit stack, into the flat [`Shape`] arena. Only
//! literals, a negated number, and array and object literals of shapes are shapes; anything else
//! is `syntax.invalid_shape` on the offending node.

use hexput_ast::*;
use hexput_lexer::tokenize;

use crate::Parser;
use crate::expressions::{ExpressionState, Progress};

/// Parse `source` as an Event's return shape (Story 6.3): one Hexput literal used as a template,
/// with nothing after it.
///
/// * A string, number (negated or not) or boolean literal means a value of that type; the
///   literal's value itself is irrelevant.
/// * `null` means any value, `null` included.
/// * An object literal means exactly its keys, each conforming, and no other key; `{}` is an
///   empty object.
/// * `[]` means any array, `[t]` an array whose every element conforms to `t`. Two or more
///   elements are refused: a shape is not a tuple language.
///
/// The conversion runs on an explicit stack, so no nesting depth recurses; the depth limit a
/// shape must keep to is the Daemon's to apply ([`Shape::depth`]).
///
/// # Errors
/// Returns lexical diagnostics unchanged, the parser's first syntax diagnostic (a repeated object
/// key is `syntax.duplicate_object_key`, a token after the shape `syntax.expected_syntax`), or
/// `syntax.invalid_shape` spanned on the first node — in source order — that is not a shape. A
/// tuple is refused on the whole array, which starts before any of its elements. The one
/// exception to source order is a function anywhere in the shape: the expression parser stops at
/// its `fn` before the shape is converted, so it is reported even when an earlier node is not a
/// shape either.
pub fn parse_shape(source: &str) -> Result<Shape, Diagnostic> {
    let mut parser = Parser::new(source, tokenize(source)?, None);
    let mut state = ExpressionState::new();
    let root = match parser.expression_step(&mut state)? {
        Progress::Complete(root) => root,
        Progress::Function(function) => {
            return Err(invalid("a function", function.keyword));
        }
    };
    if parser.pos < parser.tokens.len() {
        return Err(parser.expected("the end of the return shape"));
    }
    convert(&parser.program, root)
}

/// Convert the parsed expression `root` into a [`Shape`]. Each node's slot is allocated before its
/// children's, so a child's id is always greater than its parent's; children are pushed in
/// reverse, so the first node refused is the first in source order.
fn convert(program: &Program, root: ExprId) -> Result<Shape, Diagnostic> {
    let mut nodes = vec![ShapeNode::Any];
    let mut stack = vec![(root, ShapeId(0))];
    while let Some((id, slot)) = stack.pop() {
        let expression = program.expression(id);
        let node = match &expression.kind {
            ExpressionKind::Literal(literal) => scalar(literal),
            ExpressionKind::Unary {
                operator:
                    Spanned {
                        kind: UnaryOperator::Negate,
                        ..
                    },
                operand,
            } if matches!(
                program.expression(*operand).kind,
                ExpressionKind::Literal(Literal::Number(_))
            ) =>
            {
                ShapeNode::Number
            }
            ExpressionKind::Array { elements, .. } => match elements.as_slice() {
                [] => ShapeNode::Array(None),
                [element] => {
                    let child = allocate(&mut nodes);
                    stack.push((*element, child));
                    ShapeNode::Array(Some(child))
                }
                [_, _, ..] => {
                    return Err(Diagnostic::new(
                        Category::Syntax,
                        Code::INVALID_SHAPE,
                        "an array shape holds at most one element shape, which every element \
                         conforms to; a shape is not a tuple",
                        expression.span,
                    ));
                }
            },
            ExpressionKind::Object { entries, .. } => {
                let mut fields = Vec::with_capacity(entries.len());
                for entry in entries {
                    fields.push((entry.key.name.clone(), allocate(&mut nodes)));
                }
                stack.extend(
                    entries
                        .iter()
                        .zip(&fields)
                        .rev()
                        .map(|(entry, (_, child))| (entry.value, *child)),
                );
                ShapeNode::Object(fields)
            }
            ExpressionKind::Identifier(identifier) => {
                return Err(invalid(
                    &format!("the name `{}`", identifier.name),
                    identifier.span,
                ));
            }
            other => return Err(invalid(describe(other), expression.span)),
        };
        nodes[slot.0] = node;
    }
    Ok(Shape {
        nodes,
        root: ShapeId(0),
    })
}

/// A fresh slot in `nodes`, filled once its expression is visited.
fn allocate(nodes: &mut Vec<ShapeNode>) -> ShapeId {
    nodes.push(ShapeNode::Any);
    ShapeId(nodes.len() - 1)
}

/// What a scalar literal means as a shape.
const fn scalar(literal: &Literal) -> ShapeNode {
    match literal {
        Literal::Null => ShapeNode::Any,
        Literal::Bool(_) => ShapeNode::Boolean,
        Literal::Number(_) => ShapeNode::Number,
        Literal::String(_) => ShapeNode::String,
    }
}

/// The expression kinds a shape never holds, in words.
const fn describe(kind: &ExpressionKind) -> &'static str {
    match kind {
        ExpressionKind::Unary { .. } => {
            "an operator that is not `-` directly before a number literal"
        }
        ExpressionKind::Binary { .. } => "an operator",
        ExpressionKind::Group { .. } => "a parenthesized group",
        ExpressionKind::Access { .. } => "a call, property or index",
        ExpressionKind::Function(_) => "a function",
        _ => "this expression",
    }
}

/// `syntax.invalid_shape` for `what`, at `span`.
fn invalid(what: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        Category::Syntax,
        Code::INVALID_SHAPE,
        format!(
            "a return shape is a literal, or an array or object literal of shapes; {what} is not \
             one"
        ),
        span,
    )
}
