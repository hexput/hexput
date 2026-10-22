//! Plugin source (LANGUAGE-REFERENCE §9, Story 6.1): the `plugin { }` metadata block, the
//! `@Event`/`@Global` annotations, and the top-level rules the statement trampoline consults in
//! plugin mode. Every walk here is flat — a token scan or an explicit stack — so no input nesting
//! reaches the host stack.
use super::*;

/// What plugin mode collects while the trampoline runs.
#[derive(Default)]
pub(super) struct PluginState {
    /// The annotation just parsed, waiting for the `fn` or `let` it annotates.
    pending: Option<Annotation>,
    pub(super) global_variables: Vec<GlobalVariableDeclaration>,
    pub(super) handlers: Vec<EventHandler>,
}

pub(super) enum Annotation {
    Event {
        span: Span,
        event: Identifier,
        priority: Option<Spanned<i64>>,
        is_async: Option<Spanned<bool>>,
    },
    Global(GlobalAnnotation),
}

/// The largest `priority` magnitude: every value in range is exact as a Hexput number.
const PRIORITY_LIMIT: u64 = 1 << 53;

fn syntax(code: Code, message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new(Category::Syntax, code, message, span)
}

fn invalid_annotation(message: impl Into<String>, span: Span) -> Diagnostic {
    syntax(Code::INVALID_ANNOTATION, message, span)
}

/// `ttl`'s duration: `^[1-9][0-9]*(ms|s|m|h|d)$`, in milliseconds; `None` when malformed or
/// beyond `u64`.
fn ttl_millis(text: &str) -> Option<u64> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let (number, unit) = text.split_at(digits);
    if number.is_empty() || number.starts_with('0') {
        return None;
    }
    let factor = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    number.parse::<u64>().ok()?.checked_mul(factor)
}

impl Parser<'_> {
    /// The required `plugin { key = value, … }` block, then an optional `;`.
    pub(super) fn metadata_block(&mut self) -> Result<PluginMetadata, Diagnostic> {
        if self.peek() != Some(&TokenKind::Plugin) {
            return Err(syntax(
                Code::PLUGIN_BLOCK_REQUIRED,
                "plugin source must begin with its `plugin { }` block",
                self.span(),
            ));
        }
        let keyword = self.bump().span;
        self.expect(TokenKind::LBrace, "`{` to begin the plugin block")?;
        let mut entries: Vec<MetadataEntry> = Vec::new();
        while self.peek() != Some(&TokenKind::RBrace) {
            let key = self.identifier().map_err(|mut e| {
                e.message = format!("plugin block: {}", e.message);
                e
            })?;
            if entries.iter().any(|entry| entry.key.name == key.name) {
                return Err(syntax(
                    Code::DUPLICATE_METADATA_KEY,
                    format!(
                        "`{}` appears twice in the plugin block; each key appears once",
                        key.name
                    ),
                    key.span,
                ));
            }
            let equals = self.expect(TokenKind::Assign, "`=` after the plugin block key")?;
            let value = self.metadata_value()?;
            if key.name == "name"
                && !matches!(&value.kind, Literal::String(name) if !name.is_empty())
            {
                return Err(syntax(
                    Code::INVALID_PLUGIN_METADATA,
                    "the plugin's `name` must be a non-empty string",
                    value.span,
                ));
            }
            entries.push(MetadataEntry { key, equals, value });
            if self.peek() == Some(&TokenKind::RBrace) {
                break;
            }
            self.expect(TokenKind::Comma, "`,` or `}` in the plugin block")?;
        }
        let close = self.expect(TokenKind::RBrace, "`}` to close the plugin block")?;
        let span = cover(keyword, close);
        if !entries.iter().any(|entry| entry.key.name == "name") {
            return Err(syntax(
                Code::INVALID_PLUGIN_METADATA,
                "the plugin block must give the plugin's `name`",
                span,
            ));
        }
        if self.peek() == Some(&TokenKind::Semicolon) {
            self.bump();
        }
        Ok(PluginMetadata { span, entries })
    }

    /// One metadata value: a string, number (optionally `-`-prefixed), `true`, `false` or
    /// `null`. Anything else is refused spanning the whole value — up to the `,` or `}` that ends
    /// the entry at its own nesting level, found by a flat token scan. The scan also stops, at
    /// that level, where the next entry (`key =`) or a top-level declaration would begin, so a
    /// missing `,` or `}` is reported there by the caller rather than folded into the value.
    fn metadata_value(&mut self) -> Result<Spanned<Literal>, Diagnostic> {
        if matches!(
            self.peek(),
            None | Some(TokenKind::Comma | TokenKind::RBrace)
        ) {
            return Err(self.expected("a plugin block value"));
        }
        let start = self.span();
        let (mut depth, mut end, mut count) = (0usize, start, 0usize);
        for (index, token) in self.tokens.iter().enumerate().skip(self.pos) {
            let next_is_assign =
                self.tokens.get(index + 1).map(|t| &t.kind) == Some(&TokenKind::Assign);
            match token.kind {
                TokenKind::LBrace | TokenKind::LBracket | TokenKind::LParen => depth += 1,
                TokenKind::RBrace | TokenKind::RBracket | TokenKind::RParen => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                TokenKind::Comma
                | TokenKind::Semicolon
                | TokenKind::Let
                | TokenKind::Fn
                | TokenKind::At
                | TokenKind::Plugin
                    if depth == 0 =>
                {
                    break;
                }
                TokenKind::Ident(_) if depth == 0 && next_is_assign => break,
                _ => {}
            }
            end = token.span;
            count += 1;
        }
        let literal = match (&self.tokens[self.pos..self.pos + count], count) {
            ([token], 1) => match &token.kind {
                TokenKind::Str(text) => Some(Literal::String(text.clone())),
                TokenKind::Number(n) => Some(Literal::Number(*n)),
                TokenKind::True => Some(Literal::Bool(true)),
                TokenKind::False => Some(Literal::Bool(false)),
                TokenKind::Null => Some(Literal::Null),
                _ => None,
            },
            ([minus, number], 2) => match (&minus.kind, &number.kind) {
                (TokenKind::Minus, TokenKind::Number(n)) => Some(Literal::Number(-n)),
                _ => None,
            },
            _ => None,
        };
        let span = cover(start, end);
        let Some(kind) = literal else {
            return Err(syntax(
                Code::INVALID_PLUGIN_METADATA,
                "a plugin block value must be a string, number, `true`, `false` or `null`",
                span,
            ));
        };
        for _ in 0..count {
            self.bump();
        }
        Ok(Spanned { kind, span })
    }

    /// At the top level of Plugin source: refuse anything but `let`, `fn` or an annotation, and
    /// parse an annotation into [`PluginState::pending`] for the `fn`/`let` that follows it.
    pub(super) fn plugin_top_level_statement(&mut self) -> Result<(), Diagnostic> {
        match self.peek() {
            Some(TokenKind::Let | TokenKind::Fn) => Ok(()),
            Some(TokenKind::At) => {
                let annotation = self.annotation(false)?;
                self.plugin.as_mut().unwrap().pending = Some(annotation);
                Ok(())
            }
            Some(TokenKind::Plugin) => Err(syntax(
                Code::DUPLICATE_PLUGIN_BLOCK,
                "a plugin has exactly one `plugin { }` block",
                self.span(),
            )),
            _ => Err(syntax(
                Code::INVALID_PLUGIN_STATEMENT,
                "a plugin's top level holds only `let` (Global Variables) and `fn` declarations",
                self.span(),
            )),
        }
    }

    /// Parse an annotation at `@`. Checked in order: nesting (`nested` refuses it outright),
    /// known name, arguments, then that the next token is the declaration it may annotate.
    pub(super) fn annotation(&mut self, nested: bool) -> Result<Annotation, Diagnostic> {
        let at = self.bump().span;
        let name = self.identifier().map_err(|mut e| {
            e.message = format!("annotation name after `@`: {}", e.message);
            e
        })?;
        let head = cover(at, name.span);
        // The declaration each annotation may precede.
        let target = match name.name.as_str() {
            "Event" => Some(TokenKind::Fn),
            "Global" => Some(TokenKind::Let),
            _ => None,
        };
        if nested {
            return Err(syntax(
                Code::MISPLACED_ANNOTATION,
                format!("`@{}` may only annotate a top-level declaration", name.name),
                head,
            ));
        }
        let Some(target) = target else {
            return Err(invalid_annotation(
                format!(
                    "`@{}` is not an annotation; the annotations are `@Event` and `@Global`",
                    name.name
                ),
                name.span,
            ));
        };
        self.expect(TokenKind::LParen, &format!("`(` after `@{}`", name.name))?;
        let (annotation, keyword) = if target == TokenKind::Fn {
            (self.event_arguments(at)?, "fn")
        } else {
            (self.global_arguments(at)?, "let")
        };
        if self.peek() != Some(&target) {
            return Err(syntax(
                Code::MISPLACED_ANNOTATION,
                format!(
                    "`@{}` must directly precede a top-level `{keyword}`",
                    name.name
                ),
                head,
            ));
        }
        Ok(annotation)
    }

    /// The `key = value` arguments after `(` (and after `@Event`'s Event name), through `)`:
    /// comma-separated, trailing comma allowed, each key once. `leading` says a first argument
    /// was already read, so a `,` comes first.
    fn annotation_arguments(
        &mut self,
        mut leading: bool,
        label: &str,
        mut each: impl FnMut(&mut Self, &Identifier) -> Result<(), Diagnostic>,
    ) -> Result<Span, Diagnostic> {
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            if self.peek() == Some(&TokenKind::RParen) {
                return Ok(self.bump().span);
            }
            if leading {
                self.expect(
                    TokenKind::Comma,
                    &format!("`,` or `)` in the {label} arguments"),
                )?;
                if self.peek() == Some(&TokenKind::RParen) {
                    return Ok(self.bump().span);
                }
            }
            leading = true;
            let key = self.identifier().map_err(|mut e| {
                e.message = format!("{label} argument: {}", e.message);
                e
            })?;
            self.expect(TokenKind::Assign, &format!("`=` after `{}`", key.name))?;
            if !seen.insert(key.name.clone()) {
                return Err(invalid_annotation(
                    format!("`{}` appears twice in the {label} arguments", key.name),
                    key.span,
                ));
            }
            each(self, &key)?;
        }
    }

    fn event_arguments(&mut self, at: Span) -> Result<Annotation, Diagnostic> {
        let bare = matches!(self.peek(), Some(TokenKind::Ident(_)))
            && self.tokens.get(self.pos + 1).map(|t| &t.kind) != Some(&TokenKind::Assign);
        if !bare {
            return Err(invalid_annotation(
                "`@Event` takes the Event's name first, as a bare name",
                self.span(),
            ));
        }
        let event = self.identifier()?;
        let (mut priority, mut is_async) = (None, None);
        let close = self.annotation_arguments(true, "`@Event`", |p, key| {
            match key.name.as_str() {
                "priority" => priority = Some(p.priority_value()?),
                "async" => is_async = Some(p.bool_value("async")?),
                other => {
                    return Err(invalid_annotation(
                        format!(
                            "`@Event` has no argument `{other}`; its arguments are `priority` and `async`"
                        ),
                        key.span,
                    ));
                }
            }
            Ok(())
        })?;
        Ok(Annotation::Event {
            span: cover(at, close),
            event,
            priority,
            is_async,
        })
    }

    /// A whole number, optionally `-`, of magnitude at most 2^53 — judged on the source text, so
    /// a literal f64 would round (2^53 + 1) is refused rather than silently changed.
    fn priority_value(&mut self) -> Result<Spanned<i64>, Diagnostic> {
        let start = self.span();
        let negative = self.peek() == Some(&TokenKind::Minus);
        if negative {
            self.bump();
        }
        let refusal =
            |span| invalid_annotation("`priority` must be a whole number from -2^53 to 2^53", span);
        if !matches!(self.peek(), Some(TokenKind::Number(_))) {
            return Err(refusal(cover(start, self.span())));
        }
        let token = self.bump().span;
        let span = cover(start, token);
        let text = &self.source[token.range()];
        let magnitude = text
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text.parse::<u64>().ok())
            .flatten()
            .filter(|n| *n <= PRIORITY_LIMIT)
            .ok_or_else(|| refusal(span))?;
        let magnitude = i64::try_from(magnitude).expect("2^53 fits in i64");
        Ok(Spanned {
            kind: if negative { -magnitude } else { magnitude },
            span,
        })
    }

    fn bool_value(&mut self, key: &str) -> Result<Spanned<bool>, Diagnostic> {
        let kind = match self.peek() {
            Some(TokenKind::True) => true,
            Some(TokenKind::False) => false,
            _ => {
                return Err(invalid_annotation(
                    format!("`{key}` must be `true` or `false`"),
                    self.span(),
                ));
            }
        };
        Ok(Spanned {
            kind,
            span: self.bump().span,
        })
    }

    fn string_value(&mut self, key: &str, accepted: &str) -> Result<Spanned<String>, Diagnostic> {
        if !matches!(self.peek(), Some(TokenKind::Str(_))) {
            return Err(invalid_annotation(
                format!("`{key}` must be one of {accepted}"),
                self.span(),
            ));
        }
        let Token {
            kind: TokenKind::Str(text),
            span,
        } = self.bump()
        else {
            unreachable!()
        };
        Ok(Spanned { kind: text, span })
    }

    fn global_arguments(&mut self, at: Span) -> Result<Annotation, Diagnostic> {
        const BEHAVIORS: &str = r#""forever", "ttl", "separate_each_trigger" or "keyed""#;
        const LOCKINGS: &str = r#""safe" or "unsafe""#;
        const TTL: &str = r#"a duration such as "500ms", "30s", "5m", "1h" or "1d""#;
        // `behavior = "ttl"` is held as `None` millis until `ttl` is known.
        let mut behavior: Option<Spanned<Option<GlobalVariableBehavior>>> = None;
        let mut ttl: Option<(Span, u64)> = None;
        let mut locking = None;
        let close = self.annotation_arguments(false, "`@Global`", |p, key| {
            match key.name.as_str() {
                "behavior" => {
                    let value = p.string_value("behavior", BEHAVIORS)?;
                    let kind = match value.kind.as_str() {
                        "forever" => Some(GlobalVariableBehavior::Forever),
                        "ttl" => None,
                        "separate_each_trigger" => Some(GlobalVariableBehavior::SeparateEachTrigger),
                        "keyed" => Some(GlobalVariableBehavior::Keyed),
                        _ => {
                            return Err(invalid_annotation(
                                format!("`behavior` must be one of {BEHAVIORS}"),
                                value.span,
                            ));
                        }
                    };
                    behavior = Some(Spanned {
                        kind,
                        span: value.span,
                    });
                }
                "ttl" => {
                    let value = p.string_value("ttl", TTL)?;
                    let millis = ttl_millis(&value.kind).ok_or_else(|| {
                        invalid_annotation(
                            format!(
                                "`ttl` must be {TTL}: a whole number from 1 without leading zeros, then `ms`, `s`, `m`, `h` or `d`"
                            ),
                            value.span,
                        )
                    })?;
                    ttl = Some((key.span, millis));
                }
                "locking" => {
                    let value = p.string_value("locking", LOCKINGS)?;
                    let kind = match value.kind.as_str() {
                        "safe" => GlobalVariableLocking::Safe,
                        "unsafe" => GlobalVariableLocking::Unsafe,
                        _ => {
                            return Err(invalid_annotation(
                                format!("`locking` must be {LOCKINGS}"),
                                value.span,
                            ));
                        }
                    };
                    locking = Some(Spanned {
                        kind,
                        span: value.span,
                    });
                }
                other => {
                    return Err(invalid_annotation(
                        format!(
                            "`@Global` has no argument `{other}`; its arguments are `behavior`, `ttl` and `locking`"
                        ),
                        key.span,
                    ));
                }
            }
            Ok(())
        })?;
        let behavior = match (behavior, ttl) {
            (Some(Spanned { kind: None, span }), Some((_, millis))) => Some(Spanned {
                kind: GlobalVariableBehavior::Ttl { millis },
                span,
            }),
            (Some(Spanned { kind: None, span }), None) => {
                return Err(invalid_annotation(
                    r#"`behavior = "ttl"` requires a `ttl` duration"#,
                    span,
                ));
            }
            (_, Some((key, _))) => {
                return Err(invalid_annotation(
                    r#"`ttl` is given only with `behavior = "ttl"`"#,
                    key,
                ));
            }
            (
                Some(Spanned {
                    kind: Some(kind),
                    span,
                }),
                None,
            ) => Some(Spanned { kind, span }),
            (None, None) => None,
        };
        Ok(Annotation::Global(GlobalAnnotation {
            span: cover(at, close),
            behavior,
            locking,
        }))
    }

    /// A top-level `fn` at statement index `statement`: an Event handler when an `@Event`
    /// annotation precedes it (exactly one parameter), a helper otherwise.
    pub(super) fn record_function(
        &mut self,
        statement: usize,
        name: &Identifier,
        function: &Function,
    ) -> Result<(), Diagnostic> {
        let state = self.plugin.as_mut().unwrap();
        let Some(Annotation::Event {
            span,
            event,
            priority,
            is_async,
        }) = state.pending.take()
        else {
            return Ok(());
        };
        if function.parameters.len() != 1 {
            return Err(syntax(
                Code::INVALID_HANDLER,
                format!(
                    "an Event handler takes exactly one parameter (`params`); `{}` takes {}",
                    name.name,
                    function.parameters.len()
                ),
                cover(function.open, function.close),
            ));
        }
        state.handlers.push(EventHandler {
            statement,
            function: name.clone(),
            event,
            annotation: span,
            priority,
            is_async: is_async.is_some_and(|a| a.kind),
            async_span: is_async.map(|a| a.span),
        });
        Ok(())
    }

    /// A top-level `let` at statement index `statement`: a Global Variable, with the `@Global`
    /// annotation preceding it, if any.
    pub(super) fn record_global(&mut self, statement: usize, name: &Identifier) {
        let state = self.plugin.as_mut().unwrap();
        let annotation = match state.pending.take() {
            Some(Annotation::Global(annotation)) => Some(annotation),
            _ => None,
        };
        state.global_variables.push(GlobalVariableDeclaration {
            statement,
            name: name.clone(),
            annotation,
        });
    }

    /// A Global Variable's initializer is an expression over literals: no identifier, call or
    /// function anywhere in it. Walked with an explicit stack in source order; a chain's call is
    /// reported before its base, since the call is what the rule forbids.
    pub(super) fn check_global_initializer(&self, root: ExprId) -> Result<(), Diagnostic> {
        let refuse =
            |message: String, span| syntax(Code::INVALID_GLOBAL_INITIALIZER, message, span);
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let expression = self.program.expression(id);
            match &expression.kind {
                ExpressionKind::Literal(_) => {}
                ExpressionKind::Identifier(identifier) => {
                    return Err(refuse(
                        format!(
                            "a Global Variable's initializer is an expression over literals; it cannot read `{}`",
                            identifier.name
                        ),
                        identifier.span,
                    ));
                }
                ExpressionKind::Function(_) => {
                    return Err(refuse(
                        "a Global Variable cannot hold a function".to_owned(),
                        expression.span,
                    ));
                }
                ExpressionKind::Array { elements, .. } => stack.extend(elements.iter().rev()),
                ExpressionKind::Object { entries, .. } => {
                    stack.extend(entries.iter().rev().map(|entry| entry.value));
                }
                ExpressionKind::Group { expression, .. } => stack.push(*expression),
                ExpressionKind::Unary { operand, .. } => stack.push(*operand),
                ExpressionKind::Binary { left, right, .. } => {
                    stack.push(*right);
                    stack.push(*left);
                }
                ExpressionKind::Access { base, links } => {
                    if let Some(call) = links
                        .iter()
                        .find(|link| matches!(link.kind, AccessKind::Call { .. }))
                    {
                        return Err(refuse(
                            "a Global Variable's initializer is an expression over literals; it cannot call a function".to_owned(),
                            call.span,
                        ));
                    }
                    stack.extend(links.iter().rev().filter_map(|link| match link.kind {
                        AccessKind::Index { expression, .. } => Some(expression),
                        _ => None,
                    }));
                    stack.push(*base);
                }
            }
        }
        Ok(())
    }
}
