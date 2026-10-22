---
title: 'Story 6.1: Parse plugin source with metadata, globals, and event handlers'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'ca3912c1f1e2eb3eb39126b135dd9449516942ad'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
warnings: ['oversized']
deferred:
  - summary: >-
      AGENTS.md's "Next step" line skipped Story 6.1's own review.
    evidence: |-
      Review finding (blind-hunter). Routed to defer because the fix edits an agent-context file; settled in this story's finalization commit, whose AGENTS.md update lists Story 6.1's review first.
    location: >-
      AGENTS.md Project Status, "Next step"
    severity: low
  - summary: >-
      tests/script.rs::a_removed_handle_is_unknown_and_its_program_is_released_at_once failed once under parallel load (moka maintenance timing) and passed on reruns.
    evidence: |-
      Review finding (blind-hunter). Pre-existing Epic 4 test (AST Cache), untouched by Story 6.1; seen once in a full release run, then five isolated passes and a clean full run. Settle by making the test wait on moka's `run_pending_tasks` (or polling with a deadline) instead of relying on maintenance timing, then running the full suite repeatedly under load.
    location: >-
      crates/hexput-tests/tests/script.rs
    severity: low
---

<intent-contract>

## Intent

**Problem:** Plugin source (LANGUAGE-REFERENCE §9) — a `plugin { }` metadata block, top-level `let` Global Variables, `@Global(...)` and `@Event(...)` annotations — cannot be parsed today: `plugin` and `@` lex (as `TokenKind::Plugin` / `TokenKind::At`) but `parse` rejects both as `syntax.expected_syntax`. Every later Epic 6 story needs a Plugin AST to register, check and dispatch.

**Approach:** Add a second parser entry point, `hexput_parser::parse_plugin(source) -> Result<PluginProgram, Diagnostic>`, that reuses the existing statement trampoline in a plugin mode and produces a `PluginProgram`: an ordinary `Program` holding only the top-level `let`s and `fn`s, plus side tables for the metadata block, the Global Variable declarations (with their optional `@Global` annotation) and the Event handlers (with their `@Event` annotation's name, `priority` and `async`). Shape errors are `syntax` diagnostics with new stable codes; `parse` (Scripts) is unchanged.

## Boundaries & Constraints

**Always:**
- AST types are data only and live in `hexput-ast`; `hexput-parser` re-exports them (its consumers, `hexput-plugin` included, reach the AST through it — no new crate edge; `check-crate-graph.py` unchanged).
- Parsing stays iterative (no host recursion from input nesting), including the Global-initializer walk.
- Every new code is `Category::Syntax`, declared in `Code` with a doc comment, listed in `Code::ALL`, in `tests/shared.rs`'s enumerated list, and reached by a plugin producer sweep whose span slices the offending source.
- Decisions the reference leaves open (see Design Notes) are recorded as `[DECISION, 2026-10-22]` bullets in LANGUAGE-REFERENCE §9.
- Run every cargo command with `--release` (disk is scarce on this machine).

**Never:**
- No change to `parse`'s behavior, to Script semantics, to the interpreter, `hexput-check`, the wire protocol, `hexput-plugin`/`hexput-globalvar` (Story 6.2+), or any Event-declaration/registration logic (6.3+).
- No special meaning for `BackendRegisteredInit` here (6.4), no `priority`/`async` resolution (6.11/6.12), no Backend-permission check on `@Global` (6.8) — the parser only records structured values.
- No new dependency.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Full plugin | §9 example (block, `let`, `@Global` let, three `@Event` fns, no `;` before each `@`) | `metadata.name()` = `"loyalty_rules"`, `version` entry kept; 2 globals (second: behavior ttl 300 000 ms, locking safe); 3 handlers in source order: `BackendRegisteredInit` (no priority, async false), `OrderPlaced` priority 1 async false, `OrderPlaced` async true priority none | No error |
| Helper fn | unannotated top-level `fn helper(a, b) {}` | in `program`, not a handler | No error |
| Missing block | source starting with `let x = 1;`, or empty | no AST | `syntax.plugin_block_required` on first token (EOF span when empty) |
| Second block | `plugin {name="a"} plugin {name="b"}` | no AST | `syntax.duplicate_plugin_block` on second `plugin` |
| Duplicate key | `plugin { name = "a", name = "b" }` | no AST | `syntax.duplicate_metadata_key` on second `name` |
| Bad metadata | no `name`; `name = 1`; `name = ""`; value `[1]` or an identifier | no AST | `syntax.invalid_plugin_metadata` (block span when `name` missing, else the value) |
| Non-function target | `@Event(A) let x = 1;`; `@Global() fn f(p) {}`; two annotations in a row; annotation at EOF; `@Event` inside a fn body | no AST | `syntax.misplaced_annotation` on `@Name` |
| Bad annotation | `@Foo(A)`; `@Event(priority = 1)`; `@Event(A, priority = 1.5)`; `@Event(A, async = 1)`; `@Event(A, async = true, async = false)`; `@Event(A, colour = 1)`; `@Global(behavior = "weekly")`; `behavior = "ttl"` without `ttl`; `ttl` without behavior ttl; `ttl = "5 minutes"` | no AST | `syntax.invalid_annotation` on the offending name/value |
| Handler arity | `@Event(A) fn h() {}` or `fn h(a, b)` | no AST | `syntax.invalid_handler` on `(`…`)` |
| Other top-level statement | `if (true) {}`, `return 1;`, `f();`, `x = 1;` at top level | no AST | `syntax.invalid_plugin_statement` on its first token |
| Global initializer | `let a = b;`, `let a = f();`, `let a = { f: fn(x) {} };` | no AST | `syntax.invalid_global_initializer` on the identifier / call link / `fn` expression |
| Script parse | `parse("plugin { name = \"x\" }")`, `parse("@Event(A) fn a(p) {}")` | unchanged | `syntax.expected_syntax`, as today |

</intent-contract>

## Code Map

- `crates/hexput-lexer/src/lib.rs` -- already lexes `plugin` (`TokenKind::Plugin`, reserved) and `@` (`TokenKind::At`); `async`, `priority`, `behavior`, `ttl`, `locking` lex as `Ident`. Read-only.
- `crates/hexput-ast/src/lib.rs` -- `Program`, `Statement`/`StatementKind::{Let, Function}`, `Identifier`, `Literal`, `Spanned<T>`, `Function { open, parameters, close, .. }`, `ExpressionKind`/`AccessKind::Call`. Add the plugin types here.
- `crates/hexput-parser/src/lib.rs` -- `parse` builds `Parser { source, tokens, pos, eof, program }` and calls `run()`; helpers `peek`/`span`/`bump`/`expect`/`identifier`/`expected`, `cover`, `diagnostic_preview`; re-export list. Add `parse_plugin` and a plugin-mode field.
- `crates/hexput-parser/src/statements.rs` -- the trampoline: `Frame` stack (`frames.len() == 1` is the file frame), `Task::Statements` dispatch on the next token (L337+), `AfterExpression::Let` (L201), `Task::NamedFunction` (L132), `append`/`finish_statement` (L543, L571 — the `;`-or-`}`-or-EOF rule to relax before `@` in plugin mode), `declare` (duplicate names; `let` and `fn` share the namespace).
- `crates/hexput-parser/src/expressions.rs` -- expression parsing used for `let` initializers; read-only.
- `crates/hexput-shared/src/diagnostics.rs` -- `Code` constants (syntax group at L146+), `Code::ALL` (L308).
- `crates/hexput-tests/tests/parser.rs` -- parser tests; producer sweep `every_syntax_code_spans_the_offending_source` (L1644) is the pattern for a plugin sweep (`hexput_tests::line_and_column`).
- `crates/hexput-tests/tests/shared.rs` -- `every_code_is_enumerated_exactly_once` hand list (L114+).
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §9 (L347–376) -- normative plugin grammar.
- `AGENTS.md` -- Project Status (CLAUDE.md is a symlink to it).

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/diagnostics.rs` -- add `PLUGIN_BLOCK_REQUIRED`, `DUPLICATE_PLUGIN_BLOCK`, `DUPLICATE_METADATA_KEY`, `INVALID_PLUGIN_METADATA`, `MISPLACED_ANNOTATION`, `INVALID_ANNOTATION`, `INVALID_HANDLER`, `INVALID_PLUGIN_STATEMENT`, `INVALID_GLOBAL_INITIALIZER` (`syntax.*` spellings matching the names) under a `Story 6.1` heading and to `ALL` -- stable codes for plugin shape errors.
- `crates/hexput-ast/src/lib.rs` -- add `PluginProgram`, `PluginMetadata` (+ `name()`), `MetadataEntry`, `GlobalVariableDeclaration`, `GlobalAnnotation`, `GlobalVariableBehavior`, `GlobalVariableLocking`, `EventHandler` (see Design Notes) -- the Plugin AST.
- `crates/hexput-parser/src/lib.rs` + `statements.rs` (a new `plugin.rs` module for block/annotation parsing is fine) -- implement `parse_plugin` per the matrix and Design Notes; re-export the plugin types -- the parser entry point.
- `crates/hexput-tests/tests/parser.rs` -- tests for every matrix row, the §9 example verbatim (ttl `"5m"` → 300 000 ms), negative and absent `priority`, `async` defaulting to false, statement indexes pointing at the right `let`/`fn`, nesting (a handler body using `if`/`while`/closures parses normally), and a plugin producer sweep reaching each new code once with span slices -- pins the contract.
- `crates/hexput-tests/tests/shared.rs` -- add the nine code strings -- keeps `Code::ALL` pinned.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` -- add the Design Notes decisions to §9 as dated `[DECISION, 2026-10-22]` bullets -- the reference is normative.
- `AGENTS.md` -- add an Epic 6 paragraph to Project Status recording Story 6.1 and the new test count -- keep agent context current.

**Acceptance Criteria:**
- Given the §9 example plugin, when `parse_plugin` runs, then it returns a `PluginProgram` recording the metadata entries, both Global Variable declarations with their annotation values, and each handler's Event name, `priority` and `async` (false when absent), in source order.
- Given source missing its `plugin { }` block, declaring a duplicate metadata key, or annotating a non-function with `@Event`, when `parse_plugin` runs, then it returns a `syntax` `Diagnostic` with the matrix's code whose span slices the offending source, and no AST.
- Given any Script that parsed before this change, when `parse` runs, then its result is identical (existing parser tests pass untouched).
- Given the release CI steps, when run, then all pass.

## Design Notes

Decisions (record in §9):
1. **Top level** holds the block, then only `let` (Global Variable) and `fn` (handler if annotated, otherwise a helper callable from handlers); anything else is `invalid_plugin_statement` — a plugin has no run in which such a statement could execute.
2. **Terminators:** at the plugin's top level a statement's `;` may be omitted before `@` (both the §9 and PRD examples rely on this); the block's `}` needs no `;`, an optional one is accepted.
3. **Metadata values** are scalar literals: string, number (optionally `-`-prefixed), `true`/`false`, `null`; `name` is a non-empty string. Trailing comma allowed; `plugin {}` lacks `name`.
4. **Annotations:** one per declaration; arguments `key = value`, trailing comma allowed, each key once. `@Event` takes the Event name first as a bare identifier, then optional `priority` (whole number, optional `-`, |n| ≤ 2^53, stored `i64`) and `async` (`true`/`false`). `@Global` takes optional `behavior` (`"forever"`, `"ttl"`, `"separate_each_trigger"`, `"keyed"`), `ttl` (required with, and only with, `"ttl"`: `^[1-9][0-9]*(ms|s|m|h|d)$`, stored as milliseconds, overflow refused) and `locking` (`"safe"`, `"unsafe"`); `@Global()` is allowed and means defaults. Check order: nesting → known name → arguments → target.
5. **Handler arity** is exactly one parameter; its name is free (`params` is convention) — a different name cannot produce a wrong answer.
6. **Global initializer** may contain literals, array/object literals, grouping, unary/binary operators and property/index access, but no identifier, anonymous function, or call ("expression over literals"; Globals cannot hold functions).

Suggested shape (adjust freely if the ACs hold; keep a span on every recorded value):

```rust
pub struct PluginProgram { pub program: Program, pub metadata: PluginMetadata,
    pub global_variables: Vec<GlobalVariableDeclaration>, pub handlers: Vec<EventHandler> }
pub struct EventHandler { pub statement: usize /* into program.statements */, pub function: Identifier,
    pub event: Identifier, pub annotation: Span, pub priority: Option<Spanned<i64>>,
    pub is_async: bool, pub async_span: Option<Span> }
pub enum GlobalVariableBehavior { Forever, Ttl { millis: u64 }, SeparateEachTrigger, Keyed }
```

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: pass, unchanged
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass, including the new parser and shared tests

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/parser.rs` (`section_nine_example_records_metadata_globals_and_handlers_in_source_order`, `priority_is_signed_whole_and_bounded_by_two_to_the_fifty_three`, `unannotated_top_level_function_is_a_helper_not_a_handler`, `handler_bodies_parse_as_ordinary_code_with_nesting_and_closures`, `block_and_statement_terminators_follow_the_plugin_rules`, `metadata_block_requirements`, `misplaced_annotations_name_the_annotation`, `invalid_annotations_name_the_offending_argument_or_value`, `well_formed_annotations_are_accepted_and_recorded`, `handler_arity_is_exactly_one_parameter_of_any_name`, `other_top_level_statements_are_rejected_at_their_first_token`, `global_initializers_are_expressions_over_literals`, `deep_global_initializers_parse_without_host_recursion`, `a_function_and_a_global_share_the_declaration_namespace`, `script_parse_still_refuses_plugin_source_as_before`, `every_plugin_code_spans_the_offending_source`) and `crates/hexput-tests/tests/shared.rs` (`plugin_codes_are_stable_and_distinct`, the nine strings in `every_code_is_enumerated_exactly_once`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Test-side API assumptions the implementer should match (the spec's Code Map does not pin them): `PluginProgram { program, metadata, global_variables, handlers }`; `PluginMetadata::name(&self)` and `entries` (each with `key: Identifier`); `GlobalVariableDeclaration { statement, name, annotation: Option<GlobalAnnotation> }` with `GlobalAnnotation { behavior, locking }` as `Option<Spanned<_>>`; `GlobalVariableBehavior`/`GlobalVariableLocking` derive `PartialEq`/`Debug`/`Copy`; `EventHandler` fields exactly as the Suggested shape. Decisions the tests pin beyond the matrix: two annotations in a row report the first (`@Event`); a `ttl`/`behavior` mismatch is only required to lie inside the annotation's parentheses; a Global default `@Global()` leaves `annotation` as `Some`, an unannotated `let` as `None`.

- Implementation (2026-10-22): `parse_plugin` lives in `crates/hexput-parser/src/lib.rs`, the plugin-mode logic in the new `crates/hexput-parser/src/plugin.rs`; the trampoline in `statements.rs` consults it at the top level (`plugin_top_level_statement`, `record_function`, `record_global`, `check_global_initializer`) and `run` now takes `&mut self` so both entry points share it. `GlobalAnnotation` also carries its `span` (`@Global` through `)`), and `MetadataEntry` its `equals` span.
- Test correction (2026-10-22): `handler_bodies_parse_as_ordinary_code_with_nesting_and_closures` omitted `;` after a mid-body `if { }` and `for { }`, which LANGUAGE-REFERENCE §2 requires (only the last statement of a block may drop it) and the spec says handler bodies parse "normally"; the two `;` were added. No expectation was weakened.
- Verification note: one full `cargo test --workspace --locked --release` run saw `tests/script.rs::a_removed_handle_is_unknown_and_its_program_is_released_at_once` (Epic 4's AST cache, moka maintenance timing) fail once under parallel load; it passed on five isolated reruns and on the next full run (1113 passing). Untouched by this story.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 27 findings — high 0, medium 9, low 13, false 5, maybe-false 0
- findings:
  - `[low]` `[patch]` (blind) Tracking state contradicts itself: sprint-status `ready-for-dev`, stale Auto Run Result, spec `in-review` — finalization sets the spec `done`, sprint-status `review`, and rewrites Auto Run Result.
  - `[low]` `[defer]` (blind) AGENTS.md "Next step" omits Story 6.1's review — agent-context file, so deferred; settled by the finalization's AGENTS.md update.
  - `[low]` `[defer]` (blind) Observed flake in `tests/script.rs` not recorded as deferred work — pre-existing Epic 4 test untouched by this story; added to `deferred`.
  - `[false]` `[reject]` (blind) Release-only verification hides overflow/`debug_assert` checks — `-magnitude` has magnitude ≤ 2^53 (no i64 overflow), `-n` is f64 negation, `ttl` uses `checked_mul`; no new arithmetic can overflow, and CI still runs the debug profile. The fix would also edit this build's spec.
  - `[low]` `[reject]` (blind) `ttl`'s own span is dropped, folded into `behavior`'s — a `ttl` mismatch is still reported on the `ttl` key; a later per-annotation refusal (6.8) points at `behavior = "ttl"`, which names the forbidden behaviour. Unlikely to matter; the fix adds a public field.
  - `[medium]` `[patch]` (blind) A missing `,` in the block is reported as an invalid value spanning the next entry — verified (`name = "a" version = "1"` gave `invalid_plugin_metadata` over `"a" version = "1"`). Fixed: `metadata_value`'s scan now also stops at depth 0 at a `key =` pair, `;`, `let`, `fn`, `@` or `plugin`, so the caller reports "expected `,` or `}`" there; test `a_missing_separator_in_the_block_is_reported_where_the_next_entry_begins`.
  - `[low]` `[reject]` (blind) Loose `-` (`- 5`, `-/*c*/5`) and `-0` stored as `-0.0` — the same tokens form a negative number everywhere in Hexput, and `-0` yields `-0.0` in a Script too (the wire layer normalizes on output); nobody meets this in practice and pinning it adds guards.
  - `[medium]` `[patch]` (blind) ttl multiplication-overflow path untested — same as the verification-gap row; tests added (see there).
  - `[low]` `[patch]` (blind) Untested branches: nested `@Global`, `async = false`, negative metadata span (`@`+non-identifier, `plugin` without `{` and `@Event(A, async)` give `expected_syntax`, which is the right code for a malformed shape) — added tests for nested `@Global`, `async = false` with its span, and the `-1.5` span including `-`.
  - `[low]` `[patch]` (blind) Annotation target chosen by string compare with `""` as unknown — replaced with `Option<TokenKind>` (`Fn`/`Let`) compared against the next token directly.
  - `[low]` `[patch]` (blind) `PluginMetadata::name()` silently returns `""` for a hand-built value — doc comment now states the parser guarantee and the hand-built fallback.
  - `[low]` `[reject]` (blind) `async` held as `is_async` + `async_span` unlike other `Option<Spanned<_>>` fields — the spec's Suggested shape and pinned test API fix exactly these fields; changing them edits this build's spec.
  - `[low]` `[patch]` (blind) `;`-before-`@` relaxation applied below the top level, against the §9 decision "Inside bodies the ordinary §2 rule applies" — `finish_statement` now takes `top_level` (`frames.len() == 1` from `append`); test asserts `expected_syntax` at `@` inside a body.
  - `[medium]` `[patch]` (edge) Missing closing `}` makes the value span run to end of input — same root cause as the missing-comma row; the bounded scan now stops at `let`/`fn`/`@`, and a test pins "expected" at `let`.
  - `[medium]` `[patch]` (edge) Value longer than one scalar swallows the next entry — same root cause; fixed by the same scan change.
  - `[low]` `[reject]` (edge) Metadata number above 2^53 silently rounded — metadata numbers are Hexput number literals (f64, §4), rounded exactly as in any Script; `priority` differs only because it is stored as `i64`. Fixing adds a branch for no reported use.
  - `[low]` `[patch]` (edge) `name()` returns `""` for a value built outside the parser — same root cause as the blind row; doc comment patched.
  - `[medium]` `[patch]` (edge) Invalid-value span can include later keys or the rest of the file — same root cause as the missing-comma row; fixed by the bounded scan (test: `v = 1 + 2 w = 3` spans `1 + 2`).
  - `[medium]` `[patch]` (verification-gap) Explicit `async = false` never accepted in a test — added to `priority_is_signed_whole_and_bounded_by_two_to_the_fifty_three` (not async, `async_span` slices `false`), plus `async_span` `None` when absent.
  - `[medium]` `[patch]` (verification-gap) Global Variable `statement` index unpinned — `assert_global_statements` checks each global's statement is the `let` with its name (§9 test), and `global_statement_indexes_count_functions_declared_before_them` pins indices 1 and 3 with functions before them.
  - `[medium]` `[patch]` (verification-gap) ttl overflow after multiplication never reached — `"18446744073709551615d"` and `"213503982335d"` now asserted `invalid_annotation` on the value.
  - `[medium]` `[patch]` (verification-gap) Metadata values and `forever`/`separate_each_trigger` behaviours not asserted — the block test now asserts every entry's literal, the §9 test `version`'s string, and the behaviour loop each recorded variant.
  - `[false]` `[reject]` (intent) Diff implements the story although the intent says "Halt after planning" — the planning invocation did halt (its Auto Run Result recorded it); implementation and this review are later stages the workflow ran for this spec, so no reading of the original invocation is violated by them.
  - `[low]` `[patch]` (intent) Status artifacts contradict each other — same root cause as the first blind row; resolved in finalization.
  - `[false]` `[reject]` (intent) §9 decisions settle matters beyond the ACs (`@Global` arguments, ttl syntax) — the spec's Boundaries require recording them; Stories 6.8–6.10 enforce and give behaviour, the parser only records structured values, which the intent's "Plugin AST recording … Global Variables with their annotation values" covers.
  - `[false]` `[reject]` (intent) Tests pin behaviour beyond the ACs — extra coverage of the specified matrix is not a defect.
  - `[false]` `[reject]` (intent) `--release` request — the auditor itself reports no divergence.

## Auto Run Result

Status: done
Blocking condition: none

**Summary.** `hexput_parser::parse_plugin` parses Plugin source (LANGUAGE-REFERENCE §9) into a `PluginProgram`: an ordinary `Program` of the top-level `let`s and `fn`s plus the metadata block, the Global Variable declarations (with optional `@Global` behaviour/ttl/locking) and the `@Event` handlers (Event name, `priority`, `async`), in source order. Nine new `syntax.*` codes cover the plugin shape errors; `parse` is unchanged.

**Files changed.**
- `crates/hexput-shared/src/diagnostics.rs` — nine Story 6.1 `syntax` codes, in `Code::ALL`.
- `crates/hexput-ast/src/lib.rs` — the Plugin AST types.
- `crates/hexput-parser/src/lib.rs` — `parse_plugin`, shared `Parser::new`, re-exports.
- `crates/hexput-parser/src/plugin.rs` — block, annotation, handler/global recording and global-initializer walk.
- `crates/hexput-parser/src/statements.rs` — plugin-mode hooks in the trampoline; top-level-only `;` relaxation before `@`.
- `crates/hexput-tests/tests/parser.rs`, `crates/hexput-tests/tests/shared.rs` — the contract tests.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` — six `[DECISION, 2026-10-22]` bullets in §9.
- `AGENTS.md`, `sprint-status.yaml`, `epic-6-context.md`, this spec — tracking and context.

**Review findings.** 27 findings (medium 9, low 13, false 5). Patched: the bounded metadata-value scan (missing `,`/`}` reported where the next entry begins; medium, 4 rows), the four verification gaps (`async = false`, global statement indexes, ttl overflow after multiplication, recorded metadata values and behaviours; medium), the top-level-only `;` relaxation, the `TokenKind` annotation target, the `name()` doc, extra branch tests, and the tracking contradictions (low). Deferred: AGENTS.md "Next step" (settled in this commit) and the pre-existing Epic 4 AST-cache flake. Rejected: release-only overflow worry (false: no new arithmetic can overflow), the dropped `ttl` span (low; public field for an unlikely need), loose `-`/`-0` (low; same as Hexput everywhere), `async` representation (spec-pinned), metadata numbers above 2^53 (low; §4 number semantics), and four intent-alignment rows (false: the planning halt already happened; decisions and tests are spec-required).

**Follow-up review recommended: true** — patched medium entries: 5 (first pass, two or more), high: 0. Named risk: the metadata-value scan's new stopping rule (a depth-0 `key =`, `;`, `let`, `fn`, `@`, `plugin`) decides which diagnostic a malformed block gets and has not been through an independent review layer.

**Verification.** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (26 edges, unchanged); `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 1115 passing, 0 failing.

**Residual risks.** Verified only under the release profile on this machine (CI's debug run remains the gate for overflow checks, though none of the new arithmetic can overflow); the pre-existing AST-cache test flake is deferred.
