---
title: 'Story 4.1: Register a script once for repeated use'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_commit: '1ea97d403330baa9bdec6904c42abc2d529b9ea8'
baseline_revision: '1ea97d403330baa9bdec6904c42abc2d529b9ea8'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-4-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-3-10-turn-the-static-check-on-or-off.md'
warnings: ['oversized']
deferred:
  - summary: >-
      The AST Cache has no capacity bound and no Session-teardown cleanup, so an initialized Backend can grow Daemon memory without limit by registering repeatedly.
    evidence: |-
      AstCache::insert stores every registration in an unbounded moka cache and nothing removes entries until the Daemon exits. The intent assigns bounds, unregister and Session cleanup to Story 4.3, which must close this before a release ships Cached Execution.
    location: >-
      crates/hexput-script/src/lib.rs (AstCache::new / AstCache::insert)
    severity: medium
  - summary: >-
      A registration still parsing when its connection closes or is lost inserts its entry after the Session may be gone, under a handle nobody was told about.
    evidence: |-
      code_register inserts on the blocking pool once parse and check succeed, independent of whether the reply future is still awaited; the connection drains or drops its JoinSet before detaching, but a spawn_blocking closure runs to completion either way. Story 4.3's teardown cleanup (by Client ID) must run after, or guard against, such late inserts.
    location: >-
      crates/hexput-script/src/lib.rs (code_register) with crates/hexput-connection/src/lib.rs (register / detach)
    severity: medium
  - summary: >-
      The `variables` names a registration was checked against are not stored, so Story 4.2 must decide whether a CachedExecutionStart's variables are validated against them.
    evidence: |-
      The entry is only Arc<Program>; the check's no-false-positive judgement on undeclared names assumed the registered names. Running a registered Script is Story 4.2's scope; its spec must record whether to store and enforce the names or deliberately not.
    location: >-
      crates/hexput-script/src/lib.rs (code_register)
    severity: low
---

<intent-contract>

## Intent

**Problem:** A Backend that evaluates the same rule on every request must send and parse its source each time (Direct Execution). Nothing lets it pay the parse once (FR-5, UJ-2).

**Approach:** Add a `CodeRegister` request. The Daemon parses the source, runs the static check if the Session's check mode asks for it (AD-8: the check runs at registration, never at Cached Execution), stores the AST in a moka-backed AST Cache keyed by `(Client ID, handle)`, and answers with a fresh opaque handle. Running a registered Script (`CachedExecutionStart`) is Story 4.2. Bounds, unregister and Session cleanup are Story 4.3.

## Boundaries & Constraints

**Always:**
- `hexput-script` owns the cache: one `AstCache` per Daemon run, created by `hexput-daemon` beside `Sessions` and shared with every connection through `hexput_connection::serve`.
- An entry is keyed by the registering Session's `ClientId` and its handle. A lookup under another Client ID never finds it.
- Every `CodeRegister` creates a new entry and a new handle, even when the source is the same.
- `CodeRegister` sits behind the init gate.
- Parsing and checking run on the blocking pool, in a task the connection spawns like an execution, so a large source never stalls the loop (NFR5). No lock is held across an `.await`.
- An entry is inserted only after the parse and the check succeed. A refused registration leaves the cache exactly as it was.
- The check mode, the feature toggles and the registration names are read from the Session's live Config at registration (`Sessions::for_execution`, AD-5). Nothing about them is stored in the entry.
- Direct Execution never touches the cache (FR-4).

**Decisions (agent, 2026-10-22, unattended run; Erdem delegated these: "kendin tahmin et"):**
1. **Wire.**
   - Request: `MessageType::CodeRegister` (spelled `CodeRegister`). The payload is a map with the key `source` (a string, required) and the key `variables` (optional).
   - `variables` is an array of starting-variable **names**: §2 identifiers (`hexput_parser::is_identifier`), each given once. Absent or nil means none. Only the static check uses it, because a check needs to know which names exist and never what they hold (Spine 2026-09-22 amendment). It is a list of names, not `ExecutionStart`'s name→value map: a registration has no values.
   - Every other refusal is `protocol.invalid_payload` and names the key, the index or the name: a payload that is not a map, a missing `source`, an unknown, repeated or non-string key, a `source` that is not a string, a `variables` that is not an array, and an item that is not an identifier string or is repeated.
   - Success: `Result {handle: "<16 lowercase hex>"}`. When the check ran and found anything: `Result {handle, findings}`.
   - Before init the request is refused with `protocol.init_not_completed`.
2. **Handle.**
   - `hexput_shared::ids::ScriptHandle(u64)`, re-exported by `hexput-port` together with `ClientId`.
   - Its text form is exactly 16 lowercase hex characters (`Display`/`FromStr`, `ParseScriptHandleError`, serde as a string, like `ClientId`). It is a string so that JavaScript can hold it and so that SDKs treat it as opaque.
   - It is issued from one `AtomicU64` counter per `AstCache`, starting at 1. A handle is therefore never reused during a Daemon run, so a stale handle can never name a later Script. That matters for Story 4.2's unknown-handle error.
   - It is not a secret. The Client ID scopes the lookup.
3. **Static check at registration.** It is the same pass as Direct Execution's and the same rules apply (Story 3.10):
   - mode `off`: no pass runs;
   - the callable list is the Session's Registered Functions' names;
   - the policy is `Policy::from_features` of the Session's toggles;
   - the starting-variable names are those listed in `variables`;
   - nothing is declared keyed, because there are no values. The `method_override` finding stays silent, which is correct: never a false positive;
   - at most `MAX_FINDINGS` findings, fitted to a frame;
   - under `error`, an error finding rejects the registration exactly like a Direct Execution rejection: the first finding's body, with every finding under `findings`, logged at `debug`. No entry is created.

   It is not charged to any budget and its findings are never cached. `CodeRegister` takes no `overrides`: nothing runs at registration, and the check mode comes from the Session's Config (AD-8).
4. **Cache.**
   - `moka::sync::Cache<(ClientId, ScriptHandle), Arc<Program>>`, with no capacity bound and no eviction yet. Story 4.3 adds the configured capacity, eviction metrics, unregister and Session-teardown cleanup.
   - Until Story 4.3 lands, entries live until the Daemon exits. This is a known limitation of this story alone.
   - The workspace pin becomes `moka = { version = "0.12.16", features = ["sync"] }`. `hexput-script` gains moka as an external dependency, so no workspace edge changes.
5. **Logging.** A successful registration is logged at `debug` as "script registered" with a `handle` field, inside the request span, so the Client ID is attributed (FR-12). A refusal logs at `debug` like an execution failure.

**Never:**
- no `CachedExecutionStart` and no execution of a registered Script (that is 4.2);
- no capacity, eviction, unregister or teardown hook (that is 4.3);
- no deduplication by source;
- no re-parse anywhere but `CodeRegister`;
- no new crate edge;
- no `hexput-session` → `hexput-script` reach;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Register | initialized; `CodeRegister {source: "return 1 + x;"}` | `Result {handle}` (16 hex); `AstCache::get(client_id, handle)` returns the program |
| Twice, same source | the same request twice on one Session | two different handles, two entries |
| Parse error | `source: "let = ;"` | `Error` carrying the parser's diagnostic (`syntax.*`, with span); entry count unchanged |
| Before init | `CodeRegister` first | `protocol.init_not_completed`; no entry |
| Bad payload | nil / `{}` / `{source: 1}` / `{source: "1;", x: 1}` / `{source: "1;", variables: {}}` / `variables: ["a", "a"]` / `variables: ["1a"]` | `protocol.invalid_payload`, naming `source` / `variables` / the item; no entry |
| Check `warn` | Config `check: "warn"`; source `let unused = 1; return x;`, `variables: ["x"]` | `Result {handle, findings: [reference.unused_variable warning]}`; entry created |
| Check `error` rejects | Config `check: "error"`; `return y;` with no `variables` | `Error` `reference.undeclared_identifier` with `findings`; no entry |
| Check `error`, names given | the same with `variables: ["y"]` | `Result {handle}` (no `findings`) |
| Check `off` | default Config; `return y;` | `Result {handle}`; no pass ran |
| Two Sessions | Sessions A and B each register an identical source | different handles; `get(A, hB)` and `get(B, hA)` are `None`; `handles(A)` lists only A's |
| Direct Execution | initialized; an `ExecutionStart` runs successfully | `AstCache::entry_count()` stays 0 |

</intent-contract>

## Code Map

- `crates/hexput-shared/src/ids.rs` -- `ClientId` (text form, `ParseClientIdError`, serde) is the template for `ScriptHandle`.
- `crates/hexput-shared/src/wire.rs:54-108` -- `MessageType` enum, `ALL`, `as_str`. Add `CodeRegister` after `ConfigUpdate`, with a doc comment for the payload and its reply.
- `crates/hexput-port/src/lib.rs` -- the re-exports. Add `pub use hexput_shared::ids::{ClientId, ScriptHandle, ParseScriptHandleError};`. The Spine names `hexput-port` as the place where ids are defined once.
- `crates/hexput-script/src/lib.rs` -- `direct_execution`, and its private helpers to reuse:
  - `static_check` (generalise it so it takes the starting-variable names and the keyed pairs instead of `&[(Arc<str>, Held)]`, and keep Direct Execution's behaviour exactly);
  - `Rejection`;
  - `fitted` / `with_findings`;
  - `blocking`;
  - `present` / `missing`;
  - `wire::bounded`;
  - `findings_value`.

  The crate doc's "Not yet" paragraph must be rewritten.
- `crates/hexput-script/Cargo.toml` -- add `moka = { workspace = true }` (external).
- `Cargo.toml:58` -- `moka` pin, which needs the `sync` feature. `Cargo.lock` gains moka (crates.io is reachable).
- `crates/hexput-connection/src/lib.rs` -- `serve`, `exchange`, `answer`, `Answer::Execute`, `execute`. Model `CodeRegister` on the `ExecutionStart` arm:
  - read `for_execution(client_id)`;
  - spawn a task in `running`, instrumented with the request span;
  - write its envelope when it finishes.

  The task needs `Arc<AstCache>` and the Client ID. Update the module doc's list of what a connection is answered.
- `crates/hexput-daemon/src/lib.rs:240-250` -- creates `Arc<Sessions>` per run. Create `Arc<AstCache>` beside it and pass it to `serve`.
- `crates/hexput-session/src/lib.rs:217` -- `Sessions::for_execution` gives the registrations and settings, read once.
- `crates/hexput-check/src/lib.rs:121-210` -- `Environment::with_variables` / `with_callables`.
- `crates/hexput-tests/tests/connection.rs:93-160,1047,1271` -- the `serve` helpers and direct calls. Add an `AstCache` argument, or make a helper that returns the cache for inspection.
- `crates/hexput-tests/tests/script.rs`, `crates/hexput-tests/tests/shared.rs`, `crates/hexput-tests/tests/port.rs` -- where the new tests go. `shared.rs:254-290` enumerates `MessageType::ALL` spellings.
- `scripts/check-crate-graph.py` -- the exact sets stay unchanged. Run it to confirm.

## Tasks & Acceptance

**Execution:**
- `Cargo.toml`, `crates/hexput-script/Cargo.toml`, `Cargo.lock` -- add the moka `sync` feature and dependency -- the cache.
- `crates/hexput-shared/src/ids.rs`, `crates/hexput-shared/src/wire.rs`, `crates/hexput-port/src/lib.rs` -- add `ScriptHandle` and `MessageType::CodeRegister`, and add the re-exports -- the wire vocabulary.
- `crates/hexput-script/src/lib.rs` -- add `pub struct AstCache` with:
  - `new()`;
  - `get(ClientId, ScriptHandle) -> Option<Arc<Program>>`;
  - `handles(ClientId) -> Vec<ScriptHandle>`;
  - `entry_count() -> u64` (exact: call `run_pending_tasks` first).

  Add `pub async fn code_register(payload: Value, client_id: ClientId, cache: Arc<AstCache>, registrations: Vec<Registration>, settings: Settings) -> Result<Value, Box<ErrorBody>>`, which decodes, parses and checks on the blocking pool, inserts the entry, and replies. Share the check and the findings fitting with `direct_execution`. Document the errors as `direct_execution` does -- FR-5 registration (AD-8).
- `crates/hexput-connection/src/lib.rs` -- change the signature to `serve(port, sessions, cache: Arc<AstCache>)`, route `CodeRegister` past the gate to a spawned registration task, and log "script registered" or the failure at `debug` -- dispatch without blocking the loop.
- `crates/hexput-daemon/src/lib.rs` -- create one `AstCache` per run and pass it to every connection -- wiring.
- `crates/hexput-tests/tests/{shared,port,script,connection,daemon}.rs` -- update the call sites, and cover:
  - every row of the I/O matrix;
  - the `ScriptHandle` text form: round trip; rejecting the wrong length, uppercase and non-hex; serde as a string;
  - `CodeRegister` in `MessageType::ALL` and its spelling;
  - a large registration that does not block a concurrent fast `ExecutionStart` on the same connection -- the evidence.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` and its `.memlog.md` -- add a dated amendment `[Amended 2026-10-22, Epic 4 Story 4.1]` that fixes the `CodeRegister` wire, the handle form and where the cache lives, and update the frontmatter `amended` -- SDKs (Epic 8) rely on it.
- `AGENTS.md` -- update Project Status with an Epic 4 paragraph (Story 4.1) and the test count -- keep the file current.

**Acceptance Criteria:**
- Given an initialized connection, when it sends `CodeRegister {source}`, then the Daemon parses the source once, stores the AST in the AST Cache under the Session's Client ID, and answers `Result {handle}` with a handle that names exactly that entry.
- Given a `CodeRegister` whose source fails to parse, or whose Script the static check rejects under `error`, when it is processed, then the structured `Error` is returned and no cache entry is created.
- Given two Sessions that register identical source, when their entries are inspected, then each is scoped to its own Client ID, the handles differ, and neither Session's lookup finds the other's entry.
- Given a slow registration and a fast Direct Execution on one connection, when both are in flight, then the Direct Execution's reply is not held behind the registration.
- Given the CI commands, when they run, then all pass, and `check-crate-graph.py` passes with no edge added.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/connection.rs`, `crates/hexput-tests/tests/script.rs`, `crates/hexput-tests/tests/shared.rs`, `crates/hexput-tests/tests/port.rs` and `crates/hexput-tests/tests/daemon.rs` (`a_registration_parses_once_and_its_handle_names_the_cached_program`, `every_registration_gets_a_new_handle_even_for_the_same_source`, `a_source_that_fails_to_parse_is_answered_with_its_diagnostic_and_creates_no_entry`, `a_registration_before_init_is_refused_and_creates_no_entry`, `a_malformed_registration_is_refused_naming_what_is_wrong_and_creates_no_entry`, `under_warn_a_registration_is_cached_and_answered_with_its_findings`, `under_error_an_error_finding_rejects_the_registration_and_creates_no_entry`, `under_error_naming_the_starting_variables_is_accepted_without_findings`, `with_the_check_off_no_pass_runs_and_the_registration_is_plain`, `two_sessions_registering_the_same_source_get_entries_only_they_can_find`, `a_direct_execution_never_touches_the_cache`, `a_large_registration_does_not_hold_back_a_fast_execution_on_its_connection`, `a_successful_registration_is_logged_at_debug_under_its_request`; in script.rs `a_registration_parses_once_into_the_cache_under_its_client_id`, `every_registration_of_the_same_source_gets_its_own_handle`, `the_first_handle_of_a_cache_is_0000000000000001`, `a_parse_failure_is_the_parser_diagnostic_and_creates_no_entry`, `a_malformed_registration_is_refused_naming_the_key_and_creates_no_entry`, `under_warn_the_registration_is_cached_and_its_findings_come_with_the_handle`, `under_error_an_error_finding_rejects_the_registration_with_every_finding`, `under_error_naming_the_starting_variables_is_accepted_without_findings`, `with_the_check_off_no_pass_runs`, `the_registrations_are_the_callable_names_the_check_knows`, `a_large_warn_result_keeps_at_most_max_findings`, `two_clients_registering_the_same_source_are_scoped_apart`; in shared.rs `script_handle_text_form_is_16_lowercase_hex_and_round_trips`, `script_handle_rejects_malformed_text`, `script_handle_serializes_as_a_string`, and the `MessageType` spelling test now expects `CodeRegister` after `ConfigUpdate`; in port.rs `port_re_exports_the_script_handle_and_the_client_id`, `a_code_register_envelope_is_spelled_code_register_on_the_wire`; in daemon.rs `a_registration_over_the_socket_is_answered_with_a_handle_from_one_counter_per_run`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 25 findings — high 0, medium 8, low 14, false 3, maybe-false 0
- findings:
  - `[low]` `[patch]` (blind) sprint-status had the story at `ready-for-dev` and `epic-4` at `backlog` — set to `review` and `in-progress` at finalization.
  - `[low]` `[patch]` (blind) `code_register` re-implemented findings fitting with a clone-and-encode instead of sharing Direct Execution's rule — extracted the private `push_findings(fields, size, findings)` used by both success replies; no clone of the findings.
  - `[medium]` `[patch]` (blind) the "findings would not fit a frame" branch of a registration was untested — added `registration_findings_that_would_not_fit_a_frame_are_left_out_and_the_handle_wins` (100 unused locals with ~168 KB names).
  - `[low]` `[patch]` (blind) the "script registered" log took the handle by position — now looks the field up by the `handle` key.
  - `[low]` `[patch]` (blind) the malformed-registration test's `sessions.is_empty()` was vacuous — every case now runs against the outer `sessions`.
  - `[low]` `[reject]` (blind) the concurrency test depends on a 200k-statement parse outlasting a one-line execution and does not prove the blocking pool — the margin is two orders of magnitude and it asserts exactly the AC (reply not held back); a deterministic gate would add a test hook to production code.
  - `[medium]` `[patch]` (blind) no test that method registrations stay off the registration check's callable list — same root as the verification-gap row below; added `a_method_is_no_callable_name_for_the_registration_check`.
  - `[medium]` `[patch]` (blind) no test that registration reads the live Config (AD-5) or applies the toggles — added `a_registration_reads_the_live_config_its_check_mode_and_toggles` (ConfigUpdate to `error`, then `features.loops = false` → `policy.construct_disabled`).
  - `[low]` `[patch]` (blind) the Spine did not say a check-mode change never re-checks a registered Script — one sentence added to the 2026-10-22 amendment (and AGENTS.md).
  - `[low]` `[defer]` (blind) the registered `variables` names are discarded with no hand-off to 4.2 — running a registered Script is outside this intent (Story 4.2); deferred for 4.2's spec.
  - `[medium]` `[defer]` (blind) orphaned entries and an unbounded cache with no interim guard — the intent assigns bounds and cleanup to Story 4.3; deferred (two items) for 4.3.
  - `[low]` `[patch]` (blind) `handles` scans the whole cache with no cost note; `Debug` reports an approximate count — documented the cost on `handles` and renamed the Debug field `approximate_entries`.
  - `[low]` `[patch]` (blind) ScriptHandle tests miss 0, the high nibble and `u64::MAX`; handle 0 undocumented — added the three edges to the round-trip test and documented that every value parses, 0 included, though never issued.
  - `[medium]` `[defer]` (edge) an entry inserted after the stream is lost is never freed — same root as the orphan deferral; Session teardown cleanup is Story 4.3's.
  - `[medium]` `[defer]` (edge) unbounded cache growth from repeated 16 MiB registrations — intent excludes bounds (Story 4.3); deferred.
  - `[low]` `[reject]` (edge) the `[1, 3, 2]` order can flake on a fast machine — same reasoning as the blind-layer row: large margin, determinism would need a production hook.
  - `[false]` `[reject]` (edge) `fetch_add` wrapping reuses handles — reaching 2^64 registrations is unreachable (about 584 years at 10^9 registrations a second).
  - `[medium]` `[patch]` (verification-gap) the method filter on the registration path had no test — added `a_method_is_no_callable_name_for_the_registration_check` (`Registration::method("User", "save", true)`, `error` → `capability.unknown_function`, no entry).
  - `[medium]` `[patch]` (verification-gap) no connection test passes the Session's Registered Functions into a registration — added `under_error_a_registered_function_is_callable_in_a_registration`.
  - `[low]` `[patch]` (verification-gap other) the vacuous `sessions.is_empty()` assertion — same fix as the blind-layer row.
  - `[false]` `[reject]` (intent) the diff implements code despite "halt after planning" — the earlier planning-only halt was lifted by commit 1ea97d4 ("unpark Stories 4.1 and 4.2") and this run's orchestrator directs implementation and finalization; the stale Auto Run Result text is rewritten below.
  - `[low]` `[patch]` (intent) tracking state disagrees (sprint-status `ready-for-dev`, spec `in-review`) — same fix as the first row.
  - `[low]` `[reject]` (intent) test names saying "parses once" claim more than they check — the parse does happen exactly once, at registration, and no other path exists until 4.2; renaming is cosmetic churn.
  - `[false]` `[reject]` (intent) `variables`, the registration check, the handle form and non-blocking dispatch exceed the story — all are in this spec's `<intent-contract>` Approach and Decisions, which the intent delegated.
  - `[low]` `[reject]` (intent) the two-Session test's Sessions do not overlap in time — scoping is by key, so lookups behave identically whether or not the Sessions coexist; overlapping them adds harness complexity for no new evidence.

## Design Notes

**Placement.** The cache lives in `hexput-script`, because the Spine places the AST cache there. It is not inside `Sessions`, because `hexput-session` cannot name a `Program`. Keying by `ClientId` is what lets Story 4.3 drop a Session's entries without a new edge: for example, `serve`'s detach path, or a teardown callback that the Daemon wires up. That choice belongs to Story 4.3.

**Golden reply.**
```text
→ {id: 7, type: "CodeRegister", payload: {source: "return grade >= 50;", variables: ["grade"]}}
← {id: 7, type: "Result", payload: {handle: "0000000000000001"}}
```

**Why `variables` is names only.** Without it, a check under `warn` or `error` would report every input as undeclared. With values, a registration would have something to evaluate, and the check path promises that nothing runs.

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

**Summary.** Story 4.1 adds `CodeRegister {source, variables?}`: the Daemon parses the source once on the blocking pool, runs the static check under the Session's live check mode (AD-8, AD-5), stores the AST in the per-run `hexput_script::AstCache` keyed by `(ClientId, ScriptHandle)`, and answers `{handle}` (with `findings` when the pass found any). A parse error or an `error`-mode rejection creates no entry. An earlier planning-only halt was lifted when the story was unparked (commit 1ea97d4); step 3 implemented the spec and this review pass patched it.

**Files changed.**
- `Cargo.toml`, `Cargo.lock`, `crates/hexput-script/Cargo.toml` -- moka `sync` feature; external dependency of `hexput-script`.
- `crates/hexput-shared/src/ids.rs` -- `ScriptHandle` (16 lowercase hex text form).
- `crates/hexput-shared/src/wire.rs` -- `MessageType::CodeRegister`.
- `crates/hexput-port/src/lib.rs` -- re-exports `ClientId`, `ScriptHandle`, `ParseScriptHandleError`.
- `crates/hexput-script/src/lib.rs` -- `AstCache`, `code_register`, the shared `static_check` and `push_findings`.
- `crates/hexput-connection/src/lib.rs` -- `serve(port, sessions, cache)`; registrations as spawned tasks; debug logging.
- `crates/hexput-daemon/src/lib.rs` -- one `AstCache` per run.
- `crates/hexput-tests/tests/{shared,port,script,connection,daemon}.rs` -- the Story 4.1 tests.
- `ARCHITECTURE-SPINE.md` and `.memlog.md` -- the dated 2026-10-22 amendment.
- `AGENTS.md`, `sprint-status.yaml`, `epic-4-context.md`, this spec -- status and context.

**Review findings.** 25 findings. Patches applied: 14 rows in 11 entries (medium 4: frame-drop test, method-filter test, live-Config test, registered-function connection test; low 7: tracking state, shared findings fitting, log lookup by key, vacuous assertion, Spine sentence, `handles` cost note and Debug label, ScriptHandle edges). Deferred: 3 items (unbounded cache, late insert after a lost connection -- both Story 4.3; registered `variables` names -- Story 4.2). Rejected: concurrency-test timing (x2, large margin, a deterministic gate would need a production hook), handle counter overflow (false, unreachable), "halt after planning" (false, unparked by 1ea97d4), scope additions (false, inside the intent-contract), "parses once" test names (cosmetic), non-overlapping Sessions in the scoping test (no new evidence).

**Follow-up review recommendation:** false. Patched at entry verdict: high 0, medium 4 entries, low 7 entries. Every patch is a test, a doc line or a behaviour-preserving extraction (`push_findings`) that existing tests (`findings_that_would_not_fit_a_frame_are_left_out_and_the_value_wins`) and the new frame-drop test pin, so no specific unverified risk can be named.

**Verification.** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `python3 scripts/check-crate-graph.py`, `cargo build --workspace --all-targets --locked`, `cargo test --workspace --locked` -- all pass; 867 tests.

**Residual risks.** The AST Cache is unbounded and has no Session cleanup until Story 4.3; this story should not ship in a release on its own. `a_large_registration_does_not_hold_back_a_fast_execution_on_its_connection` relies on a large timing margin rather than a gate.
