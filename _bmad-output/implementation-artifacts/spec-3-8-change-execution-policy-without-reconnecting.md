---
title: 'Story 3.8: Change execution policy without reconnecting'
type: 'feature'
created: '2026-09-25'
status: 'in-progress'
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
- [ ] `hexput-shared`: message type.
- [ ] `hexput-session`: decode the payload; `update_config`.
- [ ] `hexput-connection`: route and answer inline.
- [ ] Tests for every matrix row.
- [ ] Spine and AGENTS.md.

**Acceptance Criteria:**
- Given an initialized connection, when it sends a valid `ConfigUpdate`, then the reply is `Result {}` and later executions on any attached Connection run under the new Config.
- Given an invalid `ConfigUpdate`, when it is refused, then the stored Config is byte-for-byte the previous one.
- Given `hexput-script` and `hexput-exec`, when a reviewer reads them, then neither holds settings beyond one dispatch.

## Spec Change Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
