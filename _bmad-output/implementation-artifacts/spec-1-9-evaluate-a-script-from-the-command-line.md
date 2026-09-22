---
title: 'Story 1.9: Evaluate a script from the command line'
type: 'feature'
created: '2026-09-22'
status: 'done'
baseline_commit: '4fba3f6523cd24d07a208e9fd10b6d24f00d2e17'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
  - '{project-root}/_bmad-output/implementation-artifacts/epic-1-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** The whole language works — `tokenize`, `parse`, `evaluate` and a terminal renderer all exist — but nothing drives them from a terminal. `hexput_cli_core::run()` is a `todo!()`, so a script author cannot run a `.hxp` file at all without the daemon, a socket and a Backend that do not exist yet. Worse, `evaluate(&Program)` takes no starting variables, so even a caller wiring it by hand cannot supply the script's inputs.

**Approach:** Give the interpreter a second entry point that binds caller-supplied starting variables into the root scope before the Script runs, with public constructors so a caller outside the crate can build array and object inputs. Then implement `hexput-cli-core`'s eval command over it: read the file, parse, bind the variables given on the command line, evaluate, print the result to stdout and exit zero — or render the diagnostic to stderr with Story 1.8's renderer and exit non-zero.

## Boundaries & Constraints

**Always:** `hexput-cli-core` keeps exactly its four *workspace* dependencies — `hexput-lexer`, `hexput-parser`, `hexput-interpreter`, `hexput-check` — plus `clap` (decision 1) and nothing else. It must reach `Diagnostic`, `render_diagnostic` and `RenderOptions` through a re-export from one of those, never by adding a `hexput-shared` or `hexput-ast` edge; the Spine's graph does not list one and `check-crate-graph.py` would not catch it (`hexput-cli-core` has no `EXACT_DEPENDENCIES` entry), so the reviewer's eye is the only guard. Only `hexput-bin` may own a `fn main()` or exit the process: `run` returns a `std::process::ExitCode` and the binary returns it. Every failure path — unreadable file, bad usage, lexical, syntax or runtime diagnostic — writes to stderr and exits non-zero; only a successful evaluation writes to stdout. The rendered diagnostic is exactly `render_diagnostic`'s output, with the script path as the origin label.

**Never:** No colour, no `is_terminal`, no TTY detection, no ANSI (decision 4). No check command, no findings, no `hexput-check` call — Story 1.10 owns those and this story leaves that dependency unused. No stdin script source, no REPL, no `--watch`, no multiple input files. No new diagnostic categories or codes, and no change to what the lexer, parser or interpreter treat as an error. No serde and no wire format. No daemon, socket, Registered Function or capability anything.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Valid script | file returning `2 + 3` | `5` on stdout, exit 0 | N/A |
| No `return` | script that runs off the end | `null` on stdout, exit 0 | N/A |
| Collection result | script returning `{ a: [1, "x"] }` | `{ a: [1, "x"] }` on stdout, exit 0 | N/A |
| Deeply nested result | a result nested thousands deep | printed in full without overflowing the host stack | N/A |
| Syntax error | `let x = 1 +;` | rendered diagnostic on stderr with the path as origin, empty stdout, exit non-zero | rendered, not panicked |
| Runtime error | `return "abc" * 2;` | same, spanned on the offending operator | rendered, not panicked |
| Function result | `fn f() {} return f;` | `type.function_result` rendered on stderr, exit non-zero | rendered |
| Bound variable read | `--var` supplying `n`, script `return n * 2;` | the bound value is used, exit 0 | N/A |
| Variable not bound | script reads a name nobody supplied | the interpreter's existing `reference` error, rendered | rendered |
| Bad variable argument | a `--var` whose value does not parse or evaluate | error naming that argument on stderr, exit 2, script never read | reported, not panicked |
| Duplicate variable | the same name supplied twice | usage error naming the repeated name, exit 2, nothing evaluated | reported, never last-wins |
| Missing / unreadable file | path does not exist, or is a directory | error on stderr naming the path and the OS reason, exit non-zero | no diagnostic rendering, no panic |
| Invalid UTF-8 file | bytes that are not UTF-8 | error naming the path, exit non-zero | reported, never lossy-converted |
| No arguments / unknown flag | `hexput` alone, or `hexput --nope` | usage text on stderr, exit non-zero | N/A |
| Empty script | zero-byte file | `null` on stdout, exit 0 | N/A |

## Decisions (approved 2026-09-22)

1. **`clap` parses the arguments, and the Stack table gains it.** Pinned once in `[workspace.dependencies]` (`clap = "4.6.7"`, `derive` feature) and inherited, never re-pinned per crate; the Spine's Stack table is amended in the same change, since "nothing not listed is permitted" is a rule about crate edges but the Stack is meant to be the one place a version lives. Story 1.10's check command and the daemon's own `--config` flag (AD-7) then inherit the same parser instead of each growing a hand-rolled one. `hexput-cli-core` gains `clap` as its only non-workspace dependency; the four workspace edges are unchanged.

2. **A starting variable's value is a Hexput expression.** `--var n=5`, `--var s='"hi"'`, `--var xs='[1,2]'`, `--var o='{ a: 1 }'`. The CLI parses each `name=<expr>` by feeding `return <expr>;` through the existing `parse` and evaluating it with no starting variables of its own, so all six types are reachable and the quoting rules are the language's own — nothing new is specified, and nothing new can drift from §3. The name must be a valid §2 identifier. An expression that fails to parse or evaluate is reported against that argument before the script is read, and nothing is evaluated.

3. **The result prints in Hexput literal form.** `"hi"` prints with its quotes and §3 escapes, numbers through the language's own `number_to_string` so a printed number has exactly one spelling, `null`/`true`/`false` bare, arrays as `[1, 2]`, objects as `{ a: 1 }` with bare keys where §2 allows and quoted keys otherwise, followed by one newline. What is printed can be pasted back into a script, and it is exactly the grammar decision 2 accepts as input. No truncation, no depth limit, and the printer must not recurse on the host stack — a returned value's nesting is attacker-controlled.

4. **No colour, this story.** Story 1.8 left colour to "the CLI in Story 1.9", but no acceptance criterion asks for it and it costs a TTY-detection dependency. The rendered diagnostic goes to stderr exactly as `render_diagnostic` produces it. Revisit when there is a reason beyond symmetry.

5. **Exit codes: `0` success, `2` usage, `1` everything else.** A rendered diagnostic (lexical, syntax or runtime), an unreadable file and non-UTF-8 bytes all exit `1`; a bad flag, a missing operand and a malformed `--var` exit `2`, the conventional split so a script can tell "I invoked it wrong" from "the Hexput was wrong". A duplicate `--var` name is a usage error, not last-wins: silently dropping one of two supplied values is the kind of thing a person debugs for an hour.

</frozen-after-approval>

## Code Map

- `crates/hexput-cli-core/src/lib.rs` -- 11 lines; `pub fn run()` is a `todo!()`. The whole CLI lands here. Its `Cargo.toml` already declares the four edges and must gain no fifth (see Boundaries); `hexput-check` stays unused this story.
- `crates/hexput-bin/src/bin/hexput.rs` -- 7 lines; `fn main() { hexput_cli_core::run(); }`. Becomes an `ExitCode`-returning hand-off. Its doc comment forbids logic here: arguments pass through untouched, parsing happens in `hexput-cli-core`.
- `crates/hexput-interpreter/src/lib.rs:47,68` -- `pub use value::{Array, Object, Value};` and `evaluate(&Program) -> Result<Value, Diagnostic>`, which calls `Machine::new(program).run()`. Add the starting-variable entry point beside it and leave `evaluate` as-is.
- `crates/hexput-interpreter/src/machine.rs:179-192` -- `Machine::new` allocates the root scope (`heap.push_scope(None)`) and then calls `open(&program.statements)`, which hoists named functions into it. Starting variables must be declared into that root scope **between** those two calls: hoisted functions and `let` share one block namespace, so binding after `open` would let a variable silently shadow a top-level `fn`.
- `crates/hexput-interpreter/src/value.rs:130,160` -- `Array::new`/`Object::new` are `pub(crate)`, so nothing outside can build a collection input. `Object` wraps `IndexMap<Arc<str>, Value>` (insertion-ordered, §3); a public constructor should not force callers to name `indexmap`.
- `crates/hexput-interpreter/src/heap.rs` -- `RtValue` and `Heap::declare(scope, name, value)`; a detached `Value` must be re-attached as `RtValue` to be bound. Invert the existing `detach`.
- `crates/hexput-interpreter/src/convert.rs` -- `to_string` (§4.3) and `number_to_string`, both `pub(crate)`. `number_to_string` is the language's own number formatting; the CLI must print through it so a number never has two spellings.
- `crates/hexput-ast/src/lib.rs:10-12` -- re-exports the diagnostics items from `hexput-shared`. The pattern to copy: `hexput-parser` and `hexput-interpreter` re-export onward for `hexput-cli-core`. Story 1.8's finding S predicted this story would otherwise reach for the forbidden edge.
- `crates/hexput-shared/src/diagnostics.rs` -- `render_diagnostic(&Diagnostic, &str, RenderOptions<'_>) -> String`, with `RenderOptions::new()`/`with_origin()`; total, no I/O. Call it; never post-process its output.
- `scripts/check-crate-graph.py:60-64` -- `EXACT_DEPENDENCIES` covers `hexput-ast`, `hexput-lexer`, `hexput-parser` only. Adding `hexput-cli-core: {hexput-lexer, hexput-parser, hexput-interpreter, hexput-check}` would make the Boundaries rule mechanical rather than reviewed — do it.
- `crates/hexput-tests/` -- integration tests, one file per crate, public API only. Add `hexput-cli-core` as a dev-dependency plus `tests/cli_core.rs`; `tests/interpreter.rs` has the helpers the starting-variable tests reuse.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` -- normative: §3 types, §4.3 conversions, §7 and its Story 1.8 rendering decisions. It says nothing yet about a CLI, starting variables, or result printing — the approved decisions land there.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-interpreter/src/value.rs` -- make building an `Array` and an `Object` possible from outside the crate, without exposing `indexmap` in the signature -- a caller cannot supply a collection as a starting variable otherwise.
- [x] `crates/hexput-interpreter/src/{lib,machine,heap}.rs` -- add the entry point that takes the program plus named starting variables, attaches each detached `Value` into the heap and declares it into the root scope before `open` hoists the top-level functions -- AC3, and the ordering is what keeps one block namespace honest.
- [x] `crates/hexput-interpreter/src/lib.rs`, `crates/hexput-parser/src/lib.rs` -- re-export the diagnostics items `hexput-cli-core` needs (`Diagnostic`, `render_diagnostic`, `RenderOptions`, and `Severity`/`Category`/`Code` where used) -- the CLI must reach them without the forbidden `hexput-shared`/`hexput-ast` edge.
- [x] `crates/hexput-interpreter/src/{lib,convert}.rs` -- expose the language's number formatting (or a result formatter built on it) publicly -- so the CLI's printed number is byte-identical to the one §4.3 stringification produces.
- [x] `Cargo.toml` (root), `crates/hexput-cli-core/Cargo.toml` -- pin `clap = "4.6.7"` with `derive` once at the workspace root and inherit it in `hexput-cli-core` -- decision 1; no member ever re-pins a version.
- [x] `crates/hexput-cli-core/src/**` -- implement the eval command end to end: `clap` arguments and usage, `--var name=<hexput expression>` parsing via `parse`/`evaluate` on `return <expr>;`, file read, parse, binding, evaluation, and the exit codes of decision 5 -- the story's deliverable.
- [x] `crates/hexput-cli-core/src/**` -- the Hexput-literal result printer of decision 3, driven by an explicit work stack so a deeply nested result cannot overflow the host stack -- the printed form is what Story 1.10 and a Backend reader see, and nesting is attacker-controlled.
- [x] `crates/hexput-cli-core/src/lib.rs` -- give `run` a form that writes through caller-supplied `io::Write` sinks so stdout and stderr can be captured in-process -- the tests must assert exact output without spawning the binary.
- [x] `crates/hexput-bin/src/bin/hexput.rs` -- pass the process arguments through and return the `ExitCode` -- no logic beyond the OS entry point (Spine's Structural Seed).
- [x] `scripts/check-crate-graph.py` -- add `hexput-cli-core`'s exact dependency set -- makes the four-edge boundary a CI failure rather than a review finding.
- [x] `crates/hexput-tests/Cargo.toml`, `crates/hexput-tests/tests/cli_core.rs` -- add the dev-dependency and cover every I/O matrix row with exact expected stdout, stderr and exit code -- the printed forms and the exit contract are what Story 1.10 and the Backend reuse.
- [x] `crates/hexput-tests/tests/interpreter.rs` -- cover starting variables: each of the six types bound and read back, a name colliding with a top-level `fn`, an empty variable set behaving exactly like `evaluate` -- the new entry point is public API.
- [x] `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` -- record decisions 2, 3 and 5 as `[DECISION, 2026-09-22]` entries -- the reference is normative, says nothing about a CLI today, and Story 1.10 reuses this surface.
- [x] `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` + its `.memlog.md` -- add `clap 4.6.7` to the Stack table and record the amendment -- decision 1 changes a document marked final.
- [x] `AGENTS.md`, `_bmad-output/implementation-artifacts/sprint-status.yaml` -- record verified progress.

**Acceptance Criteria:**
- Given a file containing a valid Hexput script, when the eval command runs against it, then the Script's result is printed to stdout and the process exits zero.
- Given a file whose script fails to lex, parse, or evaluate, when the eval command runs against it, then Story 1.8's rendering appears on stderr with the script path as origin, stdout is empty, and the exit code is non-zero.
- Given starting variables supplied on the command line, when the script reads those names, then it sees the supplied values, bound before any top-level statement runs.
- Given any input at all — missing file, non-UTF-8 bytes, unknown flag, adversarially nested script — when the CLI runs, then it returns an exit code and never panics, and no crate but `hexput-bin` owns a `main` or exits the process.
- Given the workspace, when the five CI commands run, then all pass, and `hexput-cli-core` still depends on exactly `hexput-lexer`, `hexput-parser`, `hexput-interpreter` and `hexput-check`.

## Implementation Notes

**Landed 2026-09-22.**

- `hexput_interpreter::evaluate_with_variables(&Program, IntoIterator<Item = (N: AsRef<str>, Value)>)` is the new entry point; `evaluate` now delegates to it with an empty set. `Machine::with_variables` attaches each `Value` into the fresh heap and declares it into the root scope **between** `push_scope` and `open`, so a top-level `fn` of the same name wins the shared block namespace. Binding cannot fail, so the signature returns the same `Result` as `evaluate` and nothing new.
- `Heap::attach` is the inverse of `detach`: an explicit-work-stack walk, so an adversarially nested input cannot grow the host stack. Shared structure in the input expands into separate heap slots, which is unobservable for the same reason sharing is on the way out.
- `Array::from_values` and `Object::from_entries<K: Into<Arc<str>>>` are the public constructors; `indexmap` stays out of both signatures.
- Diagnostics reach `hexput-cli-core` through new re-exports on `hexput-parser` and `hexput-interpreter` (`Category`, `Code`, `Diagnostic`, `RenderOptions`, `Severity`, `Span`, `render_diagnostic`; the parser also re-exports `Program`). `hexput_interpreter::number_to_string` exposes §4.3's formatting so the printed number has one spelling.
- `hexput-cli-core` is three files: `args.rs` (the `clap` derive types, `ColorChoice::Never`), `print.rs` (decision 3's literal printer, an explicit work stack), and `lib.rs` (`run`/`run_with`, `--var` parsing, file read, the exit contract). `is_identifier` asks the **lexer** whether a name is a single `Ident` spanning the whole text, so the reserved-word list is not duplicated — that is what `hexput-lexer`'s edge is for this story.
- Help and version text go to **stdout** with exit `0` — explicitly asked for, so neither a failure nor a result, and `hexput --help | less` must show something. `clap`'s `error.use_stderr()` separates that case from a real usage error, which stays on stderr with exit `2`.
- `evaluate_with_variables` rejects a supplied name that the Script's own top level declares with `let` or `fn` (`syntax.duplicate_declaration`, spanned on the declaration): a starting variable is a top-level binding and §5 gives the block one namespace, so keeping one of the two values silently is exactly decision 5's failure. A name declared in an *inner* block is ordinary shadowing and still fine.
- A `--var` value must parse to exactly the one synthetic `return` **with** an expression, so `n=1; let q = 2` and an empty `n=` are usage errors rather than half-honoured.
- A `--var` whose expression fails is rendered against the synthetic `return <expression>;` source with `--var` as the origin label, under a line naming the variable — so the caret points into what the caller actually typed.
- Tests: `crates/hexput-tests/tests/cli_core.rs` (39 tests, every I/O-matrix row, driving `run_with` in-process with a self-cleaning temp-directory helper — no `tempfile` dependency — plus one that spawns the real binary for argv pass-through and exit-code propagation, and one that drives both sinks with an always-`BrokenPipe` writer) and 11 new starting-variable tests in `tests/interpreter.rs`. `ExitCode` exposes no accessor, so an exit code is compared by `Debug` against a real `ExitCode::from(n)` rather than a hand-written string.
- Nesting is exercised at 20 000 levels in both directions: a result built by a loop and printed in full, and a caller-supplied input attached, read through, and detached again.

## Spec Change Log

## Review Triage Log

Pass 1: blind hunter, edge-case hunter, verification-gap reviewer.

| ID | Finding | Verdict | Evidence | Route |
|---|---|---|---|---|
| A | A `--var` whose name a top-level `let` or `fn` also declares is silently discarded | high | Reproduced: `let n = 3; return n;` with `--var n=9` prints `3`, exit 0, no diagnostic. `with_variables` declares into the root scope, then `open` hoists and the `let` statement re-declares over it. Decision 2 says a starting variable "behaves exactly like a top-level `let`", and §5 makes a duplicate top-level `let` a compile-time error; decision 5's own principle is that a supplied value is never silently dropped. One reading, not a gap: report it. | patch |
| B | `--var` accepts arbitrary statements, not one expression | medium | Reproduced: `--var 'n=1; let q = 2'` is accepted and binds `1`, discarding the rest. `evaluate_expression` wraps the text in `return {expression};` and never checks the parsed shape, though decision 2 and the reference both say the value *is* an expression. | patch |
| C | `--var n=` with nothing after `=` silently binds `null` | medium | Reproduced: prints `null`, exit 0. The synthetic source becomes `return ;`, a legal bare return. Same root cause as B — the parsed shape is never checked. | patch |
| D | `--help` and `--version` write to stderr, leaving stdout empty | medium | Reproduced: `hexput --help` writes 289 bytes to stderr and 0 to stdout, exit 0, so `hexput --help \| less` shows nothing. The frozen rule "only a successful evaluation writes to stdout" is about result-versus-failure output; explicitly requested help is neither, and every other CLI puts it on stdout. | patch |
| E | Nothing exercises the real binary, so argv pass-through and exit-code propagation are unasserted | medium | Pre-verified by the verification-gap layer: changing `main` to `run(args_os().skip(1))` breaks every real invocation (clap reads `eval` as the program name, exit 2 always) while all 33 `cli_core.rs` tests stay green, because they call `run_with` with an argv they prepend `"hexput"` to themselves. | patch |
| F | `run_with`'s documented "a failing sink does not change the exit code" is prose only | medium | Pre-verified: every test sink is a `Vec<u8>`, which never returns `Err`. Replacing one `let _ = writeln!` with `.expect(…)` leaves all 33 tests green while a closed pipe would panic, breaking AC4. | patch |
| G | Deep-nesting stack safety is tested for arrays only, in both new flat walks | medium | True: `a_deeply_nested_result_prints_in_full…` and `an_adversarially_nested_starting_variable…` both build array chains, while objects take different code (`Step::Key`/`Step::Text(" }")` in the printer, `Visit::FinishObject` in `attach`). Objects work today; nothing would notice if they stopped. | patch |
| H | `hexput-cli-core`'s `Cargo.toml` comment reads as though the graph check asserts its non-workspace dependency too | low | True: `workspace_graph` filters to `d["name"] in members`, so `EXACT_DEPENDENCIES` asserts the four workspace edges only. Fix is a one-line wording correction with no added complexity. | patch |
| I | `evaluate_with_variables` documents that binding "cannot fail" and that no name is reserved, so a library caller can bind an unreadable name or a non-finite `Number` | low | True on reading: the `# Errors` section asserts totality, `Value::Number`'s own doc promises "always finite", and the CLI's `is_identifier` guard lives in the consumer rather than the API that makes the claim. A is already rewriting that section; the caller's obligations belong in the same edit. | patch |
| J | `a_directory_is_reported_rather_than_read` clones a `PathBuf` it then takes a reference to | low | True: `eval_at(&sandbox.dir.clone(), &[])`; `&sandbox.dir` is already what the callee wants. Direct deletion. | patch |
| K | A Script result with shared subtrees prints in time exponential in its heap size | medium | Reproduced: a 4-line script doubling a shared array 30 times hangs the CLI past a 10-second timeout with no output. Real — but the property is the result's, not the printer's: `spec-interpreter-per-execution-arena.md` already recorded it against `to_vec`, `entries` and MessagePack encoding, and routed the bound to Story 3.6's output-size budget. `Heap::attach` inverts the same shape on the way in, unreachable from a single `--var` expression but open to a library caller. | defer |
| L | The `--var` diagnostic renders the synthetic `return <expr>;`, shifting the column by seven | low | Real: `--var n=0/0` reports `--var:1:10` under the line `return 0/0;`. But the line is printed in full, the marker is correct relative to it, and the `--var` origin label says whose value it is; rewriting the span to the caller's own text needs offset arithmetic and out-of-range guards for a reader who is not actually misled. | reject |
| M | `print.rs`'s `#[non_exhaustive]` catch-all, and `attach`'s `unwrap_or(Null)`, turn an impossible state into a plausible-looking `null` | low | True as written, and already commented as such. Unreachable until someone adds a seventh `Value` variant — not everyday use for a user or a developer — and making it loud means either a panic in a printer documented never to panic, or plumbing an error through a total function. | reject |
| N | The diagnostics re-export now exists in both `hexput-parser` and `hexput-interpreter` and must be kept in sync by hand | low | True, and deliberate: each crate serves its own consumers, exactly as `hexput-ast` already serves both of them. `hexput-cli-core` importing from the parser does not make the interpreter's copy dead — it is public API a non-parser consumer (`hexput-exec`, later) will use. No named caller diverges. | reject |
| O | The origin label and the file-read errors stringify the path with `Path::display`, which is lossy for non-UTF-8 bytes | low | Real on Linux, where a path is arbitrary bytes, and the change does refuse lossy conversion for file *contents*. But the path came from the caller's own shell, the message still identifies which invocation failed, and keeping the raw `OsStr` through to the message adds branching for a case no user meets. | reject |
| P | `print::literal` buffers the whole result before writing, and `push_key` runs the full lexer once per object key | low | Both true. But the fix for the second is a hand-rolled identifier check that duplicates §2's reserved-word list — precisely what calling the lexer exists to avoid — and the first trades a documented, simple contract for streaming machinery on output a one-shot CLI prints once. | reject |
| Q | "Pinned once" overstates a caret requirement: `cargo update` would move `clap` to 4.7.x | low | True of the wording, and equally true of all nine rows already in the Stack table; the lockfile is what holds the version. Changing only this row's wording would make it read as a different rule from its neighbours. | reject |
| R | `reported()` asserts `stderr.contains(needle)` while the spec task says "exact expected stderr" | low | True of most stderr assertions. The renderer's exact output is pinned separately and thoroughly in `tests/diagnostics.rs`, so the drift this would catch is already caught a layer down; one `cli_core.rs` test does assert the full rendering. | reject |
| S | Regenerating `epic-1-context.md` dropped the explicit path of the normative language reference | low | Real, and caused by this run's step-1 regeneration rather than by the implementation. The fix edits an agent-context file. | defer |

## Design Notes

The three layers stay separate, which is what makes the whole thing testable without spawning a process:

```text
hexput-bin/src/bin/hexput.rs   args_os() -> run(...) -> ExitCode        (OS entry point only)
hexput-cli-core                parse args, read file, drive the pipeline,
                               write to two io::Write sinks, return ExitCode
hexput-{parser,interpreter}    parse / evaluate — unchanged contracts
```

Starting variables cross a boundary the interpreter has not had to cross before: `Value` is the *detached* result type (owned, `Send + Sync`, no heap), while bindings hold `RtValue` (a handle into the execution's heap). Binding an input is therefore the inverse of `detach` — walk the detached value, allocate its collections into the fresh heap, and declare the root handle. Shared structure in the input (the same `Arc` reached twice) may expand into separate heap slots; that is unobservable, exactly as it is on the way out.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: all rules pass, including the new `hexput-cli-core` exact-dependency assertion
- `cargo build --workspace --all-targets --locked` -- expected: succeeds
- `cargo test --workspace --locked` -- expected: all tests pass, none skipped
- `cargo run --bin hexput -- eval <a valid script>` then `echo $?` -- expected: result on stdout, `0`; repeat with a broken script for the rendered diagnostic on stderr and a non-zero code
