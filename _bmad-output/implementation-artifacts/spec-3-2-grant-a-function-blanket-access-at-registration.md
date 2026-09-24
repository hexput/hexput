---
title: 'Story 3.2: Grant a function blanket access at registration'
type: 'feature'
created: '2026-09-24'
status: 'done'
baseline_commit: 'f2101288401c64f817488f0fcd2dad0cb56a7f46'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Since Story 3.1 every Registered Function is callable by any Script on its Session with no grant at all, so the Capability model (FR-6) does not exist yet: a Backend cannot say which functions are safe by definition, and the decision is not made where AD-3 says it must be.

**Approach:** A registration carries an optional blanket grant — a plain flag on the wire; how an SDK lets a Backend set it (the planning docs' `context.allow()` is only an example spelling) is the SDK's business, not the Daemon's. `hexput-enforce`, reached only through the Executor, decides every host call from the Session's registrations as they are when the execution is dispatched: blanket-granted → the call proceeds with no authorization round trip; anything else → the same `capability.unknown_function` an unregistered name gets.

## Boundaries & Constraints

**Always:** The grant decision lives in `hexput-enforce::Capabilities::check_call`, fed per execution from `hexput-session`'s single live copy (AD-3, AD-5); neither `hexput-connection` nor `hexput-session` decides. Unregistered and not-granted are indistinguishable to the Script (same code, same message, same span); the Daemon log tells them apart at `debug` with a `reason` field. Grants are per Session and never leak. Every existing and new test states its grants explicitly.

**Decisions (2026-09-24, Erdem):**
1. *Wire spelling* — the Daemon sees only a flag: an `Init` registration is `{name, blanket: <bool>}`, `blanket` optional (absent or `false` = no blanket grant), anything but a boolean refused. `context.allow()` in the planning docs is an example of how an SDK might expose it, not a Daemon concept; the Daemon attaches no other meaning to it.
2. *No grant, until Story 3.3* — fail closed: a function registered without the blanket grant is denied (`capability.unknown_function`, nothing sent); Story 3.3 turns "no grant" into "ask the per-call handler".
3. *AD-3 hardening from Story 3.1* — folded in: `hexput_rpc::Caller::call` becomes one unmistakable name (e.g. `dispatch_authorized`), and `scripts/check-crate-graph.py` fails CI when that name appears in any production crate but `hexput-exec` and `hexput-rpc`; the Story 3.1 `deferred-work.md` entry is marked resolved.

**Never:** No per-call handler or authorization message (Story 3.3), no Config keys, no budgets, no change to the `Call` message or its reply, no revocation or grant change after init (Story 3.8 territory), no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Blanket grant | `getOrder` registered with the grant; `return getOrder(1);` | exactly one `Call` written, no other message; Script gets the Backend's value | N/A |
| No grant | `getOrder` registered without the grant (`blanket` absent or `false`) | `capability.unknown_function` on the call, identical to unregistered; nothing sent | debug log `reason = not_granted` |
| Unregistered | `nope(1)` | `capability.unknown_function` on the call, nothing sent | N/A |
| Cross-Session | Session A grants `getOrder`, Session B registers it without the grant (or not at all); B's Script calls it | B's call is `capability.unknown_function`; A's still proceeds | N/A |
| Bad grant value | registration `{name, blanket: "yes"}` | `Init` refused with `protocol.invalid_payload` naming `registrations[i].blanket`; no Session | as other init errors |

</frozen-after-approval>

## Code Map

- `crates/hexput-session/src/init.rs` -- `RegisteredFunction { name }` (L19) gains the grant; `decode_registrations` (L231) accepts the new optional key (today any key but `name` is "unknown key", L246) and refuses a non-boolean naming `registrations[i].<key>`. Update the Story 3.2 doc note on `RegisteredFunction`.
- `crates/hexput-session/src/lib.rs` -- `registration_names` (L169) → return the registrations with their grants (e.g. `registrations(client_id) -> Option<Vec<RegisteredFunction>>`), still read per execution, never cached.
- `crates/hexput-enforce/src/lib.rs` -- `Capabilities::registered(names)` (L37) → built from `(name, granted)` pairs; `check_call` (L~50) allows only granted names; same diagnostic for unregistered and not-granted; returns the refusal reason for logging.
- `crates/hexput-exec/src/lib.rs` -- `Host::new(registrations, caller)`; log a refused call at `debug` with `reason = "unregistered" | "not_granted"`, never in the Script-visible message.
- `crates/hexput-script/src/lib.rs` (L77), `crates/hexput-connection/src/lib.rs` (L245) -- pass the registrations with grants through.
- `crates/hexput-tests/tests/{session,enforce,exec,connection,script,daemon}.rs` -- the matrix; ~35 existing `registrations` payloads/helpers (e.g. `init_payload`) state grants explicitly.
- Per decision 3: `crates/hexput-rpc/src/lib.rs` (`Caller::call`), `crates/hexput-exec/src/lib.rs`, `scripts/check-crate-graph.py`, `deferred-work.md` entry closed.
- LANGUAGE-REFERENCE §8: one sentence that a Registered Function is callable only with a grant (blanket today, per-call from Story 3.3). `AGENTS.md` Project Status.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-session/src/{init.rs,lib.rs}` -- decode and expose the grant.
- [x] `crates/hexput-enforce/src/lib.rs` -- the grant decision.
- [x] `crates/hexput-exec`, `crates/hexput-script`, `crates/hexput-connection` -- thread grants through; debug log with reason.
- [x] `crates/hexput-rpc`, `crates/hexput-exec`, `scripts/check-crate-graph.py`, `deferred-work.md` -- the AD-3 guard (decision 3).
- [x] `crates/hexput-tests/tests/*` -- every matrix row; explicit grants in existing tests.
- [x] `AGENTS.md` -- Project Status.

**Acceptance Criteria:**
- Given a blanket-granted call, when the connection's written envelopes are inspected, then the only envelope for it is the one `Call` — no authorization request.
- Given a refused call, when the Daemon log is read at `debug`, then the event names the function and whether it was unregistered or not granted, while the Script's error is identical in both cases.

## Implementation Notes

- Grants travel as `(name, blanket)` pairs from `hexput-connection` through `hexput-script` to `hexput_exec::Host::new` and `hexput_enforce::Capabilities::registered`: `hexput-script` cannot name `hexput_session::RegisteredFunction`, and a shared type would need a new crate edge.
- `check_call` returns a `Refusal { reason: Reason, diagnostic }`; the diagnostic is built identically for both reasons.
- The `debug` refusal event is emitted from `execute`'s async loop, not from the blocking segment, so it inherits the execution task's `request` span (connection, Client ID, request id); a test in `tests/connection.rs` pins that.
- A present-but-nil `blanket` is refused as not a boolean (only an absent key means "no grant").
- The AD-3 guard is a source-text scan (`RESTRICTED_NAMES` in `scripts/check-crate-graph.py`) over every `.rs` file of every workspace crate except `hexput-tests`, `hexput-exec` and `hexput-rpc`, matching the whole word `dispatch_authorized`.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | blind + edge-case | The AD-3 guard only blocks the name `dispatch_authorized`; `hexput-connection` (single writer of `Outbound`) can still build a `MessageType::Call` envelope itself, and `deferred-work.md`/rpc docs call the gap "RESOLVED" / "can never use it" | medium | Real: nothing stops `Envelope::new(_, MessageType::Call, _)` in the connection. **patch**: add `MessageType::Call` to `RESTRICTED_NAMES` (allowed only in `hexput-rpc`, plus `hexput-shared`/`hexput-port`), and reword the docs/deferred entry to "text guard; sealed token still open" |
| 2 | verification-gap + blind | `restricted_name_uses` has no negative test; a typo or dropped `extend` would silence it | medium | Pre-verified gap; the script has no harness for any rule. **defer** (script test harness) |
| 3 | edge-case + blind | `Capabilities::registered` lets the last duplicate win, so `(f,false),(f,true)` grants `f`; the AD-3 decision point relies on `hexput-session`'s dedup | low | Real for any future caller; direct fold. **patch**: a duplicate folds fail-closed (any `false` wins) + test |
| 4 | blind | Guard matches comments/doc text too, undocumented | low | Real (why connection/script docs avoid the name). **patch**: say so beside `RESTRICTED_NAMES` |
| 5 | blind | Success line counts restricted names as "edges" | low | Real, cosmetic, direct. **patch** (reword) |
| 6 | blind | `hexput_enforce::Reason` is public and not `#[non_exhaustive]`; Story 3.3 adds reasons | low | Real; direct. **patch** (`#[non_exhaustive]`, doc that `NotGranted` is interim until 3.3) |
| 7 | blind | Init wire shape `{name, blanket}` documented only in rustdoc and one LANGUAGE-REFERENCE sentence | low | Real; decision 1 makes it the Daemon's contract. **patch**: one line in the Spine's 2026-09-24 wire-contract amendment |
| 8 | blind | Two new doc lines in `hexput-connection`/`hexput-script` exceed 100 columns | low | Real, direct. **patch** (reflow) |
| 9 | blind | AGENTS.md sentence grammar ("it"), rename history inline | low | Real; fix edits an agent-context file. **defer** |
| 10 | blind | Existing Backends' `{name}` registrations silently become uncallable; only a `debug` event | low | Real but decided: decision 2 is fail-closed, and no Backend is deployed yet. Rejected |
| 11 | blind | Refused calls convert arguments before the grant decision | low | carried from Story 3.1 triage row 7 (order documented in LANGUAGE-REFERENCE §8). Rejected |
| 12 | blind | Registrations copied three times per execution | low | Real, perf only, no named harm at current scale; fix reshapes Session storage. Rejected |
| 13 | blind | `Step::Refused` clones the name | low | Rejected: negligible |
| 14 | blind | No test that grants are re-read per execution | low | Unreachable today: nothing changes registrations after init until Story 3.8. Rejected |
| 15 | edge-case | A macro-built name or an exec-exported wrapper evades the text scan | low | Real in principle; no such code, fix is the sealed token already deferred. Rejected (covered by row 1's deferred note) |
| 16 | blind | Sprint status `in-progress` vs spec `in-review` | false | Step 5 of the workflow sets `review` when the story completes |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
