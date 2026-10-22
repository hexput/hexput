//! A single trampoline drives statements, expression suspensions, and lexical body scopes.
//! All continuations own only flat data, including when an error drops active frames.
use super::*;
use expressions::Progress;

struct Frame {
    open: Option<Span>,
    statements: Vec<Statement>,
    names: HashSet<String>,
    in_loop: bool,
    label: &'static str,
}

enum Task {
    Statements,
    Expression(ExpressionState),
    AfterExpression(AfterExpression),
    FunctionValue {
        state: ExpressionState,
        function: Function,
    },
    NamedFunction {
        start: Span,
        name: Identifier,
        function: Function,
    },
    Body {
        start: Span,
        kind: StatementKind,
    },
}

enum AfterExpression {
    Let {
        start: Span,
        name: Identifier,
        equals: Span,
    },
    Return {
        keyword: Span,
    },
    Target {
        start: Span,
    },
    Assignment {
        start: Span,
        target: ExprId,
        equals: Span,
    },
    Condition {
        start: Span,
        keyword: Span,
        open: Span,
        kind: ConditionKind,
    },
    For {
        start: Span,
        open: Span,
        binding: Identifier,
        in_keyword: Span,
    },
}

enum ConditionKind {
    If {
        branches: Vec<ConditionalBranch>,
        else_keyword: Option<Span>,
    },
    While,
}

impl Parser<'_> {
    pub(super) fn run(&mut self) -> Result<(), Diagnostic> {
        let mut frames = vec![Frame {
            open: None,
            statements: Vec::new(),
            names: HashSet::new(),
            in_loop: false,
            label: "file",
        }];
        let mut tasks = vec![Task::Statements];
        let mut expression = ExprId(0);
        let mut body = BlockId(0);
        while let Some(task) = tasks.pop() {
            match task {
                Task::Expression(mut state) => {
                    match self.expression_step(&mut state).map_err(|mut e| {
                        let label = match tasks.last() {
                            Some(Task::AfterExpression(AfterExpression::Condition {
                                kind: ConditionKind::While,
                                ..
                            })) => Some("while"),
                            Some(Task::AfterExpression(AfterExpression::Condition {
                                kind:
                                    ConditionKind::If {
                                        else_keyword: Some(_),
                                        ..
                                    },
                                ..
                            })) => Some("else if"),
                            Some(Task::AfterExpression(AfterExpression::Condition { .. })) => {
                                Some("if")
                            }
                            Some(Task::AfterExpression(AfterExpression::For { .. })) => {
                                Some("for iterable")
                            }
                            _ => None,
                        };
                        if let Some(label) = label {
                            e.message = format!("{label}: {}", e.message);
                        }
                        e
                    })? {
                        Progress::Complete(id) => expression = id,
                        Progress::Function(function) => {
                            let names =
                                function.parameters.iter().map(|p| p.name.clone()).collect();
                            tasks.push(Task::FunctionValue { state, function });
                            self.push_body(&mut frames, &mut tasks, names, false, "function")?;
                        }
                    }
                }
                Task::FunctionValue {
                    mut state,
                    mut function,
                } => {
                    function.body = body;
                    let span = cover(function.keyword, self.program.block(body).span);
                    state.resume(self.add(ExpressionKind::Function(function), span));
                    tasks.push(Task::Expression(state));
                }
                Task::NamedFunction {
                    start,
                    name,
                    mut function,
                } => {
                    function.body = body;
                    self.append(
                        &mut frames,
                        start,
                        StatementKind::Function { name, function },
                    )?;
                }
                Task::Body { start, mut kind } => {
                    match &mut kind {
                        StatementKind::Block(id)
                        | StatementKind::While { body: id, .. }
                        | StatementKind::For { body: id, .. } => *id = body,
                        StatementKind::If {
                            branches,
                            else_branch,
                        } => {
                            if let Some(branch) = else_branch {
                                branch.body = body;
                            } else {
                                branches.last_mut().unwrap().body = body;
                            }
                            if else_branch.is_none() && self.peek() == Some(&TokenKind::Else) {
                                let else_keyword = self.bump().span;
                                if self.peek() == Some(&TokenKind::If) {
                                    let keyword = self.bump().span;
                                    let open = self.expect(
                                        TokenKind::LParen,
                                        "`(` in the else if condition",
                                    )?;
                                    Self::schedule_expression(
                                        &mut tasks,
                                        AfterExpression::Condition {
                                            start,
                                            keyword,
                                            open,
                                            kind: ConditionKind::If {
                                                branches: std::mem::take(branches),
                                                else_keyword: Some(else_keyword),
                                            },
                                        },
                                    );
                                } else {
                                    *else_branch = Some(ElseBranch {
                                        keyword: else_keyword,
                                        body: BlockId(0),
                                    });
                                    tasks.push(Task::Body { start, kind });
                                    let in_loop = frames.last().unwrap().in_loop;
                                    self.push_body(
                                        &mut frames,
                                        &mut tasks,
                                        HashSet::new(),
                                        in_loop,
                                        "else",
                                    )?;
                                }
                                continue;
                            }
                        }
                        _ => unreachable!(),
                    }
                    self.append(&mut frames, start, kind)?;
                }
                Task::AfterExpression(after) => match after {
                    AfterExpression::Let {
                        start,
                        name,
                        equals,
                    } => {
                        if self.plugin.is_some() && frames.len() == 1 {
                            self.check_global_initializer(expression)?;
                        }
                        self.append(
                            &mut frames,
                            start,
                            StatementKind::Let {
                                keyword: start,
                                name,
                                equals,
                                initializer: expression,
                            },
                        )?;
                    }
                    AfterExpression::Return { keyword } => self.append(
                        &mut frames,
                        keyword,
                        StatementKind::Return {
                            keyword,
                            value: Some(expression),
                        },
                    )?,
                    AfterExpression::Target { start } => {
                        if self.peek() == Some(&TokenKind::Assign) {
                            if !self.valid_target(expression) {
                                return Err(Diagnostic::new(
                                    Category::Syntax,
                                    Code::INVALID_ASSIGNMENT_TARGET,
                                    "expected a name or ordinary property/index assignment target before `=`",
                                    self.expr_span(expression),
                                ));
                            }
                            let equals = self.bump().span;
                            Self::schedule_expression(
                                &mut tasks,
                                AfterExpression::Assignment {
                                    start,
                                    target: expression,
                                    equals,
                                },
                            );
                        } else {
                            self.append(&mut frames, start, StatementKind::Expression(expression))?;
                        }
                    }
                    AfterExpression::Assignment {
                        start,
                        target,
                        equals,
                    } => self.append(
                        &mut frames,
                        start,
                        StatementKind::Assignment {
                            target,
                            equals,
                            value: expression,
                        },
                    )?,
                    AfterExpression::Condition {
                        start,
                        keyword,
                        open,
                        kind,
                    } => {
                        let label = match &kind {
                            ConditionKind::While => "while",
                            ConditionKind::If {
                                else_keyword: Some(_),
                                ..
                            } => "else if",
                            _ => "if",
                        };
                        let close = self.expect(
                            TokenKind::RParen,
                            &format!("`)` to close the {label} condition"),
                        )?;
                        let condition = Condition {
                            open,
                            expression,
                            close,
                        };
                        let (kind, in_loop) = match kind {
                            ConditionKind::While => (
                                StatementKind::While {
                                    keyword,
                                    condition,
                                    body: BlockId(0),
                                },
                                true,
                            ),
                            ConditionKind::If {
                                mut branches,
                                else_keyword,
                            } => {
                                branches.push(ConditionalBranch {
                                    else_keyword,
                                    keyword,
                                    condition,
                                    body: BlockId(0),
                                });
                                (
                                    StatementKind::If {
                                        branches,
                                        else_branch: None,
                                    },
                                    frames.last().unwrap().in_loop,
                                )
                            }
                        };
                        tasks.push(Task::Body { start, kind });
                        self.push_body(&mut frames, &mut tasks, HashSet::new(), in_loop, label)?;
                    }
                    AfterExpression::For {
                        start,
                        open,
                        binding,
                        in_keyword,
                    } => {
                        let close =
                            self.expect(TokenKind::RParen, "`)` to close the for header")?;
                        let names = HashSet::from([binding.name.clone()]);
                        tasks.push(Task::Body {
                            start,
                            kind: StatementKind::For {
                                keyword: start,
                                open,
                                binding,
                                in_keyword,
                                iterable: expression,
                                close,
                                body: BlockId(0),
                            },
                        });
                        self.push_body(&mut frames, &mut tasks, names, true, "for")?;
                    }
                },
                Task::Statements => {
                    if self.peek().is_none() || self.peek() == Some(&TokenKind::RBrace) {
                        if frames.len() == 1 {
                            if self.peek().is_some() {
                                return Err(self.expected("a statement, not an unmatched `}`"));
                            }
                            self.program.statements = frames.pop().unwrap().statements;
                            return Ok(());
                        }
                        let frame = frames.pop().unwrap();
                        let close = self.expect(
                            TokenKind::RBrace,
                            &format!("`}}` to close the {} body", frame.label),
                        )?;
                        let open = frame.open.unwrap();
                        body = BlockId(self.program.blocks.len());
                        self.program.blocks.push(Block {
                            span: cover(open, close),
                            open,
                            close,
                            statements: frame.statements,
                        });
                        continue;
                    }
                    tasks.push(Task::Statements);
                    // Plugin source: the top level holds only annotated or plain `let`s and
                    // `fn`s; an annotation anywhere deeper is misplaced.
                    let plugin_top_level = self.plugin.is_some() && frames.len() == 1;
                    if plugin_top_level {
                        self.plugin_top_level_statement()?;
                    } else if self.plugin.is_some() && self.peek() == Some(&TokenKind::At) {
                        self.annotation(true)?;
                    }
                    let start = self.span();
                    let in_loop = frames.last().unwrap().in_loop;
                    match self.peek() {
                        Some(TokenKind::Fn) => {
                            let keyword = self.bump().span;
                            let name = self.identifier()?;
                            Self::declare(&mut frames.last_mut().unwrap().names, &name)?;
                            let function = self.function_parameters(keyword)?;
                            if plugin_top_level {
                                self.record_function(frames[0].statements.len(), &name, &function)?;
                            }
                            let names =
                                function.parameters.iter().map(|p| p.name.clone()).collect();
                            tasks.push(Task::NamedFunction {
                                start,
                                name,
                                function,
                            });
                            self.push_body(&mut frames, &mut tasks, names, false, "function")?;
                        }
                        Some(TokenKind::Return) => {
                            let keyword = self.bump().span;
                            if self.peek().is_none()
                                || matches!(
                                    self.peek(),
                                    Some(TokenKind::Semicolon | TokenKind::RBrace)
                                )
                            {
                                self.append(
                                    &mut frames,
                                    start,
                                    StatementKind::Return {
                                        keyword,
                                        value: None,
                                    },
                                )?;
                            } else {
                                Self::schedule_expression(
                                    &mut tasks,
                                    AfterExpression::Return { keyword },
                                );
                            }
                        }
                        Some(TokenKind::Let) => {
                            self.bump();
                            let name = self.identifier()?;
                            Self::declare(&mut frames.last_mut().unwrap().names, &name)?;
                            if plugin_top_level {
                                self.record_global(frames[0].statements.len(), &name);
                            }
                            let equals =
                                self.expect(TokenKind::Assign, "`=` and an initializer")?;
                            Self::schedule_expression(
                                &mut tasks,
                                AfterExpression::Let {
                                    start,
                                    name,
                                    equals,
                                },
                            );
                        }
                        Some(TokenKind::If | TokenKind::While) => {
                            let token = self.bump();
                            let (label, kind) = if token.kind == TokenKind::While {
                                ("while", ConditionKind::While)
                            } else {
                                (
                                    "if",
                                    ConditionKind::If {
                                        branches: Vec::new(),
                                        else_keyword: None,
                                    },
                                )
                            };
                            let open = self.expect(
                                TokenKind::LParen,
                                &format!("`(` in the {label} condition"),
                            )?;
                            Self::schedule_expression(
                                &mut tasks,
                                AfterExpression::Condition {
                                    start,
                                    keyword: token.span,
                                    open,
                                    kind,
                                },
                            );
                        }
                        Some(TokenKind::For) => {
                            self.bump();
                            let open = self.expect(TokenKind::LParen, "`(` in the for header")?;
                            let binding = self.identifier().map_err(|mut e| {
                                e.message = format!("for binding: {}", e.message);
                                e
                            })?;
                            let in_keyword =
                                self.expect(TokenKind::In, "`in` after the for binding")?;
                            Self::schedule_expression(
                                &mut tasks,
                                AfterExpression::For {
                                    start,
                                    open,
                                    binding,
                                    in_keyword,
                                },
                            );
                        }
                        Some(TokenKind::LBrace) => {
                            tasks.push(Task::Body {
                                start,
                                kind: StatementKind::Block(BlockId(0)),
                            });
                            self.push_body(
                                &mut frames,
                                &mut tasks,
                                HashSet::new(),
                                in_loop,
                                "block",
                            )?;
                        }
                        Some(TokenKind::Break | TokenKind::Continue) => {
                            let token = self.bump();
                            let name = if token.kind == TokenKind::Break {
                                "break"
                            } else {
                                "continue"
                            };
                            if !in_loop {
                                return Err(Diagnostic::new(
                                    Category::Syntax,
                                    Code::LOOP_CONTROL_OUTSIDE_LOOP,
                                    format!("`{name}` requires an enclosing loop"),
                                    token.span,
                                ));
                            }
                            let kind = if token.kind == TokenKind::Break {
                                StatementKind::Break {
                                    keyword: token.span,
                                }
                            } else {
                                StatementKind::Continue {
                                    keyword: token.span,
                                }
                            };
                            self.append(&mut frames, start, kind)?;
                        }
                        Some(TokenKind::Else) => {
                            return Err(self.expected("an if branch before `else`"));
                        }
                        _ => {
                            Self::schedule_expression(&mut tasks, AfterExpression::Target { start })
                        }
                    }
                }
            }
        }
        unreachable!("the file frame completes the parse")
    }

    fn schedule_expression(tasks: &mut Vec<Task>, after: AfterExpression) {
        tasks.push(Task::AfterExpression(after));
        tasks.push(Task::Expression(ExpressionState::new()));
    }

    fn push_body(
        &mut self,
        frames: &mut Vec<Frame>,
        tasks: &mut Vec<Task>,
        names: HashSet<String>,
        in_loop: bool,
        label: &'static str,
    ) -> Result<(), Diagnostic> {
        let open = self.expect(
            TokenKind::LBrace,
            &format!("`{{` to begin the {label} body"),
        )?;
        frames.push(Frame {
            open: Some(open),
            statements: Vec::new(),
            names,
            in_loop,
            label,
        });
        tasks.push(Task::Statements);
        Ok(())
    }

    fn append(
        &mut self,
        frames: &mut [Frame],
        start: Span,
        kind: StatementKind,
    ) -> Result<(), Diagnostic> {
        let top_level = frames.len() == 1;
        let statement = self.finish_statement(start, kind, top_level)?;
        frames.last_mut().unwrap().statements.push(statement);
        Ok(())
    }

    pub(super) fn declare(
        names: &mut HashSet<String>,
        name: &Identifier,
    ) -> Result<(), Diagnostic> {
        if !names.insert(name.name.clone()) {
            return Err(Diagnostic::new(
                Category::Syntax,
                Code::DUPLICATE_DECLARATION,
                format!(
                    "expected a new binding name; `{}` is already declared in this scope",
                    name.name
                ),
                name.span,
            ));
        }
        Ok(())
    }
    fn finish_statement(
        &mut self,
        start: Span,
        kind: StatementKind,
        top_level: bool,
    ) -> Result<Statement, Diagnostic> {
        let end = self.tokens[self.pos - 1].span;
        let terminator = if self.peek() == Some(&TokenKind::Semicolon) {
            Some(self.bump().span)
        } else if self.peek().is_none()
            || self.peek() == Some(&TokenKind::RBrace)
            // Plugin source: at the top level a statement's `;` may be omitted before an
            // annotation (§9); inside bodies the ordinary §2 rule applies.
            || (top_level && self.plugin.is_some() && self.peek() == Some(&TokenKind::At))
        {
            None
        } else {
            let label = match &kind {
                StatementKind::If { .. } => "if",
                StatementKind::While { .. } => "while",
                StatementKind::For { .. } => "for",
                StatementKind::Break { .. } => "break",
                StatementKind::Continue { .. } => "continue",
                StatementKind::Block(_) => "block",
                _ => "statement",
            };
            return Err(self.expected(&format!(
                "`;` between statements, `}}`, or end of input after {label}"
            )));
        };
        Ok(Statement {
            span: cover(start, terminator.unwrap_or(end)),
            terminator,
            kind,
        })
    }
}
