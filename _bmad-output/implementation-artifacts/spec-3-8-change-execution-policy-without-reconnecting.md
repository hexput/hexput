---
title: 'Story 3.8: Change execution policy without reconnecting'
type: 'feature'
created: '2026-09-25'
status: 'done'
baseline_commit: '36293932d6987c337925af2cdf3b2f038c7a595a'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** A Session's Config (Story 3.7's settings) is fixed at `Init`. To change a limit, a Backend must drop its connection, lose its Session and init again. That also loses every other attached Connection's Session.

**Approach:** Add a Backend request, `ConfigUpdate {config}`. It carries a complete Config in exactly `Init.config`'s shape, decoded by the same decoder. It atomically replaces the Session's single live Config (AD-5). Every attached Connection's later executions see the new Config, because each execution already reads the Config afresh when it is dispatched (Story 3.7). Executions already running keep the limits they started with.

## Boundaries & Constraints

**Always:**
- Config lives only in `hexput-session`: one live copy per Session, replaced under its shard lock, never held across an `.await`.
- Nothing else stores the Config. `hexput-script` and `hexput-exec` take the settings per dispatch and never cache them.
- A refused update leaves the stored Config exactly as it was.
- `ConfigUpdate` sits behind the init gate.

**Decisions (agent, 2026-09-25 — Erdem delegated: "kendin tahmin et", no approvals):**
1. **Replace, not patch.** The payload's `config` is the whole new Config. A key it leaves out goes back to the Daemon default. This avoids needing a way to express "unset".
   - `config: {}` resets every setting to its default.
   - A partial change needs the Backend to resend the keys it wants to keep.
2. **Wire.**
   - Request: `MessageType::ConfigUpdate`, payload a map with exactly the key `config`. Absent or nil is missing; an unknown key is refused.
   - Success: `Result {}` (an empty map) under the request's id.
   - Refusals:
     - `protocol.invalid_payload` for a bad payload or a bad setting, naming the path and range (`config.budget.rpc_calls …`), as in Story 3.7;
     - `protocol.init_not_completed` before init.
   - It is handled inline in the connection loop, like `Init`: the update is synchronous and cheap. So a `ConfigUpdate` sent before an `ExecutionStart` on the same connection is always in force for that execution.
3. **In-flight executions** keep the settings read at their dispatch. Only later dispatches see the update.

**Never:**
- no partial or patch semantics;
- no Config versioning or change notifications to other Connections (AD-2: no broadcast);
- no feature toggles or check mode (Stories 3.9 and 3.10 add their keys to the same decoder and so inherit this message);
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Update applies | `Init` with `rpc_calls = 1`; `ConfigUpdate {config: {budget: {rpc_calls: 3}}}` → `Result {}`; Script makes 3 blanket calls | all 3 sent; succeeds |
| Replace semantics | Config `{rpc_calls: 1, argument_depth: 2}`; update `{budget: {rpc_calls: 3}}` | `argument_depth` is back to 12 |
| Invalid update | `ConfigUpdate {config: {budget: {rpc_calls: -1}}}` | `protocol.invalid_payload` naming `config.budget.rpc_calls`; the stored Config is unchanged, and the next execution uses the old limits |
| Bad payload | nil payload / `{}` / `{config: 1}` / `{config: {}, x: 1}` | `protocol.invalid_payload` (missing / not a map / unknown key) |
| Before init | `ConfigUpdate` first | `protocol.init_not_completed`; no Session |
| Several Connections | one Session with two Connections attached (`Sessions::attach`); an update through one | `for_execution` from either sees the new settings |
| In flight | an execution waiting on a host call; an update arrives; the execution then makes more calls | it keeps its original limits; the next execution uses the new ones |
| Override still wins | updated Config plus an `ExecutionStart` `overrides` | the override applies to that execution; the stored Config is unchanged |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/wire.rs`: `MessageType::ConfigUpdate` (doc as for the others; add it to `ALL` and `as_str`). Update any exhaustive test lists in `tests/shared.rs` and `tests/port.rs`.
- `crates/hexput-session/src/init.rs`:
  - add `ConfigUpdate::from_value(&Value) -> Result<Config, InitError>`, or a `Config::from_update_payload`;
  - reuse `decode_config` (prefix `config`) and the `present`/`missing`/`describe_key` helpers.
- `crates/hexput-session/src/lib.rs`: add `Sessions::update_config(client_id, Config) -> bool`, which replaces the Config under the shard lock (`get_mut`) and returns `false` when no such Session exists. Update the crate doc.
- `crates/hexput-connection/src/lib.rs`, `answer()`:
  - after the init gate, route `ConfigUpdate` to an inline `config_update(&request, connection)`, answering `Result {}` or refusing;
  - log "config updated" at `debug` in the request span;
  - an unknown Session (unreachable while attached) is refused like `init_not_completed`;
  - `ExecutionStart` stays the only `Answer::Execute`.
- `scripts/check-crate-graph.py`: nothing, unless the `MessageType` restriction lists need it (they do not; `ConfigUpdate` is unrestricted).
- Tests:
  - `tests/session.rs`: update, replace semantics, refused update leaves settings, multi-attached visibility;
  - `tests/connection.rs`: every matrix row end to end, with the in-flight row driven by a blanket host call the test answers after sending the update;
  - `tests/shared.rs`: the wire spelling.
- Docs:
  - Spine: a dated "[Amended 2026-09-25, Epic 3 Story 3.8]" paragraph after the Story 3.7 amendment (the message and replace semantics);
  - LANGUAGE-REFERENCE: nothing (Script-invisible);
  - AGENTS.md: the Epic 3 paragraph, test count, next step Story 3.9.

## Tasks & Acceptance

**Execution:**
- [x] `hexput-shared`: message type.
- [x] `hexput-session`: decode the payload; `update_config`.
- [x] `hexput-connection`: route and answer inline.
- [x] Tests for every matrix row.
- [x] Spine and AGENTS.md.

**Acceptance Criteria:**
- Given an initialized connection, when it sends a valid `ConfigUpdate`, then the reply is `Result {}` and later executions on any attached Connection run under the new Config.
- Given an invalid `ConfigUpdate`, when it is refused, then the stored Config is byte-for-byte the previous one.
- Given `hexput-script` and `hexput-exec`, when a reviewer reads them, then neither holds settings beyond one dispatch.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | verification-gap | A `ConfigUpdate` pipelined right before an `ExecutionStart` (no await of `Result {}`) is untested; every test awaits the reply first | low | Pre-verified gap; the guarantee is stated in decision 2. **patch** (test) |
| 2 | blind | `an_update_before_init_is_refused_and_creates_no_session` checks `sessions.is_empty()` after `serve` returned, true even had a Session been created | low | Real: detach on exit empties the registry. **patch**: assert no reply carries `client_id` |
| 3 | blind | Unreachable `update_config == false` branch answers "send `Init` first" to an attached connection and logs nothing | low | Real wording defect; direct. **patch**: `error` log, message naming the missing Session |
| 4 | blind | `hexput-port` docs name only `Init`/`ExecutionStart` as users of the settings decoder and sources of `invalid_payload` | low | Real; direct. **patch** |
| 5 | blind | Replace semantics give concurrent updaters last-writer-wins, and a budget-only update will reset Story 3.9/3.10 keys | medium | Real consequence of frozen decision 1 (agent-chosen under Erdem's delegation); kept as decided. **patch** (documented as a known limitation in the Spine amendment) + **defer** (revisit echoing the effective Config or a patch form when 3.9/3.10 add keys) |
| 6 | blind | Spine 2026-09-25 amendment sits between two 2026-09-24 amendments | low | Real; direct. **patch**: moved after the FR-29/FR-30 paragraph |
| 7 | blind | epic-3-context still says the runtime update message is undefined | low | Real; direct. **patch** |
| 8 | blind | `tests/connection.rs` header over 100 columns and omits Story 2.8 | low | Real; direct. **patch** |
| 9 | blind | Session test comment "An update through the second Connection" describes what `update_config(client_id, …)` cannot express | low | Real; direct. **patch** |
| 10 | blind | Multi-Connection criterion checked only on the registry, no second served Connection | low | Rejected: nothing on the wire attaches a second Connection before Epic 5 (reconnect); `for_execution` is what every connection reads at dispatch |
| 11 | blind | Connection-level malformed-payload test misses repeated key, `{config: nil}`, non-map and the unchanged-Config check | low | Rejected: the session tests cover every shape and unchanged settings; the "Invalid update" connection test checks the next execution uses the old limits |
| 12 | blind | `ConfigUpdate` errors reuse the type name `InitError` | low | Rejected: naming only; renaming a public type for one extra caller is more than a direct correction |
| 13 | blind | New log events untested inside the request span | low | Rejected: logged from `answer`, which already runs in the request span Story 2.8's tests pin; a log test adds a subscriber harness for one event |
| 14 | blind | epic-3-context lost "whether a Value Secret counts as an allocation" | low | Real planning gap for Stories 3.11–3.13, not caused by code. **defer** |
| 15 | blind | Sprint status `in-progress` vs spec `in-review` | false | Step 5 syncs the sprint status |
| 16 | blind | Code Map mentions `tests/port.rs` though it iterates `ALL` | — | Rejected: fix edits this build's spec |
| 17 | edge-case | (no findings) | — | — |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
