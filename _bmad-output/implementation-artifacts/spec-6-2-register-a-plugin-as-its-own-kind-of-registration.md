---
title: 'Story 6.2: Register a plugin as its own kind of registration'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '9f945942f50e9ce242f2ecdbdcddd85f1b65b966'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-1-register-a-script-once-for-repeated-use.md'
warnings: ['oversized']
deferred:
  - summary: >-
      The static check (FR-26, AD-8) does not run at Plugin registration yet.
    evidence: |-
      Decision 6 of this spec. AD-8 names `hexput-plugin` as the place the check runs at Plugin registration, and FR-26 adds Plugin-only findings: an `@Event` naming an undeclared Event, and a `@Global` strategy the Backend forbids. Both need the Event declarations of Story 6.3. Story 6.3's spec must add the check, run under the Session's live check mode, with the Plugin-only findings, so it runs once with complete inputs. Until then a `PluginRegister` under check mode `error` is accepted unchecked. Nothing can run a handler before Story 6.5, so nothing executes unchecked.
    location: >-
      crates/hexput-plugin/src/lib.rs (register)
    severity: medium
  - summary: >-
      Nothing limits how many Plugins one Session or the Daemon holds.
    evidence: |-
      Review finding. `hexput_plugin::Plugins` keeps every parsed `PluginProgram` with no per-Session or Daemon-wide limit, unlike the AST Cache's `ast_cache_capacity`, so one Backend can register Plugins under fresh names until memory runs out. The Daemon already leaves Sessions themselves unbounded (every `Init` creates one), so this is the pre-existing question of per-Backend resource limits; settle it with a System Config limit refused under a new `protocol.*` code. Recorded in deferred-work.md.
    location: >-
      crates/hexput-plugin/src/lib.rs (Plugins)
    severity: medium
---

<intent-contract>

## Intent

**Problem:** A Backend can run Scripts (`ExecutionStart`) and cache them (`CodeRegister`), but it has no way to hand the Daemon a Plugin. Story 6.1 parses Plugin source, but nothing on the wire accepts it, and no Session holds one (FR-17).

**Approach:** Add a `PluginRegister` request. Its message type alone marks it as a Plugin, so the Daemon never infers anything from the payload's shape. The Daemon decodes `{source}` and parses it with `hexput_parser::parse_plugin` on the blocking pool. It then reserves the Plugin's metadata `name` in the Session: names are unique per Client ID, and the Session owns its set of Plugins, each under a fresh `PluginId`. Last, it stores the parsed Plugin in a per-run `hexput_plugin::Plugins` registry and answers `Result {name}`. Event declarations, init handlers, firing, Global Variable state, unregister and teardown of the store are Stories 6.3–6.14.

## Boundaries & Constraints

**Always:**
- `PluginRegister` sits behind the init gate. It runs as its own task in the connection's `JoinSet`, with the request span, like `CodeRegister`. Decoding and parsing run on the blocking pool. No lock is held across an `.await`.
- The Session owns the Plugin set: `name → PluginId`. The uniqueness check and the reservation happen together under the Session's shard lock, so two concurrent registrations of one name cannot both succeed. Another Session's names are never consulted.
- The parsed Plugin is stored only after the parse and the reservation succeed. A refused registration changes neither the Session nor the `Plugins` registry.
- `Plugins` mirrors `AstCache`'s guard. `open(client_id)` runs at `Init`, next to `cache.open`. The Daemon's Session teardown hook calls `close(client_id)` after `cache.close`. An insert for a Client ID that is not open is refused, so a registration in flight when its Session ends leaves nothing behind.
- Direct, Cached and Plugin requests stay independent on one connection. Each is its own task, and none touches another's registry.

**Decisions (agent, 2026-10-22, unattended run):**
1. **Wire.**
   - `MessageType::PluginRegister`, spelled `PluginRegister`, placed after `CodeUnregister` in `ALL`.
   - The payload is exactly `{source}`: a string of Plugin source.
   - `protocol.invalid_payload` names the key for each of these: a payload that is not a map (nil reads as missing), a missing `source`, a non-string `source`, and an unknown, repeated or non-string key. Story 6.3 adds the Event declarations as a new key.
   - Success: `Result {name}`, the metadata `name` echoed. On the wire a Plugin is addressed by its name, and Story 6.13 fires and unregisters by name.
   - A parse failure is the parser's diagnostic as an `ErrorBody` (`syntax.*` or `lexical.*`, with span).
2. **Duplicate name.**
   - New `ProtocolCode::DuplicatePluginName`, spelled `protocol.duplicate_plugin_name`, added to `ALL`. Its message is ``this Session already has a Plugin named `<name>`; nothing was registered``, with the name passed through `hexput_exec::wire::bounded`.
   - Names compare exactly, by bytes: no case folding and no trimming.
3. **`PluginId`.**
   - `hexput_shared::ids::PluginId(u64)`. The Spine lists it in `ids.rs`. `hexput-port` re-exports it beside `ScriptHandle`.
   - `new`, `get`, and `Display` as decimal. It derives `Copy`, `Eq`, `Hash` and `Ord`.
   - It never goes on the wire. It is the key that Story 6.7's store and AD-4's `hexput_globalvar::teardown(plugin_id)` will use.
   - `Sessions` issues it from one `AtomicU64` that starts at 1, so an id is never reused within a run.
4. **Session side** (`hexput-session`).
   - `Session` gains `plugins: HashMap<Arc<str>, PluginId>`.
   - `Sessions::register_plugin(client_id, name: &str) -> Result<PluginId, PluginNameError>`, where `PluginNameError::{Taken, NoSession}`.
   - `Sessions::plugins(client_id) -> Option<Vec<(Arc<str>, PluginId)>>`, sorted by id.
   - `teardown` releases the map with the rest of the Session. Its doc names Story 6.13 as the place where each id's `hexput_globalvar::teardown` is called first.
   - No new crate edge.
5. **Plugin side** (`hexput-plugin`).
   - `pub struct Plugin` with `id()`, `name() -> &str` and `program() -> &PluginProgram`.
   - `pub struct Plugins`, built on `RwLock<HashMap<ClientId, HashMap<PluginId, Arc<Plugin>>>>`. A Client ID that is present is open. It has `new()`, `open`, `close` (which removes the client's map and drops it outside the lock), `get(client_id, id) -> Option<Arc<Plugin>>`, `ids(client_id) -> Vec<PluginId>` (sorted) and `len()`.
   - `pub enum Reservation { Reserved(PluginId), NameTaken, SessionEnded }`.
   - `pub fn register(payload: &Value, client_id: ClientId, plugins: &Plugins, reserve: impl FnOnce(&str) -> Reservation) -> Result<Value, Box<ErrorBody>>` is synchronous. It decodes, parses, reserves, inserts and builds `{name}`. `SessionEnded`, or an insert refused because the client is closed, is `protocol.init_not_completed` with the message "the Session has ended; nothing was registered".
   - `hexput-plugin` gains no tokio dependency. The connection wraps the call in `spawn_blocking` and maps `PluginNameError` to `Reservation`. A panic re-raises like `hexput-script`'s `blocking`.
6. **Static check deferred to Story 6.3.** See the frontmatter `deferred`. Plugin-only findings need Event declarations, which do not exist yet.
7. **Crate edges.** This story adds two edges to the Spine, recorded in a dated amendment:
   - `hexput-connection → hexput-plugin`: the connection routes the request, as `conn → script` does.
   - `hexput-plugin → hexput-port`: the payload, `Value` and `ErrorBody`, as `script → port` does.
   `check-crate-graph.py` pins `hexput-plugin` to the set {`hexput-parser`, `hexput-globalvar`, `hexput-exec`, `hexput-check`, `hexput-port`} and adds `hexput-plugin` to `hexput-connection`'s set.
8. **Logging.** On success, log "plugin registered" at `debug` with the fields `plugin` (the name) and `plugin_id`. On failure, log "a plugin registration failed" with `code`. Both go inside the request span.
9. **"Independent state" in this story** means that each Plugin is its own `Plugin` value under its own `PluginId`, with its own parsed program and Global Variable declarations. The runtime store and its isolation arrive with Story 6.7.

**Never:**
- No Event declarations, init handlers, Event firing, Global Variable store, unregister message, or `hexput_globalvar` call (Stories 6.3–6.14).
- No inference of the mode from payload shape.
- No daemon-wide name uniqueness.
- No `hexput-session → hexput-plugin` edge.
- No `unsafe`.
- No change to `CodeRegister`, `ExecutionStart` or `CachedExecutionStart` behaviour.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Register | initialized; `PluginRegister {source: "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { return 1; }"}` | `Result {name: "x"}`; `sessions.plugins(c)` = `[("x", id)]`; `plugins.get(c, id)` has name `x` and one handler | — |
| Before init | `PluginRegister` first | `protocol.init_not_completed` | nothing stored |
| Bad payload | nil / `{}` / `{source: 1}` / `{source: "…", events: []}` / repeated `source` | `protocol.invalid_payload` naming `source` or the key | nothing stored |
| Parse error | `source: "let x = 1;"` (no block) | `Error` `syntax.plugin_block_required` with span | nothing stored |
| Duplicate in Session | the same Session registers `x` twice | second → `protocol.duplicate_plugin_name` naming `x` | first Plugin untouched |
| Other Session | Session B registers `x` after A did | `Result {name: "x"}`, with a different `PluginId` | — |
| Several Plugins | one Session registers `a`, `b`, `c` | three ids, three distinct `Plugin`s, each with its own program | — |
| Session ended mid-flight | the reserve closure returns `SessionEnded`, or `close` ran before the insert | `protocol.init_not_completed` | `Plugins` holds nothing for the client |
| Teardown | the Session is torn down (no TTL, last detach) | the hook closes it; `plugins.ids(c)` is empty | — |
| Mixed modes | one connection: `PluginRegister`, `CodeRegister`, `CachedExecutionStart`, `ExecutionStart` | all answered correctly; a large Plugin source does not hold back a concurrent fast `ExecutionStart` | — |

</intent-contract>

## Code Map

- `crates/hexput-shared/src/ids.rs` -- `ScriptHandle` is the template for `PluginId`, which has no text parsing because it is not on the wire. Update the module doc's "`PluginId` lands with…" line.
- `crates/hexput-shared/src/wire.rs` -- `MessageType`, `ALL`, `as_str`. Add `PluginRegister` with a payload and reply doc, like `CodeRegister`'s.
- `crates/hexput-port/src/error.rs:15-95` -- `ProtocolCode`, `ALL`, `as_str`. Add `DuplicatePluginName`. Extend `InvalidPayload`'s doc to list `PluginRegister`.
- `crates/hexput-port/src/lib.rs:45` -- the ids re-export. Add `PluginId`.
- `crates/hexput-session/src/lib.rs` -- `Session` (L135), `Sessions` (L177, the counters), `create` (L260), `for_execution` (L501), `teardown` (L541). Add the Plugin map, `register_plugin`, `plugins` and `PluginNameError`, and a "What Story 6.2 adds" section in the crate doc.
- `crates/hexput-plugin/{Cargo.toml,src/lib.rs}` -- currently a stub with only a doc comment. Add the `hexput-port` dependency and implement Decision 5. Reuse `hexput_parser::parse_plugin` (`crates/hexput-parser/src/lib.rs:57`), `PluginProgram::metadata.name()` (`crates/hexput-ast/src/lib.rs:290-321`), `ErrorBody::from(&Diagnostic)` and `ErrorBody::from(&ProtocolError)`, and `hexput_exec::wire::bounded`.
- `crates/hexput-script/src/lib.rs` -- read-only patterns:
  - `AstCache` open, close and insert guard (L408-545);
  - `code_register` and `session_ended` (L609-660);
  - `decode_unregister`, a by-hand payload decode with messages that name keys;
  - `blocking` (spawn_blocking with panic resume).
- `crates/hexput-connection/{Cargo.toml,src/lib.rs}`:
  - `Connection` (L208) gains `plugins: &Arc<Plugins>`.
  - `serve(port, sessions, cache, plugins)` (L229).
  - `answer` (L602): add `PluginRegister` to the gated arm and to a new `Answer::RegisterPlugin(id, payload, client_id)`.
  - Spawn it like `Answer::Register` (L402) in a `register_plugin` task modelled on `register` (L799).
  - `init` (L872): `connection.plugins.open(client_id)`.
  - Update the module doc's message list.
- `crates/hexput-daemon/src/lib.rs:237-275` -- `registry(cache, plugins, ttl)`. Its hook closes both registries, the cache first. Create one `Arc<Plugins>` per run and pass it to every `serve`, including the TCP/WebSocket accept arms. The in-file tests use `registry` (L676-720).
- `scripts/check-crate-graph.py` -- the `EXACT_DEPENDENCIES` entries for `hexput-connection` and `hexput-plugin`, with a comment that cites the amendment.
- `crates/hexput-tests/Cargo.toml` -- add `hexput-plugin` as a dev-dependency.
- `crates/hexput-tests/tests/connection.rs` -- about 25 `serve(` call sites and the helpers at L93-160. Add the `Plugins` argument through the helpers.
- `crates/hexput-tests/tests/{shared,port,session,daemon}.rs` -- `shared.rs:303` spells `MessageType::ALL`. The coverage of `ProtocolCode::ALL` is there too.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` and its `.memlog.md` -- the amendment list (around L300-320) and the mermaid crate graph (L240-290).

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/{ids.rs,wire.rs}`, `crates/hexput-port/src/{error.rs,lib.rs}` -- add `PluginId`, `MessageType::PluginRegister`, `ProtocolCode::DuplicatePluginName`, and the re-export -- the wire vocabulary.
- `crates/hexput-session/src/lib.rs` -- implement Decision 4 -- per-Client-ID uniqueness, and the Session owns its Plugins (AD-4 groundwork).
- `crates/hexput-plugin/Cargo.toml`, `crates/hexput-plugin/src/lib.rs` -- implement Decision 5, and rewrite the crate doc with a "What Story 6.2 adds" section -- registration and the body registry.
- `crates/hexput-connection/Cargo.toml`, `crates/hexput-connection/src/lib.rs` -- add the `plugins` parameter, open the client at `Init`, and add a spawned `register_plugin` task with Decision 8's logging -- protocol-level routing that never blocks the loop.
- `crates/hexput-daemon/src/lib.rs` -- one `Plugins` per run, closed by the teardown hook -- wiring.
- `scripts/check-crate-graph.py` -- apply Decision 7 -- keep the AD edges pinned.
- `crates/hexput-tests/Cargo.toml` and `crates/hexput-tests/tests/{plugin,session,connection,shared,port,daemon}.rs` -- add the new `tests/plugin.rs`, update the call sites, and cover every I/O-matrix row:
  - `plugin.rs`: drive `register` directly with a stub `reserve`.
  - `session.rs`: `register_plugin`, covering taken, no-session, a second Session, and an id counter that starts at 1 and is never reused.
  - `connection.rs`: the wire rows, the mixed-mode row, and the large-source concurrency row (as in Story 4.1's test).
  - `shared.rs` and `port.rs`: spellings and `ALL` coverage.
  - `daemon.rs`: a `PluginRegister` over the socket is answered `{name}`, and teardown through the real hook empties `Plugins`.
- `ARCHITECTURE-SPINE.md` and `.memlog.md` -- add the dated amendment `[Amended 2026-10-22, Epic 6 Story 6.2]`, which fixes the `PluginRegister` wire, `protocol.duplicate_plugin_name`, `PluginId`, Session ownership of the Plugin set, and the two edges. Add the edges to the mermaid graph and bump the frontmatter `amended` -- SDKs (Epic 8) rely on it.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- append the static-check deferral for Story 6.3 -- so it is not lost.
- `AGENTS.md` -- update the Epic 6 Project Status for Story 6.2, the test count, and the crate graph note -- keep the file current.

**Acceptance Criteria:**
- Given an initialized connection, when it sends `PluginRegister {source}`, then the Daemon treats it as a Plugin registration because of the message type alone, and answers `Result {name}` with the Plugin held by that Session.
- Given a connection with a registered Plugin, when it also sends `CodeRegister`, `CachedExecutionStart` and `ExecutionStart`, then each is answered exactly as it would be without the Plugin, and a slow `PluginRegister` in flight does not hold back a fast `ExecutionStart`'s reply.
- Given Session A holding Plugin `x`, when A registers `x` again, then it is refused `protocol.duplicate_plugin_name` and A's Plugin is unchanged; when Session B registers `x`, then it succeeds under its own `PluginId`.
- Given one Session, when it registers `a`, `b` and `c`, then all three are listed by `Sessions::plugins` and retrievable from `Plugins`, each as a distinct `Plugin` with its own id and program.
- Given a Session that is torn down, when the Daemon's teardown hook runs, then `Plugins` holds nothing for its Client ID, and a registration still in flight gets `protocol.init_not_completed`.
- Given the CI commands in `--release` form, when they run, then all of them pass, and `check-crate-graph.py` passes with exactly the two amended edges added.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/plugin.rs` (new; `a_registration_answers_its_name_and_holds_the_plugin_under_its_id`, `several_plugins_are_each_distinct_with_their_own_id_and_program`, `plugins_are_held_per_client_id_and_never_seen_across_clients`, `closing_a_client_releases_its_plugins_and_no_other_clients`, `a_payload_that_is_not_exactly_source_is_refused_naming_the_key_and_stores_nothing`, `a_parse_failure_is_the_parsers_diagnostic_with_its_span_and_reserves_nothing`, `a_name_the_session_already_holds_is_refused_naming_it_and_the_first_is_untouched`, `a_session_that_ended_before_the_reservation_is_init_not_completed_and_holds_nothing`, `a_client_that_was_never_opened_or_is_closed_holds_nothing_and_is_init_not_completed`, `a_registration_whose_client_closes_before_its_insert_leaves_nothing_behind`), `crates/hexput-tests/tests/session.rs` (`a_registered_name_gets_a_fresh_id_and_the_session_lists_them_by_id`, `a_session_with_no_plugins_lists_none_and_is_not_missing`, `a_name_the_session_already_holds_is_taken_and_the_first_registration_stands`, `plugin_names_compare_exactly_by_bytes_with_no_case_folding_or_trimming`, `another_session_never_consults_these_names`, `registering_on_an_unknown_or_torn_down_session_is_no_session`, `the_plugin_id_counter_starts_at_one_and_an_id_is_never_reused_within_a_run`, `tearing_a_session_down_releases_its_plugin_names_with_it`, `concurrent_registrations_of_one_name_on_one_session_let_exactly_one_succeed`), `crates/hexput-tests/tests/connection.rs` (`a_plugin_registration_is_answered_with_its_name_and_held_by_its_session`, `a_bad_plugin_payload_is_refused_naming_its_key_and_nothing_is_held`, `a_plugin_that_does_not_parse_is_its_diagnostic_with_a_span_and_registers_nothing`, `a_duplicate_plugin_name_in_one_session_is_refused_naming_it_and_the_first_is_unchanged`, `the_same_name_on_another_connection_is_a_different_session_and_a_different_plugin`, `several_plugins_on_one_session_are_each_listed_and_retrievable_as_distinct_plugins`, `a_plugin_registration_before_init_is_init_not_completed_and_holds_nothing`, `a_plugin_held_by_a_session_is_released_when_its_session_ends_through_the_teardown_hook`, `plugin_code_cached_and_direct_requests_on_one_connection_are_each_answered`, `a_large_plugin_does_not_hold_back_a_fast_execution_on_its_connection`, `a_plugin_registration_is_logged_at_debug_under_its_request_and_a_failure_names_its_code`), `crates/hexput-tests/tests/daemon.rs` (`a_plugin_registration_over_the_socket_is_answered_with_its_name_and_names_are_per_session`, `a_plugin_registration_before_init_over_the_socket_is_init_not_completed`), `crates/hexput-tests/tests/shared.rs` (`message_types_spell_pascal_case_and_parse_exactly` updated, `plugin_id_is_a_copyable_ordered_key_with_a_decimal_display`), and `crates/hexput-tests/tests/port.rs` (`ProtocolCode::ALL` list updated, `duplicate_plugin_name_is_a_protocol_code_spelled_and_bodied_as_specified`, `the_port_re_exports_the_same_plugin_id_as_shared`); `crates/hexput-tests/Cargo.toml` gains the `hexput-plugin` dev-dependency. The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Test-agent decisions where the spec is silent: `protocol.duplicate_plugin_name` is asserted appended last in `ProtocolCode::ALL`; a non-map payload is asserted by code only; the failure log's level is not asserted; the Daemon's real teardown hook (`registry` in `hexput-daemon`) is private, so the connection test wires `Sessions::with_teardown` the same way and the real hook's effect on `Plugins` is not covered from `hexput-tests`; a registration in flight at teardown is covered at unit level in `plugin.rs`, not over a connection.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 28 findings — high 0, medium 2, low 17, false 9, maybe-false 0
- findings:
  - `[low]` `[reject]` blind: a name stays reserved in the Session when the `Plugins` insert is refused, with no rollback — `Plugins::open` runs at `Init` before the Client ID is ever sent, and `close` runs only from the teardown hook after the Session is removed, so through `serve` a successful reservation always finds the client open; reachable only by a caller misusing the public `register`, and the fix needs a new public `release_plugin`.
  - `[low]` `[patch]` blind: the window between "name reserved" and "body stored" is undocumented — added to `Sessions::plugins`'s doc that a listed id may have no body yet and that Story 6.5 must treat it as not ready.
  - `[low]` `[patch]` blind: `register_plugin` digs the name back out of the reply and has a dead `None` log arm — the reserve closure now captures `(PluginId, name)` and the log reads it with `if let`, no reply parsing and no dead arm.
  - `[medium]` `[patch]` blind: the success log carries the Backend's name unbounded (only the frame limit bounds it) — the closure stores the name through a private `logged_name` helper in `hexput-connection` (64 characters plus `…`, the same bound as `hexput_exec::wire::bounded`, which the connection cannot reach).
  - `[low]` `[reject]` blind: release-only builds drop `debug_assert!`s and overflow checks — CI (`.github/workflows`) runs `cargo test --workspace --locked` in the debug profile, so the asserts still run there; turning them on in `[profile.release]` would change the shipped binaries for a local disk constraint.
  - `[low]` `[reject]` blind: the large-Plugin concurrency test is timing-dependent — run eight times in release the Plugin's parse took 0.53–0.66 s against microseconds for `return 3;`, the same margin Story 4.1's test relies on; a gate would need a test hook in production code.
  - `[low]` `[patch]` blind: status tracking contradicts itself (sprint-status `ready-for-dev`, stale Auto Run Result) — sprint-status set to `review` and the Auto Run Result rewritten at finalization.
  - `[low]` `[patch]` blind: the `MessageType::PluginRegister` wire doc omits `protocol.init_not_completed` — added ("when the Session has ended before the Plugin is stored").
  - `[medium]` `[defer]` blind: nothing limits how many Plugins a Session can hold — real, but Sessions themselves are already unbounded per Backend, a pre-existing gap in per-Backend resource limits; recorded in frontmatter `deferred` and deferred-work.md.
  - `[low]` `[patch]` blind: no tests for Plugins across several Connections of one Session — added `a_reconnected_connection_registers_plugins_in_its_session_and_names_stay_unique_across_its_connections` (a `Reconnect`-attached Connection registers into the Session, a name held through the other Connection is `protocol.duplicate_plugin_name`); the TTL case adds nothing, since the Session and its Plugins are torn down only by the same hook.
  - `[low]` `[reject]` blind: `Plugins` puts every Session behind one global `RwLock` — Decision 5's design, mirroring `AstCache`; registrations and closes are rare and reads share the lock, and a `DashMap` rewrite is not a direct correction.
  - `[low]` `[patch]` blind: decoder messages inconsistent (invalid UTF-8 `source` reported as "not a string", no payload named) — `source` now distinguishes "is not valid UTF-8" from "must be a string", both naming the `PluginRegister` payload; `tests/plugin.rs` gained an invalid-UTF-8 case.
  - `[low]` `[patch]` blind: style — two over-long doc lines in `hexput-connection`, a stub line in `hexput-port/src/error.rs`, a separate `HashMap` import in `hexput-session` — all reflowed or merged.
  - `[low]` `[patch]` blind: the static-check deferral gives the Backend no signal — the `PluginRegister` wire doc now says the check does not run at Plugin registration yet, whatever the check mode, until Story 6.3; no debug log, since the Session's check mode is not read on this path and nothing can run a handler before Story 6.5.
  - `[low]` `[reject]` edge: a connection lost mid-registration leaves an unanswered Plugin stored, and a retry gets `duplicate_plugin_name` — the Session outlives its Connection (AD-2), the registration did complete, and the refusal truthfully reports it, as any request whose reply is lost; a cancellation check would race the same way and unregister arrives with Story 6.13.
  - `[false]` `[reject]` edge: `reserve` returning an id already held silently replaces a Plugin — ids come from the run-wide `AtomicU64` in `Sessions::register_plugin`, never reused, so through `serve` no held id is ever returned.
  - `[low]` `[reject]` edge: Session keeps a name with no body when the insert is refused — same root cause and verdict as the first blind finding.
  - `[false]` `[reject]` edge: `last_plugin` wraps at `u64::MAX` — that takes 2^64 registrations in one run (about 585 years at one per nanosecond); unreachable.
  - `[low]` `[patch]` edge: invalid-UTF-8 `source` reported as "not a string" — grouped with the blind decoder-message finding; fixed there.
  - `[false]` `[reject]` edge: `released.close` panicking skips `plugins.close` — `AstCache::close` takes no panicking path (moka invalidation and a poison-tolerant lock), so the hook runs to `plugins.close`; nothing shows a panic is reachable.
  - `[false]` `[reject]` edge: non-map and non-string-key messages do not name a key — the non-map message names `source`, the only key; a non-string key has no name to give, and the message says so.
  - `[false]` `[reject]` intent: the diff implements the story although the intent said "Halt after planning" — that instruction governed the earlier planning stage, which halted; this orchestrated run then implemented the story (spec status `in-review`), and the stale artifacts are the status finding above.
  - `[low]` `[reject]` intent: AC2's concurrency of Cached and Direct beside a Plugin is not exercised beyond one Direct execution — the spec's AC2 is covered as written; Cached/Direct concurrency is pinned by Epic 4's tests (`a_fast_cached_run_is_answered_before_a_slow_one_submitted_earlier`), and a Plugin shares no registry or task with them.
  - `[false]` `[reject]` intent: AC4's "independent state" is redefined — nothing in this story has runtime state; the epics place the Global Variable store in Story 6.7, and every state that exists now (id, program, declarations) is per Plugin and tested distinct.
  - `[false]` `[reject]` intent: additions beyond the story text (duplicate code, two registries, new edges) — design choices recorded in the spec and the Spine amendment, with no bad outcome named.
  - `[false]` `[reject]` intent: the Daemon's real teardown hook is not covered from `hexput-tests` — it is covered by the in-file unit test over the private `registry`, which is the documented exception for private items.
  - `[low]` `[patch]` intent: under check mode `error` a `PluginRegister` is accepted unchecked — grouped with the blind static-check-signal finding; documented on the wire doc; the check itself stays deferred (frontmatter `deferred`).
  - `[false]` `[reject]` intent: no evidence the CI commands ran — this pass ran all five in `--release` form; see Auto Run Result.

## Design Notes

**Why two registries.** The Spine has the Session own its Plugins, and AD-4 makes `hexput-session` the only caller of `hexput_globalvar::teardown(plugin_id)`. The Session therefore keeps the `name → PluginId` set, so Story 6.13 can call teardown per id from the Session's own teardown and unregister paths. The parsed body cannot live in `hexput-session`, because that crate cannot name a `PluginProgram`. The body lives in `hexput-plugin`'s per-run `Plugins`, keyed by Client ID like `AstCache`, and the teardown hook releases it after the store, which keeps AD-4's order: store first, then the actor. Between the reservation and the insert, a name exists in the Session without a body. Story 6.5's firing must treat that as "not ready", which Story 6.4 formalises anyway.

**Golden exchange.**
```text
→ {id: 3, type: "PluginRegister", payload: {source: "plugin { name = \"loyalty\" }\nlet points = 0\n@Event(OrderPlaced) fn award(params) { points = points + 1; return {}; }"}}
← {id: 3, type: "Result", payload: {name: "loyalty"}}
→ {id: 4, type: "PluginRegister", payload: {source: "plugin { name = \"loyalty\" }"}}
← {id: 4, type: "Error", payload: {code: "protocol.duplicate_plugin_name", …}}
```

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes with the amended sets
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass, including the new tests

## Auto Run Result

Status: done
Blocking condition: none.

**Summary.** A Backend registers a Plugin with `PluginRegister {source}` — the message type alone marks it. The Daemon parses it on the blocking pool, reserves its metadata `name` in the Session (unique per Client ID, under a fresh run-wide `PluginId`), stores the parsed `Plugin` in the per-run `hexput_plugin::Plugins` registry (opened at `Init`, closed by the Session teardown hook after the AST Cache) and answers `Result {name}`; a held name is `protocol.duplicate_plugin_name`, a Session gone mid-flight `protocol.init_not_completed`. The static check at registration is deferred to Story 6.3.

**Files changed.**
- `crates/hexput-shared/src/{ids.rs,wire.rs}` — `PluginId`; `MessageType::PluginRegister` with its wire doc.
- `crates/hexput-port/src/{error.rs,lib.rs}` — `ProtocolCode::DuplicatePluginName`; `PluginId` re-export.
- `crates/hexput-session/src/lib.rs` — the Session's `name → PluginId` set, `register_plugin`, `plugins`, `PluginNameError`.
- `crates/hexput-plugin/{Cargo.toml,src/lib.rs}` — `Plugin`, `Plugins`, `Reservation`, `register`.
- `crates/hexput-connection/{Cargo.toml,src/lib.rs}` — the `plugins` argument of `serve`, the gated `PluginRegister` task, opening at `Init`, logging.
- `crates/hexput-daemon/src/lib.rs` — one `Plugins` per run, closed by the teardown hook.
- `scripts/check-crate-graph.py` — the two amended edges pinned.
- `crates/hexput-tests/{Cargo.toml,tests/*.rs}` — new `plugin.rs`, plus session, connection, daemon, shared and port tests.
- Spine, `.memlog.md`, `deferred-work.md`, `epic-6-context.md`, `sprint-status.yaml`, `AGENTS.md` — amendment, deferrals, tracking.

**Review findings.** 28 findings (medium 2, low 17, false 9). Patched: 11 rows in 9 entries — unbounded log name (medium); dead log arm; undocumented reserve/store gap; status tracking; wire doc missing `init_not_completed`; multi-Connection test; decoder messages incl. invalid UTF-8 (2 rows); style; static-check signal (2 rows) — the last eight low. Deferred: 1 (no limit on Plugins per Session, medium). Rejected: 16 — 7 low (insert-refused rollback, twice: unreachable through `serve` and needs new public surface; release-only asserts: CI runs the debug profile; concurrency-test timing: 0.5 s margin measured; global `RwLock`: the spec's design, rare writes; lost-reply retry: the refusal is truthful; AC2 Cached concurrency: pinned by Epic 4) and 9 `false`, each refutation in the Review Triage Log.

**Follow-up review recommended:** false — first pass, patched entries by verdict: high 0, medium 1, low 8; no high and fewer than two medium.

**Verification.** All in `--release` form, after the patches: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (27 AD edges, 4 restricted names); `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` 1151 passed, 0 failed. The large-Plugin concurrency test was run eight more times in release, passing each time.

**Residual risks.** Plugins are accepted unchecked under any check mode until Story 6.3; the Plugin count per Session is unbounded (deferred); `Sessions::plugins` can list an id whose body is not stored yet, which Story 6.5 must handle.
