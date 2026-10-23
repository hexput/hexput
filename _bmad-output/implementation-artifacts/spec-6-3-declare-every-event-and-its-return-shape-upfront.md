---
title: 'Story 6.3: Declare every event and its return shape upfront'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'c07139e4b9e0097070b4439d53001dbae8714768'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-6-2-register-a-plugin-as-its-own-kind-of-registration.md'
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
warnings: ['oversized']
deferred: []
---

<intent-contract>

## Intent

**Problem:** A `PluginRegister` today carries only `{source}`. The Backend cannot declare which Events it may fire, what each Event returns, or whether an Event needs a partition key (FR-19, FR-25). So the Plugin's interface would only be discovered at runtime. The static check at Plugin registration (AD-8, FR-26) was deferred to this story, because its Plugin-only finding needs these declarations.

**Approach:** The `PluginRegister` payload becomes exactly `{source, events}`. `events` is required, and each Event entry gives a return shape and an optional `keyed` flag. A return shape is written as a Hexput literal used as a template, parsed by the language's own parser, with no second schema language. `hexput-plugin` decodes the declarations, stores them on the `Plugin`, and resolves an Event name for a later fire. It also runs the static check under the Session's live check mode, with one Plugin-only finding: an `@Event` naming an Event that was never declared.

## Boundaries & Constraints

**Always:**
- `events` is a map from Event name to declaration. `events: {}` is valid: only `BackendRegisteredInit` handlers can then ever run. An absent or nil `events` is `protocol.invalid_payload` naming `events`.
- An Event name must be a §2 identifier (`hexput_parser::is_identifier`), appear once, and must not be `BackendRegisteredInit`, which is reserved.
- A declaration is a map with exactly these keys:
  - `returns`: required, a string holding a shape;
  - `keyed`: optional bool, where absent or nil means `false`.
- Every refusal is `protocol.invalid_payload` and names the full path in backticks, for example `` `events.OrderPlaced.returns` ``. Echoed names are bounded with `hexput_exec::wire::bounded`. The payload is decoded fully before the source is parsed. Nothing is reserved or stored on any failure.
- Shapes, from Decision 2: they are stored as a flat arena (no recursive `Box`), and are parsed and converted without host recursion. A shape nested deeper than `hexput_exec::wire::MAX_RESULT_DEPTH` is refused, because no result could ever match it.
- Static check:
  - It runs only when the Session's check mode at dispatch is not `off`, with the same AD-5 read as `CodeRegister` (`session_now`).
  - The pass gets no starting variables. The callable list is the Session's Registered Function names (methods excluded, as in `code_register`), and the policy comes from `Policy::from_features`.
  - It also gets the declared Event names.
  - Findings use the very same bounding, rejection and frame-fitting rules as Story 3.10/4.1, so the code is shared, not copied.
  - The check runs after parsing and before the name is reserved.
- The Plugin-only finding is a **warning**, `reference.undeclared_event`. A handler for an undeclared Event can never run but can never fail, which is LANGUAGE-REFERENCE §10's severity rule. `BackendRegisteredInit` handlers are never reported.
- Under the plugin check, Global Variables and `@Event` handler functions are never reported as unused: the Daemon reads and calls them across invocations. Helpers and locals are reported as before.
- A successful reply is `Result {name}`, with `findings` appended only when the pass found anything.

**Never:**
- No Event firing message and no handler execution: those are Stories 6.4 and 6.5. No result conformance checking, which is Story 6.6. No `@Global` "strategy the Backend forbids" finding, because that Config lands with Stories 6.8–6.10.
- No new crate edge and no new Cargo dependency.
- No change to any `ExecutionStart` or `CodeRegister` reply byte.
- No `overrides` key on `PluginRegister`. An unknown key stays refused.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Declared | `{source, events: {OrderPlaced: {returns: "{ points: 0, tags: [\"\"] }", keyed: true}}}` | `Result {name}`. `Plugin::events()` lists `OrderPlaced` with its parsed shape and `keyed == true`. | None |
| No custom Events | `events: {}` | Registered. `Plugin::event("X")` is refused. | None |
| Events missing | `{source}` or `events: nil` | Nothing registered | `protocol.invalid_payload` naming `events` |
| Bad declarations | `events` not a map; a name that is not an identifier, repeated, or `BackendRegisteredInit`; a declaration that is not a map; `returns` missing or not a string; `keyed` not a bool; unknown, repeated or non-string key | Nothing registered | `invalid_payload` naming the path (`events.X.keyed`, …) |
| Bad shape | `returns: "{ a: f() }"`, `"[1, 2]"`, `"x"`, `"{ a: 1 } 2"`, `"{ a: 1, a: 2 }"`, or nesting past `MAX_RESULT_DEPTH` | Nothing registered | `invalid_payload`: `` `events.X.returns` is not a return shape: <parser message> (line L, column C) `` |
| Fire lookup | `Plugin::event("Undeclared")` / `("BackendRegisteredInit")` | `Err` body | `protocol.undeclared_event`, naming the Event (bounded). The reserved name gets its own message: `` `BackendRegisteredInit` is reserved; a Backend cannot fire it `` |
| Check off | Handler `@Event(Typo)`, mode `off` | `Result {name}`, no `findings` | None |
| Check warn | Same, mode `warn` | `Result {name, findings: [reference.undeclared_event …]}`, spanned on `Typo` in the annotation | None |
| Check error | `error` mode, handler calls an unregistered name `g()` | Nothing reserved | First error finding's body with every finding under `findings`, logged at `debug` ("rejected by the static check") |
| Globals and handlers unused | `let a = 1;` never read, and the handler is never called by the source, under `warn` | No `unused_variable` for `a` or for the handler; an unused helper `fn h()` is still reported | None |

</intent-contract>

## Code Map

- `crates/hexput-plugin/src/lib.rs` -- `register` (L~195) and `decode` (L~230) take only `{source}`. Extend them to `{source, events}`. Add `EventDeclaration`/`Plugin::events()`/`Plugin::event()`. Add the check, with its inputs taken as parameters. Add a "What Story 6.3 adds" section to the crate doc.
- `crates/hexput-ast/src/lib.rs` -- the Plugin AST lives here (L~245-391). Add the `Shape` arena type here as data only, where Story 6.6 can reach it through `hexput-parser`/`hexput-plugin`.
- `crates/hexput-parser/src/{lib.rs,plugin.rs,expressions.rs}` -- `parse_plugin` (lib.rs L57), and `check_global_initializer` (plugin.rs L557) as the model for walking an expression and refusing non-literals. Add `pub fn parse_shape(&str) -> Result<Shape, Diagnostic>` and re-export `Shape`.
- `crates/hexput-shared/src/diagnostics.rs` -- the `Code` constants and `Code::ALL` (L~163-187, 340-356). Add `syntax.invalid_shape` and `reference.undeclared_event`.
- `crates/hexput-check/src/{lib.rs,pass.rs}` -- the following anchors:
  - `check` (lib.rs L~290);
  - `Environment` (L~120);
  - `pass::run` (L77);
  - `close` reports unused (L~325);
  - `declare(…, report_unused)` (L369).
  
  Add `Environment::with_events` (an `Option` list, like `callables`) and `check_plugin(&PluginProgram, &Environment, &Policy)`. That function runs the same pass over `program.program`, exempts root-scope Global Variable and handler names from unused, and adds the undeclared-event findings in source order.
- `crates/hexput-script/src/lib.rs` -- `MAX_FINDINGS` (L128), `ENVELOPE_ROOM`, `Rejection`/`with_findings`/`fitted`/`push_findings`/`encoded_len`/`rejected` (L821-881) and `static_check` (L914). Move the mode-independent findings shaping into `hexput-port`, keep `pub use` of `MAX_FINDINGS`, and leave the behaviour byte-identical.
- `crates/hexput-port/src/{lib.rs,error.rs}` -- `ProtocolCode` (`ALL`, `as_str`, L~15-110) gains `UndeclaredEvent` (appended last). The `InvalidPayload` doc mentions `PluginRegister.events`. Add a `findings` module, re-exported at the crate root.
- `crates/hexput-connection/src/lib.rs` -- the following anchors:
  - `Answer::RegisterPlugin` dispatch (L440): pass `session_now(connection)` like `Answer::Register` (L424).
  - `register_plugin` (L880): forward the registrations and settings, and log a rejection on the async task.
  - Update the message list in the module doc.
- `crates/hexput-daemon/src/lib.rs` -- its in-file tests send `PluginRegister`. Update their payloads.
- `crates/hexput-tests/tests/{plugin,connection,daemon,shared,port,parser,check,script}.rs` -- the existing Story 6.2 payloads are `{source}` only (`plugin.rs:24` helper) and must gain `events`. `script.rs` findings tests must still pass unchanged.
- `ARCHITECTURE-SPINE.md` + `.memlog.md`, `LANGUAGE-REFERENCE.md` §9/§10 -- dated amendments.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- the Story 6.2 deferral entry for the static check is settled. Note it as settled there.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/diagnostics.rs` -- add the two `Code`s with docs and add them to `ALL` -- a stable vocabulary.
- `crates/hexput-ast/src/lib.rs` -- add `Shape` -- data only. It has a flat `nodes: Vec<ShapeNode>` with `root`, and `ShapeNode::{String, Number, Boolean, Any, Array(Option<ShapeId>), Object(Vec<(String, ShapeId)>)}` with keys in source order, plus `depth()`.
- `crates/hexput-parser/src/` -- implement `parse_shape` per Decision 2, iteratively, refusing trailing tokens. Its diagnostics are lexical, or `syntax.*` including `syntax.invalid_shape` spanned on the offending node -- a shape reuses the language's literal syntax.
- `crates/hexput-check/src/` -- add `with_events`, `check_plugin`, the unused exemption and the `reference.undeclared_event` warning. The message is `` `Typo` is not an Event the Backend declared; this handler never runs ``, spanned on `EventHandler::event` -- AD-8's Plugin-only finding.
- `crates/hexput-port/src/` -- add `UndeclaredEvent` and the shared `findings` module, which holds:
  - `MAX_FINDINGS`;
  - converting a `&[Diagnostic]` under a reject flag into kept bodies, or a rejection `{body, found}`;
  - `fitted`;
  - `push_findings`.
  
  -- one rule for both submission paths.
- `crates/hexput-script/src/lib.rs` -- use the port module and keep `hexput_script::MAX_FINDINGS` as a re-export -- no behaviour change.
- `crates/hexput-plugin/src/lib.rs` -- implement the payload, the shape conversion with path-named errors, `EventDeclaration { name, returns: Shape, keyed }`, `Plugin::events()` (in payload order) and `Plugin::event(&str) -> Result<&EventDeclaration, Box<ErrorBody>>`. Run the check through `hexput_check::check_plugin` and the port findings module. `register` gains the Registered Function names and `Settings` as parameters, and returns the rejection so the caller can log it -- FR-19, FR-25, AD-8.
- `crates/hexput-connection/src/lib.rs` -- wire `session_now` into `register_plugin`. Log "rejected by the static check" (`code`, `findings`) at `debug` on the async task -- AD-5, keeping the log under the request span.
- `crates/hexput-daemon/src/lib.rs` -- update the payloads in the in-file tests.
- `crates/hexput-tests/tests/*.rs` -- update the Story 6.2 payloads, and cover every matrix row:
  - `plugin.rs`: decode, shape, `event()` and check, including `MAX_FINDINGS` bounding;
  - `parser.rs`: `parse_shape` accept and refuse, plus a 100 000-deep nesting that neither panics nor overflows;
  - `check.rs`: `check_plugin` exemptions and the warning, plus the `None` events list suppressing it;
  - `connection.rs`: `warn`/`error`/`off` over the wire, and the check reading the live Config after a `ConfigUpdate`;
  - `shared.rs`/`port.rs`: `Code::ALL` and `ProtocolCode::ALL` coverage and spellings.
- `ARCHITECTURE-SPINE.md` + `.memlog.md` -- add `[Amended 2026-10-22, Epic 6 Story 6.3]`, which fixes the `events` wire, the shape template, `protocol.undeclared_event` and the check at Plugin registration, and bump `amended` -- SDKs rely on it.
- `LANGUAGE-REFERENCE.md` -- add a dated §9 decision defining return shapes (Decision 2), and a §10 note for the plugin check (severity, unused exemption) -- the language document is normative.
- `AGENTS.md` -- update the Epic 6 status for Story 6.3 and the test count.

**Acceptance Criteria:**
- Given an initialized connection, when it sends `PluginRegister` without `events`, then the Daemon answers `protocol.invalid_payload` naming `events`, and no Plugin name is reserved in the Session.
- Given a Plugin registered with `events: {OrderPlaced: {returns: "{ points: 0 }"}}`, when the declaration is resolved for `OrderPlaced`, `Refund` and `BackendRegisteredInit`, then the first yields its shape with `keyed == false`, and the others are refused `protocol.undeclared_event`.
- Given a return shape, when it is declared, then it is written in Hexput literal syntax and parsed by `hexput-parser`, with no separate schema grammar anywhere in the Daemon.
- Given `events: {Tick: {returns: "null", keyed: true}}`, when the Plugin is registered, then `Plugin::event("Tick")` records `keyed == true`.
- Given a Session whose Config is updated to `check = "error"` after `Init`, when a Plugin calling an unregistered function is registered, then it is rejected under the check with findings, and a later identical registration under `check = "off"` succeeds.
- Given the CI commands in `--release` form, when they run, then all of them pass, and `check-crate-graph.py` passes with no edge changes.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/plugin.rs` (`a_payload_that_is_not_exactly_source_and_events_is_refused_naming_the_key_and_stores_nothing`, `a_payload_with_bad_events_is_refused_before_its_source_is_parsed`, `a_declared_event_is_held_with_its_parsed_shape_and_keyed_flag`, `no_custom_events_registers_and_every_fire_is_refused`, `missing_events_are_refused_naming_events_and_nothing_is_reserved`, `bad_event_declarations_are_refused_naming_their_path_and_nothing_is_registered`, `keyed_nil_reads_as_false_and_keyed_true_is_recorded`, `bad_return_shapes_are_refused_naming_the_path_and_the_parser_message`, `an_event_is_resolved_only_when_declared_and_the_reserved_name_says_so`, `an_undeclared_event_name_in_an_error_is_bounded`, `check_off_registers_a_handler_for_an_undeclared_event_with_no_findings`, `check_warn_registers_with_a_reference_undeclared_event_finding_spanned_on_its_name`, `check_error_rejects_with_every_finding_and_reserves_nothing_until_an_off_registration_succeeds`, `a_listed_callable_is_silent_and_an_unlisted_one_is_the_error`, `declared_events_and_the_reserved_handler_are_silent_under_warn`, `under_warn_empty_events_leave_every_ordinary_handler_undeclared_and_never_report_the_reserved_one`, `under_warn_globals_and_handlers_are_never_unused_but_a_helper_is`, `a_handler_reading_a_global_is_clean`, `findings_are_bounded_to_max_findings_in_source_order_and_a_rejection_keeps_its_error_last`; the Story 6.2 tests were updated to the `{source, events}` payload), `crates/hexput-tests/tests/check.rs` (the `check_plugin` tests from `a_global_nothing_reads_is_exempt_from_unused_under_the_plugin_check` through `a_construct_the_policy_disables_is_reported_inside_a_handler`), `crates/hexput-tests/tests/parser.rs` (`every_literal_and_collection_form_of_a_shape_is_accepted`, `a_literal_says_its_type_null_says_any_and_brackets_say_arrays_and_objects`, `a_shape_is_a_flat_arena_whose_object_keys_keep_their_source_order`, `a_node_that_is_not_a_literal_or_collection_is_syntax_invalid_shape_on_that_node`, `trailing_tokens_repeated_keys_and_lexical_errors_are_refused_by_the_language_codes`, `a_hundred_thousand_levels_of_shape_neither_panic_nor_overflow`), `crates/hexput-tests/tests/connection.rs` (`a_plugin_without_events_is_refused_naming_events_and_no_name_is_reserved`, `a_plugin_declaring_its_events_is_held_with_them`, `check_warn_answers_the_name_with_its_findings_and_a_later_off_registration_answers_the_name_alone`, `check_error_after_a_config_update_rejects_then_an_off_update_accepts_the_same_source`, `check_off_answers_the_name_alone_for_a_handler_of_an_undeclared_event`; `bad_plugin_payloads` gained the `events` rows), `crates/hexput-tests/tests/daemon.rs` (`plugin_register_envelope` now sends `events: {}`), `crates/hexput-tests/tests/shared.rs` (`plugin_declaration_codes_are_stable_and_distinct`, and the two codes added to `every_code_is_enumerated_exactly_once`), `crates/hexput-tests/tests/port.rs` (`protocol.undeclared_event` added to the `ProtocolCode::ALL` list). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Signatures the tests assume, to be matched by the implementer (each is confined to one helper where possible): `hexput_plugin::register(payload, client_id, plugins, reserve, callables: &[String], settings: &Settings) -> Result<Value, Box<ErrorBody>>` (only `register_under` in `plugin.rs` spells it); `Plugin::events() -> &[EventDeclaration]` in payload order, `EventDeclaration { name, returns: Shape, keyed: bool }` with `name` dereferencing to `str`; `Plugin::event(&str) -> Result<&EventDeclaration, Box<ErrorBody>>`; `Shape` is `PartialEq + Debug`, has `nodes`, `root`, `depth()`, and `ShapeId` has `index()` (only the `node` helper in `parser.rs` depends on that last one); `Environment::with_events(iter of names)` with `events()` as the `None`-able list; `hexput_check::check_plugin(&PluginProgram, &Environment, &Policy) -> Findings`; `Code::INVALID_SHAPE`, `Code::UNDECLARED_EVENT`, `ProtocolCode::UndeclaredEvent`.
- Depth convention: `n` nested containers have `Shape::depth() == n`, matching the result-depth convention in `script.rs`, so `MAX_RESULT_DEPTH` nested shapes are accepted and `MAX_RESULT_DEPTH + 1` refused by the plugin. `parse_shape` may accept or refuse 100 000 levels; the test only requires no panic and, on refusal, a `syntax.*` code.
- The `reference.undeclared_event` severity is asserted as `"warning"` on the wire and `Severity::Warning` in `hexput-check`.
- Implementer decisions (agent, 2026-10-22, unattended run):
  - Three pre-written tests contradicted the normative documents and were corrected without weakening them. `check.rs::a_helper_nothing_reads_is_still_unused_under_the_plugin_check` and `plugin.rs::under_warn_globals_and_handlers_are_never_unused_but_a_helper_is` put a top-level `fn` straight after a handler's `}`; §2/§9 require a `;` between declarations except before an annotation, so the sources gained that `;`. `plugin.rs::check_error_rejects_with_every_finding_…` and `a_listed_callable_is_silent_and_an_unlisted_one_is_the_error` registered a `Ping` handler with `events: {}`, which this spec makes an undeclared-Event warning (as `under_warn_empty_events_…` asserts), contradicting their exact-findings assertions; they now declare `Ping`.
  - `register` keeps the test signature (`Result<Value, Box<ErrorBody>>`); `try_register` returns `Refusal::{Refused, Rejected(Rejection)}` so the connection can log "rejected by the static check" (`code`, `findings`) on the async task, and `register` is it with `Refusal::into_body`. A connection test (`a_plugin_rejected_by_the_static_check_is_logged_at_debug_under_its_request_with_its_findings`) pins that log.
  - Decode order: both keys missing are named together; otherwise `source` is judged (missing, then type) before `events` is required, so `{source: 1}` names `source` as the Story 6.2 connection row expects.
  - `hexput_port::findings` is a public module (`shape`, `Rejection`, `fitted`, `with_findings`, `push_findings`, `encoded_len`, `ENVELOPE_ROOM`) with `MAX_FINDINGS` also re-exported at the crate root; `hexput_script::MAX_FINDINGS` re-exports it.
  - The reserved name is `hexput_ast::BACKEND_REGISTERED_INIT`, re-exported by `hexput-parser`, used by `hexput-check` and `hexput-plugin`. A shape deeper than `MAX_RESULT_DEPTH` is refused with the same `` `events.X.returns` is not a return shape: … (line 1, column 1) `` form.

## Spec Change Log

## Review Triage Log

### 2026-10-23 — Review pass
- verdicts: 33 findings — high 0, medium 4, low 21, false 8, maybe-false 0
- findings:
  - `[medium]` `[defer]` (blind) AGENTS.md still says the static check at Plugin registration is deferred to Story 6.3 — real stale sentence in the Story 6.2 text; agent-context file, so routed defer, and resolved by this run's required AGENTS.md finalization (the sentence now says Story 6.3 settled it; the 6.3 text already names the new surface and codes). Not added to `deferred`, as nothing remains.
  - `[low]` `[patch]` (blind) sprint-status moves 6.3 to `ready-for-dev` — confirmed; set to `review`.
  - `[low]` `[patch]` (blind) epic-6-context's `PluginRegister` error list omits `protocol.invalid_payload` and the `error`-mode rejection — confirmed; the bullet now lists both.
  - `[low]` `[patch]` (blind) the tuple refusal is spanned on the second element, so `[f(), 2]` reports `2` — confirmed in `convert`; the tuple is now spanned on the whole array, which starts before any element (first non-shape node in source order).
  - `[low]` `[patch]` (blind) `-x`/`-"a"`/`-(1)` are described as "an operator other than `-`" — confirmed in `describe`; reworded to "an operator that is not `-` directly before a number literal".
  - `[low]` `[patch]` (blind) parser tests pin spans only for `{ a: f() }` and `x` — confirmed; span assertions added for tuples, unary forms, groups and nested names.
  - `[low]` `[reject]` (blind) the depth refusal appends a made-up "(line 1, column 1)" — real, but the arena carries no spans and the spec's matrix/implementer notes fix this exact form; a fix would edit this build's spec, and the message is still actionable.
  - `[low]` `[patch]` (blind) `syntax.invalid_shape` never reaches the wire as a code — confirmed (folded into `protocol.invalid_payload` as the spec's matrix requires); `Code::INVALID_SHAPE`'s doc now says it is carried in that message, not as a wire code.
  - `[low]` `[patch]` (blind) a non-identifier Event name is quoted with `{:?}` rather than a backticked path — confirmed, contradicting the spec's "names the full path in backticks"; now `` `events.<name>` is not a Hexput identifier … `` and the test pins the backticked path. The non-string-key refusals name `` `events` ``/the declaration path, which is the deepest path that exists.
  - `[low]` `[reject]` (blind) no limit on the number or width of Event declarations — real but bounded by the 16 MiB frame from an authenticated Backend; the fix adds a new limit (guards, settings), so a low unlikely to be met in everyday use is rejected; it belongs with the Story 6.2 deferred "how many Plugins a Session holds" entry when Session-level limits are designed.
  - `[low]` `[patch]` (blind) the shape vocabulary's limits (identifier keys only, no map/optional/exact-null form) are unrecorded — confirmed; LANGUAGE-REFERENCE §9's Story 6.3 decision now lists them, with `null` as the escape hatch.
  - `[low]` `[patch]` (blind) `Shape`'s public fields allow malformed arenas, and `depth()` maps an out-of-range id to `0` — confirmed; the type's doc now states the well-formedness `parse_shape` guarantees and what `node`/`depth` do on a hand-built shape that breaks it (the spec fixes the public `nodes`/`root`/`index()` surface the tests use).
  - `[medium]` `[patch]` (blind) no wire-level test of the Session's registrations feeding the plugin check — same root cause as the verification-gap callable-list finding; tests added (below).
  - `[low]` `[reject]` (blind) the unused exemption for Global Variables has a weak rationale — the exemption is a frozen `<intent-contract>` rule (Globals persist across invocations and Stories 6.8–6.10 give them Backend-visible lifetimes); changing it would edit the spec.
  - `[low]` `[patch]` (edge) a nested `fn` is reported before an earlier non-shape node (`{ a: foo, b: fn() {} }`) — confirmed; the expression parser stops at `fn` before conversion, and making it convert first would restructure the parser, so the `parse_shape` doc now states this one exception to source order.
  - `[low]` `[reject]` (edge) an object shape may declare a `__secret` key no result can carry — real but unlikely, and the fix adds a guard branch; Story 6.6's conformance will report the mismatch.
  - `[low]` `[patch]` (edge) the tuple error spans the second element — same root cause as the blind tuple finding; fixed there.
  - `[low]` `[reject]` (edge) a non-UTF-8 `events` key is described as "not a string" — real, unlikely (a MessagePack str that is not UTF-8), fix adds a branch.
  - `[low]` `[reject]` (edge) two long Event names may truncate to the same bounded prefix in a message — real, cosmetic; `seen` keeps full names so detection is correct, and every echoed name is bounded the same way across the wire.
  - `[low]` `[patch]` (edge) claim "spanned on the first non-shape node in source order" fails for a nested `fn` — same root cause as the nested-`fn` finding; documented there.
  - `[medium]` `[patch]` (verification-gap) the connection's callable list for the plugin check is never tested over the wire — tests added: `a_registered_function_of_the_session_is_callable_under_the_plugin_check` (Init registers `g`, check `error`, `Result {name}` with no findings) and `a_registered_method_of_the_same_name_is_not_callable_under_the_plugin_check` (only a method `g` keyed `User`, rejected `capability.unknown_function`).
  - `[medium]` `[patch]` (verification-gap) the feature toggles never reach the plugin check in a test — added `plugin.rs::a_construct_the_sessions_features_disable_is_the_checks_error` (`features.loops` off, check `error`, `policy.construct_disabled`, nothing reserved) and the wire test `the_sessions_feature_toggles_reach_the_plugin_check`.
  - `[low]` `[patch]` (verification-gap) source-order merging of `reference.undeclared_event` with other findings is unasserted — added `check.rs::an_undeclared_event_finding_takes_its_place_in_source_order_among_the_others`.
  - `[low]` `[patch]` (verification-gap) the negation guard and function branch of `parse_shape` are unpinned — the refused list now covers `-x`, `-"a"`, `-(1)`, `!true`, `(1)`, `fn() {}`.
  - `[low]` `[patch]` (verification-gap, other) a non-identifier Event name lacks a backticked path — same root cause as the blind finding; fixed there.
  - `[false]` `[reject]` (intent) scope: "halt after planning" versus a built story — the orchestrated run plans in one stage and implements and reviews in the next (this step); the planning stage halted as asked.
  - `[false]` `[reject]` (intent) the status artifacts disagree (spec `in-review`, sprint-status `ready-for-dev`) — the disagreement was real and is patched by the sprint-status row above; nothing further.
  - `[false]` `[reject]` (intent) AC2 tested only at library level — no fire message exists before Story 6.5 (spec Decision 3); the decision point and `protocol.undeclared_event` are what this story owns.
  - `[false]` `[reject]` (intent) AC3: a new `Shape` type versus "the existing representation" — the syntax and parser are reused unchanged; `Shape` is the parsed form, not a second grammar, which is what the spec's AC states.
  - `[false]` `[reject]` (intent) AC1: omission refused rather than accepted — one of the two outcomes the AC allows (spec Decision 1).
  - `[false]` `[reject]` (intent) AC4 recorded but not enforced — matches the AC's wording; enforcement is Story 6.5's.
  - `[false]` `[reject]` (intent) the static check and the findings move are beyond the epic text — the Story 6.2 deferral hands the check to this story and the intent contract includes it; the move is byte-identical (Script tests unchanged).
  - `[false]` `[reject]` (intent) AGENTS.md's Build section lacks `--release` — the section documents CI's commands; `--release` is this machine's disk constraint, applied to every command this run executed.

## Design Notes

**Decision 1 — wire.** `events` is required: absent declarations would make "nothing declared" ambiguous with "forgot to declare", and FR-19 says declaration is never optional. `{}` is the explicit "no custom Events". The partition-key flag is `keyed`, mirroring `@Global(behavior = "keyed")`.

```text
→ {id: 5, type: "PluginRegister", payload: {source: "…", events: {
     OrderPlaced: {returns: "{ points: 0, tags: [\"\"], note: null }", keyed: true},
     Ping: {returns: "null"}}}}
← {id: 5, type: "Result", payload: {name: "loyalty"}}
```

**Decision 2 — a return shape is a Hexput literal used as a template** (the PRD addendum's "object literal as a template"). It is written as source text, so the parser is reused and the Language Server can later share it. The only nodes allowed are literals, `-<number>`, and array and object literals. Anything else is `syntax.invalid_shape`.

Meaning, which Story 6.6 enforces:

- A string, number or boolean literal means a value of that type; the literal's value itself is irrelevant.
- `null` means any value, including `null`.
- An object means exactly its keys, each conforming, with any other key forbidden; `{}` is an empty object.
- `[]` means any array, and `[t]` means every element conforms to `t`. Two or more elements are refused: a shape is not a tuple language.

The root may be any shape.

**Decision 3 — a fire of an undeclared Event** has no wire message yet, so 6.3 owns the decision (`Plugin::event`) and the code `protocol.undeclared_event`. Story 6.5 wires `EventFire` to it.

**Decision 4 — the findings shaping moves to `hexput-port`.** It must not be duplicated into `hexput-plugin`: AD-8 exists so the submission paths cannot disagree, and `hexput-plugin` has no `hexput-script` edge.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: pass, unchanged edges
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass, including the updated Story 6.2 and 3.10/4.1 findings tests

## Auto Run Result

**Summary.** `PluginRegister` is exactly `{source, events}`; each Event declares a return shape (a Hexput literal template parsed by `hexput_parser::parse_shape` into the flat `hexput_ast::Shape` arena) and an optional `keyed` flag; `Plugin::events()`/`Plugin::event()` hold and resolve them (`protocol.undeclared_event`). The static check runs at Plugin registration under the Session's live check mode via `hexput_check::check_plugin`, with the Plugin-only warning `reference.undeclared_event` and the Global/handler unused exemption; findings shaping moved to `hexput_port::findings`, shared with Scripts byte-identically.

**Files changed.**
- `crates/hexput-ast/src/lib.rs` — `Shape`/`ShapeNode`/`ShapeId`, `BACKEND_REGISTERED_INIT`.
- `crates/hexput-parser/src/{lib.rs,shape.rs}` — `parse_shape`, iterative conversion.
- `crates/hexput-shared/src/{diagnostics.rs,wire.rs}` — `syntax.invalid_shape`, `reference.undeclared_event`; `PluginRegister` doc.
- `crates/hexput-check/src/{lib.rs,pass.rs}` — `Environment::with_events`, `check_plugin`, unused exemption.
- `crates/hexput-port/src/{lib.rs,error.rs,findings.rs}` — `ProtocolCode::UndeclaredEvent`, shared findings module.
- `crates/hexput-script/src/lib.rs` — uses the port findings module.
- `crates/hexput-plugin/src/lib.rs` — `events` decoding, shapes, `EventDeclaration`, `try_register`/`Refusal`, the check.
- `crates/hexput-connection/src/lib.rs` — `session_now` into Plugin registration, rejection log.
- `crates/hexput-daemon/src/lib.rs` — test payloads.
- `crates/hexput-tests/tests/{plugin,connection,check,parser,shared,port,daemon}.rs` — coverage.
- Spine + memlog, LANGUAGE-REFERENCE §9/§10, epic-6-context, deferred-work, sprint-status, AGENTS.md — documentation.

**Review findings.** 33 findings: 18 patched (3 medium, 15 low; several share a root cause), 0 deferred to the list (the one `defer`-routed AGENTS.md finding was resolved by finalization), rejected: depth-refusal location (spec fixes the form), no Event-count limit (low, adds a limit), Global unused rationale (frozen intent), `__secret` shape key (low, adds a guard), non-UTF-8 key message (low, adds a branch), truncated-name collision (cosmetic), and 8 `false` intent-alignment observations, each with its reason in the triage log.

**Follow-up review recommendation:** `false`. Patched by verdict: high 0, medium 3 rows in two entries (the callable-list wiring, the feature-toggle wiring), low 15. The two medium entries were verification gaps; the tests that close them now pass, so no specific unverified risk remains to name.

**Verification.** All in `--release` form: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK, unchanged edges; `cargo build --workspace --all-targets --locked --release` success; `cargo test --workspace --locked --release` 1200 passed, 0 failed.

**Residual risks.** A shape's vocabulary is deliberately small (documented in §9; `null` is the escape hatch) and Story 6.6 must enforce it; the depth refusal's "(line 1, column 1)" is a nominal location; nothing yet bounds how many Events a Plugin declares beyond the frame size.
