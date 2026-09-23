---
title: 'Story 2.7: Keep slow executions from blocking anything else'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: 'bffa300de52301df8e8291327443a0ce56ef5e5c'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** `hexput-connection` runs each Direct Execution inline in its read loop, so a slow Script stops that connection from reading anything else and pins a runtime worker that other connections' tasks may be queued behind — the head-of-line blocking FR-16 forbids (AD-6).

**Approach:** Every initialized `ExecutionStart` is dispatched as its own task owned by the connection; the loop keeps reading while executions run and writes each reply, with its request id, as soon as that execution finishes — in completion order, not submission order. `Init`, refusals and malformed-frame answers stay in the loop.

## Boundaries & Constraints

**Always:** One connection may have any number of executions in flight at once; none waits on another, on this connection or any other. There is no per-connection serial queue anywhere in the path. The loop owns the `Outbound` half and is the only writer, so no lock guards it and no lock of any kind is held across an `.await`; a send is always driven to completion (`Outbound::send` is not cancel-safe), while `Inbound::recv` may be raced (it is). Every reply still goes to the connection that sent the request, echoing its id, with 2.6's `response_too_large` fallback. `hexput_script::direct_execution` stays the one call that executes; the dispatch lives in `hexput-connection`. The init gate stays one check. Detach still happens on `serve`'s one exit path, after the connection's executions are finished or abandoned.

**Decisions (2026-09-23, Erdem):**
1. *Where a Script runs* — each execution runs through `spawn_blocking` on the shared Tokio runtime's blocking pool, awaited by the connection as a task. Runtime workers never run Script code, so slow Scripts cannot starve reads or other connections even when they outnumber cores; this is AD-6's "independent task on the shared runtime". Epic 3's mid-Script RPC can block on the runtime handle from that thread.
2. *When the peer stops sending* (clean close — possibly only its write side — or a fatal frame) — the connection stops reading, waits for its in-flight executions, writes each reply, then closes and detaches. A never-ending Script therefore keeps the connection and Session alive until shutdown, as today, until Story 3.5's budget. A failed write still abandons them at once.

**Never:** No budget, timeout or cancellation of a running Script (Epic 3, Story 3.5); no cap on in-flight executions per connection (Epic 3); no Client ID span (2.8); no change to `hexput-script`, `hexput-exec` or the wire format; no new crate edge.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Same connection | initialized; slow `ExecutionStart` id 2, then fast id 3 | `Result` for 3 arrives before `Result` for 2; both carry their ids | N/A |
| Other connection | A runs a slow Script; B inits and runs a fast one | B's result arrives while A's is still running; A later gets its own | N/A |
| Interleaved | slow execution in flight, then `Init` again / malformed frame / fast execution | each answered at once (`already_initialized`, its `protocol.*` code, its result) | N/A |
| Many in flight | several executions submitted back to back | every one answered once, each with its id | N/A |
| Failing among slow | slow in flight, then a failing Script | the failure's `Error` arrives first; the slow one still completes | its diagnostic |
| Write fails | a reply cannot be written (not `InvalidInput`) | connection ends; remaining executions are abandoned; Session detached | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-connection/src/lib.rs` -- `exchange` (line ~64) is the loop; `answer` (line ~124) runs `direct_execution` inline at line ~151. Restructure: the loop races `inbound.recv()` against the next finished execution (`tokio::task::JoinSet`, whose `join_next` is cancel-safe) with `tokio::select!`, and writes every reply through the existing `deliver`. `answer` keeps `Init`, the unexpected-message refusal and the gate, and returns either an immediate reply or "dispatch this execution" — taking the payload by value (a request can be 16 MiB; do not clone it). Dropping the `JoinSet` on a write failure abandons unfinished executions. A panicking execution task has no id to answer with: log it at `error` and keep serving (today such a panic would kill the whole connection). Rewrite the module doc's "What a connection is answered today" (inline-execution sentence) and the concurrency story.
- `crates/hexput-connection/Cargo.toml` -- add `tokio = { workspace = true }` (workspace features already include `rt-multi-thread`, `macros`); comment why. External crates are not pinned by `check-crate-graph.py`; no workspace edge changes.
- `crates/hexput-tests/tests/connection.rs` -- the scripted Port yields its script then `Closed`; every existing test depends on replies after that close, so it exercises Decision 2. Existing tests asserting reply order across several executions must compare by id where order is no longer defined. Add: fast-before-slow on one connection; interleaved `Init`/malformed/fast during a slow one; many in flight each answered once; write failure abandons. A slow Script is a counted `while` loop — size it so it takes well over the fast one in the test profile (measure; aim ≲1 s), never a sleep.
- `crates/hexput-tests/tests/daemon.rs` -- socket helpers at lines ~483–585; `a_script_runs_over_the_socket_and_a_failure_disturbs_no_one` (line ~675) has the comment 2.6 left for 2.7. Add the cross-connection row and the same-connection row over the real socket.
- `crates/hexput-daemon/src/lib.rs` -- no change expected: connection tasks already live in a `JoinSet` aborted on shutdown, and dropping a connection's own `JoinSet` with it is enough.
- `AGENTS.md` Project Status (Story 2.7 done, next 2.8); `deferred-work.md` -- update the 2.6 non-terminating-Script entry (now a blocking-pool thread, not a runtime worker) and add one for the uncapped in-flight executions per connection.
- Do not touch: `hexput-script`, `hexput-exec`, `hexput-port`, `hexput-session`, `hexput-transport`, `scripts/check-crate-graph.py`.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-connection/{Cargo.toml,src/lib.rs}` -- dispatch executions as tasks; one writer loop; module doc.
- [x] `crates/hexput-tests/tests/connection.rs` -- matrix rows through the in-memory Port; order-independent assertions where order is undefined.
- [x] `crates/hexput-tests/tests/daemon.rs` -- same- and cross-connection rows over the socket.
- [x] `AGENTS.md`, `_bmad-output/implementation-artifacts/deferred-work.md` -- status and deferrals.

**Acceptance Criteria:**
- Given the five CI commands, when run, then all pass.
- Given `hexput-connection`, when read, then no lock is held across an `.await` and nothing serializes one execution behind another (AD-6).
- Given a slow execution in flight, when the connection reads further messages, then each is answered without waiting for it (FR-16, NFR5).

## Design Notes

A `select!` loop with the connection as sole writer is chosen over a separate writer task fed by a channel: no channel, no extra task, natural backpressure (a peer that stops reading stalls only its own connection, as today), and the fatal-frame/close ordering stays in one place. The cost is that the loop does not read while one reply is being written — bounded by one frame.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green.

## Implementation Notes

- `Received::Closed(Some(_))` (a lost stream) abandons in-flight executions at once, like a failed write: Decision 2 names only a clean close and a fatal frame as "wait and reply", and a broken stream cannot carry the replies.
- The slow test Script is 150 000 loop turns — about 0.5 s in the debug test profile (300 000 measured at ~1 s through `hexput eval`).
- Tokio's `Runtime` drop waits indefinitely for running blocking tasks, so a never-ending Script still hangs Daemon shutdown (as before 2.7); recorded in `deferred-work.md` with the uncapped in-flight executions.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind, edge | A panicked execution's request is never answered (id lost with the `JoinError`) | low | Real, but nothing in `direct_execution` is known to panic; a reply needs a new internal-error code (public surface) | defer (with #6) |
| 2 | blind | `spawn_blocking` drops the tracing span, so 2.8's Client ID span would not reach `execute`'s events | low | Verified: the closure captures no `Span`; 2.8 is the next story and will meet it | patch — span captured and entered on the blocking thread |
| 3 | blind | `Inbound::recv`'s cancel-safety is not a documented trait requirement | false | `crates/hexput-port/src/port.rs` `Inbound::recv` doc: "Must be cancel-safe … so the core may race it in a `select!`" | reject |
| 4 | blind, verification-gap | No test has an execution in flight when a fatal frame arrives | medium | Pre-verified: returning after the fatal reply passes every test | patch — `a_fatal_frame_*` test |
| 5 | blind, verification-gap | No test has an execution in flight when the stream is lost | medium | Pre-verified: treating `Closed(Some)` as a clean close passes every test | patch — lost-stream sibling of the failed-write test |
| 6 | blind, verification-gap | The panic branch ("log, keep serving") is untested | medium | Pre-verified: `continue` → `return` passes; forcing a panic needs a seam the spec keeps out of `hexput-script` | defer |
| 7 | blind | `sprint-status.yaml` says `in-progress` while the spec says `in-review` | false | The workflow syncs sprint status at presentation (step 5); `in-progress` is the value it set at implementation start | reject |
| 8 | blind | `epic-2-context.md` still says "async task" for AD-6, not Decision 1's blocking pool | false | The context distils planning artifacts, which still read that way; Decision 1 is recorded in this spec and the connection's module doc | reject |
| 9 | blind | The `epic-2-context.md` recompile dropped facts (diagnostic field list, "or a cycle", amendment provenance) | low | Verified in the diff; restoring the two facts is a direct edit (provenance dates are not facts a later story needs) | patch — both facts restored |
| 10 | blind, edge | Module doc says no execution waits "however many slow Scripts outnumber the cores", yet the blocking pool caps at 512 | low | Verified; contradicts this story's own deferred entry; direct wording fix | patch |
| 11 | blind, edge | Slow-vs-fast ordering, `abandoned < full / 2` and the daemon's `WouldBlock` probe are timing-based | low | Margins are ~0.5 s against microseconds–milliseconds in the debug profile CI runs; making them deterministic needs a gate the language cannot express yet | reject |
| 12 | blind | `SLOW_TURNS` and the slow source are duplicated in two test files | low | Cosmetic; a shared module adds structure for two lines | reject |
| 13 | blind, edge | After a fatal frame the connection stays open behind a never-ending Script | low | Decision 2 (frozen) makes a fatal frame wait for in-flight executions; the never-ending case is already deferred to Story 3.5 | reject |
| 14 | blind | Finished replies pile up while the loop is blocked writing | medium | Verified: each finished task holds its result until the loop drains it; bounded only by the uncapped in-flight count, which the frozen Never keeps until Epic 3 | defer |
| 15 | edge | A cancelled `JoinError` would be logged at `error` | false | Tasks are cancelled only when the `JoinSet` is dropped, after which the loop no longer polls `join_next` | reject |
| 16 | edge | "Bounded by one frame" bounds size, not time; a non-reading peer stalls the loop indefinitely | low | Verified: `deliver` awaits `send` with no timeout (the Story 2.3 deferred write timeout); direct wording fix | patch |
