---
title: 'Story 1.7: Execute control flow, functions, and callbacks'
type: 'feature'
created: '2026-09-22'
status: 'done'
baseline_commit: 'b9e0a0a8b502ad924f996baaeba4d76128b95358'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
  - '{project-root}/_bmad-output/implementation-artifacts/epic-1-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** The interpreter evaluates expressions, scopes and assignment, but every control-flow and function construct still returns the placeholder `temporary.not_yet_implemented`, so no complete rule can run — only single expressions.

**Approach:** Extend the continuation machine in `hexput-interpreter` to run `if`/`else`, `while`, `for (x in …)`, `break`/`continue`, named and anonymous functions, closures, calls and callbacks per LANGUAGE-REFERENCE §5–§6, with a call-depth limit instead of host recursion. Delete `NOT_YET_IMPLEMENTED`.

## Boundaries & Constraints

**Always:** LANGUAGE-REFERENCE.md is normative. No host stack growth from any input nesting, iteration count, or call depth; no panics on any AST the parser produces. Function values live in the per-execution heap; a captured scope must be marked so `Frame::ExitScope` stops reclaiming it (and its parent chain). Closures capture by reference — a callback sees later mutations of a captured binding. `hexput-interpreter` still depends on `hexput-ast` only (+ `indexmap`). The machine stays `Send`. Every new failure is a `Diagnostic` with a stable `hexput-shared` code and a span on the offending construct.

**Never:** No host reach, Registered Functions, capabilities, or budgets (Epic 3) — an unknown callee is still just an undeclared identifier. No diagnostic rendering (1.8), no CLI (1.9). No re-checking what the parser already rejects (`break` outside a loop, duplicate declarations and parameters). No garbage collection within an execution beyond the existing scope reclaim. No `PartialEq`/detach identity for function values.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Branching | `if`/`else if`/`else` chain | only the taken branch's effects are observable; each body is its own scope | N/A |
| Truthiness | conditions of `null`, `false`, `0`, `""`, `[]`, `{}` and truthy counterparts | branch taken exactly per §4.1 | N/A |
| Accumulating loop | `while` accumulating into an outer binding | final value and iteration count as expected | N/A |
| Loop control | `break` / `continue` in `while` and `for`, inside nested blocks and nested loops | affects the innermost loop only; `continue` re-tests the condition | N/A |
| `for` over collections | array → elements in order; object → keys as strings in insertion order; empty → body never runs | binding is fresh per iteration | non-collection iterable → `type` |
| Mutation while iterating | `for (x in a) { a[0] = 1; }` or a key added to the iterated object | — | `reference` (`reference.collection_mutated`), spanned on the loop |
| Call | `fn add(a, b) { return a + b; } return add(1, 2);` | `3` | N/A |
| Per-call bindings | recursive and repeated calls | parameters bind per invocation with no leakage; a body ending without `return` yields `null` | N/A |
| Callback | `fn apply(f, v) { return f(v); } return apply(fn(x) { return x * 2; }, 21);` | `42` | N/A |
| Closure by reference | anonymous fn capturing a binding mutated after its creation | the call sees the new value | N/A |
| Wrong arity | `add(1)`, `add(1, 2, 3)` | — | `arity`, spanned on the call |
| Not callable | `let x = 1; x();`, `o.missing()` | — | `type` (`type.not_callable`) |
| Unbounded recursion | `fn f() { return f(); } return f();` | terminates with a defined error, host stack intact | `depth` |
| Deep input | 12,000 nested `if`/blocks; a loop of 100,000 iterations | evaluates without stack growth | N/A |
| Return placement | `return` inside a loop inside a function; top-level `return` inside an `if` | returns from the function; top-level ends the Script | N/A |
| Function as result | `return fn(x) { … };`, or an array/object containing a function | — | `type` (`type.function_result`), spanned on the returned expression |

## Decisions (approved 2026-09-22)

1. **A function cannot be the Script result** — returning a function, or a value containing one, is a `type` error (`type.function_result`) spanned on the returned expression, like `type.cyclic_result`. A Script result must be data the Backend can receive, and a function has no wire representation. Returning a re-triggerable callable handle (a frozen closure the Backend can invoke later over the socket, rebound to its outer context and Global Variables) is the intended long-term direction but is deliberately out of this story; widening this error into a value later is not a breaking change.
2. **Call-depth limit is a documented constant of 1024** inside the interpreter, not a parameter — Story 3.5's Resource Budget is the intended owner of a configurable limit.
3. **Named functions hoist within their block:** every `fn name` declared in a block is bound before the block's statements run, so mutual recursion works in any declaration order. A declaration's captured scope is that same block scope.
4. **Each loop iteration gets a fresh scope** for the binding and the body, so a closure created in iteration *i* captures that iteration's value.

</frozen-after-approval>

## Code Map

- `crates/hexput-interpreter/src/machine.rs` -- the whole story. `Frame` enum + `step`/`statement`/`expression`/`link` dispatch; `StatementKind::{Function,If,While,For,Break,Continue}` and `ExpressionKind::Function` currently return `not_yet(...)`, and `Frame::Link` rejects `AccessKind::Call`. `Frame::Return` currently always ends the Script — it must distinguish a function return from the Script result. `Frame::ExitScope` carries the reclaim site and its Story 1.7 comment. In-crate `#[cfg(test)] mod tests` builds ASTs by hand (no parser dependency) to observe the heap; extend it for capture/reclaim proofs. Delete `not_yet` and the `NOT_YET_IMPLEMENTED` import.
- `crates/hexput-interpreter/src/heap.rs` -- `Slot` (`Free`/`Array`/`Object`/`Scope`) and `RtValue`; add a function slot and `RtValue::Function(SlotId)`, extend `type_name`, `equals` (identity), `is_truthy` (a function is truthy), and `detach`, which must now distinguish its two failures (a cycle vs. a reachable function). `release` is the scope reclaim. Keep every walk iterative.
- `crates/hexput-interpreter/src/environment.rs` -- `ScopeRecord { bindings, parent }` and `push_scope`/`declare`/`lookup`/`assign`; add the captured mark and the parent-chain marking walk.
- `crates/hexput-interpreter/src/lib.rs` -- crate docs describe the memory contract; remove `NOT_YET_IMPLEMENTED`, document the 1024 call-depth limit and the function-value rules. `evaluate`'s signature does not change.
- `crates/hexput-interpreter/src/value.rs` -- the detached public result (`Value`, `Array`, `Object`, `#[non_exhaustive]`). Gains no function variant: `detach` fails instead.
- `crates/hexput-shared/src/diagnostics.rs` -- add the new codes beside the Story 1.6 block; `Category::{Arity,Depth}` already exist.
- `crates/hexput-parser/src/statements.rs` -- reference only: already rejects `break`/`continue` outside a loop and duplicate names per block namespace. Do not modify.
- `crates/hexput-tests/tests/interpreter.rs` -- 27 integration tests parsing real source via `hexput_parser::parse`; add the matrix rows here. `tests/shared.rs` asserts code strings.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-shared/src/diagnostics.rs` -- add documented codes: `type.not_callable`, `type.function_result`, `arity.argument_count`, `depth.call_depth_exceeded`, `reference.collection_mutated` -- stable machine-readable failures.
- [x] `crates/hexput-interpreter/src/{heap,environment}.rs` -- function slots, function `RtValue`, captured-scope marking over the parent chain, and a mutation guard for iterated collections -- the data the machine needs.
- [x] `crates/hexput-interpreter/src/machine.rs` -- conditionals, `while`, `for`, loop control, function values, calls, per-call scopes, function `return`, and the call-depth limit; delete `not_yet` -- the story's runtime core.
- [x] `crates/hexput-interpreter/src/lib.rs` -- drop `NOT_YET_IMPLEMENTED`, document the call-depth limit and the function-value rules.
- [x] `crates/hexput-tests/tests/interpreter.rs`, `tests/shared.rs` -- cover every matrix row with exact categories, codes and spans, plus evaluation-order and no-leakage proofs; assert the new code strings.
- [x] `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` -- record the four decisions as `[DECISION, 2026-09-22]` in §5–§7.
- [x] `_bmad-output/implementation-artifacts/deferred-work.md` -- append the re-triggerable callable-handle direction from decision 1 (new entry; do not modify existing ones).
- [x] `AGENTS.md`, `sprint-status.yaml` -- record verified progress.

**Acceptance Criteria:**
- Given any parsed program, when evaluated, then the interpreter never panics, never recurses on the host stack, and no `temporary.not_yet_implemented` can be produced (the constant no longer exists).
- Given a script whose execution fails, when evaluated, then exactly one `Diagnostic` returns with the §7 category, a stable code, and a span on the offending construct.
- Given a function that is never captured out of its block, when its block exits, then its scope is still reclaimed; given one that is captured, then the capture is intact for the rest of the execution.
- Given the workspace, when the five CI commands run, then all pass.

## Implementation Notes

- **Frames, not recursion.** `Frame::Loop` is a boundary frame that stays directly below the running body for the whole loop and re-schedules itself each turn; `break` pops it, `continue` leaves it, and both run every `Frame::ExitScope` they pass so per-iteration scopes are reclaimed rather than leaked. `Frame::CallEnd` is the call boundary: popping it is a body that ran off its end (yields `null`), and `return` unwinds to it. Both restore the scope and truncate the value stack to what they recorded.
- **Top-level vs. function `return`** is decided by the call-depth counter: `depth == 0` detaches and ends the Script, otherwise it unwinds to the nearest `CallEnd`. No separate marker frame is needed.
- **`type.function_result` and `type.cyclic_result` share one walk.** `Heap::detach` now returns `Result<Value, DetachFailure>` with `Cycle` and `Function` variants, so the single iterative walk reports both, and `value.rs` gains no function variant.
- **Mutation during `for`** is a per-slot `version` counter on the `Array`/`Object` slots, bumped by `array_store`/`object_store` and compared at every advance (the terminating one included) against the version snapshotted when the loop opened. Mutating a *nested* collection bumps that collection's counter, not the iterated one, so it stays legal.
- **A non-collection `for` iterable reuses `type.operand_mismatch`** rather than adding a sixth code: the matrix specifies the category (`type`) but no code, and the spec's Execution task lists exactly five new codes. `OPERAND_MISMATCH`'s doc comment now names this case.
- **Capture marking is conservative.** `Heap::mark_captured` runs when a function *value is created*, marking the defining scope and its whole ancestor chain, and `release_scope` skips marked scopes. It records that a closure could have been made here, not that one escaped — proving escape would need reachability analysis the interpreter deliberately does not do. Scopes that create no function value, which includes every call scope of a function whose body makes no closure, are reclaimed exactly as in Story 1.6; four in-crate tests observe the heap to prove both halves.
- **Function definitions are interned by address** (`HashMap<usize, usize>` from `&Function`'s address to a table index), so a closure created inside a loop adds one table entry in total rather than one per iteration, and the heap never carries the program's lifetime. The key is a `usize`, never dereferenced, so the `Machine` stays `Send`.

## Spec Change Log

## Review Triage Log

Pass 1: blind hunter, edge-case hunter, verification-gap reviewer.

| ID | Finding | Verdict | Evidence | Route |
|---|---|---|---|---|
| V1 | Ancestor-chain capture marking is exercised by no test | medium | Reviewer replaced the parent walk with a single-scope mark and the whole suite still passed; the demonstrating program (`{ let a = 1; { g = fn(){return a;}; }; }`) then hangs, so the walk is load-bearing and unpinned. | patch |
| V2/E1 | The `for` mutation guard is skipped when the body mutates and then `break`s or `return`s | low | Confirmed: `for (x in a) { a[0] = 9; break; }` succeeds. Detect-on-advance is the conventional model (Java throws on the next advance, not on the mutation), so the behavior stands; it is the reference's unconditional wording and the missing test that are wrong. | patch |
| B1 | Hoisting/closure creation inside a loop body pins one scope (and a function slot) per iteration, even for a function never referenced | medium | Real: `open` runs per block entry and `make_function` marks the scope captured, so heap growth is linear in iterations with no reclaim. Same class as the accepted "no GC within an execution" design; the honest fix is escape analysis or Story 3.5's memory budget, not a patch. | defer |
| B2 | `detach`'s final `out.pop().ok_or(DetachFailure::Cycle)` reports an internal inconsistency as a cycle | low | Unreachable from machine-produced handles (every walk pushes exactly one value per visit); a third variant guards state never shown to occur. | reject |
| B3 | `for` over a non-collection reuses `type.operand_mismatch` instead of a distinct code | low | Category `type` is right and the message names the case; a `type.not_iterable` code is purely additive and non-breaking whenever a consumer needs it (Epic 2/3). | reject |
| B4 | Replacing an existing key or element while iterating trips the guard | false | The approved matrix names `for (x in a) { a[0] = 1; }` as an error, so this is the decided behavior, not a defect. The reference's wording is patched under V2/E1. | reject |
| B5 | The call-depth boundary is untested; both tests use an unexplained `CALL_DEPTH_LIMIT - 2` | medium | Nothing asserts the last-passing or first-failing depth, so an off-by-one in `self.depth >= CALL_DEPTH_LIMIT` passes the suite. | patch |
| B6 | No test asserts the span of `depth.call_depth_exceeded` | low | Every other new code has its span asserted via `assert_error`; the AC requires a span on the offending construct for every failure. | patch |
| B7 | LANGUAGE-REFERENCE spans the depth error "on the call" but arity/not-callable "on the call's argument list" | low | All three use `link.span`; the normative document should not describe one span two ways. | patch |
| B8 | Function values as operands are untested, and two code docs omit the function receiver | low | `convert.rs`, `article`, `no_properties` and `not_indexable` all gained function arms that no test reaches. | patch |
| B9 | `unwind_to_loop`'s `CallEnd` comment says the call is not torn down, but the body's frames are already discarded | low | Behavior is defined and safe; only the comment is wrong. | patch |
| B10 | `mark_captured` returns silently when a scope handle resolves to no scope | low | Called only with `self.scope`, which is always live; a `debug_assert` guards state never shown to occur. | reject |
| B11/E2/V3 | `epic-1-context.md` was regenerated wholesale, dropping the empty-vs-missing callable-name-list distinction, the summary-table precedence rule, the toolchain pin, and the CLI starting-variables note | medium | Real losses, and the empty-vs-missing distinction matters to Story 1.10 (it is also already an open deferred-work entry). Caused by this run's step-1 context regeneration, not by the implementation, and the fix edits an agent-context file. | defer |
| B12 | AGENTS.md says Stories 1.1-1.7 are done while the tracker says `review` | low | The same convention AGENTS.md has used since 1.1; rejected on identical evidence in Story 1.6's triage (B1). | reject |
| E3 | `break`/`continue` with no `Loop` and no `CallEnd` frame ends the Script as `null` | low | Unreachable from parsed source: the parser rejects loop control outside a loop. A guard here protects state never shown to occur. | reject |

Patches V1, V2/E1, B5, B6, B7, B8, B9 applied. B1 and B11 appended to deferred-work.md.

## Design Notes

Calls and loops are frame bookkeeping, not recursion. Push a boundary frame recording what to restore — the caller's scope, the value-stack depth, and (for loops) the loop's identity — then push the body's statements. `break`, `continue` and a function `return` pop frames until the matching boundary, truncating the value stack to the recorded depth; only the top-level `return` (no enclosing call boundary) detaches and ends the Script. A call binds parameters in a fresh scope whose parent is the callee's *captured* scope, never the caller's, and increments the depth counter that the `depth` error trips.

Function values are heap slots holding the defining scope's handle plus an index into a machine-side table of `&'p Function`, which keeps the heap free of the program's lifetime and stops a closure created in a loop from cloning its parameter list every iteration.

Mutation during `for` is detected with a version counter bumped by `array_store`/`object_store`, compared each iteration against the version snapshotted when the loop began — mutating a *nested* collection is untouched.

## Verification

All five ran clean on 2026-09-22 against the finished change:

**Commands:**
- `cargo fmt --all --check` -- clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` -- no warnings
- `python3 scripts/check-crate-graph.py` -- all rules pass
- `cargo build --workspace --all-targets --locked` -- succeeds
- `cargo test --workspace --locked` -- all tests pass, none skipped
