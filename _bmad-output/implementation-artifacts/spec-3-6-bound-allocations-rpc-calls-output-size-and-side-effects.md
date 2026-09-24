---
title: 'Story 3.6: Bound allocations, RPC calls, output size, and side effects'
type: 'feature'
created: '2026-09-24'
status: 'in-review'
baseline_commit: 'a280880e9d5a33f238bf97b3b31e40d90890b8fc'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Only CPU time and memory are bounded (Story 3.5). A Script can still churn millions of short-lived strings and collections, hammer the Backend with host calls, return a result as large as a frame, or perform unbounded side effects — the abuse FR-8's other four dimensions exist for.

**Approach:** Enforce allocation count, RPC call count, output size and side-effect count, each independently, through the same per-execution `hexput-enforce::Budget` and the one Executor. The interpreter counts allocations (it already meters) and stops at a ceiling it is handed; `hexput-exec` charges RPC calls and side effects at each host call and output size on the result; `hexput-enforce` owns every limit and error. Each dimension has its own `budget.*` code and a test pinning exact counts for a known Script, so a refactor cannot silently change what a dimension means.

## Boundaries & Constraints

**Always:** Dimension definitions are fixed (epic-3-context), made precise here:
- **Allocations** — every string, array or object the Script constructs: each evaluation of a string literal, concatenation, to-string conversion and key handed out by `for … in`; each array or object literal; plus one per *growth* of a collection, defined by the language (not by Rust's `Vec` policy) as every append that takes its length past a power of two (1, 2, 4, 8, …). Scalars, rebindings, starting variables and Backend replies do not count.
- **RPC calls** — every host call the Script makes, counted when it stops at the call, before any capability decision: a refused, denied or failed call counts. An `Authorize` question is part of its call, not a second count.
- **Side effects** — every host call (counted exactly as RPC calls) plus every committed Global Variable write (none exist until Epic 6).
- **Output size** — the exact MessagePack byte length of the Script's result `{value}` payload, measured in `hexput-exec` before the reply leaves it.

Each is its own `Dimension`, code (`budget.allocations_exceeded`, `budget.rpc_calls_exceeded`, `budget.output_size_exceeded`, `budget.side_effects_exceeded`, all in `Code::ALL`) and limit; setting one never stands in for another. The call that would cross an RPC or side-effect limit is refused before it is sent (spanned on the call); calls already made stand. Every decision and error lives in `hexput-enforce`, reached only from `hexput-exec`.

**Decisions (2026-09-24, Erdem):**
1. *Default limits* — allocations 1 000 000, RPC calls 100, output size 1 MiB, side effects 100 per execution; documented constants in `hexput-enforce` until Story 3.7 makes them Config values.

**Never:** No Config keys or overrides (Story 3.7), no metrics (Epic 7), no Global Variables (Epic 6), no change to CPU time or memory semantics, no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Exact counts | a fixed Script building strings, arrays, objects and growing one array to length 9 | tests pin its allocation, RPC, side-effect and output-size counts exactly | N/A |
| Allocation churn | a loop building short-lived strings past the limit | `budget.allocations_exceeded`, even though memory stays low | N/A |
| RPC flood | a loop calling a blanket-granted function past the RPC limit | `budget.rpc_calls_exceeded` on the call that would cross it; that call never sent; earlier calls stand | N/A |
| Refused calls count | a loop calling an unregistered name — its first refusal already ends it; a per-call function whose handler says `false` | the refusal itself counts (a counted call ending in `capability`) | N/A |
| Large result | a result whose encoding passes the output limit but fits a frame | `budget.output_size_exceeded` | N/A |
| Independence | each limit crossed alone | only that dimension's code | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/diagnostics.rs` -- four codes into `Code::ALL`; `budget.rs` `Dimension` already has all six.
- `crates/hexput-interpreter/src/{heap.rs,machine.rs,lib.rs}` -- an allocation counter beside the memory count (string construction sites, literal evaluation, collection literals, power-of-two growth on append); `Meter` gains `allocation_ceiling`; `Outcome` gains an allocation stop (or reuse a generic "ceiling" stop naming which) spanned on the constructing site; `Execution::allocations()` for tests.
- `crates/hexput-enforce/src/lib.rs` -- `Limits` grows four fields and default constants; `Budget::charge_rpc_call(span)` (charges RPC calls and side effects together, refusing the call that would cross either), `allocation_ceiling()`/`allocations_exceeded(span)`, `charge_output(bytes)`.
- `crates/hexput-exec/src/lib.rs` -- charge at `Outcome::HostCall` (after the CPU charge, before `advance`); pass the allocation ceiling in `Meter`; measure the result's exact encoded length (move/extend `wire` so exec computes it; `hexput-script` keeps only the frame/depth protocol checks) and charge it before returning.
- `crates/hexput-tests/tests/{interpreter,enforce,exec,connection,shared}.rs` -- matrix, exact-count test.
- LANGUAGE-REFERENCE §7 `budget` row (codes, the power-of-two growth rule), `AGENTS.md`.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-shared` -- codes.
- [x] `crates/hexput-interpreter` -- allocation count and ceiling.
- [x] `crates/hexput-enforce` -- limits, charges, errors.
- [x] `crates/hexput-exec` -- charge RPC/side effects at each call, output size on the result.
- [x] `crates/hexput-tests/tests/*` -- every matrix row.
- [x] LANGUAGE-REFERENCE §7, `AGENTS.md`.

**Acceptance Criteria:**
- Given the six dimensions, when any one is exceeded alone, then the error names that one and no other.
- Given a reviewer tracing the four new dimensions, when they follow the code, then every limit and decision is in `hexput-enforce`, reached only from `hexput-exec`.

## Implementation Notes

- **To-string conversion** is counted per non-string operand a concatenation converts (`"a" + 1` = literal + conversion + concatenation = 3); the language has no other to-string site.
- **Growth** is an append whose collection length *before* it is a power of two (1→2, 2→3, 4→5, 8→9); an array grown from `[]` to length 9 counts 4. Object keys grow the same way. Counted in `Heap::array_store`/`object_store`, which `Heap::attach` never calls, so starting variables and host-call values never count.
- **Output size** is computed exactly without encoding (`hexput_exec::wire::payload_size`, an iterative walk that stops once past the limit), pinned against `rmp_serde` at every header boundary. It is charged before `hexput-script`'s frame/depth checks, so with the default 1 MiB limit `protocol.response_too_large` is unreachable through Direct Execution; the three `tests/script.rs` frame tests now assert the budget error and check `wire::check_result` directly.
- **RPC and side effects** are charged together in `Budget::charge_rpc_call`; RPC is checked first when both would cross.
- **`hexput_exec::execute_with_limits`** (re-exporting `hexput_enforce::Limits`, which gained `with_*` builders; `Budget::with_limits`) lets tests cross one dimension alone — a 1 000 000-allocation churn takes ~0.7 s of the 1 s CPU budget on a debug build, too close to rely on. Nothing in production calls it; Story 3.7 feeds it from Config.
- Two Story 3.5 CPU tests made their per-segment loops 10x longer (20 000 turns) so CPU time crosses before the 100-call RPC budget.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | verification-gap + blind | Two Story 3.5 CPU tests cross the 100-call RPC limit before the CPU limit on a release build (measured: 100 segments take 0.925 s), so they pass only because CI builds debug | medium | Reproduced by the reviewer with `cargo test --release`. **patch**: run them under `execute_with_limits` with RPC-call and side-effect limits raised, so CPU time is the only dimension that can cross |
| 2 | blind + edge-case | `type.function_argument`/`type.cyclic_argument` end the Script before it stops at the host call, so such calls are never counted, while a `depth.argument_too_deep` call is counted | low | Real inconsistency. **patch**: charge after the argument checks, so a call whose arguments cannot be sent is never a counted host call; document it in LANGUAGE-REFERENCE §7 |
| 3 | blind | On finish, output size is charged before the last slice's CPU time | low | Real; direct. **patch**: charge CPU first |
| 4 | blind + edge-case + verification-gap | `a_failed_call_counts` never makes a failed call (`fail()` is the 101st, refused unsent) | low | Real. **patch**: remove or rename to what it checks |
| 5 | blind | Unfinished comment in `a_denied_call_counts_and_its_question_is_part_of_it` | low | Real; direct. **patch** |
| 6 | edge-case | `payload_size` walk is uncapped when a caller passes a huge output limit | low | Real via `execute_with_limits`; direct. **patch**: cap the walk at `min(limit, MAX_FRAME_LEN)` |
| 7 | edge-case | `Machine::execute` doc omits `AllocationsExceeded` among terminal stops | low | Real; direct. **patch** |
| 8 | blind | Enforce independence test does not call `charge_rpc_call` on the zero-limit budget ("the reverse") | low | Real; direct. **patch** |
| 9 | blind | `protocol.response_too_large` via Direct Execution lost its end-to-end test (unreachable under the 1 MiB default) | low | Real, becomes reachable in Story 3.7. **defer** |
| 10 | blind | Side effects can only be charged with an RPC call; Global Variable writes (Epic 6) need a separate charge | low | Real, nothing to charge yet. **defer** |
| 11 | blind | AGENTS.md's `Execution::run` outcome list is stale | low | Real; agent-context file. **defer** |
| 12 | blind | The result is walked twice for size (`payload_size`, then `check_result`) | low | Rejected: perf only, two definitions are for different limits |
| 13 | blind | Spec Change Log empty though earlier behaviour changed | — | Rejected: the log records review loopbacks; the changes are in Implementation Notes |
| 14 | edge-case | The allocation comparison happens in the interpreter | low | Rejected: same design as Story 3.5's memory ceiling — `hexput-enforce` sets the ceiling and builds the error |
| 15 | blind | Sprint status `in-progress` vs spec `in-review` | false | Step 5 sets `review` |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
