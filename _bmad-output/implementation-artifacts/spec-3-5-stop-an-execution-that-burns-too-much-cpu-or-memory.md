---
title: 'Story 3.5: Stop an execution that burns too much CPU or memory'
type: 'feature'
created: '2026-09-24'
status: 'in-review'
baseline_commit: '7d160d51a159e91cc7e0b18f6b1deeda52c5b3ad'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Nothing bounds an execution's CPU or memory. `while (true) {}` pins a blocking-pool thread forever, keeps its connection and Session alive and hangs Daemon shutdown (deferred from Story 2.6), and a Script that keeps growing a string or collection can take the Daemon's memory with it — one tenant degrading everyone (NFR4).

**Approach:** The first two Resource Budget dimensions, enforced through the one Executor (AD-3). The interpreter stays host-blind but *meterable*: it runs in bounded slices and yields between them, and it keeps an approximate count of the memory its heap holds, stopping at a ceiling it is handed. `hexput-enforce` owns the budget — the dimensions (the shared `budget.rs` enum), the limits, and the decision — and `hexput-exec` charges it between slices and before resuming after a host call. Crossing either limit ends only that execution with a `budget` error naming the dimension; its thread is freed and nothing else is disturbed.

## Boundaries & Constraints

**Always:** The decision and the error live in `hexput-enforce`, reached only through `hexput-exec` (the graph check already pins it). The six dimensions are one `#[non_exhaustive]` enum in `hexput-shared::budget` (CPU time, memory, allocations, RPC calls, output size, side effects); only CPU time and memory are enforced here. Each dimension has its own stable code (`budget.cpu_time_exceeded`, `budget.memory_exceeded`) in `Code::ALL`, category `budget`, distinct from `capability`/`policy`/`host`. The error is spanned on the construct running when the limit was crossed. Waiting for a host call's reply or a per-call handler's answer is never charged as CPU time. The interpreter is checked often enough that a runaway loop stops within a small fraction of its CPU limit, and memory is checked at allocation so one step cannot overshoot by more than that step's own allocation. A terminated execution never panics the Daemon and never disturbs other in-flight executions; RPC calls it already made stand.

**Decisions (2026-09-24, Erdem):**
1. *Default limits* — CPU time 1 second and memory 64 MiB per execution, documented constants in `hexput-enforce` until Story 3.7 makes them Config values.
2. *What CPU time measures* — the monotonic time (`Instant`) the execution spends running Script code on its blocking thread, summed across slices and segments, excluding every wait on the Backend. No OS thread-CPU API, no `unsafe`; the doc says an oversubscribed host inflates it.

**Never:** No Config keys or per-execution overrides (Story 3.7 — limits are documented constants here), no allocation/RPC/output/side-effect counting (Story 3.6), no metrics or budget log events beyond one `debug` event (Epic 7), no OS-level limits, threads killed, or `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Runaway loop | `while (true) {}` | ends with `budget.cpu_time_exceeded`; its blocking thread is free again | N/A |
| Deep recursion within depth | a tight recursive loop that stays under 1024 frames but never ends | `budget.cpu_time_exceeded` | N/A |
| Growing string | `let s = "x"; while (true) { s = s + s; }` | `budget.memory_exceeded` before the process allocates far past the limit | N/A |
| Growing collection | `let a = []; while (true) { a[len] = …; }`-style append loop (e.g. via a counter) | `budget.memory_exceeded` | N/A |
| Host wait not charged | a Script whose Backend takes longer than the CPU limit to answer one call | completes normally | N/A |
| Neighbours unaffected | one runaway and one normal execution on the same connection, and one on another | the normal ones complete; the runaway gets its budget error | N/A |
| Shutdown | Daemon shut down while a runaway runs | exits once the runaway hits its CPU limit (no indefinite hang) | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/budget.rs` -- stub today; the `Dimension` enum (six variants, `ALL`, `as_str`).
- `crates/hexput-shared/src/diagnostics.rs` -- `Category::Budget` exists (L94); add the two codes to `Code::ALL`.
- `crates/hexput-interpreter/src/machine.rs` -- the step loop (`execute`, ~L332): count steps and stop with a new "slice ended" outcome every N steps; `heap.rs`: approximate live-bytes accounting (charge on slot alloc/growth and string creation stored in the heap, credit on slot release) with a ceiling that stops the machine with an "out of memory" outcome spanned on the current construct. Keep `Execution` `Send + 'static`; `evaluate*` (CLI) run unmetered (no limits) unless the Executor asks.
- `crates/hexput-interpreter/src/lib.rs` -- extend `Outcome` (e.g. `Sliced`, `MemoryCeiling`) and the `Execution` API (`run` gains a slice size / memory ceiling, or a `Meter` config struct) without host knowledge.
- `crates/hexput-enforce/src/lib.rs` -- `Budget` (limits per dimension, documented default constants), `charge_cpu(Duration)`, the memory ceiling it hands out, and the `budget.*` diagnostics.
- `crates/hexput-exec/src/lib.rs` -- `execute`: create the `Budget`, run each blocking segment as a loop of slices timing each with `Instant`, charge between slices, stop on excess; never charge `ask`/`dispatch_authorized` waits.
- `crates/hexput-tests/tests/{interpreter,enforce,exec,connection,shared,daemon}.rs` -- matrix; daemon shutdown test with a runaway.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- mark the Story 2.6 runaway/shutdown entry resolved; LANGUAGE-REFERENCE §7 `budget` row codes; `AGENTS.md`.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-shared` -- `Dimension` enum; two codes.
- [x] `crates/hexput-interpreter` -- slices and memory metering with a ceiling.
- [x] `crates/hexput-enforce` -- `Budget`, defaults, decisions, diagnostics.
- [x] `crates/hexput-exec` -- charge between slices, exclude waits.
- [x] `crates/hexput-tests/tests/*` -- every matrix row.
- [x] LANGUAGE-REFERENCE §7, `deferred-work.md`, `AGENTS.md`.

**Acceptance Criteria:**
- Given a reviewer tracing budget enforcement, when they follow the code, then every limit and decision is in `hexput-enforce` and reached only from `hexput-exec`; the interpreter only meters.
- Given a runaway execution, when it is stopped, then the stop happens within a small fraction of its CPU limit past the limit (pinned by a test with a generous bound).

## Implementation Notes

- **Strings meter themselves.** `RtValue::String` holds a `Text` — shared text plus a handle on the heap's atomic string counter — that credits its charge when its last handle drops. So a string shared by many bindings counts once, one no longer reachable stops counting at once, and `s = s + "x"` in a loop is linear, not quadratic. Slots are charged by footprint on allocation/growth and credited on release. Detach/attach stay zero-copy (`Text` wraps an `Arc<str>`). The frame and value stacks, `for … in` key snapshots and detached copies are not counted (deferred-work entry).
- **Slices count work, not steps alone.** One unit per frame plus one per 256 string bytes an operation reads or writes (binary operands, unary operand, index keys; object keys at `for … in` start), so a slice of 64 MiB string compares is not 10 000 × 10 ms. `hexput_exec::SLICE` is 10 000 units.
- **Memory is checked after every frame, and before a concatenation** whose result (an upper bound: a number spells in at most 32 bytes) would cross the ceiling — spanned on the `+`. A frame with no place of its own (scope exit, discard) keeps the last construct's span; after a host call's value arrives, the call is the running construct.
- **The slice that finishes the Script is not charged**: the work is done, and failing a completed Script over its last few milliseconds helps no one. A host call's slice is charged before anything about the call is decided or sent.
- **Test timing.** The Story 2.7 "slow Script" loops (connection/daemon tests) went from 150 000 to 50 000 turns: ~0.6 s in a debug build was too close to the 1 s CPU budget on a loaded machine, since the budget measures wall time on the thread.

## Spec Change Log

## Review Triage Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
