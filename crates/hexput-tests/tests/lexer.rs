//! Tokenizer tests (Epic 1 Story 1.2) — LANGUAGE-REFERENCE §2-§6.
//!
//! Organised by the spec's I/O & Edge-Case Matrix: literals and identifiers, positions,
//! strings and escapes, then each error the lexer can raise.

use hexput_lexer::{Token, TokenKind, tokenize};
use hexput_shared::diagnostics::{Category, Code, Span};

fn kinds(source: &str) -> Vec<TokenKind> {
    tokenize(source)
        .expect("expected source to tokenize")
        .into_iter()
        .map(|t| t.kind)
        .collect()
}

fn toks(source: &str) -> Vec<Token> {
    tokenize(source).expect("expected source to tokenize")
}

fn err(source: &str) -> hexput_shared::diagnostics::Diagnostic {
    tokenize(source).expect_err("expected source to fail lexing")
}

fn ident(name: &str) -> TokenKind {
    TokenKind::Ident(name.to_owned())
}

fn string(value: &str) -> TokenKind {
    TokenKind::Str(value.to_owned())
}

// --- matrix: empty and trivia-only ---

#[test]
fn empty_source_yields_empty_stream() {
    assert_eq!(tokenize(""), Ok(vec![]));
}

#[test]
fn whitespace_and_comments_only_yield_empty_stream() {
    assert_eq!(tokenize("  // hi\n/* x */"), Ok(vec![]));
    assert_eq!(tokenize("\n\n\t  \r\n"), Ok(vec![]));
}

#[test]
fn a_leading_bom_is_skipped_without_shifting_offsets() {
    let tokens = toks("\u{feff}let");
    assert_eq!(tokens[0].kind, TokenKind::Let);
    // The BOM is 3 bytes, so the offset still indexes the ORIGINAL source; the column does
    // not count it, because a BOM is not something a human sees.
    assert_eq!(tokens[0].span, Span::new(3, 3, 1, 1));
    // A BOM anywhere else is still an unknown character.
    assert_eq!(err("a\u{feff}b").code, Code::UNKNOWN_CHARACTER);
}

#[test]
fn crlf_counts_as_one_line_break_and_a_lone_cr_counts_as_one() {
    assert_eq!(toks("a\r\nb")[1].span.line, 2, "CRLF must count once");
    assert_eq!(toks("a\rb")[1].span.line, 2, "a lone CR ends a line");
    assert_eq!(toks("a\r\n\r\nb")[1].span.line, 3);
    // A `//` comment ends at a CR just as it does at a newline.
    assert_eq!(kinds("// c\rb"), vec![ident("b")]);
}

#[test]
fn block_comment_does_not_nest() {
    // The inner `/*` opens nothing, so the single `*/` closes the whole comment and `c` is
    // left as ordinary source.
    assert_eq!(
        kinds("/* a /* b */ c"),
        vec![ident("c")],
        "the first `*/` must close the comment"
    );
}

// --- matrix: positions ---

#[test]
fn position_after_comment() {
    let tokens = toks("// c\nx");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, ident("x"));
    assert_eq!(tokens[0].span, Span::new(5, 1, 2, 1));
}

#[test]
fn discarded_trivia_does_not_shift_neighbouring_positions() {
    let with = toks("a /* gap */ b");
    let expected_b = Span::new(12, 1, 1, 13);
    assert_eq!(with[0].span, Span::new(0, 1, 1, 1));
    assert_eq!(with[1].span, expected_b);
    // The span still slices the original source correctly.
    assert_eq!(&"a /* gap */ b"[with[1].span.range()], "b");
}

#[test]
fn positions_across_multiple_lines() {
    let source = "let a = 1;\n  b == c;\n\nreturn;";
    let tokens = toks(source);
    let located: Vec<(&TokenKind, usize, usize, usize)> = tokens
        .iter()
        .map(|t| (&t.kind, t.span.offset, t.span.line, t.span.column))
        .collect();
    assert_eq!(
        located,
        vec![
            (&TokenKind::Let, 0, 1, 1),
            (&ident("a"), 4, 1, 5),
            (&TokenKind::Assign, 6, 1, 7),
            (&TokenKind::Number(1.0), 8, 1, 9),
            (&TokenKind::Semicolon, 9, 1, 10),
            (&ident("b"), 13, 2, 3),
            (&TokenKind::EqEq, 15, 2, 5),
            (&ident("c"), 18, 2, 8),
            (&TokenKind::Semicolon, 19, 2, 9),
            (&TokenKind::Return, 22, 4, 1),
            (&TokenKind::Semicolon, 28, 4, 7),
        ]
    );
    for token in &tokens {
        assert!(
            source.get(token.span.range()).is_some(),
            "every span must slice the source: {token:?}"
        );
    }
}

#[test]
fn spans_record_token_extent_not_just_start() {
    let tokens = toks("continue >= \"ab\"");
    assert_eq!(tokens[0].span.len, "continue".len());
    assert_eq!(tokens[1].span.len, 2);
    assert_eq!(tokens[2].span.len, 4, "a string's span covers its quotes");
}

// --- matrix: words ---

#[test]
fn keyword_versus_identifier() {
    assert_eq!(kinds("let letter"), vec![TokenKind::Let, ident("letter")]);
    assert_eq!(
        kinds("_in in2 in"),
        vec![ident("_in"), ident("in2"), TokenKind::In]
    );
}

#[test]
fn every_reserved_word_has_its_own_kind() {
    assert_eq!(
        kinds("let fn if else while for in return break continue true false null plugin"),
        vec![
            TokenKind::Let,
            TokenKind::Fn,
            TokenKind::If,
            TokenKind::Else,
            TokenKind::While,
            TokenKind::For,
            TokenKind::In,
            TokenKind::Return,
            TokenKind::Break,
            TokenKind::Continue,
            TokenKind::True,
            TokenKind::False,
            TokenKind::Null,
            TokenKind::Plugin,
        ]
    );
}

// --- matrix: maximal munch ---

#[test]
fn maximal_munch_on_ambiguous_prefixes() {
    assert_eq!(kinds("a==b"), vec![ident("a"), TokenKind::EqEq, ident("b")]);
    assert_eq!(
        kinds("a=b"),
        vec![ident("a"), TokenKind::Assign, ident("b")]
    );
    assert_eq!(
        kinds("!a != b"),
        vec![TokenKind::Bang, ident("a"), TokenKind::BangEq, ident("b")]
    );
    assert_eq!(
        kinds("a<=b<c"),
        vec![
            ident("a"),
            TokenKind::LtEq,
            ident("b"),
            TokenKind::Lt,
            ident("c")
        ]
    );
    assert_eq!(
        kinds("a>=b>c"),
        vec![
            ident("a"),
            TokenKind::GtEq,
            ident("b"),
            TokenKind::Gt,
            ident("c")
        ]
    );
    assert_eq!(
        kinds("a&&b||c"),
        vec![
            ident("a"),
            TokenKind::AmpAmp,
            ident("b"),
            TokenKind::PipePipe,
            ident("c")
        ]
    );
    // `/` vs `//` vs `/*`
    assert_eq!(
        kinds("a/b // c\n/* d */ e"),
        vec![ident("a"), TokenKind::Slash, ident("b"), ident("e")]
    );
}

#[test]
fn every_operator_and_punctuator_lexes() {
    assert_eq!(
        kinds("+ - * / % ! = == != < <= > >= && || . ?. ( ) { } [ ] , ; : @"),
        vec![
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Percent,
            TokenKind::Bang,
            TokenKind::Assign,
            TokenKind::EqEq,
            TokenKind::BangEq,
            TokenKind::Lt,
            TokenKind::LtEq,
            TokenKind::Gt,
            TokenKind::GtEq,
            TokenKind::AmpAmp,
            TokenKind::PipePipe,
            TokenKind::Dot,
            TokenKind::QuestionDot,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::Comma,
            TokenKind::Semicolon,
            TokenKind::Colon,
            TokenKind::At,
        ]
    );
}

// --- matrix: numbers ---

#[test]
fn unary_minus_is_never_part_of_a_literal() {
    assert_eq!(
        kinds("a-3"),
        vec![ident("a"), TokenKind::Minus, TokenKind::Number(3.0)]
    );
    assert_eq!(kinds("-3"), vec![TokenKind::Minus, TokenKind::Number(3.0)]);
}

#[test]
fn exponent_sign_belongs_to_the_literal() {
    assert_eq!(kinds("1e-3"), vec![TokenKind::Number(0.001)]);
    assert_eq!(kinds("1e3"), vec![TokenKind::Number(1000.0)]);
    assert_eq!(kinds("1E+3"), vec![TokenKind::Number(1000.0)]);
    // An incomplete exponent does not join the literal, and the `e` left abutting the number
    // is then rejected rather than becoming an identifier — `1e` is a typo, not `1` times a
    // variable named `e`, and this language has no implicit multiplication.
    assert_eq!(err("1e").code, Code::INVALID_NUMBER);
    assert_eq!(err("1e+").code, Code::INVALID_NUMBER);
}

#[test]
fn dot_joins_a_literal_only_before_a_digit() {
    assert_eq!(kinds("1.5"), vec![TokenKind::Number(1.5)]);
    assert_eq!(kinds("1."), vec![TokenKind::Number(1.0), TokenKind::Dot]);
    assert_eq!(
        kinds("1.a"),
        vec![TokenKind::Number(1.0), TokenKind::Dot, ident("a")]
    );
    assert_eq!(kinds("0.1"), vec![TokenKind::Number(0.1)]);
}

#[test]
fn a_literal_must_start_with_a_digit() {
    // `.5` is not 0.5: a literal starts with a digit, so this is `Dot` then `Number(5)` and
    // the parser is what rejects it.
    assert_eq!(kinds(".5"), vec![TokenKind::Dot, TokenKind::Number(5.0)]);
}

#[test]
fn an_identifier_character_may_not_abut_a_number() {
    // Without this the language's lack of hex literals and digit separators would show up as
    // a baffling syntax error somewhere downstream instead of here.
    for source in ["0x10", "1abc", "1_000", "1e3x"] {
        let d = err(source);
        assert_eq!(d.code, Code::INVALID_NUMBER, "for {source}");
        assert_eq!(d.category, Category::Lexical, "for {source}");
    }
    // A number may still be followed by anything that is not an identifier character.
    assert_eq!(
        kinds("1+a"),
        vec![TokenKind::Number(1.0), TokenKind::Plus, ident("a")]
    );
}

#[test]
fn numeric_overflow_is_a_lexical_error() {
    let d = err("1e999");
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::INVALID_NUMBER);
    assert_eq!(d.span, Span::new(0, 5, 1, 1));
}

// --- matrix: optional access ---

#[test]
fn optional_access_forms() {
    assert_eq!(
        kinds("a?.b"),
        vec![ident("a"), TokenKind::QuestionDot, ident("b")]
    );
    assert_eq!(
        kinds("a?.[0]"),
        vec![
            ident("a"),
            TokenKind::QuestionDot,
            TokenKind::LBracket,
            TokenKind::Number(0.0),
            TokenKind::RBracket
        ]
    );
}

#[test]
fn a_lone_question_mark_is_an_error() {
    // There is no ternary operator (§11), so `?` only ever introduces `?.`.
    assert_eq!(err("a ? b").code, Code::UNKNOWN_CHARACTER);
}

// --- matrix: strings ---

#[test]
fn string_escapes_decode() {
    assert_eq!(kinds(r#""a\n\u{1F600}""#), vec![string("a\n\u{1F600}")]);
    assert_eq!(kinds(r#""\t\r\\\"\'""#), vec![string("\t\r\\\"'")]);
}

#[test]
fn both_quote_styles_are_equivalent() {
    assert_eq!(kinds("'x'"), kinds(r#""x""#));
    assert_eq!(kinds("'x'"), vec![string("x")]);
    // The other quote needs no escape inside a literal.
    assert_eq!(kinds("'he said \"hi\"'"), vec![string("he said \"hi\"")]);
}

#[test]
fn non_ascii_string_content_is_supported_and_columns_count_characters() {
    let source = "\"café\" x";
    let tokens = toks(source);
    assert_eq!(tokens[0].kind, string("café"));
    assert_eq!(tokens[0].span, Span::new(0, 7, 1, 1));
    // `café` is 4 characters but 5 bytes: the byte offset and the column must disagree.
    assert_eq!(tokens[1].kind, ident("x"));
    assert_eq!(tokens[1].span, Span::new(8, 1, 1, 8));
}

#[test]
fn multibyte_comment_content_keeps_columns_in_characters() {
    // `✓` is 3 bytes but 1 column, so offset and column must disagree.
    let source = "/* ✓ */ a";
    let tokens = toks(source);
    assert_eq!(tokens[0].span, Span::new(10, 1, 1, 9));
}

#[test]
fn an_escape_is_measured_in_source_width_not_decoded_width() {
    // `\u{1F600}` is 9 source characters decoding to one scalar value: the span must cover
    // the source text, and the next token's column must follow the source, not the value.
    let tokens = toks(r#""\u{1F600}" a"#);
    assert_eq!(tokens[0].kind, string("\u{1F600}"));
    assert_eq!(tokens[0].span, Span::new(0, 11, 1, 1));
    assert_eq!(tokens[1].span, Span::new(12, 1, 1, 13));
}

#[test]
fn unterminated_string_points_at_the_opening_quote() {
    let d = err("let s = \"abc");
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::UNTERMINATED_STRING);
    assert_eq!(d.span, Span::new(8, 1, 1, 9));
}

#[test]
fn string_literals_are_multi_line() {
    // §3: a raw newline between the quotes is ordinary content.
    assert_eq!(kinds("\"ab\ncd\""), vec![string("ab\ncd")]);
    assert_eq!(
        kinds("let a = \"merhaba\nsosis\nben\";"),
        vec![
            TokenKind::Let,
            ident("a"),
            TokenKind::Assign,
            string("merhaba\nsosis\nben"),
            TokenKind::Semicolon,
        ]
    );
}

#[test]
fn a_multi_line_string_spans_lines_and_does_not_disturb_later_positions() {
    let tokens = toks("let a = \"x\ny\";\nb");
    let s = &tokens[3];
    assert_eq!(s.kind, string("x\ny"));
    // The span opens at the quote on line 1 and covers the newline.
    assert_eq!(s.span, Span::new(8, 5, 1, 9));
    // The token after it must report its true line, not line 1.
    let b = tokens.last().expect("trailing identifier");
    assert_eq!(b.kind, ident("b"));
    assert_eq!(b.span.line, 3);
    assert_eq!(b.span.column, 1);
}

#[test]
fn a_string_is_unterminated_only_at_end_of_input() {
    let d = err("\"ab\ncd");
    assert_eq!(d.code, Code::UNTERMINATED_STRING);
    assert_eq!(d.span, Span::new(0, 1, 1, 1));
}

#[test]
fn a_backslash_before_a_newline_is_not_a_line_continuation() {
    let d = err("\"a\\\nb\"");
    assert_eq!(d.code, Code::INVALID_ESCAPE);
    assert_eq!(d.span.line, 1);
}

#[test]
fn unknown_escape_points_at_the_escape() {
    let d = err(r#""a\q""#);
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::INVALID_ESCAPE);
    assert_eq!(d.span, Span::new(2, 2, 1, 3));
}

#[test]
fn bad_unicode_escapes_are_errors() {
    for source in [
        r#""\u{110000}""#,  // above U+10FFFF
        r#""\u{D800}""#,    // lone surrogate
        r#""\u{}""#,        // no digits
        r#""\u{1234567}""#, // too many digits
        "\"\\uABCD\"",      // missing braces
        r#""\u{12G}""#,     // not hex
        r#""\u{12"#,        // unterminated at EOF
    ] {
        let d = err(source);
        assert_eq!(d.category, Category::Lexical, "for {source}");
        assert_eq!(d.code, Code::INVALID_UNICODE_ESCAPE, "for {source}");
    }
    assert_eq!(kinds(r#""\u{10FFFF}""#), vec![string("\u{10FFFF}")]);
}

#[test]
fn escape_at_end_of_input_reports_the_incomplete_escape() {
    // `Category::Lexical` alone would be trivially true for any lexer error, so pin the code
    // and the span: this is the diagnostic a Backend sees for a truncated script.
    let d = err("\"a\\");
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::INVALID_ESCAPE);
    assert_eq!(d.span, Span::new(2, 1, 1, 3));
}

// --- matrix: unknown characters ---

#[test]
fn unknown_character_names_the_character_and_position() {
    let d = err("a $ b");
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::UNKNOWN_CHARACTER);
    assert_eq!(d.span, Span::new(2, 1, 1, 3));
    assert!(d.message.contains('$'), "message must name the character");
    // The position belongs to the span alone. Story 1.8 renders the span with a caret, so a
    // message that also spelled out line/column would print it twice.
    assert!(
        !d.message.contains("column"),
        "position must live in the span, not the message: {}",
        d.message
    );
}

#[test]
fn a_control_character_is_escaped_in_the_message() {
    // Diagnostics travel over the wire and into logs; a raw control byte must not ride along.
    let d = err("a \u{7} b");
    assert_eq!(d.code, Code::UNKNOWN_CHARACTER);
    assert!(!d.message.contains('\u{7}'), "raw control char in message");
    assert!(d.message.contains("\\u{7}"), "got: {}", d.message);
}

#[test]
fn a_single_ampersand_or_pipe_is_an_error() {
    assert_eq!(err("a & b").code, Code::UNKNOWN_CHARACTER);
    assert_eq!(err("a | b").code, Code::UNKNOWN_CHARACTER);
}

#[test]
fn non_ascii_identifier_is_rejected() {
    let d = err("café = 1");
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::NON_ASCII_IDENTIFIER);
    // `caf` lexes, then `é` (byte 3, column 4) is rejected.
    assert_eq!(d.span, Span::new(3, 2, 1, 4));
}

// --- matrix: unterminated block comment ---

#[test]
fn unterminated_block_comment_points_at_the_opening() {
    let d = err("let x = 1;\n/* x");
    assert_eq!(d.category, Category::Lexical);
    assert_eq!(d.code, Code::UNTERMINATED_COMMENT);
    assert_eq!(d.span, Span::new(11, 2, 2, 1));
}

#[test]
fn a_star_without_a_slash_does_not_close_a_comment() {
    assert_eq!(err("/* a * b").code, Code::UNTERMINATED_COMMENT);
    assert_eq!(kinds("/* a * b */ c"), vec![ident("c")]);
}

// --- broad coverage ---

#[test]
fn a_representative_program_lexes_with_trivia_removed() {
    let source = "\
plugin { name = \"p\", }
@Event(OrderPlaced, priority = 1)
fn on_order(params) { // a line comment
  let total = 0.5 + params.items?.[0].price * -2;
  /* block */
  if (total >= 1e2 && !params.free) { return { ok: true, tag: 'x' }; }
  for (k in params) { continue; }
  return null;
}";
    let tokens = toks(source);
    assert!(
        tokens
            .iter()
            .all(|t| source.get(t.span.range()).is_some() && t.span.len > 0),
        "every span must be a non-empty slice of the source"
    );
    assert!(
        !tokens
            .iter()
            .any(|t| matches!(&t.kind, TokenKind::Ident(n) if n == "a")),
        "comment contents must not reach the token stream"
    );
    assert_eq!(tokens[0].kind, TokenKind::Plugin);
    assert_eq!(tokens.last().map(|t| &t.kind), Some(&TokenKind::RBrace));
}

#[test]
fn stops_at_the_first_error_without_recovering() {
    // Two malformed constructs; only the first is reported.
    let d = err("a $ b # c");
    assert_eq!(d.code, Code::UNKNOWN_CHARACTER);
    assert_eq!(d.span, Span::new(2, 1, 1, 3));
    assert!(d.message.contains('$'), "the `$` is reported, not the `#`");
    assert!(!d.message.contains('#'), "the second error is not reported");
}

#[test]
fn never_panics_on_awkward_input() {
    for source in [
        "",
        "\u{0}",
        "\\",
        "/*",
        "//",
        "\"",
        "'",
        "?",
        "1e",
        "1.",
        "\u{1F600}",
        "@",
        ".5",
        "\r\n\r\n",
        "\u{feff}a",
        "0.0.0",
        "'''",
    ] {
        let _ = tokenize(source);
    }
}

// --- Story 1.8: the producer sweep ---

/// Every `lex.*` code, reached from real source, with the text its span must slice out.
///
/// Story 1.8's acceptance is that a span points at the offending construct, and the only proof
/// of that is slicing the original source with it. A code added to the lexer belongs here —
/// `tests/shared.rs` pins the full list of code strings.
#[test]
fn every_lexical_code_spans_the_offending_source() {
    let cases: [(&str, Code, &str); 7] = [
        ("let s = \"abc", Code::UNTERMINATED_STRING, "\""),
        ("let x = 1;\n/* x", Code::UNTERMINATED_COMMENT, "/*"),
        ("a $ b", Code::UNKNOWN_CHARACTER, "$"),
        ("café = 1", Code::NON_ASCII_IDENTIFIER, "é"),
        (r#""a\q""#, Code::INVALID_ESCAPE, r"\q"),
        (r#""\u{}""#, Code::INVALID_UNICODE_ESCAPE, r"\u{}"),
        ("1e999", Code::INVALID_NUMBER, "1e999"),
    ];
    for (source, code, offending) in cases {
        let d = err(source);
        assert_eq!(d.category, Category::Lexical, "{source}: {d}");
        assert_eq!(d.code, code, "{source}: {d}");
        assert_eq!(&source[d.span.range()], offending, "{source}: {d}");
        // The line and column must agree with the offset, or rendering would mark the wrong
        // place: re-derive them from the source and compare.
        assert_eq!(
            hexput_tests::line_and_column(source, d.span.offset),
            (d.span.line, d.span.column),
            "{source}: {d}"
        );
    }
    for (i, (_, code, _)) in cases.iter().enumerate() {
        for (_, other, _) in &cases[i + 1..] {
            assert_ne!(code, other, "the sweep must reach each code once");
        }
    }
}
