---
title: 'Story 6.4: Run initialization handlers before the plugin serves anything'
type: 'feature'
created: '2026-10-23'
status: 'done'
baseline_revision: '303499cb2d0c69d79635d3412017ff04fb032d18'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
warnings: ['oversized']
deferred:
  - summary: >-
      No test fires an Event during a pending init or fires BackendRegisteredInit over the wire; both wait for Story 6.5's fire message.
    evidence: |-
      Intent-alignment review: AC3 and AC4's fire half are pinned only through Plugins::get/ids and Plugin::event, because no fire message exists before Story 6.5. Story 6.5 must resolve fires through Plugins and add the two wire-level tests.
    location: >-
      crates/hexput-tests/tests/connection.rs (Story 6.5)
    severity: low
---

<intent-contract>

## Intent

**Problem:** A registered Plugin is stored and answered `Result {name}` the moment it parses; its `@Event(BackendRegisteredInit)` handlers never run (FR-18), so nothing guarantees a Plugin is prepared before real work reaches it, and no Executor path can invoke a handler at all.

**Approach:** Add the one handler-invocation function to `hexput-exec` (run the Plugin's top level, then call one handler with one argument, through the same budgeted, capability-checked loop as Direct Execution). Split Plugin registration into prepare (decode, parse, check, reserve name) → initialize (each init handler once, source order, sequentially) → store; the Plugin becomes visible in `Plugins` — i.e. ready — only after every init handler succeeded, and a failure releases the reserved name and stores nothing.

## Boundaries & Constraints

**Always:**
- AD-3: init handlers run only through `hexput_exec::invoke_handler`, under the Session's live settings at registration (`Limits::from_settings`, feature toggles) and its registrations as host (`Host::new(registrations, caller)`); each handler is its own execution with its own Resource Budget. Story 6.5 reuses this same function for fires.
- Every init handler runs exactly once, in source order, one after another; the next starts only after the previous finished successfully. The first failure stops the rest.
- Readiness = presence in `hexput_plugin::Plugins`. The Plugin is inserted only after all init handlers completed; until then `Plugins::get`/`ids` do not return it (its name is reserved in the Session, so a duplicate registration is still `protocol.duplicate_plugin_name`).
- A failure (parse/check/runtime/budget/host error of an init handler, or the Session ending) releases the reservation via new `Sessions::release_plugin(client_id, id)` (removes the name only if it still maps to that id) and stores nothing; the reply is `Error` with the failing handler's `Diagnostic` as `ErrorBody` unchanged (code, message, span). Findings from the static check are not attached to an init failure body (decision: keep the runtime error body exact, like a Script runtime failure without a pass would be; see Design Notes).
- An init handler's argument (`params`) is an empty object `{}`; its return value and its `modifications` are discarded.
- `BackendRegisteredInit` stays reserved: declaring it in `events` is refused (Story 6.3, unchanged) and `Plugin::event("BackendRegisteredInit")` is `protocol.undeclared_event` — pin both with tests.
- No lock held across `.await`; the connection stays the one writer; registration remains its own task in the connection's `JoinSet`; decode/parse/check/reserve and insert still run on the blocking pool.
- `scripts/check-crate-graph.py` sets unchanged (no new edge: `hexput-plugin → hexput-exec` and `hexput-connection → hexput-plugin` exist). `RESTRICTED_NAMES` still hold.

**Never:**
- No `PluginFire`/Event firing message (Story 6.5), no shape validation (6.6), no persistent Global Variable store (6.7): in this story every handler invocation evaluates the Global Variable initializers fresh and its writes end with the invocation.
- No new wire message type; no change to `PluginRegister`'s payload or success reply shape.
- No retry of a failed init; no partial registration.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| No init handler | Plugin without `@Event(BackendRegisteredInit)` | `Result {name}` as today; stored | none |
| Two init handlers | `a` then `b` both call blanket-granted `log(n)` | `Call log(1)` then `Call log(2)` in source order, each answered before the next; then `Result {name}` | none |
| Init pending | init handler blocked on a `Call` | `Plugins::ids` lacks it; same-name `PluginRegister` → `protocol.duplicate_plugin_name`; after reply, `Result {name}` and stored | none |
| Runtime failure | init handler does `null.x` | `Error` with that runtime diagnostic; name released (same name re-registers OK); not stored; later init handlers not run | Error reply |
| Budget | init handler `while (true) {}` | `Error` `budget.cpu_time_exceeded` | Error reply, released |
| Host error | init calls unregistered `f()` | `Error` `capability.unknown_function` | Error reply, released |
| Session ends mid-init | Session torn down while init pending | `Error` `protocol.init_not_completed`; nothing stored | Error reply |
| Reserved name | `events: {BackendRegisteredInit: …}` | `protocol.invalid_payload` (6.3 behavior) | unchanged |

</intent-contract>

## Code Map

- `crates/hexput-interpreter/src/lib.rs` (`Execution::with_variables` ~l.348) / `src/machine.rs` -- add `Execution::handler(program: Arc<Program>, function: &str, argument: Held) -> Result<Self, Diagnostic>`: runs the top level (lets + hoisted fns), then calls the named top-level function with one argument; `Finished.result` is its return (`null` without `return`). Unknown function name → internal-error diagnostic (cannot happen from a parsed Plugin).
- `crates/hexput-exec/src/lib.rs` (`execute_held` ~l.379) -- extract the segment/host-call loop into a private driver taking a built `Execution`; `execute_held` and new `pub async fn invoke_handler(program, function, argument: Held, host, limits) -> Result<Finished, Diagnostic>` both use it. Re-exports `Caller`, `Limits`, `Registration`, `Held` already.
- `crates/hexput-plugin/src/lib.rs` (`try_register` ~l.360, `Plugins::insert` ~l.225) -- split into `prepare(...) -> Result<Prepared, Refusal>` (sync: decode, parse, check, reserve; holds program/events/id/found findings), `pub async fn initialize(&Prepared, registrations, caller, settings) -> Result<(), Box<ErrorBody>>` (init handlers = `program.handlers` whose `event.name == BACKEND_REGISTERED_INIT`, in order, via `hexput_exec::invoke_handler` with `Arc<Program>` shared), and `finish(Prepared, client_id, &Plugins) -> Result<Value, Refusal>` (insert + `{name, findings?}`). `Plugin.program` becomes shareable (`Arc<Program>` alongside the `PluginProgram` metadata) so 6.5 reuses it. `register`/`try_register` are replaced by these three phases; move their existing tests to the phases (prepare + finish for no-init Plugins).
- `crates/hexput-session/src/lib.rs` (`register_plugin` ~l.564) -- add `release_plugin(client_id, id) -> bool`.
- `crates/hexput-connection/src/lib.rs` (`register_plugin` ~l.895, dispatch ~l.703) -- obtain registrations + settings via `Sessions::for_execution`, a `Caller::for_execution(id)`; blocking prepare → `initialize(...).await` (in the request span) → blocking finish; on any failure after reservation call `release_plugin`; log `debug` "plugin init failed" with `code`.
- `crates/hexput-tests/tests/{interpreter,exec,plugin,connection,session}.rs` -- existing test homes; `connection.rs` has the in-memory Port harness and discarding-subscriber rule.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-interpreter/src/{lib,machine}.rs` -- add `Execution::handler` -- the handler-invocation entry the Executor needs.
- `crates/hexput-exec/src/lib.rs` -- shared driver + `invoke_handler` -- AD-3 single path.
- `crates/hexput-session/src/lib.rs` -- `release_plugin` -- teardown of a failed registration.
- `crates/hexput-plugin/src/lib.rs` -- prepare/initialize/finish; update module docs ("What Story 6.4 adds") -- readiness after init.
- `crates/hexput-connection/src/lib.rs` -- wire the three phases, release on failure, docs -- registration flow.
- `crates/hexput-tests/tests/*.rs` -- tests for every matrix row plus: interpreter `handler` returns the function's value and sees Global Variable initializers; init params is `{}`; a handler's writes do not persist into a second init handler (pins the pre-6.7 decision).
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §9 -- add `[DECISION, 2026-10-23, Story 6.4]` init-handler semantics (order, `{}` params, result discarded, failure body, readiness) -- normative record.
- `AGENTS.md` -- Project Status: Story 6.4 summary, test count.

**Acceptance Criteria:**
- Given a Plugin with one or more `@Event(BackendRegisteredInit)` handlers, when it is registered, then each runs exactly once in source order and the `Result {name}` arrives (and the Plugin appears in `Plugins`) only after all completed.
- Given an init handler that fails or exceeds its budget, when it runs, then the registration answers `Error` with that diagnostic, the Plugin is absent from `Plugins`, the name is free again in the Session, and later init handlers did not run.
- Given a registration whose init is still running, when anything looks the Plugin up (`Plugins::get`/`ids`), then it is not there — so no Event can execute ahead of init.
- Given a Backend declaring `BackendRegisteredInit` in `events` or resolving it as a fire target (`Plugin::event`), then it is refused (`protocol.invalid_payload` / `protocol.undeclared_event`).

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/interpreter.rs` (`a_handler_returns_the_value_its_function_returns`, `a_handler_without_a_return_returns_null`, `a_handler_runs_the_top_level_first_so_its_globals_and_hoisted_helpers_are_bound`, `a_handler_sees_the_global_variable_initializers_it_is_run_with`, `a_handler_receives_its_argument_as_its_parameter`, `an_init_handler_given_empty_params_returns_an_empty_object`, `a_write_by_one_handler_invocation_does_not_persist_into_the_next`, `a_handler_that_disables_a_construct_is_refused_as_the_runtime_would`), `crates/hexput-tests/tests/exec.rs` (`a_handler_invocation_returns_the_value_its_function_returns`, `a_handler_invocation_runs_the_top_level_first_so_its_globals_and_helpers_are_bound`, `the_params_are_the_empty_object_and_a_handler_without_return_returns_null`, `each_invocation_evaluates_the_globals_fresh_and_a_write_does_not_persist_into_the_next`, `a_blanket_granted_host_call_from_a_handler_is_sent_and_its_reply_resumes_it`, `an_unregistered_host_call_from_a_handler_is_a_capability_error_and_nothing_is_sent`, `a_handler_runtime_failure_is_its_diagnostic`, `a_handler_past_its_cpu_time_budget_is_budget_cpu_time_exceeded`, `a_handler_runs_under_the_session_feature_toggles_it_is_given`), `crates/hexput-tests/tests/plugin.rs` (`register_under` now runs prepare/initialize/finish; `a_prepared_plugin_is_not_held_until_it_is_finished`, `a_prepared_plugin_dropped_before_it_is_finished_is_never_held`, `a_plugin_is_not_ready_while_its_init_handler_is_pending_and_is_ready_once_it_completes`, `a_failing_init_handler_yields_its_diagnostic_and_nothing_is_held`, `an_init_handler_whose_budget_is_spent_yields_its_diagnostic_and_nothing_is_held`), `crates/hexput-tests/tests/session.rs` (`releasing_a_reserved_name_frees_it_for_a_fresh_registration_and_its_id_is_never_reused`, `releasing_under_another_id_leaves_the_name_held_by_its_first_registration`, `releasing_a_name_that_is_not_held_or_a_session_that_is_missing_is_false`, `releasing_after_the_session_is_torn_down_is_false`), and `crates/hexput-tests/tests/connection.rs` (`a_plugin_without_an_init_handler_is_answered_its_name_and_held`, `two_init_handlers_each_run_once_in_source_order_and_the_name_is_answered_after_both`, `an_init_handler_is_called_with_empty_params_and_its_return_value_is_discarded`, `an_init_handler_that_fails_is_answered_with_its_diagnostic_releases_the_name_and_runs_no_later_handler`, `an_init_handler_past_its_budget_is_a_budget_error_and_the_name_is_free_again`, `an_init_handler_calling_an_unregistered_function_is_an_error_and_the_name_is_free_again`, `a_plugin_whose_init_is_pending_is_not_held_and_a_same_name_registration_is_a_duplicate`, `a_session_that_ends_while_its_init_is_pending_answers_init_not_completed_and_stores_nothing`, `a_plugin_declaring_the_reserved_event_is_refused_and_no_init_handler_runs`, `an_init_handler_runs_under_the_sessions_live_feature_toggles`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it. Test-agent notes for the implementer: (1) the Code Map gives no parameter lists for `prepare`/`initialize`/`finish`; the tests assume `prepare(payload, reserve, callables, settings)`, `initialize(&Prepared, Vec<Registration>, Caller, &Settings)` and `finish(Prepared, client_id, &Plugins)` (`Result<Value, Refusal>`), and `Refusal::into_body`. (2) `a_session_that_ends_while_its_init_is_pending...` pins the matrix's `protocol.init_not_completed` for a clean close during init; the connection closes the pending `Call` at once, so the init may naturally fail with `host.no_reply` instead. The implementer must make the wire reply `protocol.init_not_completed` as the matrix states, or record a Spec Change Log entry; the test was not weakened.

## Spec Change Log

- 2026-10-23 (implementation) — **Session ends mid-init.** On a clean close the connection closes its pending `Call`s at once, so a waiting init handler fails with `host.no_reply` while the Session still exists (it is detached only after the registration task finishes). To keep the matrix row's `protocol.init_not_completed`, `hexput_plugin::initialize` maps an init handler's `host.no_reply` — the Backend can no longer answer — to `protocol.init_not_completed` ("the Session ended before the Plugin's initialization completed; nothing was registered"); every other failure is the handler's `Diagnostic` unchanged. A Session gone by `finish` is `protocol.init_not_completed` as before. KEEP: the matrix row and its test are unchanged.
- 2026-10-23 (implementation) — **Shared program.** `PluginProgram.program` stays an owned `Program` (the pre-written tests build `Arc::new(plugin.program)`), so `prepare` clones it once into the `Arc<Program>` handlers run against (`Prepared`/`Plugin::executable()`); one AST copy per registration, none per invocation.
- 2026-10-23 (implementation) — **`initialize` signature.** It is a plain `fn` returning `impl Future + Send + 'p` that borrows only `&'p Prepared`: the settings are read (`Limits::from_settings`) when it is called, which the pre-written plugin test relies on (it passes a temporary `&Settings`).
- 2026-10-23 (implementation) — **Pre-written test corrections (none weakened).** `connection.rs::an_init_handler_is_called_with_empty_params…` asserted the `log(p)` Call's arguments were `[]`; per this spec `params` is `{}`, so the one argument is the empty map — now `vec![Value::Map(vec![])]` (stricter). Three sources wrote `while (false) {} return 1;`, a §2 syntax error (a block statement needs `;` before the next statement); now `while (false) {}; return 1;`. `interpreter.rs` compared `hexput_interpreter::Value` with `assert_eq!`, which it does not implement (`PartialEq` is absent by design); those use `as_number()`/`matches!(…, Value::Null)` instead. `exec.rs` needed a `Vec<Registration>` annotation to compile.

## Review Triage Log

### 2026-10-23 — Review pass
- verdicts: 29 findings — high 4, medium 0, low 21, false 4, maybe-false 0
- findings:
  - `[high]` `[patch]` (blind) The reservation leaks when the registration task is aborted (lost stream drops the `JoinSet`) or a blocking-pool panic resumes; since Epic 5 the Session outlives the Connection, so the name stays `protocol.duplicate_plugin_name` with no Plugin — verified: `release_plugin` was only on the `Err` arms. Fixed: private `ReleaseOnDrop` guard in `hexput-connection::register_plugin`, disarmed only after `finish` stores the Plugin; test `a_lost_stream_mid_init_releases_the_name_for_another_connection_of_the_session`.
  - `[low]` `[patch]` (blind) A close during an init handler's `Authorize` wait ends as `capability.unknown_function`, not `protocol.init_not_completed`, contradicting the §9 decision's wording — verified real, but a denial is the Story 3.3 contract for every unanswered question, Scripts included. Fixed by narrowing the decision (LANGUAGE-REFERENCE §9, `initialize` and connection docs) to an unanswered `Call`, an unanswered `Authorize` being a denial; test `an_init_handler_whose_question_is_left_unanswered_by_a_close_is_a_denial`.
  - `[low]` `[patch]` (blind) The `init_not_completed` message says the Session ended although only this connection stopped reading (AD-2, Story 5.8) — verified. Message reworded to "the connection stopped reading before the Plugin's initialization completed; nothing was registered".
  - `[low]` `[patch]` (blind) Under `features.callbacks = false` every handler fails `policy.construct_disabled` (top-level `fn` hoist), undecided and untested — verified at `machine.rs` `allow_hoisting`; it follows §3.9's rule ("defining a function, named or anonymous") and Story 6.3's check already reports it. Recorded in LANGUAGE-REFERENCE §9 and `initialize`'s docs; test `under_disabled_callbacks_every_handler_is_refused_as_construct_disabled`.
  - `[low]` `[reject]` (blind) An init failure body drops the static check's findings, while the Design Notes call it "same as a Script runtime error" — the behaviour is fixed by the intent contract's Always rule; only the Design Notes rationale is imprecise, and a fix edits this build's spec.
  - `[low]` `[patch]` (blind) Story status inconsistent: sprint-status says `ready-for-dev` — finalization sets it to `review`.
  - `[low]` `[patch]` (blind) AGENTS.md headline says 6.1–6.4 implemented but its parenthetical omits init handlers — fixed in the run's mandated AGENTS.md Project Status update.
  - `[low]` `[patch]` (blind) `register_plugin` captured `reserved: Option<String>` and logged `unwrap_or_default()` — removed; the name is now `logged_name(prepared.name())`.
  - `[low]` `[patch]` (blind) An init failure logged only "plugin init failed", never "a plugin registration failed" — now both are emitted (in that order) and the module docs say which path emits which.
  - `[low]` `[patch]` (blind) Missing tests (init `Call`s' `execution`, unknown handler name, discarded modifications, per-handler budget) — added: the `execution` assertion in the lost-stream test, `a_handler_name_that_is_not_a_top_level_one_parameter_function_is_an_internal_syntax_error`, `an_init_handlers_modifications_never_reach_the_registration_reply`, `each_init_handler_has_a_resource_budget_of_its_own`.
  - `[low]` `[reject]` (blind) Registration time and reservation unbounded across many init handlers / an unanswered `Call` — each handler is budgeted; a `Call` without timeout is the pre-existing contract for every execution, and the cost lands on the Backend's own Session exactly as many `ExecutionStart`s would; a cap needs a new setting.
  - `[low]` `[patch]` (blind) `on_blocking_pool`'s cancellation panic always named a Plugin registration — message is now "work on the blocking pool was cancelled".
  - `[high]` `[patch]` (edge) Lost connection drops the `JoinSet` mid-init; name stays reserved after reconnect — same root cause and fix as the first row (guard).
  - `[high]` `[patch]` (edge) `finish` panicking on the blocking pool skips the release — same root cause; the guard releases on unwind.
  - `[low]` `[patch]` (edge) `host.no_reply` on one Connection reported as the Session ending while others keep it — same as the message row; reworded.
  - `[low]` `[patch]` (edge) A large discarded init result fails `budget.output_size_exceeded` — verified (`drive` charges output). Not worth a new invocation parameter for an unlikely case; the decision text (§9, `initialize` docs) now states the result is produced, detached and charged like any execution's before it is discarded.
  - `[low]` `[patch]` (edge) An init handler returning a function or cycle fails `type.function_result` — verified; same root cause and documentation fix; test `a_handler_returning_a_function_fails_as_any_execution_would`.
  - `[low]` `[patch]` (edge) `features.callbacks = false` blocks every init handler — same as the callbacks row; documented and tested.
  - `[high]` `[patch]` (edge, claim) The spec says a failure after the reservation releases the name, but abort/panic skipped it — same root cause; guard.
  - `[low]` `[patch]` (edge, claim) "Discarded" result can still fail the init — same root cause as the output-size row; documented.
  - `[low]` `[patch]` (verification-gap, other) Init handlers always fail under `features.callbacks = false`, untested — same as the callbacks row.
  - `[false]` `[reject]` (intent) The diff implements the story although the intent says "halt after planning" — the run is staged: planning halted at its stage and step-03 implemented the spec; this review stage is directed to triage and finalize that implementation.
  - `[low]` `[patch]` (intent) Artifacts disagree on the story's status — same as the sprint-status row; finalization sets `review`.
  - `[false]` `[reject]` (intent) AC1 readiness is registry presence, no explicit marker — the spec's Always rule defines readiness as presence in `Plugins`, which satisfies "only marks the Plugin ready after they complete".
  - `[low]` `[patch]` (intent) AC2: Global Variable teardown untested and only CPU-time budget tested on this path — no Global Variable store exists before 6.7 (nothing to tear down); the budget part was real and is covered by `each_init_handler_has_a_resource_budget_of_its_own` (`rpc_calls`).
  - `[low]` `[defer]` (intent) AC3: no test sends a fire during init — no fire message before Story 6.5; deferred to 6.5 (frontmatter `deferred`, deferred-work.md).
  - `[low]` `[defer]` (intent) AC4: no wire-level fire-refusal test for `BackendRegisteredInit` — same root cause; deferred with the previous row.
  - `[false]` `[reject]` (intent) The diff makes decisions beyond the story (params `{}`, ordering, failure body, `no_reply` mapping) — the unattended run requires deciding and recording; the spec's intent contract mandates each, and §9 records them as the spec's Tasks require.
  - `[false]` `[reject]` (intent) `--release` appears only in the spec's Verification, AGENTS.md's Build section untouched — the user's request governs how commands run on this machine, not the documented CI steps (already deferred by Story 5.7).

## Design Notes

- Fire-before-init (AC 3): no fire message exists until 6.5; readiness by insertion makes "not executed ahead of init" structural, and `Sessions::plugins` already documents that a reserved id without a body is not ready. 6.5 must resolve fires through `Plugins`.
- State teardown (AC 2): with no Global Variable store yet, a failed init leaves nothing to tear down beyond the reservation; 6.7 hooks `hexput_globalvar::teardown` into this same failure path.
- Failure body: exactly the handler's `Diagnostic` (most defensible: same as a Script runtime error). Registration-time findings ride only the success reply.
- `params = {}` rather than `null`, so `params.x` reads `null` as in a fire with no such field.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: pass
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass

## Auto Run Result

**Implemented:** A Plugin's `@Event(BackendRegisteredInit)` handlers run once each, in source order, through the one handler-invocation function `hexput_exec::invoke_handler` (shared `drive` loop with `execute_held`), before the Plugin is stored in `Plugins` — readiness is presence there. Registration is `prepare` → `initialize` → `finish`; a failure releases the reserved name (`Sessions::release_plugin`) and stores nothing.

**Files changed:**
- `crates/hexput-interpreter/src/{lib,machine}.rs` — `Execution::handler` and `Frame::Handler`.
- `crates/hexput-exec/src/lib.rs` — `invoke_handler`, private `drive`, re-exports `Code`/`Object`.
- `crates/hexput-plugin/src/lib.rs` — `prepare`/`initialize`/`finish`, `Prepared`, `Plugin::executable`.
- `crates/hexput-session/src/lib.rs` — `release_plugin`.
- `crates/hexput-connection/src/lib.rs` — three-phase `register_plugin` with a release-on-drop guard.
- `crates/hexput-daemon/src/lib.rs` — tests adapted to the new phases.
- `crates/hexput-tests/tests/{interpreter,exec,plugin,connection,session}.rs` — tests for every matrix row plus the review's additions.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` — §9 init-handler decision.
- `AGENTS.md`, `sprint-status.yaml`, `deferred-work.md` — status and deferral.

**Review findings:** 29 — patches applied for 20 rows in 9 entries (high 1 entry: reservation release on abort/panic; low 8 entries: message wording, `Authorize` close semantics, callbacks decision, discarded-result semantics, logging, name bookkeeping, panic message, tests, status/AGENTS); 2 rows deferred (fire-path tests, to Story 6.5); rejected: findings omitted from init failure body (fix edits this build's spec; behaviour fixed by the intent contract), unbounded registration time (Backend-inflicted on its own Session, pre-existing `Call` contract, cap needs a new setting), and four `false` intent-alignment rows (staged run, readiness by presence is the spec's definition, decisions mandated, `--release` is a local-run rule).

**Follow-up review recommended: true.** Patched by verdict: high 1, medium 0, low 8. Unverified risk: the `ReleaseOnDrop` guard is tested for a lost stream only; its release on a panic resumed from the blocking pool (`finish` or the `prepare` result) and its interaction with a Session torn down by TTL while the task unwinds have no test.

**Verification:** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (27 edges, 4 restricted names); `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 1243 passed, 0 failed.

**Residual risks:** Every handler invocation re-evaluates Global Variable initializers until Story 6.7; an init `Call` has no timeout, so a silent Backend holds the reservation until its connection ends; fire-path guarantees (AC3/AC4 on the wire) depend on Story 6.5 resolving fires through `Plugins`.
