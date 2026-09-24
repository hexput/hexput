---
title: 'Story 3.3: Decide per call whether a function may be used'
type: 'feature'
created: '2026-09-24'
status: 'done'
baseline_commit: 'c886f12eec35a84807460d20273cfeddf8cffeb8'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Since Story 3.2 a Registered Function without a blanket grant is simply uncallable (fail closed). A Backend cannot let access depend on the individual call — who the Script acts for, what it passes — which FR-6's per-call handler exists for.

**Approach:** A call to a function registered without the blanket grant is decided by the Backend itself, per call: `hexput-enforce` answers "ask the handler", and the Executor puts the question to the Backend over the submitting connection before any `Call` is sent. Only an explicit `true` lets the call proceed; `false`, a non-boolean, an error, no answer in time, or a connection that ends first all deny it with the same `capability.unknown_function` an unregistered name gets. The Daemon log tells every denial reason apart.

## Boundaries & Constraints

**Always:** The decision stays in `hexput-enforce` and is driven only by `hexput-exec` (AD-3); the question travels through `hexput-rpc`'s correlation and the connection's single writer, and waiting for its answer holds no thread and no lock (AD-6). Blanket-granted calls still send exactly one `Call` and no question (Story 3.2). A denial is never catchable — it ends the Script like every error (LANGUAGE-REFERENCE §7 wins over the story's "catchable"). The Script's error is identical for every denial reason and for an unregistered name; the Daemon logs each denial at `debug` with `function` and a distinct `reason` (`refused`, `handler_invalid`, `handler_failed`, `handler_timeout`, `handler_no_reply`, `unregistered`). The new message type joins `RESTRICTED_NAMES` like `MessageType::Call`.

**Decisions (2026-09-24, Erdem):**
1. *How the Daemon asks* — a separate message before the call: `MessageType::Authorize` with payload `{name, arguments}` under a Daemon-issued id (the same per-connection counter as `Call`), answered `Result {value: <bool>}` or `Error`. Only `value: true` proceeds, and the Daemon then sends the ordinary `Call`: two round trips, the guard kept apart from the implementation (FR-6).
2. *Timeout* — a documented constant, `AUTHORIZATION_TIMEOUT` = 5 seconds, which Story 3.7 later makes the default of a Config value. An answer arriving after it is dropped (routed to no one).

**Never:** No Config keys or per-execution override for the timeout (Story 3.7), no budget counting (Story 3.6), no grant change after init, no caching of a handler's answer across calls, no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Handler allows | `getOrder` registered without `blanket`; handler answers `true` | the question, then one `Call`; Script gets the Backend's value | N/A |
| Handler refuses | handler answers `false` | no `Call`; `capability.unknown_function` on the call | debug `reason = refused` |
| Non-boolean | handler answers `"yes"` / `1` / `null` | denied, no `Call` | `handler_invalid` |
| Handler error | handler answers `Error` | denied, no `Call` | `handler_failed` |
| No answer | handler silent past the timeout | denied, no `Call`; a late answer is dropped | `handler_timeout` |
| Connection ends | peer closes while a question is pending | denied, no `Call` | `handler_no_reply` |
| Blanket unchanged | `blanket: true` | exactly one `Call`, no question | N/A |
| Per call, not cached | same function called twice | asked twice | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/wire.rs` -- `MessageType` gains `Authorize`; `ALL`/`as_str` updated.
- `crates/hexput-enforce/src/lib.rs` -- `check_call` becomes three-way: allowed (blanket), ask the handler (registered, no blanket — replaces 3.2's interim `Reason::NotGranted`, which `#[non_exhaustive]` anticipated), refused (unregistered); a helper turns a handler outcome into allow or a `Refusal` with its reason, same diagnostic for all.
- `crates/hexput-rpc/src/lib.rs` -- `Calls::issue` builds either envelope; the pending table routes the answer back; `Caller` gains the question alongside `dispatch_authorized` (the question sends nothing that runs host code, but it too is only for `hexput-exec`).
- `crates/hexput-exec/src/lib.rs` -- `execute`'s loop: on "ask", await the answer under `tokio::time::timeout`, classify it, then dispatch or refuse; the refusal log already lives in this loop (Story 3.2).
- `crates/hexput-connection/src/lib.rs` -- writes the question like a `Call`; routes its answer via `Calls::complete`; close fails it like a pending `Call`.
- `scripts/check-crate-graph.py` -- restrict the new message type to `hexput-rpc` (+ `hexput-shared`, `hexput-port`).
- `crates/hexput-tests/tests/{enforce,rpc?,exec,connection,shared}.rs` -- matrix; Story 3.2's "no grant is refused" tests become "no grant asks the handler".
- LANGUAGE-REFERENCE §8 (per-call decision replaces "from Story 3.3"), Spine wire-contract amendment (the question message), `AGENTS.md`.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-shared`, `crates/hexput-enforce` -- message type; three-way decision and outcome classification.
- [x] `crates/hexput-rpc`, `crates/hexput-connection` -- issue, route, close for the question.
- [x] `crates/hexput-exec` -- ask with timeout, classify, dispatch or refuse, log reason.
- [x] `scripts/check-crate-graph.py` -- restricted name.
- [x] `crates/hexput-tests/tests/*` -- every matrix row; update Story 3.2 tests.
- [x] LANGUAGE-REFERENCE §8, Spine, `AGENTS.md`.

**Acceptance Criteria:**
- Given any denial, when the Script's error is compared with an unregistered call's, then code, message and span are identical, and only the `debug` log's `reason` differs.
- Given a question waiting for its answer, when another execution on the same connection runs, then it completes meanwhile (no thread held).

## Implementation Notes

- `hexput-enforce`: `check_call -> Result<Decision, Refusal>`; `Decision::{Allowed, AskHandler(Question)}`; `Question::decide(HandlerAnswer) -> Result<(), Refusal>` is the only way to turn an answer into a decision. `Reason::NotGranted` is gone.
- `hexput-exec` classifies the raw answer: `Result {value: bool}` → `Boolean`; any other value or a malformed `Result` → `NotBoolean` (`handler_invalid`); `Error` → `Failed`; an unframable question (the arguments pass the frame lower bound but the envelope still does not fit) is also `Failed` (`handler_failed`) — no seventh reason; connection gone → `NoReply`; `tokio::time::timeout` elapsed → `TimedOut`. The question's arguments are a clone of the call's.
- A timed-out question stays in the connection's pending table until its answer arrives (then consumed and dropped silently) or the connection closes. A Backend that never answers therefore grows its own connection's table by one entry per timed-out question; bounded per connection, never shared.
- `execute`/`direct_execution` now need a runtime with timers enabled when a question may be asked; the Daemon's runtime uses `enable_all`.

- Post-review follow-up (Erdem, 2026-09-24, triage row 1 answered A): `Call` and `Authorize` payloads gain `execution`, the originating `ExecutionStart`'s correlation id, set per execution through `hexput_rpc::Caller::for_execution`; additive, omitted when no execution is named. Test: `every_call_and_question_names_the_execution_that_made_it`.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | blind | `Authorize` (and `Call`) carry only `{name, arguments}`; a Backend with several executions in flight on one connection cannot tell which execution a question belongs to, though the story's point is "who the Script acts for" | medium | Real: the payload names no execution. Fix changes frozen decision 1's payload → **intent_gap**, surfaced to Erdem; code kept because the fix is additive (an optional field), not a re-derivation |
| 2 | blind + edge-case + verification-gap | A timed-out question's entry stays in `Calls.pending` until an answer or close; a silent handler grows its own connection's table | medium | Real, self-inflicted by the Backend and confined to its connection; removing entries early turns late answers into stray replies, which the matrix rules out. **defer** (bound with Story 3.6's RPC budget / a tombstone scheme) |
| 3 | edge-case | A question whose timeout elapses while still queued is written anyway, asking the Backend about a call the Script already failed on | low | Real; direct fix. **patch**: `Calls::issue`/the loop skips a call whose reply receiver is gone |
| 4 | blind + edge-case | `Question` derives `Clone`, contradicting "one question, one decision" | low | Real; direct. **patch**: drop `Clone`/`PartialEq` from `Question` (and `Decision` if needed) |
| 5 | blind + verification-gap | `Calls::close`/`pending()` docs name only `dispatch_authorized`/"calls" | low | Real; direct. **patch** |
| 6 | blind + verification-gap | `Calls::complete` doc line ~128 columns | low | Real; direct. **patch** (reflow) |
| 7 | blind | `epics.md` Story 3.3 still requires a "catchable" denial | low | Real; LANGUAGE-REFERENCE wins but the epic carries no note. **patch**: dated note on that criterion |
| 8 | verification-gap + blind | An unframable `Authorize` (`CallFailure::Unsendable`) → `handler_failed` is untested | low | Pre-verified gap; cheap test. **patch** (test) |
| 9 | blind | "Not cached" tests answer `true` twice, proving nothing | low | Real; direct. **patch**: answer `true` then `false`, expect the second call denied |
| 10 | blind | A late answer is dropped with no log | low | Real; direct. **patch**: `debug` event when a routed answer finds its receiver gone |
| 11 | blind | The Spine amendment for `Authorize` is appended inside the FR-27/FR-28 paragraph | low | Real; direct. **patch**: its own paragraph |
| 12 | blind | The 5 s timeout also counts queueing before the question is written | low | Real but the writer queue is normally immediate; fix needs a written-notification. Rejected |
| 13 | blind + edge-case | A runtime without timers panics inside `tokio::time::timeout` | low | The Daemon's runtime always enables timers; documented on `execute`/`direct_execution`. Rejected |
| 14 | blind | The two timeout tests each wait a real 5 s | low | Real; fix needs tokio `test-util` pinned workspace-wide. Rejected |
| 15 | blind | Status spellings disagree | false | Step 5 sets `review` |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
