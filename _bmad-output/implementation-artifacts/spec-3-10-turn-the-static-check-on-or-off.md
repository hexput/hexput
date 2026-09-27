---
title: 'Story 3.10: Turn the static check on or off'
type: 'feature'
created: '2026-09-27'
status: 'done'
baseline_commit: '48393cd9af945a44a0876c0438a57e37c387326f'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** The static check pass (`hexput-check`, Story 1.10) only runs from the CLI. A Backend cannot have its submitted Scripts checked by the Daemon.
- A broken rule is found only mid-run, possibly after host calls have already been made.
- A call to an unregistered name surfaces late, as a `capability` error.

**Approach:** Add a `check` mode to the Config shape: `off` (default), `warn` or `error`. It is decoded by the one settings decoder, so it works in `Init.config`, `ExecutionStart.overrides` and `ConfigUpdate`.
- `hexput-script` is the one Direct Execution site that runs the pass (AD-8). It runs after parsing and before the Executor, with:
  - the starting-variable names;
  - the Session's registration names as the callable list;
  - a `hexput-check` `Policy` built from the effective feature toggles.
- `error` rejects a Script that has any error-severity finding before anything runs. `warn` runs the Script and returns the findings with the result. `off` runs no pass.

## Boundaries & Constraints

**Always:**
- The check grants nothing and charges nothing. Enforcement stays in `hexput-enforce` through the Executor (AD-3, AD-8), and `hexput-check` stays `hexput-ast`-only.
- An override applies to one execution only. The Config remains a single live copy (AD-5).
- `hexput check` (CLI) is unchanged.

**Decisions (agent, 2026-09-27 — Erdem delegated: "kendin tahmin et", no approvals):**
1. **Wire value.** `check` is a root key holding a string: `"off"`, `"warn"` or `"error"`.
   - Anything else is refused with `` `config.check` must be one of "off", "warn", "error"; found … `` (the found value is bounded; a non-string is named by its type).
   - `ConfigUpdate`'s replace semantics apply: an update without `check` is back to `off`.
2. **Findings on the wire.** A finding travels in the same shape as the one error body (`ErrorBody`: severity, category, code, message, span).
   - **Successful execution:** when the pass ran (`warn` or `error`) and produced at least one finding, the `Result` payload is `{value, findings: [ … ]}`. The findings are in source order; under `error` only warnings can be left. With no findings, the payload stays exactly `{value}`.
   - **Rejection under `error`:** the `Error` payload is the first error-severity finding's body, plus `findings`, which lists every finding, warnings included. Nothing runs and no host call is made.
3. **Callable list.** The list holds every registered name, whether or not it has a blanket grant: a per-call function is still callable. So an unregistered call is a `capability.unknown_function` finding.
4. **Policy from toggles.** `hexput-check` gains `Policy::from_features(Features)` (it reaches `Features` through `hexput-ast`'s re-export). A disabled construct is reported as `policy.construct_disabled`, with the same message and span the runtime uses (Story 3.9). This resolves the deferred wording/span mismatch; the CLI's all-enabled `Policy` still produces no policy findings.
5. **Cost.** The pass runs on the blocking pool with decoding and parsing, and it is not charged to the Script's CPU budget, because it is not Script code. Cached Execution (Epic 4) will run it once at registration. That is recorded only; no code for it now.

**Never:**
- no check on any path but Direct Execution;
- no new finding rules;
- no caching of findings;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Default off | `config: {}`; a Script with an undeclared read on a path never taken | runs, `{value}` only; no pass |
| Error rejects | `check = "error"`; `f(1); return x;` (`x` undeclared, `f` blanket) | `Error` with `reference.undeclared_identifier` plus `findings`; no `Call` sent |
| Unregistered call | `check = "error"`; `g(1)`, `g` not registered | a `capability.unknown_function` finding at submission; nothing sent |
| Warn attaches | `check = "warn"`; `let unused = 1; return 2;` | `{value: 2, findings: [warning]}` |
| Error mode, warnings only | `check = "error"`; `let unused = 1; return 2;` | runs; `{value: 2, findings: [warning]}` |
| Toggle as finding | `check = "error"`, `features.loops = false`; a `while` on an untaken path | rejected at submission with `policy.construct_disabled`, same message and span as the runtime's |
| Override | Config `off`; one `ExecutionStart` with `overrides.check = "error"` | only that execution is checked; the stored mode is still `off` |
| Bad value | `check = "strict"` / `1` / `true` | `protocol.invalid_payload` naming `config.check` (or `overrides.check`) |
| Starting variables | `check = "error"`; the Script reads a supplied starting variable | no finding for it |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/policy.rs`: add `CheckMode {Off, Warn, Error}`, with `ALL`, `as_str`, `from_name`, `Display`, and default `Off`.
- `crates/hexput-shared/src/budget.rs`: `Settings` gains `check: Option<CheckMode>`, with `set_check`, `check()` (unset means `Off`) and overlay.
- `crates/hexput-port/src/settings.rs`: decode the root `check` string (decision 1). Re-export `CheckMode`.
- `crates/hexput-port/src/error.rs`: `ErrorBody` gains `findings: Vec<ErrorBody>` (serde default and skip-if-empty). `to_value` appends `findings` only when non-empty. Update the constructors and literal sites so every existing body has none.
- `crates/hexput-ast/src/lib.rs`: re-export `CheckMode` beside `Feature`/`Features` if `hexput-check` needs it; it may not, since the mode is `hexput-script`'s to read.
- `crates/hexput-check`:
  - add `Policy::from_features(Features)`;
  - align the disabled-construct finding's message and span with the interpreter's (read `hexput-interpreter`'s refusal sites for the exact spans and wording) and update `tests/check.rs` expectations accordingly;
  - no other rule changes.
- `crates/hexput-script/src/lib.rs`:
  - after `prepare` parses, when the effective settings' `check()` is not `Off`, build the `Environment` (starting-variable names, registration names) and `Policy::from_features(effective.features())` and run `hexput_check::check`, on the blocking pool;
  - under `Error` with error findings, return the rejection (decision 2) and never call the Executor;
  - otherwise carry the findings to `reply`, which adds `findings` when non-empty;
  - update the crate docs ("Not yet: the static check" goes away).
- `crates/hexput-connection`: nothing, unless a doc names the payload shape.
- Tests:
  - `tests/shared.rs`: `CheckMode`;
  - `tests/port.rs`: decode and refusals; `ErrorBody` with and without findings;
  - `tests/check.rs`: `from_features`, aligned wording and spans;
  - `tests/script.rs` and/or `tests/connection.rs`: every matrix row end to end, with "no `Call` sent" asserted over the wire.
- Docs:
  - LANGUAGE-REFERENCE §10 (the check section): a dated decision recording the mode key, the wire shapes and that the callable list comes from registrations;
  - the Spine: a dated amendment for `ErrorBody.findings` and the `check` key;
  - AGENTS.md: the Epic 3 paragraph, test count, next step Story 3.11;
  - `deferred-work.md`: mark the Story 3.9 check/runtime alignment entry resolved.

## Tasks & Acceptance

**Execution:**
- [x] `hexput-shared`, `hexput-port`: `CheckMode`, the `Settings` field, the decoder, and `ErrorBody.findings`.
- [x] `hexput-check`: `Policy::from_features`; aligned policy findings.
- [x] `hexput-script`: run the pass per mode; reject or attach.
- [x] Tests for every matrix row.
- [x] Docs.

**Acceptance Criteria:**
- Given mode `error` and an error finding, when the Script is submitted, then the Backend receives the findings and no statement runs and no host call is sent.
- Given mode `warn`, when the Script has findings, then it runs and the `Result` carries `findings` beside `value`.
- Given mode `off` (default), when a Script is submitted, then `hexput_check::check` is never called and the reply is exactly `{value}` or the runtime error.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | blind + edge-case + verification-gap | Findings from `warn` (or `error` with warnings only) are dropped when the Script then fails at runtime | medium | Real: only the success `reply` attached them. Decision 2 was silent on failures; the one reading that serves the Backend is to always return findings once the pass ran. **patch** |
| 2 | blind + edge-case | `findings` is unbounded: a success or rejection reply can pass a frame and be replaced by `protocol.response_too_large`, losing the result | medium | Real. **patch**: at most `MAX_FINDINGS` (100), dropped entirely if the reply still would not fit |
| 3 | blind | `findings` sits outside the output-size budget, and docs call that budget the exact payload length | low | Real; bounded by #2. **patch** (docs) |
| 4 | blind | An `error`-mode rejection is not logged | low | Real; direct. **patch** (`debug` log) |
| 5 | blind | The pass runs with no time bound before any `Budget` exists | low | Real; linear in a source bounded by one frame. **defer** |
| 6 | blind + verification-gap | Untested: bad `check` in `ConfigUpdate`, no `Authorize` on rejection, `warn` override over `error`, `rpc_calls` disabled for a registered name under `error`, non-UTF-8 `check` message | low | Pre-verified / real; cheap. **patch** (tests) |
| 7 | blind | `ErrorBody.findings` is recursive, and a rejection's top body is duplicated in `findings` | low | Design is intentional (one error shape); nested findings are never produced. **patch** (doc) |
| 8 | blind | Edited doc comment in `hexput-session/src/init.rs` runs past 100 columns | low | Real; direct. **patch** |
| 9 | blind + edge-case | Under `error`, a disabled construct on an untaken path rejects, though the runtime would not; only the 3.10 decision text implies it | low | Intended (the check reports anywhere in the source); undocumented. **patch** (LANGUAGE-REFERENCE) |
| 10 | blind + verification-gap | Sprint status `in-progress` vs spec `in-review` | false | Step 5 syncs the sprint status |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
