---
title: 'Story 1.10: Check a script without running it'
type: 'feature'
created: '2026-09-22'
status: 'done'
baseline_commit: 'bbd331830c5869a19ad0d32f4e2b634e898ec175'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
  - '{project-root}/_bmad-output/implementation-artifacts/epic-1-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** `hexput-check` is an empty stub with a doc comment. Nothing inspects a Script before it runs, so a typo'd variable, a wrong argument count or `"abc" * 2` inside a rarely taken branch is found only when execution reaches it — halfway through a rule that has already called back into the host. The CLI can evaluate a file but cannot tell an author what is wrong with one without running it.

**Approach:** Implement the check pass as one pure function over a parsed AST plus what the environment provides and the active policy, returning findings in the same `Diagnostic` shape as every runtime error. Then add `hexput check <script>` to the CLI over Story 1.9's surface: it parses, checks, renders the findings with Story 1.8's renderer, and exits non-zero only when an error-severity finding exists. Nothing is executed at any point.

## Boundaries & Constraints

**Always:** `hexput-check` depends on `hexput-ast` alone (AD-8) — diagnostics reach it through that crate's re-export, and `scripts/check-crate-graph.py` pins the set. Its entry point is one pure function holding no state between calls: same inputs, same findings, every time. Findings carry the §7 category, a stable code, a message and a span exactly as a runtime error does, so the CLI, a Backend response and the language server render them identically. Every check the pass performs is decidable from the AST alone — no inference across bindings, no judgement depending on a runtime value, and therefore no false positive: a finding means that code, if reached, really would fail. `hexput-cli-core` keeps exactly its four workspace edges. Only `hexput-bin` owns a `fn main()` or exits the process.

**Never:** No execution of any kind — no interpreter edge, no evaluation of a `--var` value under the check command, no constant folding that could raise. Not a type system: never infer a binding's type from its initializer, never check a call's return value, never report anything that depends on what a value turns out to be. No check `off`/`warn`/`error` mode selection and no per-execution override (Story 3.10), no capability grant or denial and no budget charge (AD-3), no plugin grammar findings (`@Event`, `@Global`), no autofix or suggestion machinery, no colour. No change to what the lexer, parser or interpreter treats as an error, and no new runtime behaviour.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Clean script | a script with no findings | `<path>: no findings` on stdout, exit 0 | N/A |
| Undeclared read | `return x;` | `reference.undeclared_identifier` spanned on the name, exit 1 | finding, never a panic |
| Undeclared assignment | `x = 1;` at top level | `reference.undeclared_assignment` spanned on the name, exit 1 | finding |
| Declared later in the block | `return x; let x = 1;` | still undeclared — `let` does not hoist, only `fn` does | finding |
| Arity mismatch | `fn f(a) {} f(1, 2);` | `arity.argument_count` spanned on the call, exit 1 | finding |
| Arity, reassigned name | `fn f(a) {} f = 1; f(1, 2);` | no arity finding — the name no longer certainly names that function | silence, not a guess |
| Literal operand | `return "abc" * 2;` | `type.operand_mismatch` spanned on the offending operand, exit 1 | finding |
| Literal operand, collection | `return [1] + "x";` | `type.operand_mismatch` — a collection has no to-string | finding |
| Not a literal case | `let s = "abc"; return s * 2;` | no finding — that needs inference across a binding | silence by design |
| Unreachable code | a statement after `return` in the same block | warning spanned on the first unreachable statement, exit 0 | warning only |
| Hoisted declaration after `return` | `return g(); fn g() { return 1; };` | no finding — a hoisted `fn` is bound before the block runs, so it is not dead code | silence |
| Unused local | `let x = 1; return 2;` | warning spanned on the name, exit 0 | warning only, never rejects |
| Warnings only | a script whose findings are all warnings | findings on stderr, `<path>: 2 findings, 0 errors` on stdout, exit 0 | N/A |
| Unknown call, list given | `hexput check --callable log` on `f();` | a finding naming `f`, exit 1 | finding |
| Unknown call, no list | `hexput check` on `f();`, no `--callable` given | no finding at all — the caller did not claim to know what is callable | silence |
| Declared callable is called | `hexput check --callable log` on `log(1);` | no finding; arity is unknown for a non-local name and is never guessed | silence |
| Starting variable | `hexput check --var n` on `return n * 2;` | clean — `n` is declared, exit 0 | N/A |
| Starting variable, bad name | `--var 1n`, or `--var n=5` on the check command | usage error naming the argument, exit 2, nothing parsed | reported, not panicked |
| Disabled construct | a policy disabling `loops`, and a script with a `while` | `policy` finding naming the toggle, spanned on the construct's keyword, exit 1 | finding |
| Unparseable script | `let x = 1 +;` | the parse diagnostic rendered as today, exit 1, no check pass runs | rendered, not panicked |
| Adversarial nesting | a script nested tens of thousands deep | findings or clean, never a host stack overflow | bounded walk |
| Same surface as eval | a missing file, non-UTF-8 bytes, a bad flag | byte-identical behaviour and exit codes to `hexput eval` | Story 1.9's contract |

## Decisions (approved 2026-09-22)

1. **The callable-name list is opt-in on the CLI.** `hexput check` supplies **no** list unless `--callable <name>` is given, so a Script developed locally against host functions checks clean instead of reporting every legitimate call. Giving the flag at least once supplies a list, and any called name that is neither in it nor a locally declared function is a finding — which is how an author asks for typo detection over the names they expect. Each `--callable` value must be a §2 identifier, validated by the lexer through `is_identifier`, and a repeated name is a usage error exactly as a repeated `--var` is. The daemon's own caller (Story 3.10) passes its Session's Registered Function names the same way. The distinction the pass must honour is unchanged: a **missing** list suppresses the unknown-call finding entirely, while an **empty** list does not — an empty list means "nothing but local functions is callable".

2. **The check command declares starting variables by name only.** `hexput check --var <name>`, repeatable, declares a starting variable in the Script's root scope for the pass exactly as `evaluate_with_variables` binds one for the interpreter — before the top-level `fn` hoisting, so a name a top-level `let` or `fn` also declares is a finding rather than a silent double binding. No value is accepted and none is evaluated: a static check needs the name, never the value, and evaluating a `--var` expression would be execution on a path whose entire promise is that nothing runs. The check entry point therefore takes a starting-variable name list beside the callable-name list — the Spine's "AST plus the callable-name set and the active policy" gains one more thing the environment provides, and the Spine is amended to say so. `hexput eval`'s `--var name=<expression>` is unchanged; the two commands' flags share a name because they declare the same thing, and differ because only one of them can have a value.

3. **`hexput check` always writes a one-line summary to stdout.** `rules.hxp: no findings` when clean, `rules.hxp: 3 findings, 1 error` otherwise — so a human sees an answer either way, and the epic's "distinguishes clean from warnings-only" is visible where a person actually reads it. The findings themselves are diagnostics and go to **stderr** through `render_diagnostic` with the script path as origin, exactly as every diagnostic does under `hexput eval`, so the summary can be piped while the findings stay on the error channel. Exit `0` when there is no error-severity finding — warnings included — `1` when there is one or when the source cannot be parsed or read, `2` for a usage error. Findings are reported in source order.

4. **Duplicate `let` and loop control outside a loop are the parser's, and stay there.** Both are rejected during parsing, so neither can appear in a parsed AST and the pass cannot produce them; it documents that rather than carrying unreachable code, and a test pins the parser's rejection so the guarantee cannot quietly disappear. A caller sees the same category, stable code, message and span either way, which is what §10's table is actually promising.

5. **Severity follows one line.** A finding naming something that would certainly fail if reached is an **error**: undeclared read or assignment, wrong argument count, a literal-operand type error, a disabled construct, a call to a name the supplied list does not contain. A finding describing code that cannot fail is a **warning**: an unused local, and code unreachable after `return`/`break`/`continue`. This is what makes "a warning-only finding can never reject a script" true by construction instead of by a maintained list.

6. **`hexput-check` depends on `hexput-ast` alone.** The `hexput-shared` edge Story 1.1 gave it is dropped: AD-8's text and the Spine's graph both say `hexput-ast` alone, and that crate re-exports the whole diagnostics shape. When Epic 3 needs `hexput-shared::budget` here, adding the edge back is a deliberate amendment rather than a pre-opened door.

</frozen-after-approval>

## Code Map

- `crates/hexput-check/src/lib.rs` -- 7 lines of doc comment, no code. The whole pass lands here. Its `Cargo.toml` currently declares `hexput-ast` **and** `hexput-shared`; AD-8 and the Spine's graph say `hexput-ast` alone, and `hexput-ast` re-exports the whole diagnostics shape, so drop the `hexput-shared` edge.
- `crates/hexput-ast/src/lib.rs` -- the AST the pass walks. `Program::block`/`expression` resolve `BlockId`/`ExprId` into flat arenas; **arena order is not tree order** (index expressions are forward references), so traverse by following IDs. `StatementKind` and `ExpressionKind` are the two matches the pass is built around; `AccessKind::Call` carries the arguments, and a call is one link in an `Access` chain, never a node of its own.
- `crates/hexput-ast/src/lib.rs:10-12` -- re-exports `Category`, `Code`, `Diagnostic`, `RenderOptions`, `Severity`, `Span`, `render_diagnostic` from `hexput-shared`. This is the only path `hexput-check` may use to reach them.
- `crates/hexput-shared/src/diagnostics.rs:120-230` -- `Code`'s associated constants. Reuse `UNDECLARED_IDENTIFIER`, `UNDECLARED_ASSIGNMENT`, `ARGUMENT_COUNT`, `OPERAND_MISMATCH` — a finding must carry the same code the runtime failure would. The new codes (unreachable, unused, unknown call, disabled construct) are added here as associated constants, next to the existing ones. `Diagnostic::warning` already exists and is unused so far.
- `crates/hexput-parser/src/statements.rs:486,554-568` -- **the parser already rejects** `break`/`continue` outside a loop and duplicate `let`/`fn`/parameter names in one block, including the `for` binding and function parameters seeding their body's namespace. Neither can appear in a parsed AST, so the check pass cannot produce them; do not write dead code for them. Confirm this with a test rather than reimplementing it.
- `crates/hexput-interpreter/src/machine.rs:214-236,555-575,685-700` -- the scoping the pass must mirror exactly: `open` hoists every named `fn` in a block before its statements run; `enter` gives each block a fresh scope; a `for` binding and the loop body share one scope per iteration; a call's parameters and the body's own statements share one scope whose parent is the function's **defining** scope, never the caller's.
- `crates/hexput-interpreter/src/machine.rs:367,823` -- the exact messages the runtime uses for an undeclared assignment and an undeclared read. A finding for the same mistake should not read differently from the error.
- `crates/hexput-cli-core/src/args.rs` -- `Command` is a `clap` `Subcommand` with one variant; `check` is the second. The doc comment already says so.
- `crates/hexput-cli-core/src/lib.rs:95-140` -- `eval` is the shape `check` follows: read the file, render with the script path as origin, one exit code per outcome. `read_script` and the `render` closure are reusable as-is; `starting_variables`/`evaluate_expression` stay eval's alone — decision 2 gives check a name-only `--var`. `is_identifier` asks the lexer whether a name is a §2 identifier; both `--var` and `--callable` validate through it, and its duplicate-name rejection is the pattern to copy.
- `scripts/check-crate-graph.py:52-66` -- `EXACT_DEPENDENCIES`. Add `hexput-check`; `hexput-cli-core`'s entry already lists the `hexput-check` edge this story finally uses.
- `crates/hexput-tests/{Cargo.toml,tests/}` -- integration tests, one file per crate, public API only. Add `hexput-check` as a dev-dependency plus `tests/check.rs`; extend `tests/cli_core.rs` for the check command. `tests/shared.rs` is the hand-maintained enumeration of every `Code`.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §10 -- normative for what the pass checks and what it deliberately does not. §4.2/§4.3 are normative for the literal-operand rules the pass duplicates.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- the Story 1.8 entry routes `pub const ALL: &[Code]` to "the next change that adds a code". This is that change.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-check/Cargo.toml`, `scripts/check-crate-graph.py` -- drop the `hexput-shared` edge and pin `hexput-check`'s exact dependency set to `hexput-ast` -- AD-8's normative text and the Spine's graph both say `hexput-ast` alone, and nothing else makes that mechanical.
- [x] `crates/hexput-shared/src/diagnostics.rs` -- add the finding codes this pass needs, and `pub const ALL: &[Code]` enumerating every code -- a finding must carry a stable code, and the Story 1.8 deferred entry routes the coverage list to the next change that adds one.
- [x] `crates/hexput-check/src/lib.rs` -- the public surface: the policy (the six FR-3 construct toggles, all defaulting to enabled), what the environment provides (the starting-variable names, and the callable-name list as an *optional* set — decisions 1 and 2), and a findings result that distinguishes clean from warnings-only from errors -- the entry point is public API three later crates call.
- [x] `crates/hexput-check/src/**` -- the scope-and-flow walk, driven by an explicit work stack: lexical scopes mirroring the interpreter's (block, `for` binding with its body, parameters with the function body, `fn` hoisting), undeclared reads and assignments, arity against locally declared functions, unreachable code, unused locals, unknown calls, disabled constructs -- the story's deliverable, and nesting is attacker-controlled so the host stack may not grow with it.
- [x] `crates/hexput-check/src/**` -- the literal-operand type rules of §4.2/§4.3, reported only when every operand involved is a literal -- "no inference across bindings" is what keeps the pass free of false positives.
- [x] `crates/hexput-cli-core/src/{args,lib}.rs` -- the `check` subcommand over Story 1.9's surface: repeatable `--var <name>` and `--callable <name>`, both validated by `is_identifier` and both rejecting a repeat; parse, check, render each finding to stderr with the script path as origin, write decision 3's summary line to stdout, and exit non-zero only for an error-severity finding -- the story's user-facing half; nothing is executed.
- [x] `crates/hexput-tests/Cargo.toml`, `crates/hexput-tests/tests/check.rs` -- cover every finding, the clean/warnings-only/error distinction, the omitted-versus-empty callable list, starting-variable names (including one colliding with a top-level declaration), and a deeply nested script -- the pass is public API and its silence is as load-bearing as its findings.
- [x] `crates/hexput-tests/tests/check.rs` -- cross-check the literal-operand findings against the interpreter: for each case, the pass reports exactly when evaluating the same source fails, with the same code -- the pass duplicates §4's rules and nothing else would catch them drifting apart.
- [x] `crates/hexput-tests/tests/{cli_core,check,shared}.rs` -- the check command's exact stdout, stderr and exit code for each I/O-matrix row; a test pinning that the parser rejects duplicate `let` and loop control outside a loop (decision 4), placed beside the rule it protects rather than in the parser's own file; and the `Code::ALL` coverage assertion -- the exit contract is what a calling script depends on, and decision 4 is a guarantee borrowed from another crate.
- [x] `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §10/§12, `ARCHITECTURE-SPINE.md` + its `.memlog.md` -- record decisions 1-5 in §10/§12, and amend the Spine's AD-8 rule for the starting-variable names the entry point also takes -- both documents are normative and marked final.
- [x] `AGENTS.md`, `_bmad-output/implementation-artifacts/sprint-status.yaml`, `deferred-work.md` -- record verified progress, and close the Story 1.3 and Story 1.8 entries this story resolves.

**Acceptance Criteria:**
- Given a parsed AST, when the check pass runs twice with the same inputs, then it returns the same findings and retains nothing between calls.
- Given the workspace, when the five CI commands run, then all pass, and `hexput-check` depends on exactly `hexput-ast` — never the interpreter, `hexput-rpc` or `hexput-enforce`.
- Given a Script the pass reports clean, when the same Script is evaluated, then it does not fail for anything the pass claims to check — the pass has no false positives.
- Given any script at all, including adversarially nested or malformed source, when the check command runs, then it returns an exit code and never panics or overflows the host stack.
- Given a Script with findings, when the check command runs, then no statement of it is executed and no value is computed from it.

## Implementation Notes

**Landed 2026-09-22.**

- `hexput_check::check(&Program, &Environment, &Policy) -> Findings` is the entry point. `Environment` carries the starting-variable names and an **optional** callable-name list (`with_callables` on an empty iterator supplies an empty list; never calling it supplies none). `Policy` is the six FR-3 toggles, all `true` by default. `Findings` exposes `diagnostics()`, `outcome()` (`Clean`/`Warnings`/`Errors`), `has_errors()`, `len()` and `error_count()`.
- `pass.rs` walks on an explicit work stack of `Job`s with a `Vec<Scope>` mirroring the interpreter's scope chain: block scopes, `fn` hoisting per block, a `for` binding sharing one scope with its body, parameters sharing one scope with the function body. **Function bodies are deferred to the end of the block that defines them** — walking them where they are written would report `fn f() { return x; }; let x = 1;` as undeclared, which is the false positive the pass must never produce. The cost is a false negative for a body genuinely called before a `let` it reads; the runtime still catches that, and the trade only ever goes this direction.
- Findings are sorted by span offset at the end, because unused locals surface when a scope closes and deferred bodies are walked after the statements around them. A stable sort keeps two findings at one offset in production order.
- `operands.rs` reimplements §4.2/§4.3 for literal operands. It reports only when **both** operands are literals: the runtime's message names both types (`cannot apply \`*\` to array and number: …`), so a half-known pair could not carry it verbatim. The offending operand is the one the runtime reaches first (left before right) and the span is that operand's, not the operator's. The frozen matrix originally said "spanned on the operator", which contradicted the frozen Boundaries rule that a finding carries "a span exactly as a runtime error does"; the human authorised correcting that cell on 2026-09-22, and the matrix now reads "the offending operand". The same authorisation added the hoisted-declaration row below it. `a_literal_operand_finding_matches_what_the_interpreter_does` compares all 2,197 operator/operand combinations against `hexput_interpreter::evaluate` — code, message, span and category — which is what keeps a second implementation of §4 from drifting.
- Arity is checked against a name that certainly still holds the function it was declared with: a named `fn`, or a `let` whose initializer is written as a function literal, and only while **no assignment anywhere in the Script targets that name**. The reassigned-name pre-pass reads `program.blocks` directly, since the block arena holds every block and needs no traversal of its own.
- An unresolved *callee* is never an undeclared-identifier finding: without a callable list the pass cannot tell a Registered Function from a typo, which is exactly why a missing list suppresses the unknown-call finding. With `rpc_calls` disabled it is a `policy` finding instead, and the unknown-call check is skipped — no host call is permitted at all, which is the stronger statement.
- A hoisted `fn` declaration after a `return` is **not** unreachable code. Caught by `a_hoisted_function_is_callable_before_its_declaration` during implementation: hoisting means declaration order does not matter, so flagging it was wrong.
- `hexput-check`'s `hexput-shared` edge is gone (decision 6) and `scripts/check-crate-graph.py` pins `{hexput-ast}` exactly. `Code::ALL` enumerates all 31 codes and `tests/shared.rs` asserts it against the file's own list, closing Story 1.8's deferred entry.
- The CLI's `check` shares `read_program` with `eval` — one read-and-parse, so the two commands cannot diverge on an unreadable file, non-UTF-8 bytes or a syntax diagnostic. `eval` was refactored onto it in the same change.
- Tests: 49 in `crates/hexput-tests/tests/check.rs` (findings, silences, the producer sweep, the interpreter cross-check, decision 4's parser pin, 20 000-deep nesting in blocks, groups, arrays, function bodies and call chains) and 18 added to `tests/cli_core.rs`. 289 workspace tests pass.

**Changed by review (pass 1).** Four behaviours moved, and they are worth knowing about beyond the triage rows: statements past a `return`/`break`/`continue` are **no longer walked at all**, so nothing inside dead code is reported except the one warning saying it is dead (hoisted `fn` declarations are still walked — they really are callable); a **named function nothing reads is now reported unused**, like any other declaration the Script makes; a **repeated starting-variable name binds once and reports nothing**, matching `evaluate_with_variables`'s documented last-wins rather than inventing a finding with no span to point at; and `check`'s contract now names the caller obligation it always had — ids must index the program's own arenas, which is `hexput-ast`'s standing rule.

**Not done, and why:** the check command has no flag for the `Policy` toggles, so `policy.construct_disabled` is reachable only from a library caller — Story 3.9 owns that surface and inventing a CLI spelling for it now would fix an interface that story has not designed. Recorded in `deferred-work.md`, along with the literal-operand cases the both-operands rule gives up.

## Spec Change Log

## Review Triage Log

Pass 1: blind hunter, edge-case hunter, verification-gap reviewer.

| ID | Finding | Verdict | Evidence | Route |
|---|---|---|---|---|
| A | The crate's top doc comment still states the pre-amendment AD-8 signature, omitting the starting-variable names | medium | True, and in the very change that amended `ARCHITECTURE-SPINE.md` and `LANGUAGE-REFERENCE.md` to add them. The first doc a reader of `hexput-check` meets contradicted the two normative documents. | patch |
| B | Statements the pass has just called unreachable are still walked, so an error can be reported inside dead code | high | Reproduced: `return 1; x = 2;` reports `reference.undeclared_assignment` and exits 1, while `hexput eval` on the same file prints `1` and exits 0. Code that can never run can never fail, so an error there is a certain claim about code that is certainly not reached. | patch |
| C | An unused named function is never reported, while an unused function-valued `let` is | medium | Reproduced: `fn helper() { return 1; }; return 2;` is silent and `let helper = fn() { return 1; }; return 2;` warns, for the same mistake. `Binding::report_unused`'s own doc listed the intended exemptions and did not name declarations, so this was divergence rather than decision. | patch |
| D | Literal failures other than operand mismatch — `2 / 0`, `1e308 * 10` — are equally decidable and neither reported nor recorded as deferred | low | True. §10 scopes the pass to *type* errors between literal operands, so not reporting them is in scope; the defect is only that the audit trail did not say so. | defer |
| E | The interpreter cross-check compares `type.operand_mismatch` alone, so a pass that stayed silent on a decidable non-type failure would not be caught | low | True: the sweep generates `2 / 0` and the non-matching arm falls through silently, while the test's doc claimed the broader property. The restriction is correct; only the wording was wrong. | patch |
| F | `sprint-status.yaml` says `in-progress` while every other artifact says the story is finished | false | The build workflow sets the story to `review` in its presentation step, not here; `in-progress` is the correct value for this point in the run, and stories 1.1–1.9 reached `review` the same way. | reject |
| G | `Code::ALL` is hand-maintained, so a constant added to neither list is still asserted by nothing | low | True, and the deferred-work entry written in this change overclaimed the guard as catching "a 32nd code added without touching either list". A macro emitting both would make the claim true; correcting the claim is the direct fix. | patch |
| H | A repeated starting-variable name produces a `syntax.duplicate_declaration` spanned over the whole program | medium | True and reachable: the CLI rejects a repeated `--var` at usage level, but Story 3.10's daemon caller passes the list directly, and the synthesized `Identifier`s all carry `program.span`, so `render_diagnostic` would point a caret at the entire source. | patch |
| I | `with_variables`/`with_callables` replace rather than accumulate, and neither doc says so | low | True. On a type whose whole contract is the difference between "no list" and "an empty list", a caller composing the environment in two places is exactly the caller who would be surprised. | patch |
| J | `function_arity` is not group-transparent, unlike `operands::literal` | low | Reproduced: `let f = (fn(a) { return a; }); return f();` got no arity check. A false negative rather than a false positive, but the two helpers disagreed about the same question for no reason. | patch |
| K | The shared-fixture doc comment says "a `const`" above two `LazyLock` statics | low | True, and a `const` is impossible there — `Environment::new` is not a `const fn`. | patch |
| L | The check command decides the empty-list question from the raw arguments rather than the validated list, and `eval` carries a comment left behind by the `read_program` refactor | low | Both true and both direct deletions. | patch |
| M | `check` documents that no input panics, but a hand-built `Program` with an out-of-range `ExprId`/`BlockId` panics in `Program::expression` | medium | True as a documentation defect. `hexput-ast` already makes id validity a standing caller rule and documents the panic; this crate's doc promised more than the AST contract allows. | patch |
| N | A hand-built AST whose `Group` or `Block` ids form a cycle makes the job stack grow without bound | medium | True, same root cause as M: the totality claim covered ASTs the parser cannot produce. Grouped with M and answered by the same corrected contract; `operands::literal` and `function_arity` already bound their own loops. | patch |
| O | An `if` with an empty `branches` vector and `conditionals` disabled produces no policy finding at all | low | True of a hand-built AST; the parser always produces at least one branch. The fix is a direct correction to a `map_or` fallback rather than an added guard. | patch |
| P | The Implementation Notes say 20 tests were added to `tests/cli_core.rs`; the diff adds 18 | low | True — counted 18 `#[test]` functions in the added section. Rejected as a finding because its only fix edits this build's spec; the count was corrected as a plain accuracy fix rather than through triage. | reject |
| Q | A task line names `tests/parser.rs` for decision 4's pin, but the pin landed in `tests/check.rs` | low | True, and `tests/parser.rs` is untouched. Rejected for the same reason as P, and corrected the same way — the pin belongs beside the rule it protects. | reject |
| R | The block-arena half of `reassigned_names` — the whole nested-scope side of the arity guard — is unverified | medium | Pre-verified by the verification-gap layer: every existing assignment in the tests is top-level, so deleting the `.chain(program.blocks…)` clause leaves all tests green while `fn f(a) {…}; if (true) { f = 1; }; return f(1, 2);` starts reporting a false positive. | patch |
| S | The interpreter cross-check never exercises a function literal as an operand | medium | Pre-verified: `Lit::Function`'s `type_name`/`article`/`not_a_number_reason` arms are reachable from source but absent from both sweeps' operand lists, so changing any of their wording diverges from the interpreter with every test still green. | patch |
| T | The re-implemented string-to-number rule is pinned only by strings that fail on the first byte | medium | Pre-verified: of the five must-fail strings the doc names, only `""` appears in any test, and it fails before the trailing-garbage guard is reached. Deleting `at == bytes.len()` leaves every test green while `"0x10" * 2` starts checking clean. | patch |
| U | `operands.rs` and `hexput-interpreter/src/convert.rs` hold two byte-for-byte copies of `is_number_literal` | low | True, and inherent: AD-8 forbids the edge that would let them share one. The duplication is the story's stated design and the cross-check sweep is its stated answer — an answer that now actually reaches the duplicated code (findings S and T). | reject |

## Design Notes

The pass is a second, independent implementation of the scoping and conversion rules the interpreter already implements — it has to be, because AD-8 forbids the edge that would let it share one. That duplication is the main risk in this story, and the answer is not care but a test: every literal-operand case is asserted against what the interpreter actually does with the same source, from `hexput-tests`, which may dev-depend on both.

Two of the findings §10 lists are unreachable here and that is correct, not a gap. Duplicate `let` and `break`/`continue` outside a loop are rejected by the *parser*, so a parsed AST never contains them; a caller who submits such a Script gets the same category, code, message and span from parsing that the check would have produced. The pass documents this instead of carrying dead code for it.

Severity follows one line: a finding that names something which would certainly fail if reached is an error (undeclared name, wrong argument count, a literal-operand type error, a disabled construct, an unknown call); a finding that describes code which cannot fail is a warning (unused local, unreachable code). That is what makes "warnings can never reject a script" true by construction rather than by a list.

```text
hexput check rules.hxp
  parse ──► Program ──► check(program, environment, policy) ──► findings
                                                                  │
                        render_diagnostic(finding, source, origin) ┘
```

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: all rules pass, including `hexput-check`'s new exact-dependency assertion
- `cargo build --workspace --all-targets --locked` -- expected: succeeds
- `cargo test --workspace --locked` -- expected: all tests pass, none skipped
- `cargo run --bin hexput -- check <a script with an undeclared name>` then `echo $?` -- expected: the finding rendered, exit `1`; repeat with a clean script for exit `0`
