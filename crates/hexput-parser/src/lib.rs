#![forbid(clippy::undocumented_unsafe_blocks)]

//! Pure source-to-AST parser for expressions, scoped blocks, and control flow.
//! Explicit operator and delimiter stacks avoid native-stack recursion, including on errors.

use hexput_ast::*;
use hexput_lexer::{Token, TokenKind, tokenize};
use std::collections::HashSet;

/// The parse result and the diagnostics shape with its rendering, re-exported so a consumer of
/// [`parse`] — the CLI in particular — can hold and report what it returns without a
/// `hexput-shared` or `hexput-ast` edge, neither of which the Spine's crate graph lists for it.
pub use hexput_ast::{
    Category, Code, Diagnostic, Program, RenderOptions, Severity, Span, Statement, StatementKind,
    render_diagnostic,
};

mod expressions;
mod statements;
use expressions::{ExpressionState, Pending};

/// Parse an entire file, preserving grouping, optional access links, and source locations.
///
/// # Errors
/// Returns lexical diagnostics unchanged, or the first syntax diagnostic. Unsupported language
/// constructs are rejected. Names need not be declared; duplicate declarations in a scope fail.
pub fn parse(source: &str) -> Result<Program, Diagnostic> {
    let tokens = tokenize(source)?;
    Parser {
        source,
        tokens,
        pos: 0,
        eof: eof_span(source),
        program: Program {
            span: Span::new(0, source.len(), 1, 1),
            statements: Vec::new(),
            expressions: Vec::new(),
            blocks: Vec::new(),
        },
    }
    .run()
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    eof: Span,
    program: Program,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }
    fn span(&self) -> Span {
        self.tokens.get(self.pos).map_or(self.eof, |t| t.span)
    }
    fn bump(&mut self) -> Token {
        let token = &mut self.tokens[self.pos];
        // Consumed kinds are never inspected again; retain spans for statement endings.
        let kind = std::mem::replace(&mut token.kind, TokenKind::Null);
        let span = token.span;
        self.pos += 1;
        Token { kind, span }
    }
    fn expected(&self, expected: &str) -> Diagnostic {
        let found = self.tokens.get(self.pos).map_or_else(
            || "end of input".to_owned(),
            |t| diagnostic_preview(&self.source[t.span.range()]),
        );
        Diagnostic::new(
            Category::Syntax,
            Code::EXPECTED_SYNTAX,
            format!("expected {expected}, found {found}"),
            self.span(),
        )
    }
    fn expect(&mut self, kind: TokenKind, label: &str) -> Result<Span, Diagnostic> {
        if self.peek() == Some(&kind) {
            Ok(self.bump().span)
        } else {
            Err(self.expected(label))
        }
    }
    fn identifier(&mut self) -> Result<Identifier, Diagnostic> {
        if !matches!(self.peek(), Some(TokenKind::Ident(_))) {
            return Err(self.expected("an identifier"));
        }
        let Token {
            kind: TokenKind::Ident(name),
            span,
        } = self.bump()
        else {
            unreachable!()
        };
        Ok(Identifier { name, span })
    }
    fn add(&mut self, kind: ExpressionKind, span: Span) -> ExprId {
        let id = ExprId(self.program.expressions.len());
        self.program.expressions.push(Expression { kind, span });
        id
    }
    fn expr_span(&self, id: ExprId) -> Span {
        self.program.expression(id).span
    }

    fn valid_target(&self, id: ExprId) -> bool {
        match &self.program.expression(id).kind {
            ExpressionKind::Identifier(_) => true,
            // Grouping ends a chain; optional reads inside a receiver group or index are reads.
            ExpressionKind::Access { links, .. } => {
                links.iter().all(|link| !link.optional)
                    && links
                        .last()
                        .is_some_and(|link| !matches!(link.kind, AccessKind::Call { .. }))
            }
            _ => false,
        }
    }

    fn access(&mut self, base: ExprId, link: AccessLink) -> ExprId {
        let span = cover(self.expr_span(base), link.span);
        if let ExpressionKind::Access { links, .. } = &mut self.program.expressions[base.0].kind {
            links.push(link);
            self.program.expressions[base.0].span = span;
            base
        } else {
            self.add(
                ExpressionKind::Access {
                    base,
                    links: vec![link],
                },
                span,
            )
        }
    }

    fn reduce(&mut self, pending: Pending, values: &mut Vec<ExprId>) {
        let right = values.pop().expect("a complete operand precedes reduction");
        let id = match pending {
            Pending::Unary(operator) => self.add(
                ExpressionKind::Unary {
                    operator,
                    operand: right,
                },
                cover(operator.span, self.expr_span(right)),
            ),
            Pending::Binary(operator, _) => {
                let left = values
                    .pop()
                    .expect("binary operators follow a left operand");
                self.add(
                    ExpressionKind::Binary {
                        left,
                        operator,
                        right,
                    },
                    cover(self.expr_span(left), self.expr_span(right)),
                )
            }
            _ => unreachable!("delimiters are closed separately"),
        };
        values.push(id);
    }
}

fn binary(kind: &TokenKind) -> Option<(BinaryOperator, u8)> {
    use BinaryOperator as B;
    use TokenKind as T;
    Some(match kind {
        T::PipePipe => (B::Or, 1),
        T::AmpAmp => (B::And, 2),
        T::EqEq => (B::Equal, 3),
        T::BangEq => (B::NotEqual, 3),
        T::Lt => (B::Less, 4),
        T::LtEq => (B::LessEqual, 4),
        T::Gt => (B::Greater, 4),
        T::GtEq => (B::GreaterEqual, 4),
        T::Plus => (B::Add, 5),
        T::Minus => (B::Subtract, 5),
        T::Star => (B::Multiply, 6),
        T::Slash => (B::Divide, 6),
        T::Percent => (B::Remainder, 6),
        _ => return None,
    })
}

fn cover(start: Span, end: Span) -> Span {
    Span::new(
        start.offset,
        end.end() - start.offset,
        start.line,
        start.column,
    )
}

/// Match the lexer's scalar-column, BOM, and CRLF conventions, including discarded trivia.
fn eof_span(source: &str) -> Span {
    let mut chars = source
        .strip_prefix('\u{feff}')
        .unwrap_or(source)
        .chars()
        .peekable();
    let (mut line, mut column) = (1, 1);
    while let Some(c) = chars.next() {
        if c == '\n' || (c == '\r' && chars.peek() != Some(&'\n')) {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    Span::new(source.len(), 0, line, column)
}

/// Bound scalars before escaping while leaving diagnostic spans untouched.
fn diagnostic_preview(text: &str) -> String {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(64).collect();
    let truncated = if chars.next().is_some() {
        "… (truncated)"
    } else {
        ""
    };
    format!("`{}{truncated}`", preview.escape_debug())
}
