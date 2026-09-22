---
title: 'Story 1.8: Report errors with precise source locations'
type: 'feature'
created: '2026-09-22'
status: 'done'
baseline_commit: '5fd0e08aa73393f12546dc4f4d6e738f2dc98f7b'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
  - '{project-root}/_bmad-output/implementation-artifacts/epic-1-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Every diagnostic Stories 1.2–1.7 produce already carries a category, a stable code, a message and a byte/line/column span, but the only way to show one to a human is `Display`, which prints `type error [code] at 3:5: message` and never shows the offending source. A script author still has to count lines by hand, and the CLI (1.9), the check pass (1.10) and the language server (Epic 9) have no shared rendering to reuse.

**Approach:** Add a terminal renderer to `hexput-shared::diagnostics` that takes a `Diagnostic` plus the source it came from and produces a snippet showing the offending line with the span marked, handling multi-line spans without losing location information; re-export it through `hexput-ast` so every language crate reaches it, and prove by test that every diagnostic the lexer, parser and interpreter can produce renders with an accurate span.

## Boundaries & Constraints

**Always:** `hexput-shared` stays dependency-free — pure `std`, no `annotate-snippets`, `ariadne`, `codespan` or colour crate. Rendering is a pure function of `(&Diagnostic, &str)` plus caller-supplied options: no file reads, no stdout/stderr, returns a `String`. Column arithmetic matches the lexer's conventions exactly (1-based, Unicode scalar values, BOM-stripped, CRLF-aware). The structured fields stay public and primary — rendering is one consumer of them, never the only access path. Every span a producer emits points at the offending construct and slices the original source correctly.

**Never:** No ANSI colour, no TTY detection, no `is_terminal` — colour belongs to the CLI in Story 1.9, layered over this plain-text output. No CLI, no argument parsing, no process exit codes (1.9). No check pass, no findings, no unused-local warnings (1.10). No serde, no wire representation for `Diagnostic` (already deferred work; Epic 2 owns it). No new failure categories or codes. No changes to lexer, parser or interpreter *behaviour* — if the sweep finds a wrong span, fix that span, but do not widen or narrow what is an error.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Single-line span | `let x = 1 +;` — syntax error on `;` | location line, the source line, and a marker underlining exactly the span's columns | N/A |
| Zero-length span | end-of-input diagnostic (`eof_span`) | marker is a single caret at the reported column, on the last line | N/A |
| Multi-line span | unterminated `/* …` comment or a string literal spanning lines | start line rendered and marked to its end, and the end line:column reported so no location is lost | N/A |
| First / last line | span on line 1, and on the final line with no trailing newline | rendered without an out-of-range line lookup | N/A |
| Tabs in the line | offending line contains `\t` before the span | marker stays aligned under the span | N/A |
| Non-ASCII before the span | `let s = "ünïcøde" + [];` | marker aligns to the scalar column, not the byte offset | N/A |
| CRLF source | line ends `\r\n` | the rendered line excludes the `\r`; the marker is unaffected | N/A |
| Very long line | offending line far wider than a terminal | printed in full, never windowed or elided; the marker keeps its true column | N/A |
| Origin label | caller supplies a file/script name | it appears in the location line; absent, the location line still renders | N/A |
| Span past the source | a span whose offset exceeds the source length (caller mismatched source) | renders the header without a snippet rather than panicking or slicing out of bounds | no panic, no `Result` |
| Warning severity | a `Diagnostic` whose severity is `Warning` | header reads `warning`, not `error`, with the same category, code, message and marker | N/A |
| Producer sweep | every code in `Code`, reached from real source through `parse`/`evaluate` | the diagnostic's span slices the original source to the offending text | N/A |

## Decisions (approved 2026-09-22)

1. **`Severity` joins the shared shape now.** `Diagnostic` gains a `severity: Severity` field with variants `Error` and `Warning`; every existing construction site is `Error`, and `Display` and the renderer print the severity word instead of the hardcoded `error`. Story 1.10's findings and Epic 9's language server then reuse one type rather than a second wrapper shape. The pinned `Display` test in `crates/hexput-tests/tests/diagnostics.rs` is updated with it. Keep the existing `Diagnostic::new`/`Diagnostic::lexical` ergonomics for the `Error` case so 1.2-1.7's call sites do not all grow an argument.
2. **The rendered form is compact, not a rustc-style block.** One header line `origin:line:col: severity category[code]: message` (the `origin:` prefix present only when the caller supplies a label), then the offending source line, then the marker line beneath it. Three lines per diagnostic, so a check pass emitting many findings at once stays scannable.
3. **Long lines are never truncated or windowed.** The offending source line is always printed in full and the terminal may wrap it; the marker keeps its true column. A windowing rule would make the printed line no longer the literal source, and correctness of the text beats fitting the viewport.

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/diagnostics.rs` -- 270 lines; the whole story lands here. `Span { offset, len, line, column }` (bytes for offset/len, 1-based line, scalar-counted column) with `end()`/`range()`, `Category`, `Code` (a `&'static str` newtype, every code an associated constant), and `Diagnostic { category, code, message, span }` with `Display` and `core::error::Error`. Its module doc already assigns rendering to this story and notes that `Span` carries line/column so rendering needs no re-scan — hold to that. Add the renderer and `Severity` here. `crates/hexput-shared/Cargo.toml` has no `[dependencies]` table and must still have none afterwards.
- `crates/hexput-ast/src/lib.rs:7` -- `pub use hexput_shared::diagnostics::{Category, Code, Diagnostic, Span};`. This is the only path by which `hexput-parser` and `hexput-interpreter` see the diagnostics module — `scripts/check-crate-graph.py`'s `EXACT_DEPENDENCIES` pins `hexput-parser` to `{hexput-lexer, hexput-ast}` and the interpreter depends on `hexput-ast` only, so **neither may gain a `hexput-shared` edge**. Add the new items to this re-export instead.
- `crates/hexput-lexer/src/lib.rs:251,262` -- the two `Span::new` sites and the authority on column conventions (BOM stripped, CRLF handled, scalar columns). Agree with this code; do not reimplement a second convention.
- `crates/hexput-parser/src/lib.rs:182` (`cover`, builds a span across two spans — so parser spans legitimately cross lines) and `:206` (`eof_span`, the zero-length end-of-input span). Sources of the multi-line and zero-length matrix rows; change neither unless the sweep proves a span wrong.
- `crates/hexput-interpreter/src/machine.rs` -- runtime diagnostics spanned on the offending construct (`link.span` and friends); reference only. Its `Span::new(0, 0, 1, 1)` at `:1341` is an in-crate test helper, not a producer.
- `crates/hexput-tests/tests/diagnostics.rs` -- the shape's tests; `diagnostic_display_carries_category_code_position_and_message` pins the exact `Display` string, so decision 1 edits it. Rendering tests belong here. `tests/shared.rs` pins every code string.
- `crates/hexput-tests/tests/{lexer,parser,interpreter}.rs` -- real source already parsed and evaluated; the producer sweep reuses their existing helpers (`assert_error` and friends) rather than inventing a fixture harness.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-shared/src/diagnostics.rs` -- add the terminal renderer: a public function taking a `&Diagnostic`, the source `&str`, and a small options value carrying the optional origin label, returning a `String` in the approved form -- the story's deliverable, and the one rendering the CLI, the check pass and the language server all reuse.
- [x] `crates/hexput-shared/src/diagnostics.rs` -- implement line extraction and marker placement against the lexer's conventions (BOM, CRLF, scalar columns, tabs), the multi-line rule, the long-line rule, and the out-of-range-span guard -- every matrix row, with no panic on any input.
- [x] `crates/hexput-shared/src/diagnostics.rs` -- add `Severity`, give `Diagnostic` the field, and make `Display` and the renderer print the severity word -- one shape for Story 1.10's warnings and Epic 9's LSP (decision 1).
- [x] `crates/hexput-ast/src/lib.rs` -- extend the re-export with the renderer and `Severity` -- the parser and interpreter must reach it without a forbidden `hexput-shared` edge.
- [x] `crates/hexput-tests/tests/diagnostics.rs` -- cover every I/O matrix row with exact expected output, and update the pinned `Display` string for the severity word -- the rendered form is an external contract, so assert it literally rather than structurally.
- [x] `crates/hexput-tests/tests/{lexer,parser,interpreter}.rs` -- sweep every `Code` constant: reach it from real source and assert its span slices the source to the offending text -- proves AC1 across all of 1.2–1.7 rather than for the renderer's fixtures only.
- [x] `crates/hexput-{lexer,parser,interpreter}/src/**` -- fix any span the sweep proves wrong, changing location only, never what is or is not an error -- the AC is precise locations, not new behaviour.
- [x] `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` -- record the three approved decisions as `[DECISION, 2026-09-22]` entries in §7 -- the reference is normative and currently says nothing about rendering.
- [x] `AGENTS.md`, `_bmad-output/implementation-artifacts/sprint-status.yaml` -- record verified progress.

**Acceptance Criteria:**
- Given any diagnostic from Stories 1.2–1.7, when a caller inspects it programmatically, then `category`, `code`, `message` and `span` (with line and column) are readable as structured fields without parsing formatted text.
- Given a diagnostic and the source it came from, when it is rendered for a terminal, then the output names the category, code, message and location, and shows the offending source line with the span marked.
- Given a span crossing more than one line, when it is rendered, then both the start and the end location remain readable in the output.
- Given any `(Diagnostic, source)` pair, including a mismatched source, when it is rendered, then the renderer returns a `String` and never panics, slices a non-boundary byte index, or reads outside the source.
- Given the workspace, when the five CI commands run, then all pass, and `hexput-shared` still declares no dependencies.

## Implementation Notes

- The renderer is `hexput_shared::diagnostics::render_diagnostic(&Diagnostic, &str, RenderOptions<'_>) -> String`, with `RenderOptions { origin: Option<&str> }` (`new()` / `with_origin()`). `hexput-shared` still declares no `[dependencies]`.
- The source line and its marker both carry a four-space indent, matching the Design Notes illustration; the marker therefore still sits at the span's true column *relative to the printed line*. Whitespace before the span is copied through from the line (a tab stays a tab), so a tab-indented line stays aligned whatever width the terminal draws it at.
- End of input after a trailing newline — the common case, since nearly every file ends with one — would otherwise render a blank line and a lone caret. `snippet` falls back to the preceding line and marks one past its last scalar; the header keeps the span's own `line:column`.
- Tabs are copied through into the marker both before **and inside** the span, so a tab in the marked construct does not make the carets underrun it. Wide and combining characters are a documented accepted limitation: a column is one scalar, and measuring display width needs a Unicode table this crate cannot depend on.
- Line extraction searches for **either** `\n` or `\r`, not just `\n`: a lone `\r` really is a line break to the lexer (§2), and stopping at the `\r` of a `\r\n` pair is also what keeps the printed line free of a carriage return. A leading BOM is stripped from line 1, because the lexer does not count it as a column.
- Totality is by construction, not by `Result`: a span whose offset is not a character boundary, or whose saturating end runs past `source.len()`, renders the header alone — that is the whole mismatched-source fallback the doc promises — `span.len` is added with `saturating_add`, and every computed end index is snapped forward to a char boundary. A brute-force test renders ~2000 (span, source) combinations over adversarial sources without a panic.
- `Display` prints `{category} {severity} [{code}] at {span}: {message}`, so an `Error` diagnostic's string is byte-identical to the one Story 1.2 pinned — the pinned test kept its expectation and gained a `Warning` case instead.
- **The producer sweep found no wrong spans.** All 27 codes (7 lexical, 5 syntax, 15 runtime) already slice the offending text, and each sweep additionally re-derives line/column from the byte offset with an independent §2 walk and compares — so a span cannot be accurate in bytes but wrong in the coordinates the renderer marks. No lexer, parser or interpreter change was needed.

## Spec Change Log

## Review Triage Log

Pass 1: blind hunter, edge-case hunter, verification-gap reviewer.

| ID | Finding | Verdict | Evidence | Route |
|---|---|---|---|---|
| A | A zero-length EOF span in a source ending with a newline renders a blank source line and a lone caret | high | Confirmed by probe: `parse("let x = 1;\nreturn 1 +\n")` spans `3:1` (past the last line), so `snippet` extracts an empty line and the offending `return 1 +` is never shown. Every real file ends with a newline, so this is the *common* EOF case, not an exotic one. | patch |
| B | The out-of-range guard checks `span.offset` but never `span.len`, so the documented totality contract overclaims | low | Confirmed: `Span::new(4, usize::MAX/2, 1, 5)` against `"abcdefgh"` renders a snippet with a marker to the line end, not the documented header-alone form. Reachable only from a mismatched source, but the fix is a one-line guard. | patch |
| N | A tab *inside* the span makes the marker underrun it | low | Confirmed: span `0..5` over `"ab\tcd"` renders five `^` under a line the terminal draws eight columns wide. The indent loop copies a tab through, the width loop does not. | patch |
| O | A span ending exactly on the `\r` of a `\r\n` pair is reported as multi-line | low | Confirmed: span `0..4` over `"abc\r\ndef"` renders `1:1-2:1` though it ends on line 1. `ends_line` peeks only inside the span, so the `\r` looks like a lone `\r`. | patch |
| C | `snap_forward`'s doc comment describes the opposite of what it does | low | True on reading: it returns the *smallest* boundary index `>=` its argument, and names a `limit` parameter that does not exist. Exactly the off-by-one class this story exists to prevent. | patch |
| H | Wide and combining characters misalign the marker, and the limitation is undocumented | low | Real: both the indent and the width count scalars, so a CJK or emoji scalar shifts the caret one cell where the terminal draws two. `hexput-shared` must stay dependency-free, so `unicode-width` is out; the tab and BOM rules are documented and this one is not. | patch |
| I | Whether the end location is the last character or one past it is undocumented | low | Real: `end_location` returns one past, and neither its doc nor the reference's `[DECISION]` says so. The rendered form is a declared external contract, so a consumer highlighting a range is off by one half the time. | patch |
| S | Nothing exercises the `hexput-ast` re-export path the spec names as a task | medium | Pre-verified by the verification-gap layer, which reverted the re-export to its old four names and saw the whole suite stay green. The deliverable is asserted only by a doc comment; the breakage would surface in Story 1.9, where the tempting local fix is the forbidden `hexput-shared` edge. | patch |
| L | Tests pin category/code pairings no producer could emit | low | True: `Category::Policy` + `Code::UNKNOWN_CHARACTER` / `Code::EXPECTED_SYNTAX` produce pinned literals like `policy warning [lex.unknown_character]`. The rendering assertions are right; the fixtures read as a bug. | patch |
| J | `line_and_column` is copy-pasted verbatim into three test files | low | True: identical 14-line bodies in `tests/{lexer,parser,interpreter}.rs`, each encoding the §2 line-break rule the spec warns against reimplementing. A change to it now needs three synchronized edits or the sweeps diverge silently. | patch |
| F | `epic-1-context.md` was rewritten wholesale, reversing one contract and dropping three others | medium | Real, but caused by this run's step-1 context regeneration, not by the implementation. The reversed contract (empty vs missing callable-name list) was verified against `epics.md` and the *new* wording is the correct one. Dropped: functions not storable in Global Variables, the interpreter growing no host-call path, and the check's `off`/`warn`/`error` modes. Fix edits an agent-context file. | defer |
| T | Nothing mechanically ties the `Code` constants to the three producer sweeps | low | True: the sweeps hard-code arities of 7/5/15 and `tests/shared.rs` is itself hand-maintained, so a 28th code with a wrong span would ship green. All 27 codes are covered today. The filing reviewer judged it out of scope for this change and so do I. | defer |
| R | `Code::UNTERMINATED_STRING`'s doc says a raw newline ends a string, but the lexer legally allows multi-line strings | low | Found during verification: `let x = "a\nb";` parses clean, and this story's own multi-line test depends on that. The doc comment is stale from Story 1.2, not caused by this change. | defer |
| D | The spec's Design Notes example disagrees with the normative reference on column and marker width | low | Real — the reference's `3:9` with five carets is right and the spec's `3:11` with nine is wrong — but the fix edits this build's spec, which triage rejects by rule. The normative document is the correct one. | reject |
| E | The matrix's multi-line row names an unterminated `/* …` comment, whose span the lexer emits as two characters on one line | low | Real: the sweep pins that code's span as exactly `"/*"`. The row reads "comment **or** a string literal spanning lines" and the string-literal half is covered and tested, so the row is satisfied; only the spec's wording is wrong, and its fix edits this build's spec. | reject |
| G | `Display` and `render_diagnostic` order and punctuate the same fields differently | low | Real inconsistency, but `Display`'s string is a pinned external contract deliberately kept byte-identical for errors (decision 1), so the fix is not a direct correction — it renegotiates a contract. | reject |
| K | The §2 line rule is encoded twice inside `diagnostics.rs` (`ends_line` vs `snippet`'s `rfind`/`find`) | low | True but cosmetic: `snippet`'s comment explains why it searches for either terminator directly, and the two agree. The fix is a refactor, not a direct correction. | reject |
| M | `AGENTS.md` says Stories 1.1–1.8 are done while the tracker says `review` | low | The convention `AGENTS.md` has used since 1.1; rejected on identical evidence in Story 1.6's triage (B1) and Story 1.7's (B12). | reject |
| P | A span whose offset points at the `\n` of a `\r\n` pair prints an empty line | low | No producer emits such a span — every recorded span starts at a token or construct, never inside a line terminator. A guard here protects state never shown to occur. | reject |
| Q | A `span.column` beyond the printed line's length draws carets past the end of the line | low | Reachable only from a mismatched source, which is already the documented header-or-nothing case; adding a clamp is a guard on state never shown to occur. | reject |

Patches A, B, N, O, C, H, I, S, L, J applied. F, T and R appended to deferred-work.md.

## Design Notes

The compact form, with and without an origin label:

```
rules.hxp:3:11: error type[type.operand_mismatch]: cannot multiply a string by a number
    let y = "abc" * 2;
            ^^^^^^^^^
3:11-5:2: error lexical[lex.unterminated_comment]: block comment never closed
    let z = /* opened here
            ^^^^^^^^^^^^^^
```

A multi-line span keeps its end location in the header (`line:col-line:col`); only a single-line span prints the short `line:col` form. Nothing is elided, so the reader always sees where the construct opened and where it ran to.

The span already carries `line` and `column`, so the renderer never rescans the source to find *where* the diagnostic is — it scans only to extract the text of the line it prints. Walk back from `span.offset` to the previous `\n` for the line start and forward to the next `\n` for its end, trimming a trailing `\r`; the marker starts at `span.column` (already in scalar units) and is as wide as the scalar count of `source[span.range()]`, clamped to what remains on that line, with a minimum of one caret so a zero-length span still points somewhere. That is O(one line), plus one walk over `source[span.range()]` for a multi-line span's end location.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: all rules pass, `hexput-parser` still exactly `{hexput-lexer, hexput-ast}`
- `cargo build --workspace --all-targets --locked` -- expected: succeeds
- `cargo test --workspace --locked` -- expected: all tests pass, none skipped
