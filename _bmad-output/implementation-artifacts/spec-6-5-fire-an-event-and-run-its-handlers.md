---
title: 'Story 6.5: Fire an event and run its handlers'
type: 'feature'
created: '2026-10-23'
status: 'done'
baseline_revision: 'b16dcff2996c71c42574a0b1aa4cfc677b96a0b9'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
warnings: ['oversized']
deferred: []
---

<intent-contract>

## Intent

**Problem:** A registered, ready Plugin can never run anything after its init: no wire request fires a declared Event, so its `@Event(<Name>)` handlers are unreachable (FR-19), and Story 6.4's deferred fire-path guarantees (no fire ahead of init, `BackendRegisteredInit` not firable) have no wire test.

**Approach:** Add the Backend request `EventFire {plugin, event, params}`. The connection fetches the named Plugin from `hexput_plugin::Plugins` at dispatch, `hexput-plugin` decodes the payload and resolves the declared Event, then runs every handler bound to that Event once, in source order, each through `hexput_exec::invoke_handler` (AD-3) as its own execution and budget, and answers one `Result {results: [...]}` with one entry per handler — its value (and modifications) or its error — so one handler's failure never hides or stops another's.

## Boundaries & Constraints

**Always:**
- Wire (decided, recorded in the Spine and `wire.rs`): `MessageType::EventFire`, appended after `PluginRegister` in `MessageType::ALL`, behind the init gate (`protocol.init_not_completed` before init), served as its own task in the connection's `JoinSet` like an execution, in the request span, its handlers' `Call`s carrying `execution` = the fire's request id (`caller.for_execution(id)`). Payload exactly `{plugin, event, params}`: `plugin` a string (the metadata `name` the registration answered), `event` a string, `params` a map. Absent or nil is missing (several missing keys named together, as `ExecutionStart` does); an unknown, repeated or non-string key, a mistyped value, or `params` that converts to anything but an object is `protocol.invalid_payload` naming the key or the path. No `overrides`, no `key` (Story 6.10 adds the partition key; until then a `keyed: true` Event fires like any other).
- Order of refusals: the payload is decoded in full first (`invalid_payload`), then the Plugin (`protocol.unknown_plugin`, new, appended to `ProtocolCode::ALL`: "this Session has no ready Plugin named `<name>`; nothing was fired", name `bounded`, the same for never-registered, pending-init and other-Session names), then the Event via the existing `Plugin::event` (`protocol.undeclared_event`, including its reserved-name message for `BackendRegisteredInit`). Nothing runs on any refusal; each is an `Error` reply.
- `params` converts with the starting-variable conversion (`hexput_exec::wire::to_hexput`, holders/Value Secrets honoured) under a new path root `params` (`Path::params()`, rendering `` `params.user.tags[2]` ``); the converted value must be an object, else `` `params` must be a map ``. Every handler receives its own clone of the converted `Held`.
- The Plugin is fetched when the connection dispatches the request (like `cached_execution`'s handle), from `Plugins` only — readiness is presence there (Story 6.4), so a Plugin whose init is pending is `unknown_plugin`, never queued.
- Handlers: those in `PluginProgram::handlers` whose `event.name` equals the fired name, in source order, run one after another (FR-22's default order; `priority` and `async` are recorded but have no effect until Stories 6.11/6.12). Each runs through `hexput_exec::invoke_handler(plugin.executable(), function, params, Host::new(registrations, caller), Limits::from_settings(&settings))` with the Session's registrations and settings read at dispatch (`session_now`, AD-5); each its own execution and Resource Budget. A handler's failure (any `Diagnostic`: runtime, budget, capability, host, policy) is recorded and the next handler still runs; nothing about the Plugin changes.
- Reply: `Result {results}`, `results` an array in the order the handlers ran; a success entry is `{handler, value}` with `modifications` after `value` only when non-empty (the same conversion as a Script reply: `result_to_wire_within` then `modifications_to_wire` under one per-entry `MAX_FRAME_LEN` budget); a failure entry is `{handler, error}`, `error` the `ErrorBody::to_value()` of the diagnostic (span included). A result the entry cannot carry becomes that entry's error: `protocol.result_too_deep` / `protocol.response_too_large` / `type.function_result`, worded "the handler's result …". No handler bound → `Result {results: []}` (the defined no-op). A whole reply past a frame is the connection's existing `protocol.response_too_large` replacement. No `findings` ever.
- Each fire evaluates the Global Variable initializers afresh per invocation (no store until Story 6.7). No lock held across `.await`; the connection stays the one writer; decoding/conversion and reply building run on the blocking pool (`on_blocking_pool`).
- Logging at `debug` in the request span: "event fired" (`plugin` bounded via `logged_name`, `event` bounded, `handlers`, `failed`), "a handler failed" (`handler`, `code`) per failed entry, "an event fire failed" (`code`) for a refused fire.
- No crate edge changes; `scripts/check-crate-graph.py` and `RESTRICTED_NAMES` unchanged and passing.

**Never:**
- No shape validation of results (Story 6.6), no Global Variable store (6.7), no priority/async scheduling (6.11/6.12), no partition key (6.10), no unregister (6.13).
- No change to any existing message's bytes (`PluginRegister`, Script replies) and no refactor of `hexput-script`'s reply.
- A failing handler never removes, poisons or re-initializes its Plugin, and never ends the connection.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| One handler | `@Event(Ping) fn h(p) { return p.n + 1; }`, fire `{plugin, event: "Ping", params: {n: 1}}` | `Result {results: [{handler: "h", value: 2}]}` | none |
| Two handlers | `a` then `b` on `Ping`, each calls blanket `log(x)` | `Call log` from `a` answered before `b`'s; entries `a`, `b` in order | none |
| No handler | `Pong` declared, nothing bound | `Result {results: []}`; Plugin still fires `Ping` later | none |
| Handler fails | `a` does `null.x`, `b` returns 1 | `[{handler:"a", error:{code: reference…, span…}}, {handler:"b", value:1}]` | entry error, siblings run |
| Over budget | `a` loops forever | `a`'s entry `budget.cpu_time_exceeded`; a later fire still works | entry error |
| Unregistered call | `a` calls `f()` unregistered | entry `capability.unknown_function`, nothing sent | entry error |
| Modifications | `params.user` holder, handler writes `params.user.n` | entry carries `modifications` with that ref | none |
| Unknown plugin | name never registered / init pending / other Session's | `Error protocol.unknown_plugin` | nothing runs |
| Undeclared / reserved | `event: "Nope"` or `"BackendRegisteredInit"` | `Error protocol.undeclared_event` | nothing runs |
| Bad payload | missing `params`, `params: 1`, extra `key`, `plugin: 3` | `Error protocol.invalid_payload` naming it | nothing runs |
| Before init | fire on an unattached connection | `Error protocol.init_not_completed` | nothing runs |

</intent-contract>

## Code Map

- `crates/hexput-shared/src/wire.rs` (`MessageType` l.54–135, `ALL` l.139) -- add `EventFire` after `PluginRegister`, documented like its neighbours.
- `crates/hexput-port/src/error.rs` (`ProtocolCode` l.70–118, `ALL` l.82) -- add `UnknownPlugin` = `protocol.unknown_plugin`, last in `ALL`.
- `crates/hexput-exec/src/wire.rs` (`Path` l.89–140) -- add `pub const fn params() -> Self` (root `params`, no segment).
- `crates/hexput-plugin/src/lib.rs` -- `Plugins` (l.196–275): add a per-client name index kept in step with `insert`/`close` and `pub fn named(&self, client_id, name: &str) -> Option<Arc<Plugin>>`; `Plugin::handlers_for(&self, event) -> impl Iterator<Item=&str>` (source order). New fire API mirroring `prepare`/`initialize`/`finish`: `pub fn fetch(payload: &Value, client_id, &Plugins) -> Option<Arc<Plugin>>` (scan for a string `plugin` key, no decoding); `pub fn prepare_fire(payload: &Value, fetched: Option<Arc<Plugin>>) -> Result<Fire, Box<ErrorBody>>` (sync: decode, unknown_plugin, `Plugin::event`, params → object); `pub fn run_fire<'f>(&'f Fire, Vec<Registration>, Caller, &Settings) -> impl Future<Output = Vec<HandlerOutcome>> + Send + 'f` (settings read at call, like `initialize` l.467); `pub fn fire_reply(&[HandlerOutcome]) -> Value` (sync). `HandlerOutcome { handler: String, result: Result<Finished, Diagnostic> }` public (accessors fine). Reuse `bounded`, `protocol()` (l.750), `hexput_exec::wire::{to_hexput, result_to_wire_within, modifications_to_wire, MODIFICATIONS_KEY, Unsendable, MAX_RESULT_DEPTH, MAX_MODIFICATION_DEPTH}`; mirror `hexput-script` `reply` (`crates/hexput-script/src/lib.rs` l.922–990) for the Unsendable mapping; `hexput-script` `decode` shows the "missing keys named together" style. Module docs: "What Story 6.5 adds".
- `crates/hexput-connection/src/lib.rs` -- `answer` (l.663): add `EventFire` to the gated arm and an `Answer::Fire(id, payload, client_id)`; `exchange` (l.344): on `Answer::Fire`, `session_now`, `fetch` in the request span at dispatch, spawn `fire(...)` instrumented with the span; new `async fn fire` doing `on_blocking_pool(prepare_fire)` → `run_fire().await` → `on_blocking_pool(fire_reply)` and the logging above. Reuse `on_blocking_pool` (l.1026), `logged_name` (l.1042), `error_response`. Module docs.
- `crates/hexput-tests/tests/{plugin,connection,shared,port}.rs` -- `connection.rs` has the in-memory Port harness, `PluginRegister` helpers (~l.6627–6700) and the discarding-subscriber rule; `shared.rs`/`port.rs` pin `MessageType::ALL`/`ProtocolCode::ALL`.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/wire.rs` -- add `MessageType::EventFire` + docs -- the fire request.
- `crates/hexput-port/src/error.rs` -- add `ProtocolCode::UnknownPlugin` -- defined unknown-name refusal (FR-21 reuses it).
- `crates/hexput-exec/src/wire.rs` -- `Path::params()` -- path-named param refusals.
- `crates/hexput-plugin/src/lib.rs` -- name index, `named`, `handlers_for`, `fetch`/`prepare_fire`/`run_fire`/`fire_reply`, `Fire`, `HandlerOutcome`, docs -- fire resolution and per-handler invocation.
- `crates/hexput-connection/src/lib.rs` -- route and spawn `EventFire`, logging, docs -- wire path.
- `crates/hexput-tests/tests/*.rs` -- one test per matrix row (connection level), plus: plugin-level `prepare_fire` refusal order (bad payload beats unknown plugin beats undeclared event); a fire while init is pending is `unknown_plugin` and the same fire after init succeeds; `BackendRegisteredInit` fire over the wire is `undeclared_event` (settles Story 6.4's deferral); each handler gets its own budget (`rpc_calls` 1, two handlers each making one call both succeed); `params` is a clone per handler (a write by `a` to `params.x` not seen by `b`); the `Call`s carry `execution` = fire id; two concurrent fires on one connection both answered; a failed handler leaves the Plugin firable; `ALL` lists updated.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §9 -- `[DECISION, 2026-10-23, Story 6.5] Firing an Event` (handler set and order, `params`, per-handler result/failure, no-op, no persistence yet).
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- dated `[Amended 2026-10-23, Epic 6 Story 6.5]` paragraph after Story 6.3's: the `EventFire` wire, reply, codes, no edge change.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- mark Story 6.4's fire-path entry settled by this story.
- `AGENTS.md` -- Project Status: Story 6.5 summary, test count, next step.

**Acceptance Criteria:**
- Given a ready Plugin with handlers bound to a declared Event, when the Backend sends `EventFire` with `params`, then every bound handler is invoked once with those parameters, in source order, and the `Result` lists one entry per handler.
- Given a handler invocation, when it runs, then it goes through `hexput_exec::invoke_handler` with the Session's live registrations and settings — an unregistered call is `capability.unknown_function` and a runaway is `budget.cpu_time_exceeded`, exactly as for a Script.
- Given a declared Event with no bound handler, when fired, then the reply is `Result {results: []}` and the Plugin still serves later fires.
- Given one handler that fails or exceeds its budget, when the fire completes, then its entry carries the error, its siblings' entries carry their results, and the Plugin, the connection and the Daemon keep serving.

## Design Notes

- One reply per fire, entries per handler: Story 6.6 validates each result independently against the Event's shape, so it slots a shape error into the failing entry without touching the others.
- Sequential in source order is FR-22's default order; it is not a mailbox — each fire is its own task, so fires never wait on each other (AD-6). Story 6.11 replaces the loop's order with the per-(Plugin, Event) sequencing task; 6.12 lifts `async` handlers out of it.
- `params` must be a map so `params.x` behaves as in an init handler's `{}`.
- Fetch at dispatch keeps "requests take effect in the order sent" ready for Story 6.13's unregister.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: pass
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/connection.rs` (`a_fire_runs_its_one_bound_handler_and_answers_one_entry_with_its_value`, `two_handlers_run_in_source_order_and_each_call_is_answered_before_the_next_begins`, `an_event_with_no_bound_handler_is_answered_with_no_results_and_the_plugin_still_fires`, `a_failing_handler_is_its_entry_error_and_its_sibling_still_runs_and_answers`, `a_handler_over_its_budget_is_its_entry_error_and_a_later_fire_still_works`, `a_handler_calling_an_unregistered_function_is_its_entry_error_and_nothing_is_sent`, `a_handler_writing_into_a_referenced_parameter_is_its_entry_modifications`, `a_fire_for_a_plugin_this_session_has_no_ready_plugin_for_is_unknown_and_runs_nothing`, `a_fire_while_the_init_is_pending_is_unknown_and_the_same_fire_after_it_completes_runs`, `a_fire_before_init_is_init_not_completed_and_runs_nothing`, `a_fire_of_an_undeclared_or_reserved_event_is_undeclared_event`, `a_malformed_fire_payload_is_refused_naming_what_is_wrong_and_no_handler_runs`, `each_handler_of_a_fire_has_a_resource_budget_of_its_own`, `each_handler_gets_its_own_copy_of_the_parameters`, `two_fires_on_one_connection_are_each_answered_with_their_id_whatever_order_they_finish_in`, `a_failed_handler_leaves_the_plugin_firable_and_its_next_fire_succeeds`, `a_fire_runs_under_the_sessions_live_feature_toggles_and_a_refusal_is_its_entry_error`, `a_fire_of_a_plugin_with_warnings_carries_no_findings_in_its_reply`, `a_result_that_is_no_wire_value_is_its_entry_error_and_its_sibling_still_answers`, `a_fire_is_logged_at_debug_under_its_request_and_a_refusal_names_its_code`, `a_fire_of_an_ordinary_event_never_runs_the_init_handlers`) and `crates/hexput-tests/tests/plugin.rs` (`a_fire_is_refused_bad_payload_first_then_unknown_plugin_then_undeclared_event`, `a_non_map_params_and_a_nested_bad_integer_are_refused_naming_their_path`, `fetch_finds_only_a_ready_plugin_of_the_callers_session_and_never_decodes_the_payload`, `named_finds_a_plugin_by_its_client_and_name_and_closing_the_client_forgets_it`, `handlers_for_lists_the_handlers_bound_to_an_event_in_source_order`, `a_fire_runs_each_bound_handler_once_and_its_reply_has_one_entry_per_handler_in_order`, `an_event_with_no_bound_handler_fires_to_an_empty_results_list`, `a_failing_handler_is_an_error_entry_and_its_sibling_still_has_its_value`), and the `MessageType::ALL` and `ProtocolCode::ALL` pins in `shared.rs` and `port.rs`. The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Implementation decisions (2026-10-23): (1) a fire reply entry nests two levels deeper than a Script's `{value}` (the `results` array and the entry map sit between the payload and the value), so `fire_reply` also refuses an entry whose `value` or `modifications` array nests past `MAX_RESULT_DEPTH - 2` as `protocol.result_too_deep` — otherwise a deep result would make a frame the Backend's decoder refuses, and the whole reply would be lost instead of one entry; (2) "event fired"'s `failed` and the "a handler failed" events count every error entry of the reply, an unsendable result included; (3) two pre-written tests were corrected where they contradicted the language and the spec, never weakened: `a_failed_handler_leaves_the_plugin_firable_and_its_next_fire_succeeds` gained the `;` §2 requires after an `if` block before the next statement, and `a_fire_of_a_plugin_with_warnings_carries_no_findings_in_its_reply` reads its registration reply as `{name, findings}` (a warning is a finding the registration reply carries, Story 6.3) instead of through the exactly-`{name}` helper; `tests/plugin.rs` uses `expect_err`/`expect` where clippy refused `.err().expect()`/`.ok().expect()`.
- Review decisions (2026-10-23): (4) the entries of a fire reply share one frame — `fire_reply` gives each entry only what the entries before it left of `MAX_FRAME_LEN` (less `findings::ENVELOPE_ROOM` and the payload's own headers), measured exactly with `findings::encoded_len`, and an entry that does not fit becomes its own `protocol.response_too_large` — so one large result can no longer make the connection replace the whole reply and lose every sibling's entry (the Approach's "one handler's failure never hides another's"; each entry's budget is still at most one frame, as the contract says); (5) a `params` holder around a non-map is refused with `` `params` must be a map …; a Value Secret holder around anything but a map is not one ``; (6) a modifications depth refusal names the "`modifications` list", which is what the limit counts; (7) a fire's whole cost is its handler count times one budget, stated in LANGUAGE-REFERENCE §9 and the Spine, a fire-wide limit recorded in `deferred-work.md`.

## Spec Change Log

(none — the review routed no `bad_spec` finding)

## Review Triage Log

### 2026-10-23 — Review pass
- verdicts: 27 findings — high 0, medium 9, low 15, false 3, maybe-false 0
- findings:
  - `[low]` `[patch]` blind-hunter: the sequencing test does not show `b` waits for `a`'s answer — `two_handlers_run_in_source_order_…` now calls `assert_silent` (a sentinel execution's reply must be the next envelope) before answering `a`'s `Call`.
  - `[low]` `[reject]` blind-hunter: the Backend cannot tell which handler made a `Call` — handlers run strictly one after another, so order identifies them; a `handler` field would be new wire surface for no defect today (Story 6.12's `async` handlers are where it would matter). The Spine amendment now says the Backend tells handlers apart by order.
  - `[medium]` `[patch]` blind-hunter: one large handler result can wipe out every entry (no shared budget) — `fire_reply` now shares one frame across entries; an entry past what is left is its own `protocol.response_too_large`; pinned by `the_entries_of_a_fire_reply_share_one_frame_and_a_large_result_costs_only_its_own_entry` (two 8 MiB results: the first carried, the second refused, the third answered).
  - `[low]` `[patch]` blind-hunter: a fire has no overall limit and the docs do not say so — LANGUAGE-REFERENCE §9, the Spine amendment and AGENTS.md now state a fire may cost its handler count times one budget; a fire-wide limit is recorded in `deferred-work.md`.
  - `[low]` `[reject]` blind-hunter: `type.function_result` entries carry span 1:1 — the entry already names its handler, a function result is rare, and pointing at the handler's span needs new plumbing through the public `HandlerOutcome`; `hexput-script` carries the same span for the same case.
  - `[low]` `[reject]` blind-hunter: the connection rebuilds failure counts by re-reading the reply — the layout is this story's own and `a_fire_is_logged_at_debug_…` pins the counts and codes, so a layout change fails a test rather than breaking silently; returning counts would widen `fire_reply`'s public signature.
  - `[medium]` `[patch]` blind-hunter: `MAX_ENTRY_DEPTH` is untested — added `a_too_deep_handler_result_is_its_entry_error_and_its_sibling_still_answers` (a result at `MAX_RESULT_DEPTH - 1` and a write whose `modifications` list nests 125 are each `protocol.result_too_deep`, a sibling still answers, the reply round-trips the codec); verified it fails with the check removed.
  - `[low]` `[patch]` blind-hunter: `sprint-status.yaml` says `ready-for-dev` — set to `review` at finalization.
  - `[medium]` `[patch]` blind-hunter: a `params` holder around a scalar is untested and its message confusing — added the case to `a_malformed_fire_payload_…` and the message now says a Value Secret holder around anything but a map is not one.
  - `[medium]` `[patch]` blind-hunter: no fire from a second Connection of the Session — added `a_fire_from_another_connection_of_the_session_runs_there_and_its_calls_go_to_it_alone` (Reconnect, fire, the `Call` and reply on the firing Connection, the registering one silent).
  - `[low]` `[patch]` blind-hunter: byte-exact name matching untested — `a_fire_for_a_plugin_this_session_has_no_ready_plugin_for_…` now fires `X`, `x ` and a fullwidth `ｘ` and expects `protocol.unknown_plugin`.
  - `[low]` `[patch]` blind-hunter: `span.in_scope` around `fetch` does nothing — removed.
  - `[low]` `[patch]` blind-hunter: the refusal ordering has no wire test — added a malformed payload naming an unknown Plugin to `a_malformed_fire_payload_…`, expecting `protocol.invalid_payload`.
  - `[medium]` `[patch]` edge-case-hunter: several large results lose the whole reply — same root cause and fix as the shared-frame finding above.
  - `[low]` `[patch]` edge-case-hunter: one fire holds its task for handlers × 1 s — same root cause as the no-overall-limit finding; documented and deferred as above.
  - `[low]` `[patch]` edge-case-hunter: the modifications depth message misstates what nests — the message now names the "`modifications` list", which is what the limit counts.
  - `[low]` `[reject]` edge-case-hunter: "event fired" is logged for a reply the connection replaces — with the shared frame the reply fits by construction (barring tens of thousands of error entries), and the connection's replacement path keeps its own handling; not worth another branch.
  - `[false]` `[reject]` edge-case-hunter: `Plugins::insert` can orphan an id under a reused name — a name is reserved per Session (`Sessions::register_plugin`) and released only before `finish` stores the Plugin (`ReleaseOnDrop` is disarmed after), and nothing unregisters until Story 6.13, so no insert ever meets a name indexed under another id.
  - `[medium]` `[patch]` verification-gap: entry-level depth refusal has no test — same as the `MAX_ENTRY_DEPTH` finding; test added.
  - `[medium]` `[patch]` verification-gap: a `params` holder around a scalar is never tested — same as the holder finding; case added.
  - `[medium]` `[patch]` verification-gap: no fire after a `ConfigUpdate` — added `a_fire_after_a_config_update_runs_under_the_updated_settings` (loops off by update → `policy.construct_disabled`; `config: {}` → back to default).
  - `[medium]` `[patch]` verification-gap (other): per-entry frame budget can lose the whole reply — same root cause and fix as the shared-frame finding.
  - `[false]` `[reject]` intent-alignment: the diff implements beyond "Halt after planning" — that instruction bounded the planning stage; this orchestrated run's later stages implement and review the story, as the workflow assigns.
  - `[low]` `[patch]` intent-alignment: the status records disagree — same as the sprint-status finding; spec `done`, sprint-status `review`.
  - `[low]` `[reject]` intent-alignment: NFR4 is shown at connection level, not against a running Daemon or per transport — the Daemon hands every connection to the same `serve` and AD-1's conformance suite already proves transports identical; a Daemon-level fire test would exercise no new code.
  - `[low]` `[reject]` intent-alignment: nothing mechanically pins `invoke_handler` as the only handler entry point — the spec keeps `RESTRICTED_NAMES` unchanged, and `hexput-enforce` stays reachable only through `hexput-exec` by the graph check; `run_fire` calling `invoke_handler` is the one path.
  - `[false]` `[reject]` intent-alignment: a handler failure is an entry in a `Result`, not an `Error` — "reported to the Backend" is met; the spec's per-entry reply is the defensible reading and lets Story 6.6 slot shape errors per handler.

## Auto Run Result

- **Summary:** `EventFire {plugin, event, params}` fires a declared Event of a ready Plugin: the connection fetches the Plugin at dispatch, `hexput-plugin` decodes and resolves it (`invalid_payload` → new `protocol.unknown_plugin` → `undeclared_event`), runs every bound handler once, in source order, sequentially, each through `hexput_exec::invoke_handler` with its own budget and copy of `params`, and answers `Result {results}` with one `{handler, value, modifications?}` or `{handler, error}` entry per handler, the entries sharing one frame.
- **Files changed:** `crates/hexput-shared/src/wire.rs` (`MessageType::EventFire`); `crates/hexput-port/src/error.rs` (`ProtocolCode::UnknownPlugin`); `crates/hexput-exec/src/wire.rs` (`Path::params()`); `crates/hexput-plugin/src/lib.rs` (name index, `named`, `handlers_for`, `fetch`/`prepare_fire`/`run_fire`/`fire_reply`, `Fire`, `HandlerOutcome`, shared-frame entries); `crates/hexput-connection/src/lib.rs` (routing, task, logging); `crates/hexput-tests/tests/{connection,plugin,shared,port}.rs` (fire tests, `ALL` pins); LANGUAGE-REFERENCE §9 and the Spine (dated decisions); `deferred-work.md` (6.4 entry settled, fire-wide limit deferred); `epic-6-context.md`; `sprint-status.yaml`; `AGENTS.md`.
- **Review findings:** 27 — 18 routed `patch` (9 medium, 9 low; grouped into 12 fixes: shared reply frame, depth tests, holder case and message, second-Connection test, ConfigUpdate test, sequencing silence check, near-miss names, ordering case, `in_scope` removal, modifications message, fire-cost documentation, sprint status); 0 deferred by route (the fire-wide limit is recorded in `deferred-work.md` as the documented follow-up of a patched low); 9 rejected — 3 false (insert orphaning, "Halt after planning", `Error` vs entry) and 6 low (handler attribution, 1:1 span, log re-parsing, log on a replaced reply, Daemon-level test, `invoke_handler` restriction), each with its reason above.
- **Follow-up review recommended:** `true` — 5 medium entries were patched (0 high). The named unverified risk: `fire_reply`'s shared-frame arithmetic (`MAX_FRAME_LEN - ENVELOPE_ROOM - headers`, exact `encoded_len` per entry) is new and only one test (two 8 MiB results) exercises it; a reviewer should confirm the bound holds at the edge (an entry just fitting) and that measuring each entry by encoding it is an acceptable cost.
- **Verification:** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 1276 passing (`--no-fail-fast`). One earlier full run failed the pre-existing Story 2.7 timing test `many_executions_in_flight_are_each_answered_once_with_their_ids` ("the slow one finishes last") under load; it passed on its own three times and in four further full runs of `tests/connection.rs` and in the full suite rerun.
- **Residual risks:** the timing-sensitive Story 2.7 test above can flake under heavy parallel load (pre-existing); the shared-frame bound is exact per entry but the payload-header allowance is a fixed constant; a fire's total cost scales with its handler count until a fire-wide limit lands.

