---
title: 'Story 3.3: Decide per call whether a function may be used'
type: 'feature'
created: '2026-09-24'
status: 'draft'
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
- [ ] `crates/hexput-shared`, `crates/hexput-enforce` -- message type; three-way decision and outcome classification.
- [ ] `crates/hexput-rpc`, `crates/hexput-connection` -- issue, route, close for the question.
- [ ] `crates/hexput-exec` -- ask with timeout, classify, dispatch or refuse, log reason.
- [ ] `scripts/check-crate-graph.py` -- restricted name.
- [ ] `crates/hexput-tests/tests/*` -- every matrix row; update Story 3.2 tests.
- [ ] LANGUAGE-REFERENCE §8, Spine, `AGENTS.md`.

**Acceptance Criteria:**
- Given any denial, when the Script's error is compared with an unregistered call's, then code, message and span are identical, and only the `debug` log's `reason` differs.
- Given a question waiting for its answer, when another execution on the same connection runs, then it completes meanwhile (no thread held).

## Implementation Notes

## Spec Change Log

## Review Triage Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
