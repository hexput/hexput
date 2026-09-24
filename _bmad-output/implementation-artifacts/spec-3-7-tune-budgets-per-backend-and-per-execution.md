---
title: 'Story 3.7: Tune budgets per backend and per execution'
type: 'feature'
created: '2026-09-24'
status: 'done'
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
- [x] `hexput-shared`, `hexput-port`: setting table, `Settings`, and one decoder.
- [x] `hexput-session`: Config holds `Settings`; read per execution.
- [x] `hexput-enforce`, `hexput-exec`: limits from settings; depth and timeout from limits.
- [x] `hexput-script`, `hexput-connection`: `overrides`; effective limits.
- [x] Tests for every matrix row.
- [x] Docs.

**Acceptance Criteria:**
- Given a Config value, when an execution runs without overrides, then it is the enforced limit.
- Given an override, when its execution ends, then the Session's stored settings are unchanged and the next execution uses them.
- Given any out-of-range or mistyped value in either place, when it is decoded, then it is `protocol.invalid_payload` naming the path, never clamped.

## Implementation Notes

As built, 2026-09-24. No crate edge was added; `scripts/check-crate-graph.py` is unchanged.

- **Setting table.** `Setting::default()` is a `const fn` taking `self`, so `hexput-enforce`'s `DEFAULT_*` constants are now *derived* from the table (`Duration::from_millis(Setting::CpuTimeMs.default())`, …) rather than duplicated. The output ceiling is written in `hexput-shared` as 16 MiB (it cannot see `hexput-port`), and `hexput-port/src/settings.rs` asserts at compile time that it equals `MAX_FRAME_LEN`. `Settings` stores its values as a private `[Option<u64>; 8]` indexed by setting, and adds `Settings::new()` (a `const` empty set) and `effective(setting)` (the value or its default) beside the specified `set`/`get`/`overlay`. `Setting` implements `Display` as its path.
- **Decoder.** `decode_settings` walks the map generically from the table's dotted paths: a key whose relative path is a setting's is decoded as an integer, one that is a proper prefix of some path (`budget`) as a nested map, anything else — including a dotted key such as `"budget.rpc_calls"` at the root — is `` `<prefix>.<path>` is not a known setting ``. The echoed key is bounded to 64 characters; a non-string key is never echoed (`` `config.budget` has a key that is not a string ``). A wrong type reads `found a string` / `a float` / `nil` / `a boolean` / …, a negative integer `found -1`; no float is ever echoed (a float's `Display` is unbounded). A nil *inside* the settings map is refused like any other non-integer; only the `overrides` key itself may be absent or nil.
- **Init refusal wording changed.** The old "`config` accepts no keys yet; found …, and N more" message and its three-key listing are gone: the first bad key alone is named. `tests/session.rs`'s bounded-echo test was adjusted accordingly.
- **Session.** `InitRequest::config()` and `Config::settings()` are public. Besides `Sessions::settings(client_id)`, `Sessions::for_execution(client_id) -> Option<(Vec<RegisteredFunction>, Settings)>` reads both under one shard lock; the connection uses it. `hexput-session` re-exports `Setting`/`Settings`.
- **Script.** `overrides` is decoded after `source` and `variables` are type-checked but before any variable is converted, so a refused override costs no conversion. `direct_execution` now always calls `execute_with_limits`.
- **Exec.** `Limits` also gained `with_argument_depth` / `with_authorization_timeout` builders (for tests, like the other `with_*`). The `depth.argument_too_deep` message prints the limit in force.
- **`protocol.response_too_large` (deferred from 3.6).** The reachable path is the *connection's* frame check, not `hexput-script`'s `check_result`: with `output_size_bytes` at its maximum (`MAX_FRAME_LEN`) the exact `{value}` payload is at most a frame, and `check_result`'s lower bound never exceeds that, so the result always passes it — only the envelope around it can overflow. The e2e test returns a string of `MAX_FRAME_LEN - 12` bytes (a payload of exactly `MAX_FRAME_LEN`), built by binary doubling, under raised `memory_bytes`/`cpu_time_ms` overrides. The test's in-memory `Wired` adapter now refuses an unframable envelope with `InvalidInput`, as a real adapter does. `check_result`'s size refusal remains a defensive guard.
- **Faster timeout tests.** Both the Executor's and the connection's silent-handler tests now run under a 100 ms authorization timeout (the connection's via `config.authorization_timeout_ms`), asserting the elapsed time lies between it and the 5 s default; the default itself is still pinned by assertion.
- **Docs.** LANGUAGE-REFERENCE §7 gained a dated decision with the language-visible limits and ranges (the authorization timeout is wire-only and lives in the Spine amendment). Tests: 593 → 619.

## Spec Change Log

## Review Triage Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
