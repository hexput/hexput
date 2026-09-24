---
title: 'Story 3.7: Tune budgets per backend and per execution'
type: 'feature'
created: '2026-09-24'
status: 'in-progress'
baseline_commit: '3d54919'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Every Resource Budget limit, the argument depth limit and the per-call handler's timeout are compile-time constants (Stories 3.1, 3.3, 3.5, 3.6). A Backend cannot give an expensive report more room than a cheap rule check, and an `Init` whose `config` names any key is refused because nothing defined one.

**Approach:** Define the first Config keys: the six budget limits, the argument depth and the authorization timeout. The Session holds them as its live Config (AD-5). An `ExecutionStart` may carry `overrides` of the same shape for that execution alone. The effective limits are the Daemon default, overlaid by Config, overlaid by the override. Every value is range-checked when it is decoded, and one outside the Daemon's allowed range is refused as `protocol.invalid_payload` naming its path and range, never clamped. The Executor receives the effective limits and enforces them as it does today.

## Boundaries & Constraints

**Always:**

- Config lives only in `hexput-session` and is read afresh for each execution, never snapshotted across executions (AD-5).
- An override changes nothing stored.
- The limits reach enforcement only through `hexput-exec` → `hexput-enforce` (AD-3).
- One decoder serves both `Init.config` and `ExecutionStart.overrides`, so the two can never disagree on a key, type or range.
- Range validation lives with the setting table in `hexput-shared::budget`. `Settings` can only be built through its validating setter.

**Decisions (agent, 2026-09-24 — Erdem delegated: "kendin tahmin et"):**

1. **Wire shape.** Both `Init.config` and `ExecutionStart.overrides` (optional; absent or nil means none) are maps. Every key is optional:
   ```
   { budget: { cpu_time_ms, memory_bytes, allocations, rpc_calls, output_size_bytes, side_effects },
     argument_depth, authorization_timeout_ms }
   ```
   - Values are MessagePack integers. A float, even a whole one, is refused.
   - Unknown, repeated or non-string keys are refused.
   - `config: {}` stays valid.
   - Stories 3.9 and 3.10 add `features` and `check` beside these keys.
2. **Allowed ranges** (inclusive; documented constants, the Daemon's ceiling):

   | Key | Allowed range | Default |
   |---|---|---|
   | `cpu_time_ms` | 1..=60 000 | 1 000 |
   | `memory_bytes` | 1 024..=1 GiB | 64 MiB |
   | `allocations` | 0..=100 000 000 | 1 000 000 |
   | `rpc_calls` | 0..=100 000 | 100 |
   | `output_size_bytes` | 1..=`MAX_FRAME_LEN` (16 MiB) | 1 MiB |
   | `side_effects` | 0..=100 000 | 100 |
   | `argument_depth` | 1..=64 | 12 |
   | `authorization_timeout_ms` | 1..=60 000 | 5 000 |

   A zero is meaningful only for the counted dimensions ("no host calls at all").
3. **Error.** A bad value is refused with `protocol.invalid_payload`, and the message names the path and the range. For example:
   - `` `config.budget.rpc_calls` must be an integer from 0 to 100000; found 200000 ``
   - `` `overrides.argument_depth` … ``

   A refused `Init` creates no Session. A refused `ExecutionStart` runs nothing.

**Never:**
- no runtime Config update (Story 3.8);
- no feature toggles (Story 3.9);
- no check mode (Story 3.10);
- no System Config key for the ranges;
- no clamping;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Config applies | `config.budget.rpc_calls = 2`; Script makes 3 blanket calls | two `Call`s; the third is `budget.rpc_calls_exceeded`, unsent |
| Override for one execution | same Session; one `ExecutionStart` with `overrides.budget.rpc_calls = 5` | that execution makes 5 calls; the next, with no override, is back to 2 |
| Override out of range | `overrides.budget.cpu_time_ms = 0` | `protocol.invalid_payload` naming `overrides.budget.cpu_time_ms` and its range; nothing runs |
| Config out of range | `Init` with `config.budget.memory_bytes = 2^40` | `protocol.invalid_payload`; no Session |
| Wrong type / unknown key | `config.budget.rpc_calls = "5"`, `1.0`, `-1`; `config.budget.cpu = 1`; `config.speed = 1` | refused naming the path / key |
| Argument depth | `config.argument_depth = 2`; an argument nested 3 deep | `depth.argument_too_deep`, nothing sent; an override can raise it for one execution |
| Authorization timeout | `config.authorization_timeout_ms = 100`; silent handler | denied after ~100 ms (`handler_timeout`) |
| Output above default | `overrides.budget.output_size_bytes = MAX_FRAME_LEN`; result payload ~MAX_FRAME_LEN bytes | passes the budget; `protocol.response_too_large` from the frame check (deferred from Story 3.6) |
| Defaults | `config: {}` | every limit is today's constant |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/budget.rs`:
  - `Setting` enum (eight settings; `ALL`, `path()` such as `budget.rpc_calls`, `min()`, `max()`, `default()`);
  - `Settings` (all `Option<u64>`, private fields; `set(Setting, u64) -> Result<(), OutOfRange>`, `get(Setting)`, `overlay(&self, &Settings) -> Settings`);
  - `OutOfRange` (Display with the range).
- `crates/hexput-port/src/settings.rs` (new): `decode_settings(&Value, prefix: &str) -> Result<Settings, String>` walks the map and names the path in every refusal. Re-exports `Settings`/`Setting`.
- `crates/hexput-session/src/init.rs`:
  - `Config { settings }` decoded via `decode_settings(value, "config")`;
  - `Config::settings()`;
  - `Sessions::settings(client_id)` (or one read returning registrations and settings together).
- `crates/hexput-enforce/src/lib.rs`:
  - `Limits` gains `argument_depth` and `authorization_timeout`, with `DEFAULT_ARGUMENT_DEPTH` 12 and `DEFAULT_AUTHORIZATION_TIMEOUT` 5 s;
  - `Limits::from_settings(&Settings)` (defaults for unset).
- `crates/hexput-exec/src/lib.rs`:
  - use `limits.argument_depth()` and `limits.authorization_timeout()` instead of the constants;
  - keep `ARGUMENT_DEPTH_LIMIT` and `AUTHORIZATION_TIMEOUT` as the documented defaults (aliases).
- `crates/hexput-script/src/lib.rs`:
  - decode the optional `overrides`;
  - `direct_execution(payload, registrations, settings, caller)` overlays the Session's settings with the overrides and calls `execute_with_limits`.
- `crates/hexput-connection/src/lib.rs`: reads the Session's settings with its registrations for each execution.
- Tests (`crates/hexput-tests/tests/{shared,port,session,enforce,exec,script,connection}.rs`): every matrix row. Existing tests that assert "`config` accepts no keys" change to assert unknown keys.
- Docs:
  - LANGUAGE-REFERENCE §7 `budget`/`depth` rows (limits configurable);
  - the Spine wire-contract amendment (Config keys and `overrides`);
  - AGENTS.md;
  - `deferred-work.md` (resolve the `response_too_large` entry).

## Tasks & Acceptance

**Execution:**
- [ ] `hexput-shared`, `hexput-port`: setting table, `Settings`, and one decoder.
- [ ] `hexput-session`: Config holds `Settings`; read per execution.
- [ ] `hexput-enforce`, `hexput-exec`: limits from settings; depth and timeout from limits.
- [ ] `hexput-script`, `hexput-connection`: `overrides`; effective limits.
- [ ] Tests for every matrix row.
- [ ] Docs.

**Acceptance Criteria:**
- Given a Config value, when an execution runs without overrides, then it is the enforced limit.
- Given an override, when its execution ends, then the Session's stored settings are unchanged and the next execution uses them.
- Given any out-of-range or mistyped value in either place, when it is decoded, then it is `protocol.invalid_payload` naming the path, never clamped.

## Spec Change Log

## Review Triage Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
