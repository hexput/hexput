//! Diagnostics tests (Epic 1 Story 1.2) — the workspace-wide error shape.
//!
//! The category and code strings are a declared external contract: Backends match on them,
//! and the doc comments promise an existing code's string never changes. Pin them literally,
//! because comparing a constant against itself would not catch a rename.

use hexput_parser::parse;
use hexput_shared::diagnostics::{
    Category, Code, Diagnostic, RenderOptions, Severity, Span, render_diagnostic,
};

/// Render with no origin label — the CLI's plain form.
fn render(d: &Diagnostic, source: &str) -> String {
    render_diagnostic(d, source, RenderOptions::new())
}

/// The diagnostic a source fails to parse with. Real producers, not hand-built spans: the point
/// of the renderer is that it is accurate against spans the language actually emits.
fn parse_error(source: &str) -> Diagnostic {
    parse(source).expect_err("expected source to fail parsing")
}

/// The diagnostic a source fails to evaluate with.
fn eval_error(source: &str) -> Diagnostic {
    let program = parse(source).expect("expected source to parse");
    hexput_interpreter::evaluate(&program).expect_err("expected source to fail evaluating")
}

/// The category strings are a declared external contract — Backends match on them — so pin
/// every one. Comparing a constant against itself would not catch a rename.
#[test]
fn category_wire_strings_are_stable() {
    for (category, expected) in [
        (Category::Lexical, "lexical"),
        (Category::Syntax, "syntax"),
        (Category::Type, "type"),
        (Category::Reference, "reference"),
        (Category::Arity, "arity"),
        (Category::Arithmetic, "arithmetic"),
        (Category::Depth, "depth"),
        (Category::Capability, "capability"),
        (Category::Host, "host"),
        (Category::Budget, "budget"),
        (Category::Policy, "policy"),
    ] {
        assert_eq!(category.as_str(), expected);
        assert_eq!(category.to_string(), expected);
    }
}

/// Likewise for codes: "the string of an existing code never changes".
#[test]
fn code_strings_are_stable() {
    for (code, expected) in [
        (Code::UNTERMINATED_STRING, "lex.unterminated_string"),
        (Code::UNTERMINATED_COMMENT, "lex.unterminated_comment"),
        (Code::UNKNOWN_CHARACTER, "lex.unknown_character"),
        (Code::NON_ASCII_IDENTIFIER, "lex.non_ascii_identifier"),
        (Code::INVALID_ESCAPE, "lex.invalid_escape"),
        (Code::INVALID_UNICODE_ESCAPE, "lex.invalid_unicode_escape"),
        (Code::INVALID_NUMBER, "lex.invalid_number"),
    ] {
        assert_eq!(code.as_str(), expected);
        assert_eq!(code.to_string(), expected);
    }
}

#[test]
fn span_end_and_range_describe_the_same_region() {
    let span = Span::new(4, 3, 2, 5);
    assert_eq!(span.end(), 7);
    assert_eq!(span.range(), 4..7);
    assert_eq!(&"0123456789"[span.range()], "456");
}

#[test]
fn an_empty_span_is_legal_and_points_between_characters() {
    let span = Span::new(2, 0, 1, 3);
    assert_eq!(span.end(), 2);
    assert!(span.range().is_empty());
}

#[test]
fn span_displays_as_line_and_column() {
    assert_eq!(Span::new(40, 2, 7, 12).to_string(), "7:12");
}

#[test]
fn diagnostic_display_carries_category_severity_code_position_and_message() {
    let d = Diagnostic::lexical(
        Code::UNKNOWN_CHARACTER,
        "unexpected character `$`",
        Span::new(2, 1, 1, 3),
    );
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(
        d.to_string(),
        "lexical error [lex.unknown_character] at 1:3: unexpected character `$`"
    );
    // The severity is a word in the line, not the hardcoded `error` it replaced. The category
    // and code here are arbitrary filler — no producer pairs them, and no warning-bearing code
    // exists until Story 1.10 adds the unused-local finding.
    let w = Diagnostic::warning(
        Category::Policy,
        Code::UNKNOWN_CHARACTER,
        "unexpected character `$`",
        Span::new(2, 1, 1, 3),
    );
    assert_eq!(w.severity, Severity::Warning);
    assert_eq!(
        w.to_string(),
        "policy warning [lex.unknown_character] at 1:3: unexpected character `$`"
    );
}

#[test]
fn severity_words_are_stable() {
    assert_eq!(Severity::Error.as_str(), "error");
    assert_eq!(Severity::Warning.as_str(), "warning");
    assert_eq!(Severity::Error.to_string(), "error");
    assert_eq!(Severity::Warning.to_string(), "warning");
    assert_eq!(Severity::default(), Severity::Error);
}

#[test]
fn the_lexical_constructor_sets_the_category() {
    let d = Diagnostic::lexical(Code::INVALID_NUMBER, "x", Span::new(0, 1, 1, 1));
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(
        d,
        Diagnostic::new(
            Category::Lexical,
            Code::INVALID_NUMBER,
            "x",
            Span::new(0, 1, 1, 1)
        )
    );
}

// --- Story 1.8: terminal rendering. Every row of the spec's I/O & Edge-Case Matrix. ---
//
// The rendered form is an external contract — the CLI (1.9), the check pass (1.10) and the
// language server all print it — so these assert the literal string, not its shape.

#[test]
fn a_single_line_span_renders_location_line_and_marker() {
    let source = "let x = 1 +;";
    let d = parse_error(source);
    assert_eq!(d.code, Code::EXPECTED_SYNTAX);
    assert_eq!(&source[d.span.range()], ";");
    assert_eq!(
        render(&d, source),
        format!(
            "1:12: error syntax[syntax.expected_syntax]: {}\n\
             \x20   let x = 1 +;\n\
             \x20              ^",
            d.message
        )
    );
}

#[test]
fn an_origin_label_prefixes_the_location_line_and_is_optional() {
    let source = "let x = 1 +;";
    let d = parse_error(source);
    let labelled = render_diagnostic(&d, source, RenderOptions::new().with_origin("rules.hxp"));
    assert!(
        labelled.starts_with("rules.hxp:1:12: error syntax["),
        "{labelled}"
    );
    // Absent, the location line still renders — it just starts at the line number.
    assert!(render(&d, source).starts_with("1:12: error syntax["));
    // Only the header differs; the snippet is the same two lines.
    assert_eq!(
        labelled.lines().skip(1).collect::<Vec<_>>(),
        render(&d, source).lines().skip(1).collect::<Vec<_>>()
    );
}

#[test]
fn a_zero_length_end_of_input_span_renders_one_caret_on_the_last_line() {
    let source = "let x = 1;\nreturn 1 +";
    let d = parse_error(source);
    assert_eq!(d.span.len, 0, "end of input is a zero-length span");
    assert_eq!(d.span.offset, source.len());
    assert_eq!(
        render(&d, source),
        format!(
            "2:11: error syntax[syntax.expected_syntax]: {}\n\
             \x20   return 1 +\n\
             \x20             ^",
            d.message
        )
    );
}

#[test]
fn end_of_input_after_a_trailing_newline_falls_back_to_the_preceding_line() {
    // Virtually every real file ends with a newline, so this is the *common* end-of-input case.
    // The empty final line has nothing to show, so the offending line is the one before it,
    // marked one past its last scalar. The header keeps the span's own line:column.
    for source in [
        "let x = 1;\nreturn 1 +\n",
        "let x = 1;\r\nreturn 1 +\r\n",
        "let x = 1;\rreturn 1 +\r",
    ] {
        let d = parse_error(source);
        assert_eq!(d.span.len, 0);
        assert_eq!(d.span.offset, source.len());
        assert_eq!((d.span.line, d.span.column), (3, 1));
        assert_eq!(
            render(&d, source),
            format!(
                "3:1: error syntax[syntax.expected_syntax]: {}\n\
                 \x20   return 1 +\n\
                 \x20             ^",
                d.message
            ),
            "{source:?}"
        );
    }
    // Nothing to fall back to: a source that is nothing but a newline keeps the empty line.
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "expected an expression, found end of input",
        Span::new(1, 0, 2, 1),
    );
    assert_eq!(
        render(&d, "\n"),
        "2:1: error syntax[syntax.expected_syntax]: expected an expression, found end of input\n\
         \x20   \n\
         \x20   ^"
    );
}

#[test]
fn a_multi_line_span_keeps_its_end_location_and_marks_to_the_line_end() {
    // A string literal legally spans lines (§3), so the operand of a bad conversion does too.
    let source = "return \"a\nb\" * 2;";
    let d = eval_error(source);
    assert_eq!(d.code, Code::OPERAND_MISMATCH);
    assert_eq!(&source[d.span.range()], "\"a\nb\"");
    // Start AND end stay readable: the header carries both, and nothing is elided.
    assert_eq!(
        render(&d, source),
        format!(
            "1:8-2:3: error type[type.operand_mismatch]: {}\n\
             \x20   return \"a\n\
             \x20          ^^",
            d.message
        )
    );
}

#[test]
fn a_span_on_the_first_line_and_on_an_unterminated_last_line_both_render() {
    // Line 1, column 1 — no preceding line to walk back to.
    let source = "nope + 1;";
    let first = eval_error(source);
    assert_eq!(
        render(&first, source),
        format!(
            "1:1: error reference[reference.undeclared_identifier]: {}\n\
             \x20   nope + 1;\n\
             \x20   ^^^^",
            first.message
        )
    );
    // Last line, no trailing newline — the line lookup must not run off the end.
    let source = "let a = 1;\nlet b = 2;\nreturn nope;";
    let last = eval_error(source);
    assert_eq!(
        render(&last, source),
        format!(
            "3:8: error reference[reference.undeclared_identifier]: {}\n\
             \x20   return nope;\n\
             \x20          ^^^^",
            last.message
        )
    );
}

#[test]
fn a_tab_before_the_span_is_copied_through_so_the_marker_stays_aligned() {
    // Spaces under a tab would drift by however wide the terminal draws the tab, so the
    // marker's indent reuses the line's own whitespace.
    let source = "\tlet x = 1 +;";
    let d = parse_error(source);
    assert_eq!(d.span.column, 13);
    assert_eq!(
        render(&d, source),
        format!(
            "1:13: error syntax[syntax.expected_syntax]: {}\n\
             \x20   \tlet x = 1 +;\n\
             \x20   \t           ^",
            d.message
        )
    );
}

#[test]
fn non_ascii_before_the_span_aligns_the_marker_to_the_scalar_column() {
    let source = "let s = \"ünïcøde\" + [];";
    let d = eval_error(source);
    assert_eq!(&source[d.span.range()], "[]");
    // The byte offset is past column 21 — three two-byte scalars precede the span.
    assert_eq!(d.span.offset, 23);
    assert_eq!(d.span.column, 21);
    assert_eq!(
        render(&d, source),
        format!(
            "1:21: error type[type.operand_mismatch]: {}\n\
             \x20   let s = \"ünïcøde\" + [];\n\
             \x20                       ^^",
            d.message
        )
    );
}

#[test]
fn a_crlf_line_renders_without_the_carriage_return() {
    let source = "let a = 1;\r\nreturn nope;\r\n";
    let d = eval_error(source);
    let rendered = render(&d, source);
    assert!(!rendered.contains('\r'), "rendered a raw CR: {rendered:?}");
    assert_eq!(
        rendered,
        format!(
            "2:8: error reference[reference.undeclared_identifier]: {}\n\
             \x20   return nope;\n\
             \x20          ^^^^",
            d.message
        )
    );
    // A lone CR is a line break to the lexer, so it must be one here too.
    let source = "let a = 1;\rreturn nope;";
    let d = eval_error(source);
    assert_eq!(d.span.line, 2);
    assert!(
        render(&d, source).contains("\n    return nope;\n"),
        "{}",
        render(&d, source)
    );
}

#[test]
fn a_very_long_line_is_printed_in_full_and_never_windowed() {
    // A windowed line would no longer be the literal source; correctness of the text beats
    // fitting the viewport, and the terminal is free to wrap.
    let padding = "+ 1 ".repeat(400);
    let source = format!("return 1 {padding}+ nope;");
    let d = eval_error(&source);
    let rendered = render(&d, &source);
    let line = rendered.lines().nth(1).expect("a source line");
    assert_eq!(line, format!("    {source}"));
    let marker = rendered.lines().nth(2).expect("a marker line");
    assert_eq!(marker.len(), 4 + d.span.column - 1 + 4);
    assert!(marker.ends_with("^^^^"));
    assert!(rendered.starts_with(&format!("1:{}: error reference[", d.span.column)));
}

#[test]
fn a_warning_renders_the_same_shape_with_the_warning_word() {
    let source = "let unused = 1;";
    let d = Diagnostic::warning(
        Category::Policy,
        Code::EXPECTED_SYNTAX,
        "unused local `unused`",
        Span::new(4, 6, 1, 5),
    );
    assert_eq!(
        render(&d, source),
        "1:5: warning policy[syntax.expected_syntax]: unused local `unused`\n\
         \x20   let unused = 1;\n\
         \x20       ^^^^^^"
    );
}

#[test]
fn a_span_that_does_not_fit_the_source_renders_the_header_alone() {
    let source = "let x = 1;";
    // A caller that rendered a diagnostic against the wrong source: past the end.
    let past = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "expected `;`",
        Span::new(500, 3, 9, 2),
    );
    assert_eq!(
        render(&past, source),
        "9:2: error syntax[syntax.expected_syntax]: expected `;`"
    );
    // ... and one whose offset is not a character boundary.
    let split = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "expected `;`",
        Span::new(1, 1, 1, 1),
    );
    assert_eq!(
        render(&split, "é"),
        "1:1: error syntax[syntax.expected_syntax]: expected `;`"
    );
}

#[test]
fn rendering_never_panics_on_any_span_source_pair() {
    // Total over adversarial input: no panic, no non-boundary slice, no read past the end.
    let sources = [
        "",
        "\u{feff}let x = 1;",
        "\n\n\n",
        "\r\n\r\n",
        "\r",
        "é\tü\r\nabc",
        "a",
        "\u{1F600}\u{1F600}",
        "let x = \"ünïcøde\";",
    ];
    for source in sources {
        for offset in 0..=source.len() + 4 {
            for len in [0usize, 1, 2, 7, usize::MAX] {
                for (line, column) in [(1, 1), (1, 3), (2, 1), (9, 40), (1, 0)] {
                    let d = Diagnostic::new(
                        Category::Syntax,
                        Code::EXPECTED_SYNTAX,
                        "x",
                        Span::new(offset, len, line, column),
                    );
                    let rendered =
                        render_diagnostic(&d, source, RenderOptions::new().with_origin("s"));
                    assert!(rendered.starts_with("s:"));
                }
            }
        }
    }
}

#[test]
fn a_leading_bom_is_not_printed_and_does_not_shift_the_marker() {
    // The lexer does not count the BOM as a column, so neither may the rendered line.
    let source = "\u{feff}return nope;";
    let d = eval_error(source);
    assert_eq!(d.span.column, 8);
    assert_eq!(
        render(&d, source),
        format!(
            "1:8: error reference[reference.undeclared_identifier]: {}\n\
             \x20   return nope;\n\
             \x20          ^^^^",
            d.message
        )
    );
}

#[test]
fn a_span_whose_length_runs_past_the_source_renders_the_header_alone() {
    // The offset alone fitting is not enough: an absurd length is just as much a mismatched
    // source, and the documented fallback is the location line by itself.
    let source = "let x = 1;";
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "expected `;`",
        Span::new(4, usize::MAX / 2, 1, 5),
    );
    assert_eq!(
        render(&d, source),
        "1:5: error syntax[syntax.expected_syntax]: expected `;`"
    );
    // One byte past the end is already past the end.
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "expected `;`",
        Span::new(4, 7, 1, 5),
    );
    assert_eq!(
        render(&d, source),
        "1:5: error syntax[syntax.expected_syntax]: expected `;`"
    );
    // Exactly to the end still fits.
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "expected `;`",
        Span::new(4, 6, 1, 5),
    );
    assert!(render(&d, source).ends_with("\n    let x = 1;\n        ^^^^^^"));
}

#[test]
fn a_tab_inside_the_span_is_copied_through_so_the_marker_does_not_underrun() {
    // Five carets under a line the terminal draws eight columns wide would stop short of the
    // construct; the tab has to be a tab in the marker too, exactly as before the span.
    let source = "ab\tcd";
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "the whole thing",
        Span::new(0, 5, 1, 1),
    );
    assert_eq!(
        render(&d, source),
        "1:1: error syntax[syntax.expected_syntax]: the whole thing\n\
         \x20   ab\tcd\n\
         \x20   ^^\t^^"
    );
}

#[test]
fn a_span_ending_on_the_cr_of_a_crlf_pair_is_not_multi_line() {
    // The `\r` only ends a line when no `\n` follows, and the `\n` that follows here is outside
    // the span — so deciding from the span's own text alone would invent a line break.
    let source = "abc\r\ndef";
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "up to the break",
        Span::new(0, 4, 1, 1),
    );
    assert_eq!(
        render(&d, source),
        "1:1: error syntax[syntax.expected_syntax]: up to the break\n\
         \x20   abc\n\
         \x20   ^^^"
    );
    // Taking the `\n` too really does cross into line 2.
    let d = Diagnostic::new(
        Category::Syntax,
        Code::EXPECTED_SYNTAX,
        "across the break",
        Span::new(0, 5, 1, 1),
    );
    assert!(render(&d, source).starts_with("1:1-2:1: error syntax["));
}

#[test]
fn the_renderer_is_reachable_through_the_ast_re_export() {
    // `hexput-parser` and `hexput-interpreter` depend on `hexput-ast` alone — a direct
    // `hexput-shared` edge from either is one the crate-graph check rejects — so the re-export
    // is the only path they have to rendering. Exercise it, or reverting it stays green.
    let source = "let x = 1 +;";
    let d: hexput_ast::Diagnostic = parse_error(source);
    assert_eq!(d.severity, hexput_ast::Severity::Error);
    let through_ast = hexput_ast::render_diagnostic(
        &d,
        source,
        hexput_ast::RenderOptions::new().with_origin("rules.hxp"),
    );
    assert_eq!(
        through_ast,
        format!(
            "rules.hxp:1:12: error syntax[syntax.expected_syntax]: {}\n\
             \x20   let x = 1 +;\n\
             \x20              ^",
            d.message
        )
    );
    // Both paths are the same function, so they must agree byte for byte.
    assert_eq!(
        through_ast,
        render_diagnostic(&d, source, RenderOptions::new().with_origin("rules.hxp"))
    );
}
