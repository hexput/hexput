---
title: 'Story 4.2: Re-run a registered script with fresh variables'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '931107ad6c49219d0c4c3f84234d78e04a9e741b'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-4-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-1-register-a-script-once-for-repeated-use.md'
warnings: ['oversized']
deferred:
  - summary: >-
      Pin with a test that a Cached Execution which already fetched its `Arc<Program>` finishes safely when its AST Cache entry is removed meanwhile.
    evidence: |-
      The spec's Always list and `cached_execution`'s doc promise it (the lookup clones the `Arc`), but `AstCache` has no removal API until Story 4.3, so no test can exercise it yet. Story 4.3 adds eviction, unregister and teardown and should add this test with them.
    location: >-
      crates/hexput-script/src/lib.rs (AstCache::get, cached_execution)
    severity: low
---

<intent-contract>

## Intent

**Problem:** Story 4.1 lets a Backend register a Script and get a handle, but nothing can run it. A rule evaluated on every request still costs a parse per request (FR-5, UJ-2).

**Approach:** Add a `CachedExecutionStart {handle, variables, overrides?}` request. The Daemon looks the handle up in the `AstCache` under the caller's Client ID, binds fresh starting variables, and runs the cached `Arc<Program>` through exactly the path a Direct Execution takes after parsing: the same `hexput_exec::execute_held`, `Host`, `Limits` and reply conversion. Nothing is parsed and the static check does not run. An unknown handle is a new, defined error that tells the Backend to register again.

## Boundaries & Constraints

**Always:**
- One Executor (AD-3). Cached and Direct Execution share one private run-and-reply helper in `hexput-script`, factored out of `direct_execution`'s tail: `Limits::from_settings(&effective)`, `Host::new(registrations, caller)`, `execute_held`, and `reply`. Cached Execution adds no capability, budget or toggle logic of its own.
- Each execution gets fresh state. The cached `Program` is immutable and shared through an `Arc`, and every run builds its own heap, scopes and budget. Nothing a run writes (starting variables, top-level `let`s, collections) is visible to another run.
- Settings are read live (AD-5). The registrations, grants and Config settings are read from `Sessions::for_execution` at dispatch, exactly as for `ExecutionStart`, and then overlaid with the payload's `overrides`. Nothing is taken from registration time.
- The task is dispatched like an execution. It is its own task in the connection's `JoinSet`, instrumented with the request span, with `caller.for_execution(id)` (so a `Call`'s `execution` is this request's id). Its reply is written in completion order. It sits behind the init gate.
- The lookup is scoped by the connection's Client ID. A handle that another Session registered gets the same error as a handle never issued, so the error does not reveal whether that handle exists.
- `AstCache::get` hands out a clone of the `Arc`. Story 4.3 may remove an entry while a run that already fetched it continues safely.

**Decisions (agent, 2026-10-22, unattended run; Erdem delegated these: "kendin tahmin et"):**
1. **Wire.** The request is `MessageType::CachedExecutionStart`, placed after `CodeRegister`. Its payload is a map with these keys:
   - `handle`: the 16-lowercase-hex `ScriptHandle` text.
   - `variables`: a name→value map, decoded exactly as `ExecutionStart`'s, including holders. `{}` is valid.
   - `overrides`: optional. It has the same shape and decoder as `ExecutionStart`'s.

   Absent or nil is missing, and when `handle` and `variables` are both missing, both are named together. The reply is the same as a Direct Execution's: `Result {value, modifications?}`, or the `Error`. A Cached Execution never carries `findings`.

   Every malformed payload is `protocol.invalid_payload` and names the key:
   - an unknown, repeated or non-string key, including `source` (there is nothing to parse);
   - a `handle` that is not a string;
   - a `handle` that does not parse as a `ScriptHandle`. The message echoes it bounded (`wire::bounded`).
2. **Unknown handle.** A new code, `ProtocolCode::UnknownScriptHandle`, spelled `protocol.unknown_script_handle`, is appended to `ProtocolCode::ALL`. It is answered for a well-formed handle that has no entry under this Client ID: never issued, another Session's, or (after Story 4.3) evicted or unregistered. The message names the handle and says to register the Script again with `CodeRegister`. Nothing runs and nothing is parsed. It belongs to the wire-only `protocol` category because it concerns the request, not the Script (no span). The payload is decoded fully before the lookup, so `invalid_payload` takes precedence over `unknown_script_handle`.
3. **No static check at Cached Execution (AD-8).** The pass ran at `CodeRegister`.
   - `overrides.check` on a `CachedExecutionStart` is refused as `protocol.invalid_payload` naming `` `overrides.check` ``, with a message saying the static check runs at registration (`CodeRegister`). Silently ignoring a requested check would mislead the Backend.
   - The Session's live `check` mode is likewise not consulted.
   - Feature toggles (live Config plus `overrides.features`) are still enforced by the interpreter at runtime, as on Direct Execution.
4. **Registered `variables` names (4.1 deferral).** They are neither stored nor enforced. They existed only so the check would know which inputs exist. At runtime a missing input fails as on Direct Execution (`reference.undeclared_identifier`), and an extra one simply binds (a top-level redeclaration is `syntax.duplicate_declaration`). Enforcing the list would add a second contract that the check does not need.
5. **Logging.** A failure is logged at `debug` as "a Cached Execution failed", with `code`, in the request span. An unknown handle is one such failure.

**Never:**
- no parse and no `hexput-check` call on this path;
- no capability, budget or toggle check of its own;
- no fallback to re-parsing or to another Session's entry;
- no cache insert or removal (capacity, eviction, unregister and teardown belong to Story 4.3);
- no new crate edge;
- no `unsafe`;
- no lock held across an `.await`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Run | `h` registered `return grade >= 50;`; `{handle: h, variables: {grade: 72}}` | `Result {value: true}`; entry count unchanged |
| Fresh variables | the same `h`, then `{grade: 12}` | `Result {value: false}` |
| No bleed (mutation) | `o.n = o.n + 1; return o.n;` run twice with `{o: {n: 1}}` | `2` both times |
| No bleed (top-level state) | `let c = 0; c = c + x; return c;`, `x: 5` twice | `5` both times |
| Same as Direct | the same source and variables as `ExecutionStart` | byte-identical `Result` payload |
| Unknown handle | `h` never issued (e.g. `"00000000000000ff"`) | `protocol.unknown_script_handle`, naming it, saying to re-register |
| Other Session | Session B sends A's `h` | `protocol.unknown_script_handle`, the same message shape |
| Malformed | nil / `{}` / `{handle: 1, …}` / `{handle: "ABC", …}` / `{handle: h}` / `{…, source: "1;"}` / `{…, overrides: {check: "warn"}}` | `protocol.invalid_payload` naming `handle` / `variables` / the key / `` `overrides.check` `` |
| Before init | `CachedExecutionStart` first | `protocol.init_not_completed` |
| No check | registered under `warn` with findings; then run | the registration had `findings`; the run's `Result` is exactly `{value}` |
| Live Config | `ConfigUpdate` sets `budget.rpc_calls: 0` (or `features.loops: false`) after registration; the Script calls a function (or loops) | `budget.rpc_calls_exceeded` (or `policy.construct_disabled`) |
| Overrides | `overrides: {budget: {cpu_time_ms: …}}` / `features` | applied for this run alone |
| Host call | blanket-granted `f` called; unregistered `g` called | a `Call` with `execution` equal to this request id; `capability.unknown_function` for `g` |
| Value Secret | a holder variable written by the Script | `Result {value, modifications}`, as on Direct Execution |
| Concurrency | a slow and then a fast run of handles on one connection | the fast reply comes first; both are correct |

</intent-contract>

## Code Map

- `crates/hexput-script/src/lib.rs`:
  - `direct_execution` (lines 167-216): factor the part after the check into a shared private async helper, for example `run(program, variables, effective, findings, registrations, caller)`. Direct Execution's behaviour stays byte-identical.
  - `decode` (lines 680-764): generalise it so it decodes `variables` and `overrides` for both payloads, parameterised by the payload name and the first key (`source` or `handle`). `missing()` hard-codes "ExecutionStart", so make it take the payload name too.
  - `AstCache::get` (line 244).
  - `invalid_payload`, `blocking`, `present`, `reply`.
  - Add `pub async fn cached_execution(payload: Value, client_id: ClientId, cache: Arc<AstCache>, registrations: Vec<Registration>, settings: Settings, caller: Caller) -> Result<Value, Box<ErrorBody>>`, documented like `direct_execution`, with its `# Errors` list.
  - Rewrite the crate doc's "Not yet" paragraph (lines 70-71) and add a Cached Execution paragraph.
- `crates/hexput-shared/src/ids.rs`: `ScriptHandle`'s `FromStr` and `ParseScriptHandleError` are reused for `handle`.
- `crates/hexput-shared/src/wire.rs:50-115`:
  - Add `CachedExecutionStart` after `CodeRegister` in the enum, in `ALL` and in `as_str`, with a doc comment.
  - Drop it from the `#[non_exhaustive]` "later epics add" example list.
- `crates/hexput-port/src/error.rs:18-88`:
  - Add `UnknownScriptHandle` to `ProtocolCode`, to `ALL` (at the end) and to `as_str`.
  - Extend `InvalidPayload`'s doc so it names `CodeRegister` and `CachedExecutionStart`.
- `crates/hexput-shared/src/budget.rs:207,287`: `Settings` (re-exported by `hexput-port`); `check_setting()` is `Some` exactly when `overrides.check` was given, which is how decision 3 detects it.
- `crates/hexput-connection/src/lib.rs`:
  - `answer` (line 480): add the type to the arm at line 484 and route it past the gate as `Answer::CachedExecute(id, payload, client_id)`.
  - `exchange` (lines 276-306): spawn like `Answer::Execute`, together with `Arc::clone(connection.cache)`.
  - Add an `async fn cached_execute` modelled on `execute` (line 574).
  - Update the module doc's list and the Concurrency paragraph.
- `crates/hexput-tests/tests/script.rs:1024-1062`: the `registered`/`registration` helpers, to reuse when building a cached run.
- `crates/hexput-tests/tests/connection.rs`:
  - `every_request_before_init_is_refused` (line 306);
  - `run_timed_with_cache` (line 142);
  - `slow` (line 760);
  - the Story 4.1 registration tests.
- `crates/hexput-tests/tests/port.rs:460-480`: the `ProtocolCode::ALL` spelling list.
- `crates/hexput-tests/tests/shared.rs`: the `MessageType::ALL` spelling list (around lines 254-290).
- `crates/hexput-tests/tests/daemon.rs`: `a_registration_over_the_socket_is_answered_with_a_handle_from_one_counter_per_run` (line 1392), which is the template for the new socket test.
- `scripts/check-crate-graph.py`: unchanged. Run it to confirm.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/wire.rs`, `crates/hexput-port/src/error.rs`: add `MessageType::CachedExecutionStart` and `ProtocolCode::UnknownScriptHandle`. This is the wire vocabulary.
- `crates/hexput-script/src/lib.rs`: add `cached_execution`:
  - decode on the blocking pool, look up `(client_id, handle)` and execute the shared helper (with no findings);
  - share the decoder and the run-and-reply helper with `direct_execution`;
  - update the docs.

  This is FR-5 under AD-3 and AD-8.
- `crates/hexput-connection/src/lib.rs`: route, spawn and log the Cached Execution, and update the docs. This dispatches it without blocking the loop (AD-6).
- `crates/hexput-tests/tests/{script,connection,port,shared,daemon}.rs`: cover:
  - every row of the I/O matrix;
  - the spellings of the new type and the new code;
  - a socket round trip in `daemon.rs`: register, then run twice with different variables;
  - a Direct Execution never consulting the cache still holds (the existing test).
- `ARCHITECTURE-SPINE.md` and `.memlog.md` (in `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/`): add a dated `[Amended 2026-10-22, Epic 4 Story 4.2]` paragraph that fixes the `CachedExecutionStart` wire, `protocol.unknown_script_handle`, the refusal of `overrides.check`, and the decision not to enforce registered variable names. Update the frontmatter `amended`. SDKs (Epic 8) depend on it.
- `AGENTS.md`: add Story 4.2 to the Project Status Epic 4 paragraph, update the test count, and update the "Next step" line.

**Acceptance Criteria:**
- Given a registered handle, when a `CachedExecutionStart` with variables is submitted, then the cached AST runs without any parse and the result returns as `Result {value}`.
- Given two Cached Executions of one handle with different (or mutated) variables, when both complete, then each result depends only on its own variables, with no state bleeding between them.
- Given a Cached Execution, when it runs, then it passes through the same Executor, capability decisions, Resource Budget and runtime feature toggles as a Direct Execution, under the Session's live Config and the request's overrides.
- Given an unknown, foreign or (later) evicted handle, when it is submitted, then `protocol.unknown_script_handle` tells the Backend to re-register, and nothing is parsed or run.
- Given the CI commands, when they run, then all pass, and `check-crate-graph.py` passes with no edge added.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/script.rs` (a_registered_script_runs_with_fresh_variables_and_its_entry_is_untouched, a_run_never_parses_so_a_registered_entry_is_the_one_it_runs, a_mutated_object_does_not_bleed_into_the_next_run, top_level_state_does_not_bleed_into_the_next_run, a_cached_run_replies_with_the_same_bytes_as_a_direct_execution, an_unknown_handle_is_answered_with_the_re_register_error_and_nothing_runs, another_sessions_handle_gets_the_same_error_shape_as_a_handle_never_issued, a_malformed_payload_is_refused_naming_the_key_and_runs_nothing, an_invalid_payload_takes_precedence_over_an_unknown_handle, an_overlong_handle_is_echoed_bounded_in_its_refusal, a_registration_with_findings_runs_with_a_reply_of_exactly_value, the_check_is_not_run_at_a_cached_execution_whatever_the_live_mode, registered_variable_names_are_neither_enforced_nor_stored, a_starting_variable_the_script_redeclares_is_a_duplicate_declaration, a_live_config_limit_applies_to_a_cached_run, a_live_feature_toggle_refuses_a_construct_at_a_cached_run, overrides_apply_to_one_run_alone, a_holder_the_script_writes_comes_back_as_a_modification_as_on_direct_execution, a_blanket_granted_call_is_made_and_an_unregistered_one_is_refused), `crates/hexput-tests/tests/connection.rs` (a_cached_run_is_answered_with_its_value_and_its_entry_is_reused, a_cached_run_before_init_is_refused_as_init_not_completed, a_cached_run_of_an_unknown_handle_names_it_and_says_to_register_again, a_handle_another_session_registered_is_unknown_here, a_malformed_cached_run_is_answered_as_invalid_payload_and_the_connection_keeps_serving, a_fast_cached_run_is_answered_before_a_slow_one_submitted_earlier, a_cached_run_under_a_config_update_sees_the_live_budget_and_toggles, a_cached_run_is_checked_by_registration_only_and_an_override_check_is_refused, a_cached_run_makes_its_call_with_its_own_request_id_as_the_execution, a_failed_cached_run_is_logged_at_debug_under_its_request), `crates/hexput-tests/tests/daemon.rs` (a_cached_execution_over_the_socket_runs_the_registered_script_with_fresh_variables), and the spelling lists in `crates/hexput-tests/tests/port.rs` (`protocol.unknown_script_handle`) and `crates/hexput-tests/tests/shared.rs` (`CachedExecutionStart`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 19 findings — high 0, medium 1, low 15, false 3, maybe-false 0
- findings:
  - `[low]` `[patch]` (blind) The story's status markers disagree: sprint-status `ready-for-dev`, the Auto Run Result "halted after planning", the code and AGENTS.md implemented — resolved by finalization: sprint-status set to `review`, spec `done`, Auto Run Result rewritten to describe the build (grouped with the intent-alignment status row).
  - `[medium]` `[patch]` (blind) `epic-4-context.md` lacks Story 4.2's contract, which Story 4.3 compiles from — added a Technical Decisions bullet for the `CachedExecutionStart` wire, `unknown_script_handle` vs `invalid_payload`, the refused `overrides.check`, the shared helper and the log, plus one for the cloned-`Arc` guarantee and its deferred test.
  - `[low]` `[patch]` (blind) Every `overrides.check`, `"off"` included, is refused and only `"warn"` was tested — kept the refusal (decision 3: no value of `check` means anything on this request), pinned `"off"` in `a_malformed_payload_is_refused_naming_the_key_and_runs_nothing`, and the Spine amendment now says "whatever its value (`"off"` included)".
  - `[low]` `[patch]` (blind) No test pins strict handle parsing — added uppercase (`00000000000000FF`), space-padded and 17-character handles to the malformed cases, each `invalid_payload` naming `handle` (`ScriptHandle::from_str` accepts only 16 lowercase hex).
  - `[low]` `[patch]` (blind) Handle `0000000000000000` untested — added `handle_zero_is_well_formed_but_never_issued_so_it_is_unknown` (`unknown_script_handle`, naming it); the epic context records it.
  - `[low]` `[patch]` (blind) The failure log does not carry the handle — `hexput-connection`'s `cached_execute` reads the payload's handle (only when it parses as a `ScriptHandle`) before the payload moves and logs it as `handle`; `a_failed_cached_run_is_logged_at_debug_under_its_request` asserts it; module doc updated.
  - `[low]` `[patch]` (blind) `ProtocolCode::InvalidPayload`'s doc line overruns the 100-column wrap — paragraph re-flowed.
  - `[low]` `[patch]` (blind) The `hexput-connection` module doc's `CachedExecutionStart` bullet omits `modifications?` — now `Result { value, modifications? }`.
  - `[low]` `[patch]` (blind) `overrides_apply_to_one_run_alone` skips the matrix's `cpu_time_ms` case — added a registered bounded loop run under `budget.cpu_time_ms: 1` (`budget.cpu_time_exceeded`) and then without it (completes) (grouped with the intent-alignment CPU row).
  - `[low]` `[patch]` (blind) The daemon socket test reads the handle by position — it now finds the `handle` key.
  - `[false]` `[reject]` (blind) Nothing checks Direct Execution still accepts `overrides.check` after the `decode` refactor — `script.rs` already runs `ExecutionStart` with `overrides: {check: "error"}` (rejected with findings) and `{check: "off"}` (runs), around lines 697-733, and the connection tests `a_warn_override_over_an_error_config_runs_a_script_with_an_error_finding` cover it on the wire; an inverted branch fails them.
  - `[low]` `[defer]` (blind) No test pins that a run which fetched its `Arc<Program>` survives the entry's removal — removal is Story 4.3's (this story's intent: "no cache insert or removal"); recorded in `deferred` for 4.3.
  - `[low]` `[patch]` (verification-gap) Refusal messages are never checked for the payload type they name — the cached malformed cases now require "the `CachedExecutionStart` payload" (nil, `{}`, missing `variables`, unknown `source`/`x`, repeated `handle`), and the direct malformed test requires "the `ExecutionStart` payload is missing `source` and `variables`".
  - `[false]` `[reject]` (intent-alignment) The diff implements past "halt after planning" — that phrase governed the earlier planning stage of the orchestrated run only; the captured intent (`<intent-contract>`) is the full Story 4.2, and the orchestrator continued the run into implementation and review.
  - `[low]` `[patch]` (intent-alignment) The status markers contradict one another — same root cause and fix as the first row (finalization).
  - `[low]` `[reject]` (intent-alignment) "No re-parse" is pinned structurally (`Arc::ptr_eq`), not by observing that no parse runs — `cached_execution` contains no parse call and `hexput-script` exposes no parse counter; adding one would be new public surface for a property the code shape already guarantees, and a re-parse-and-discard is not a defect anyone meets.
  - `[low]` `[reject]` (intent-alignment) No bleed test under concurrent runs — each run builds its own heap, scopes and budget from an immutable shared `Arc<Program>`, and `a_fast_cached_run_is_answered_before_a_slow_one_submitted_earlier` already runs two at once and checks both values; a dedicated concurrent-bleed test adds nothing the sequential bleed tests do not.
  - `[low]` `[patch]` (intent-alignment) CPU-time and memory dimensions not exercised on Cached Execution — CPU time added (see the `cpu_time_ms` row); memory flows through the same shared `run` helper and `Limits::from_settings` as the four dimensions already tested, so no separate memory case was added.
  - `[false]` `[reject]` (intent-alignment) The decisions beyond the ACs written into the Spine exceed the intent — the run is unattended and was directed to decide and record every open question in the spec; dated Spine amendments are this project's standing convention for wire contracts SDKs depend on (Stories 2.x–4.1 did the same), and `status: final` permits dated amendments.

## Design Notes

**Golden exchange.**
```text
→ {id: 8, type: "CachedExecutionStart", payload: {handle: "0000000000000001", variables: {grade: 72}}}
← {id: 8, type: "Result", payload: {value: true}}
→ {id: 9, type: "CachedExecutionStart", payload: {handle: "00000000000000ff", variables: {}}}
← {id: 9, type: "Error", payload: {category: "protocol", code: "protocol.unknown_script_handle",
     message: "no Script is registered under handle `00000000000000ff` for this Session; register it again with `CodeRegister`"}}
```

**Why a new protocol code rather than `invalid_payload`.** A Backend has to tell "my request was wrong" apart from "my cache entry is gone, re-register and retry", so that its SDK can recover automatically after Story 4.3's eviction.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes unchanged
- `cargo build --workspace --all-targets --locked` -- expected: success
- `cargo test --workspace --locked` -- expected: all pass, including the new tests

## Auto Run Result

Status: done
Blocking condition: none.

**Summary.** A Backend runs a registered Script with `CachedExecutionStart {handle, variables, overrides?}`. The Daemon looks the handle up in the `AstCache` under the caller's Client ID and runs the cached `Arc<Program>` through the one private run-and-reply helper it now shares with Direct Execution (`Limits::from_settings`, `Host::new`, `execute_held`, `reply`), with the Session's registrations and Config read live and overlaid with `overrides`; nothing is parsed or checked. A well-formed handle with no entry is the new `protocol.unknown_script_handle`; a malformed payload (including `source` and any `overrides.check`) is `protocol.invalid_payload`.

**Files changed.**
- `crates/hexput-shared/src/wire.rs` — `MessageType::CachedExecutionStart`.
- `crates/hexput-port/src/error.rs` — `ProtocolCode::UnknownScriptHandle`; `InvalidPayload` doc.
- `crates/hexput-script/src/lib.rs` — `cached_execution`, the shared `run` helper, `decode(payload, Request)`, `unknown_handle`, crate docs.
- `crates/hexput-connection/src/lib.rs` — routing behind the init gate, `JoinSet` dispatch, `cached_execute` with its `debug` failure log (`code`, `handle`), module docs.
- `crates/hexput-tests/tests/{script,connection,daemon,port,shared}.rs` — the I/O matrix, spellings, a socket round trip.
- `ARCHITECTURE-SPINE.md` / `.memlog.md` — the dated Story 4.2 amendment; `epic-4-context.md` — Story 4.1 and 4.2 contracts; `AGENTS.md`, `sprint-status.yaml` — status.

**Review findings.** 19 findings: 13 rows patched (11 entries after grouping; medium 1, low 10 at entry verdict), 1 deferred (a test for an entry removed under a running execution, needing Story 4.3's removal API), 5 rejected — 3 false (Direct Execution's `overrides.check` is already tested; "halt after planning" governed only the planning stage; dated Spine amendments are the project's convention) and 2 low not worth the surface (an observable no-parse counter; a concurrent-bleed test duplicating what separate heaps and the concurrent fast/slow test already show). Patches: epic context brought up to Story 4.2; `overrides.check: "off"` refusal pinned and stated in the Spine; strict handle parsing and handle zero pinned; the handle logged on a failed Cached Execution; payload type names asserted in refusal messages; a CPU-time override case; the daemon test reads `handle` by key; two doc fixes.

**Follow-up review recommended:** false — one `medium` entry patched (a documentation file), no `high`.

**Verification.** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (26 edges, 4 restricted names, unchanged); `cargo build --workspace --all-targets --locked` OK; `cargo test --workspace --locked` 898 passed, 0 failed.

**Residual risks.** The removal-while-running guarantee is structural until Story 4.3 adds its test. A memory-budget case on the cached path is not separately exercised (it shares the helper with Direct Execution's).
