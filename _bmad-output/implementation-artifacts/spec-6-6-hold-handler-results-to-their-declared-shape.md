---
title: 'Story 6.6: Hold handler results to their declared shape'
type: 'feature'
created: '2026-10-23'
status: 'done'
baseline_revision: '9fe4edbd0bbc16f7c7f6128dc77b17846515c4b4'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
warnings: ['oversized']
deferred:
  - summary: >-
      AGENTS.md's Epic 6 parenthetical summary did not mention that handler results are now held to their declared return shape.
    evidence: |-
      The Project Status line said Stories 6.1-6.6 are implemented while the parenthetical listed only Story 6.1-6.5 capabilities. Agent-context edits route to defer; this story's own finalization rewrites that Project Status paragraph and adds the return-shape clause, so nothing remains open.
    location: >-
      AGENTS.md Project Status, Epic 6 paragraph
    severity: low
---

<intent-contract>

## Intent

**Problem:** Story 6.3 makes a Backend declare each Event's return shape and Story 6.5 fires Events, but a handler's result reaches the Backend unchecked, so a Plugin author's mistake corrupts the Backend's data instead of surfacing as a clear error (FR-19).

**Approach:** When a fire's reply is built, validate each successful handler's converted result against the fired Event's declared `Shape`, independently per entry. A conforming result is sent unchanged; a non-conforming one turns only that entry into `{handler, error}` with a new `protocol.shape_mismatch` naming the first offending path.

## Boundaries & Constraints

**Always:**
- Meaning of a shape is LANGUAGE-REFERENCE §9's (Story 6.3 decision), unchanged: `String`/`Number`/`Boolean` nodes accept any value of that kind (a MessagePack integer or float is a number); `Any` (`null`) accepts every value, `null` included, and is not descended into; `Array(None)` (`[]`) any array, not descended; `Array(Some(t))` an array whose every element conforms to `t` (`[]` value conforms); `Object(fields)` an object with exactly those keys, each conforming, and no other key.
- Validation runs on the **wire value** `entry_value` already produced (after `result_to_wire_within` and the `MAX_ENTRY_DEPTH` check, before `modifications` are converted), in `fire_reply` on the blocking pool. A Value Secret holder (a map with the key `__secret`, `hexput_exec::wire::SECRET_KEY`; a Hexput object never carries that key) is seen through: its `value` is validated in its place and the holder adds no path segment. Holders are never altered — a conforming entry is byte-identical to Story 6.5's.
- Error (decided): new `ProtocolCode::ShapeMismatch` = `protocol.shape_mismatch`, appended last to `ProtocolCode::ALL` (the `result_too_deep`/`response_too_large` family: a result the reply refuses to carry; deliberately not a §7 `type` code, so it is never mistaken for an ambient type mismatch). No span. Message: `` the handler's result does not conform to the `<Event>` return shape: <path> <problem> `` with exactly one of `must be a string|a number|a boolean|an array|an object; found <kind>`, `is missing`, `is not a key of the shape`. `<kind>` is `a string`, `a number`, `a boolean`, `an array`, `an object` or `null`. Event name and path rendered bounded (`bounded`/`Path`'s own limits).
- Path: root `value` (the entry's key), `.key` for an identifier-like key and `["key"]` otherwise, `[i]` for an element — exactly `hexput_exec::wire::Path`'s rendering, made reusable (below). E.g. `` `value.tags[1]` must be a string; found a number ``, `` `value` must be an object; found null ``.
- Only the first failure is reported, found deterministically in pre-order: at a node, its own failure first (kind; for an object, the first shape key absent from the value in shape order, then the first value key not in the shape in value order), then its children in value order, depth-first. The walk is iterative (explicit stack), never recursive.
- A failing entry is `{handler, error}` and carries no `modifications` (an error never does); its size is accounted against the shared frame exactly as other entries' are. Every other entry is untouched: each result is validated independently and one entry's mismatch never removes, reorders or alters a sibling.
- A handler that failed (runtime, budget, capability, policy) keeps its own error; a result an entry cannot carry keeps `result_too_deep`/`response_too_large`/`type.function_result`, which take precedence over a shape check (conversion runs first).
- The check is not charged to any Resource Budget and the handler's side effects stand (no rollback). Init (`BackendRegisteredInit`) handlers are never shape-checked — their results are discarded and no shape is declared for them.

**Never:**
- No change to how shapes are declared, parsed or stored, nor to `EventFire`'s payload, refusal order or the `{results}` reply layout.
- No validation inside the interpreter or `hexput-exec`'s execution loop, and no static (check-pass) result-shape analysis (LANGUAGE-REFERENCE §10 keeps it runtime-only).
- No crate edge change; no coercion or reshaping of a result (an extra key is an error, never stripped).

## I/O & Edge-Case Matrix

| Scenario | Shape / result | Entry |
|---|---|---|
| Conforming | `{ points: 0, tags: [""] }` / `{points: 3, tags: ["a"]}` | `{handler, value}` unchanged |
| Any | `null` / anything (incl. `null`) | unchanged |
| Missing key | `{ points: 0 }` / `{}` | `` `value.points` is missing `` |
| Extra key | `{ a: 0 }` / `{a: 1, "b-c": 2}` | `` `value["b-c"]` is not a key of the shape `` |
| Wrong type nested | `{ tags: [""] }` / `{tags: ["a", 2]}` | `` `value.tags[1]` must be a string; found a number `` |
| Root kind | `[0]` / `null` | `` `value` must be an array; found null `` |
| Holder | `{ n: 0 }` / `n` read from a Backend-secreted place | conforms, holder kept; a holder around a string → `` `value.n` must be a number; found a string `` |
| Several handlers | `{ ok: true }`; h1 `{ok: true}`, h2 `{ok: 1}`, h3 runtime error | h1 value, h2 shape_mismatch, h3 its own error, in run order |
| Mismatch + modifications | non-conforming result that wrote a referenced place | `{handler, error}`, no `modifications` |

</intent-contract>

## Code Map

- `crates/hexput-plugin/src/lib.rs` -- `Fire` (l.629–660): add a private `declaration: usize` (the index of the fired Event in `plugin.events()`, set by `prepare_fire`) and `pub fn returns(&self) -> &Shape` reading it. `fire_reply` (l.776) becomes `pub fn fire_reply(fire: &Fire, outcomes: &[HandlerOutcome]) -> Value`, using `fire.returns()` and `fire.event()` for the message. `entry_value` (l.822): after the depth check on `value`, run the shape check and return its `ErrorBody` on failure. `protocol()` (l.1160), `bounded`, `MAX_ENTRY_DEPTH` (l.141) reused. Module docs: add "# What Story 6.6 adds" after Story 6.5's section (l.70–88).
- `crates/hexput-plugin/src/shape.rs` (new, private module, re-exported fn) -- `pub fn conform(value: &Value, shape: &Shape) -> Result<(), String>` returning the `<path> <problem>` text; iterative walk over `(&Value, ShapeId)` with a `Path`. `Shape`/`ShapeNode`/`ShapeId` from `hexput_parser` (re-exported from `hexput-ast`, l.399–480).
- `crates/hexput-exec/src/wire.rs` -- `Path` (l.84–151): add public `Path::result()` (root `value`; document it as a fire entry's result), `push_key(&mut self, &'a str)`, `push_index(&mut self, usize)`, `pop(&mut self)` and `pub fn rendered(&self) -> String` (the existing private `render`), so the plugin crate reuses the one path rendering. `SECRET_KEY` (l.63) is public; make `HOLDER_VALUE_KEY` (l.65) `pub` too, so the validator names both holder keys by constant.
- `crates/hexput-port/src/error.rs` -- `ProtocolCode` (doc variants ~l.40–80, `ALL` l.86, `as_str` l.106): add `ShapeMismatch`, documented like `ResultTooDeep`.
- `crates/hexput-connection/src/lib.rs` -- `fire` (l.1036–1060): pass the Event's shape to `fire_reply`; module docs (l.94–102) mention the shape check. Its per-entry error logging already covers the new code.
- `crates/hexput-tests/tests/plugin.rs` -- `fired` helper (l.1193–1203) calls `fire_reply`; extend with shape tests. `tests/connection.rs` -- `fire`/`fire_payload` (l.7945–7960), `plugin_register_declaring` (l.6633). `tests/port.rs` l.482 pins `ProtocolCode::ALL`.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-port/src/error.rs` -- add `ShapeMismatch` (`protocol.shape_mismatch`) last in `ALL` -- the defined shape-conformance error.
- `crates/hexput-exec/src/wire.rs` -- make `Path` buildable and renderable from outside as listed -- one path rendering for every payload path.
- `crates/hexput-plugin/src/shape.rs` + `lib.rs` -- validator, `Fire::returns`, `fire_reply`/`entry_value` wiring, module docs -- FR-19's runtime check, per entry.
- `crates/hexput-connection/src/lib.rs` -- pass the shape; docs -- the fire path uses it.
- `crates/hexput-tests/tests/plugin.rs` -- direct `conform` tests for every matrix row and every shape node kind (int and float numbers; empty object shape vs `{}`; `[[0]]` nesting; non-identifier key quoting; pre-order first-failure order: own kind before children, missing before extra), plus `fired` tests for several handlers and mismatch-with-modifications; `tests/connection.rs` -- one `EventFire` over the in-memory Port with a conforming and a non-conforming handler, asserting the exact reply; `tests/port.rs`/`shared.rs` -- the `ALL` pin. Adjust any Story 6.5 test whose handlers' results no longer conform to its declared shape (declare `null` or fix the result), never weakening what it asserts.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §9 -- dated `[DECISION, 2026-10-23, Story 6.6]` bullet: when and how a result is held to its shape, the path form, the first-failure order, independence per handler, holders seen through. `ARCHITECTURE-SPINE.md` -- dated Story 6.6 amendment (new code, where the check runs, no crate edge). `AGENTS.md` Project Status -- Story 6.6 sentence and test count.

**Acceptance Criteria:**
- Given an Event declared with a return shape, when a handler returns a conforming value, then its entry is `{handler, value}` (with `modifications` when written) byte-identical to Story 6.5's.
- Given a handler returning a value with a missing key, a wrong value type or an extra key an object shape forbids, when the fire's reply is built, then that entry is `{handler, error}` whose error code is `protocol.shape_mismatch` and whose message names the offending path — never a `type.*` code.
- Given several handlers on one Event, when one result does not conform, then every other entry is exactly what it would be alone, in run order.
- Given the workspace, when the CI steps run in their `--release` form, then all pass, including `check-crate-graph.py` with no edge change.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/plugin.rs`, `crates/hexput-tests/tests/connection.rs`, `crates/hexput-tests/tests/exec.rs` and `crates/hexput-tests/tests/port.rs` (`a_conforming_object_result_is_accepted_and_a_missing_key_is_named`, `an_extra_key_the_object_shape_forbids_is_named_quoted_when_not_an_identifier`, `a_wrong_type_nested_in_an_array_is_named_by_its_full_path`, `a_root_of_the_wrong_kind_is_named_as_value_with_its_kind`, `a_number_node_accepts_an_integer_and_a_float_and_refuses_other_kinds`, `a_string_node_accepts_any_string_and_refuses_other_kinds`, `a_boolean_node_accepts_true_and_false_and_refuses_other_kinds`, `an_any_node_accepts_every_value_and_is_not_descended_into`, `an_untyped_array_node_accepts_any_array_and_is_not_descended_into`, `a_typed_array_node_accepts_every_element_conforming_and_the_empty_array`, `nested_arrays_are_checked_element_by_element_at_every_depth`, `an_empty_object_shape_accepts_only_the_empty_object`, `a_non_identifier_key_of_the_shape_is_quoted_in_its_path`, `an_overlong_key_in_a_path_is_bounded_with_an_ellipsis`, `an_own_failure_is_reported_before_the_failures_of_its_children`, `children_are_checked_in_value_order_not_shape_order`, `a_missing_key_is_the_first_shape_key_in_shape_order_then_an_extra_key_in_value_order`, `missing_is_reported_before_an_extra_key`, `the_walk_is_depth_first_so_the_first_failure_is_the_earliest_in_pre_order`, `a_holder_is_seen_through_and_adds_no_path_segment_at_the_root_or_in_an_array`, `a_holder_under_an_object_key_conforms_or_names_the_key_not_the_holder`, `a_holder_around_an_object_is_validated_in_its_place`, `a_holder_around_null_conforms_to_any_and_is_null_to_every_other_node`, `a_fire_holds_a_shaped_result_to_its_own_events_shape_and_an_unshaped_one_to_any`, `a_mismatching_entry_is_only_its_own_and_its_siblings_are_exactly_what_they_are_alone`, `a_handler_that_failed_keeps_its_own_error_and_a_result_that_is_no_wire_value_keeps_its_code`, `a_root_mismatch_names_the_root_and_its_kind_in_the_entry_error`, `a_mismatch_that_wrote_a_referenced_place_is_an_error_entry_with_no_modifications`, `a_fire_answers_a_conforming_entry_unchanged_and_a_mismatch_as_its_own_shape_mismatch`, `a_missing_key_and_an_extra_key_are_each_a_shape_mismatch_naming_their_path`, `a_holder_that_conforms_is_kept_as_it_is_in_the_entry`, `a_holder_around_a_string_under_a_number_key_is_a_shape_mismatch_naming_the_key`, `a_mismatch_that_wrote_a_referenced_parameter_is_an_error_entry_with_no_modifications`, `a_result_path_renders_its_root_and_each_segment_as_a_payload_path_does`, `protocol_codes_are_stable_and_in_the_protocol_category`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

## Spec Change Log

- 2026-10-23 (implementation): the pre-written test `a_conforming_object_result_is_accepted_and_a_missing_key_is_named` asserted that `{points: 0}` conforms to `{ points: 0, tags: [""] }`, contradicting the Always rule that an object shape requires exactly its keys; corrected to expect `` `value.tags` is missing ``. Decided: a wire map with a non-string key (never produced by result conversion) is reported at the map's path as `must be an object; found a map with a key that is not a string`; `fire_reply` takes the whole `&Fire` (shape via `Fire::returns`, Event name via `Fire::event`), as the Code Map lists.

## Review Triage Log

### 2026-10-23 — Review pass
- verdicts: 25 findings — high 0, medium 1, low 17, false 7, maybe-false 0
- findings:
  - `[low]` `[patch]` sprint-status.yaml left Story 6.6 at `ready-for-dev` while the spec is in review — set to `review` at finalization.
  - `[medium]` `[patch]` a shape-mismatch entry drops the handler's `modifications`, so the Backend's copy of a written Reference ID goes stale — the behaviour is the spec's decision (an error never carries modifications, as for a runtime failure); the consequence is now stated plainly in the LANGUAGE-REFERENCE §9 Story 6.6 bullet.
  - `[low]` `[patch]` a shape key `__secret` can never be satisfied — documented in LANGUAGE-REFERENCE §9 and pinned by `a_shape_key_named_secret_can_never_be_satisfied`; refusing it at registration would add a new refusal the spec does not settle.
  - `[low]` `[patch]` object conformance is O(shape keys × value keys), uncharged — `conform` now builds a key set of the value and a field map of the shape once per object node (linear); order of reported failures unchanged (a non-string key, an own-kind failure, is now found before the missing-key scan).
  - `[low]` `[patch]` no test of a conforming result that also wrote a referenced place — added `a_conforming_result_that_wrote_a_referenced_place_keeps_its_value_and_modifications`.
  - `[low]` `[patch]` "init handlers are never shape-checked" untested — added `an_init_handler_result_is_never_held_to_any_declared_shape` (registration succeeds with an init result no shape would accept, while `Ping` is still checked).
  - `[low]` `[patch]` connection logging of a mismatch untested — added `a_shape_mismatch_is_logged_as_a_failed_handler_with_its_code_and_counted_as_failed` (JSON log: `failed = 1`, "a handler failed" with `code = protocol.shape_mismatch`).
  - `[low]` `[patch]` `conform`'s documented message contract omitted the non-string-key message and the `binary data`/`an extension value` kinds — its doc now names them as the cases no converted result produces.
  - `[false]` `[reject]` a holder with no `value` key is silently read as `null` — `to_wire_held` always emits both `__secret` and `value`, and the checked value is `entry_value`'s own conversion output, so the fallback is unreachable.
  - `[low]` `[reject]` `Path::result()` puts reply vocabulary into `hexput-exec` and `pop()` is silent at the root — follows Story 6.5's `Path::params()` precedent; reshaping the public constructor gains nothing a reader meets, and push/pop pairing is local to one walk.
  - `[low]` `[patch]` the fired Event was looked up twice with panicking indexes — `prepare_fire` now resolves the declaration index once through a private `Plugin::declaration`, which `Plugin::event` also uses.
  - `[low]` `[patch]` unwrapped `hexput-connection` module-doc line — rewrapped at 100 columns (and the `fire` comment).
  - `[low]` `[patch]` misleading test name mentioning an "unshaped" Event — renamed to `a_fire_holds_a_shaped_result_to_its_own_events_shape_and_an_any_shaped_one_to_any`.
  - `[low]` `[reject]` mismatch-assertion helpers duplicated between `tests/plugin.rs` and `tests/connection.rs` — each follows its own file's helper conventions (`s`/`string`); separate test targets, and a shared `#[path]` module for two small assertions is more structure than the drift risk warrants.
  - `[low]` `[reject]` the spec's Tasks name `tests/shared.rs` for the `ALL` pin, which was not touched — the fix would edit this build's spec.
  - `[low]` `[patch]` (edge-case) O(K×N) object check on the blocking pool — same root cause and fix as the quadratic finding above.
  - `[low]` `[defer]` AGENTS.md Epic 6 parenthetical omits return-shape enforcement — agent-context file; resolved by this story's finalization of the Project Status paragraph (recorded in `deferred`).
  - `[false]` `[reject]` (intent) "Halt after planning" means the diff should hold only a spec — that directive governs the planning stage of the build loop; this run's step is the full build-and-review of Story 6.6, which the implementation fulfils.
  - `[low]` `[reject]` (intent) no shape case in the cross-transport conformance suite — the check runs in `hexput-plugin` behind the transport-neutral `Port` (AD-1) and is exercised end to end through `serve` over the in-memory Port; a conformance case adds no transport-specific behaviour.
  - `[false]` `[reject]` (intent) `protocol.shape_mismatch` without a span is one reading among several — the AC asks only for a defined non-`type.*` shape-conformance error, which this is; the choice is recorded in the spec and §9.
  - `[false]` `[reject]` (intent) conversion errors winning over a shape mismatch is undecided by the AC — it is decided in the spec's Always rules and §9, and is the only order in which the check can see a wire value.
  - `[false]` `[reject]` (intent) "an extra key where the shape forbids it" could imply open object shapes — §9's Story 6.3 decision makes every object shape exact; the implementation follows it.
  - `[false]` `[reject]` (intent) independence tested only for sequential handlers — sequential source-order handlers are the only dispatch that exists (async/priority are later Epic 6 stories).
  - `[false]` `[reject]` (intent) spec drift during implementation — both changes are recorded in the Spec Change Log as the process requires; the corrected test enforces the spec's exact-keys rule.
  - `[low]` `[patch]` (intent) sprint-status `ready-for-dev` inconsistent with the implemented story — same root cause and fix as the first finding.

## Design Notes

Validating the wire value rather than the interpreter's `Held` keeps the walk linear in bytes already bounded by the frame and depth limits — an interpreter value may share one collection many times (exponential walk) or be cyclic, which conversion has already refused. Paths use root `value` because the Backend reads the result under the entry's `value` key.

Example: shape `{ points: 0, tags: [""] }`, result `{points: "3", extra: 1}` → own failures first: `tags` missing → `` the handler's result does not conform to the `OrderPlaced` return shape: `value.tags` is missing ``.

## Verification

**Commands:**
- `cargo fmt --all --check` -- clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- no warnings
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked --release` -- all pass

## Auto Run Result

**Summary.** A fire's handler results are held to the fired Event's declared return shape (FR-19): `hexput_plugin::conform` walks each successful entry's converted wire value iteratively, seeing through Value Secret holders, and a non-conforming result makes only its own entry `{handler, error}` with the new `protocol.shape_mismatch`, naming the Event and the first offending path under `value`. Conforming entries are unchanged.

**Files changed.**
- `crates/hexput-plugin/src/shape.rs` — new: the conformance walk (linear per object node).
- `crates/hexput-plugin/src/lib.rs` — `Fire::returns`, one declaration lookup, `fire_reply(&Fire, …)`, the check in `entry_value`, Story 6.6 module docs.
- `crates/hexput-exec/src/wire.rs` — `Path` buildable/renderable from outside; `HOLDER_VALUE_KEY` public.
- `crates/hexput-port/src/error.rs` — `ProtocolCode::ShapeMismatch`, last in `ALL`.
- `crates/hexput-connection/src/lib.rs` — passes the fire to `fire_reply`; docs.
- `crates/hexput-tests/tests/{plugin,connection,exec,port}.rs` — conformance, fire, wire, logging and code-pin tests.
- `LANGUAGE-REFERENCE.md` §9, `ARCHITECTURE-SPINE.md`, `epic-6-context.md`, `AGENTS.md`, `sprint-status.yaml` — decision, amendment, status.

**Review findings.** 25 findings: 13 rows patched (1 medium, 12 low; two of them duplicates grouped with another row's root cause), 1 deferred (AGENTS.md wording, resolved in finalization), 11 rejected — 7 false (an unreachable holder fallback; intent-alignment readings already settled by the spec/§9 or by the build loop's stage) and 4 low not worth fixing (`Path::result` placement following the 6.5 precedent; duplicated per-target test helpers; a fix that would edit this spec; a conformance-suite case for a transport-neutral check already exercised through `serve`).

**Follow-up review recommended:** false — patched counts: high 0, medium 1, low 12; the one medium patch is documentation of a decided behaviour.

**Verification.** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (no edge change); `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 1314 passing, 0 failing.

**Residual risks.** A mismatching handler's writes to referenced places are applied in the Daemon but never reported to the Backend (documented). A declared shape with a `__secret` key is accepted at registration but can never be satisfied (documented, pinned).
