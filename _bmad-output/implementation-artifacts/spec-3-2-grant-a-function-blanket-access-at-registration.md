---
title: 'Story 3.2: Grant a function blanket access at registration'
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

**Problem:** Since Story 3.1 every Registered Function is callable by any Script on its Session with no grant at all, so the Capability model (FR-6) does not exist yet: a Backend cannot say which functions are safe by definition, and the decision is not made where AD-3 says it must be.

**Approach:** A registration carries an optional blanket grant (`context.allow()` in an SDK). `hexput-enforce`, reached only through the Executor, decides every host call from the Session's registrations as they are when the execution is dispatched: blanket-granted → the call proceeds with no authorization round trip; anything else → the same `capability.unknown_function` an unregistered name gets.

## Boundaries & Constraints

**Always:** The grant decision lives in `hexput-enforce::Capabilities::check_call`, fed per execution from `hexput-session`'s single live copy (AD-3, AD-5); neither `hexput-connection` nor `hexput-session` decides. Unregistered and not-granted are indistinguishable to the Script (same code, same message, same span); the Daemon log tells them apart at `debug` with a `reason` field. Grants are per Session and never leak. Every existing and new test states its grants explicitly.

**Never:** No per-call handler or authorization message (Story 3.3), no Config keys, no budgets, no change to the `Call` message or its reply, no revocation or grant change after init (Story 3.8 territory), no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Blanket grant | `getOrder` registered with the grant; `return getOrder(1);` | exactly one `Call` written, no other message; Script gets the Backend's value | N/A |
| No grant | `getOrder` registered without the grant | per open question 2 | per open question 2 |
| Unregistered | `nope(1)` | `capability.unknown_function` on the call, nothing sent | N/A |
| Cross-Session | Session A grants `getOrder`, Session B registers it without the grant (or not at all); B's Script calls it | B's call is `capability.unknown_function`; A's still proceeds | N/A |
| Bad grant value | registration `{name, <grant key>: "yes"}` | `Init` refused with `protocol.invalid_payload` naming `registrations[i].<key>`; no Session | as other init errors |

</frozen-after-approval>

## Open Questions

1. **Wire spelling of the blanket grant in an `Init` registration** — options: (A, recommended) `{name, allow: true}`; `allow` optional, absent or `false` = no blanket grant, any non-boolean refused — mirrors `context.allow()` and leaves room for `key` (Story 3.12) / a handler flag (3.3) beside it / (B) `{name, grant: "blanket" | "per_call"}` — one enum field that 3.3 fills in, but `per_call` would mean nothing until 3.3 ships.
2. **A function registered *without* the grant, until Story 3.3 adds per-call handlers** — options: (A, recommended — fail closed) its calls are denied (`capability.unknown_function`, nothing sent); 3.3 later turns "no grant" into "ask the handler". Existing Backends must add the grant to keep calling. / (B, fail open) it stays callable exactly as in 3.1 until 3.3; the trust boundary is open by default in between.
3. **The AD-3 hardening deferred from Story 3.1** (`hexput_rpc::Caller::call` is public, so `hexput-connection`/`hexput-script` could send a `Call` without `check_call`; a compile-time seal is impossible within the crate graph, since `hexput-rpc` may not depend on `hexput-enforce` or `hexput-exec`) — options: (A, recommended) fold it in: rename the method to one unmistakable name (e.g. `dispatch_authorized`) and make `scripts/check-crate-graph.py` fail CI when that name appears in any production crate but `hexput-exec` and `hexput-rpc` / (B) keep it deferred to Story 3.3 / (C) accept it as a documented convention and close the deferred entry.

## Code Map

- `crates/hexput-session/src/init.rs` -- `RegisteredFunction { name }` (L19) gains the grant; `decode_registrations` (L231) accepts the new optional key (today any key but `name` is "unknown key", L246) and refuses a non-boolean naming `registrations[i].<key>`. Update the Story 3.2 doc note on `RegisteredFunction`.
- `crates/hexput-session/src/lib.rs` -- `registration_names` (L169) → return the registrations with their grants (e.g. `registrations(client_id) -> Option<Vec<RegisteredFunction>>`), still read per execution, never cached.
- `crates/hexput-enforce/src/lib.rs` -- `Capabilities::registered(names)` (L37) → built from `(name, granted)` pairs; `check_call` (L~50) allows only granted names; same diagnostic for unregistered and not-granted; returns the refusal reason for logging.
- `crates/hexput-exec/src/lib.rs` -- `Host::new(registrations, caller)`; log a refused call at `debug` with `reason = "unregistered" | "not_granted"`, never in the Script-visible message.
- `crates/hexput-script/src/lib.rs` (L77), `crates/hexput-connection/src/lib.rs` (L245) -- pass the registrations with grants through.
- `crates/hexput-tests/tests/{session,enforce,exec,connection,script,daemon}.rs` -- the matrix; ~35 existing `registrations` payloads/helpers (e.g. `init_payload`) state grants explicitly.
- Per Q3(A): `crates/hexput-rpc/src/lib.rs` (`Caller::call`), `crates/hexput-exec/src/lib.rs`, `scripts/check-crate-graph.py`, `deferred-work.md` entry closed.
- LANGUAGE-REFERENCE §8, PRD untouched unless Q2 changes documented behaviour; `AGENTS.md` Project Status.

## Tasks & Acceptance

**Execution:**
- [ ] `crates/hexput-session/src/{init.rs,lib.rs}` -- decode and expose the grant.
- [ ] `crates/hexput-enforce/src/lib.rs` -- the grant decision.
- [ ] `crates/hexput-exec`, `crates/hexput-script`, `crates/hexput-connection` -- thread grants through; debug log with reason.
- [ ] per Q3 -- the AD-3 guard.
- [ ] `crates/hexput-tests/tests/*` -- every matrix row; explicit grants in existing tests.
- [ ] `AGENTS.md` -- Project Status.

**Acceptance Criteria:**
- Given a blanket-granted call, when the connection's written envelopes are inspected, then the only envelope for it is the one `Call` — no authorization request.
- Given a refused call, when the Daemon log is read at `debug`, then the event names the function and whether it was unregistered or not granted, while the Script's error is identical in both cases.

## Implementation Notes

## Spec Change Log

## Review Triage Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
