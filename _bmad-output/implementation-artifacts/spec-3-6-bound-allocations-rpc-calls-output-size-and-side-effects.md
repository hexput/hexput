---
title: 'Story 3.6: Bound allocations, RPC calls, output size, and side effects'
type: 'feature'
created: '2026-09-24'
status: 'ready-for-dev'
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
- [ ] `crates/hexput-shared` -- codes.
- [ ] `crates/hexput-interpreter` -- allocation count and ceiling.
- [ ] `crates/hexput-enforce` -- limits, charges, errors.
- [ ] `crates/hexput-exec` -- charge RPC/side effects at each call, output size on the result.
- [ ] `crates/hexput-tests/tests/*` -- every matrix row.
- [ ] LANGUAGE-REFERENCE §7, `AGENTS.md`.

**Acceptance Criteria:**
- Given the six dimensions, when any one is exceeded alone, then the error names that one and no other.
- Given a reviewer tracing the four new dimensions, when they follow the code, then every limit and decision is in `hexput-enforce`, reached only from `hexput-exec`.

## Implementation Notes

## Spec Change Log

## Review Triage Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
