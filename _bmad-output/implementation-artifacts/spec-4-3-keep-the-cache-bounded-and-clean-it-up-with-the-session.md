---
title: 'Story 4.3: Keep the cache bounded and clean it up with the Session'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'cb492dadfc12e66318fb50eddbc911ce9aacd321'
review_loop_iteration: 1
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-4-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-2-re-run-a-registered-script-with-fresh-variables.md'
warnings: ['oversized']
deferred:
  - summary: >-
      No test interleaves an AstCache insert with a close of the same Session, so a regression that checks the open set and then inserts without holding its read lock would pass every test.
    evidence: |-
      Blind-hunter and verification-gap findings (the gap layer's filed disposition was defer). The code is correct as written: the read lock covers the open-set check and moka's insert. The late-registration tests (a_registration_for_a_session_never_opened_or_already_ended_inserts_nothing, a_session_whose_only_connection_closes_cleanly_leaves_no_entry) are serial. A deterministic interleaving needs a test seam inside AstCache::insert or a stress loop, and the spec settles neither.
    location: >-
      crates/hexput-script/src/lib.rs (AstCache::insert / AstCache::close)
    severity: medium
---

<intent-contract>

## Intent

**Problem:** Today the `AstCache` has no limit. An entry stays until the Daemon exits, with no way to unregister it and no cleanup when its Session ends, so a long-running Daemon can grow without bound (Story 4.3, FR-5).

**Approach:**
- Bound the cache by a System Config capacity. Eviction follows moka's own policy, and every capacity eviction is counted so Epic 7's metrics can read it.
- Add a `CodeUnregister {handle}` request that removes an entry at once.
- Release all of a Session's entries when that Session is torn down. `hexput-session` calls a teardown hook that the Daemon wires to the cache, so no `hexput-session → hexput-script` edge is needed.
- Close the race where a registration still in flight could insert an entry after its Session has ended.

## Boundaries & Constraints

**Always:**
- **Removing an entry never breaks a run that already started.** A run that fetched its `Arc<Program>` keeps it and finishes normally (this is the deferred test from Story 4.2).
- **Every missing handle gets the same answer.** An evicted, unregistered or torn-down handle is answered `protocol.unknown_script_handle` (Story 4.2's code and message), exactly like a handle that was never issued.
- **Removal releases memory now.** After any removal (unregister, teardown, eviction), `AstCache` runs `run_pending_tasks()` before returning. Otherwise moka's internal write log can keep holding a clone of the value.
- **No entry survives its Session.**
  - `AstCache` keeps a set of open Client IDs.
  - `open(client_id)` is called by `hexput-connection`'s `init` right after `Sessions::create`.
  - `close(client_id)` is called through the teardown hook. It removes the ID from the open set under the write lock, then invalidates every entry keyed by that ID.
  - `insert` keeps the read lock while it inserts into moka. That makes an insert and a `close` strictly ordered.
  - An insert for an ID that is not open is refused. `code_register` then answers `protocol.init_not_completed` ("the Session has ended; nothing was registered").
  - This works even when the connection task was aborted, because the insert happens inside the blocking closure.
- **The teardown hook lives in `hexput-session`.** `Sessions::with_teardown(hook: impl Fn(ClientId) + Send + Sync + 'static) -> Self` stores the hook, and the one private `teardown` calls it with the Client ID, outside any lock, after the Config and registrations are released. When Story 5.8 delays teardown behind a TTL, the call moves with it. If no hook is set, nothing is called.
- **Locking.**
  - No lock is held across an `.await`.
  - The open-set lock is a std `RwLock`, and it is never held while calling user code.
  - The eviction listener only increments an atomic counter and logs.

**Decisions (agent, 2026-10-22, unattended run, recorded here):**
1. **Capacity.**
   - The capacity counts entries, not bytes. Each entry's source is already bounded by `MAX_FRAME_LEN`, and an entry count is predictable for operators and in tests.
   - It is a new top-level System Config key, `ast_cache_capacity`: a whole number, at least 1, default `DEFAULT_AST_CACHE_CAPACITY = 10_000` in `hexput-config`. It is validated like `session_ttl_secs`, naming the field and its position.
   - `AstCache::new()` and `Default` are removed. `AstCache::with_capacity(u64)` is the only constructor, so the default lives in exactly one place.
   - `insert` runs pending maintenance, so the capacity holds as soon as a registration returns.
   - moka's TinyLFU may refuse to admit the new entry rather than evict an old one. In that case the handle just issued is unknown at once. This is "the cache's policy", and an SDK re-registers.
2. **Metrics.**
   - There is no metrics surface before Epic 7.
   - `AstCache::evictions() -> u64` counts entries the capacity removed (moka `RemovalCause::Size`, including a refused admission). It runs pending tasks first, as `entry_count` does.
   - Unregister and teardown are not evictions, so they are not counted. Tests rely on the invariant `entry_count() + evictions()` = entries inserted − unregistered − torn down. If moka 0.12.16 does not notify the listener for a refused admission, the implementer makes the counter account for it anyway (verify, do not assume).
   - Each eviction is also logged at `debug` ("evicted a cached Script over the cache's capacity", `handle` field).
   - Story 7.2 publishes the counter as `hexput_ast_cache_evictions_total`.
3. **Unregister wire.**
   - The message is `MessageType::CodeUnregister`, placed after `CachedExecutionStart`, behind the init gate.
   - Its payload is exactly `{handle}`. Each of these is `protocol.invalid_payload` naming the key:
     - a missing or nil handle;
     - an unknown, repeated or non-string key;
     - a handle that is not a string or not a Script handle (echoed bounded).
   - A found entry is removed and answered `Result {}`.
   - A handle with no entry under this Client ID is `protocol.unknown_script_handle`. A repeated unregister therefore tells the Backend the entry was already gone.
   - It is served inline in the connection loop, like `ConfigUpdate`, through a synchronous `hexput_script::code_unregister(&Value, ClientId, &AstCache) -> Result<Value, Box<ErrorBody>>`. A failure is logged at `debug` ("an unregister failed", `code`).
4. **Teardown cost.** `close` walks the whole cache once, which is linear in the capacity. It runs once per Session teardown and never on a hot path.

**Never:**
- no `hexput-session → hexput-script` edge, and no other new crate edge;
- no re-parse fallback;
- no TTL or idle expiry on entries (only capacity, unregister and teardown);
- no change to the `CodeRegister` or `CachedExecutionStart` payloads;
- no `unsafe`;
- no lock held across an `.await`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Bounded | capacity 2; five registrations by one Session | `entry_count() <= 2`; `entry_count() + evictions() == 5` |
| Evicted handle | a handle no longer in `handles(client)` | `CachedExecutionStart` → `protocol.unknown_script_handle` |
| Unregister | `CodeUnregister {handle: h}` for this Session's `h` | `Result {}`; `handles` lacks `h`; a `Weak` of its program cannot upgrade once the test drops its own `Arc` |
| Unregister twice / foreign / never issued | the same `h` again, or B unregisters A's `h` | `protocol.unknown_script_handle`; A's entry is untouched |
| Malformed unregister | nil, `{}`, `{handle: 1}`, `{handle: "ABC"}`, `{handle: h, x: 1}`, repeated `handle` | `protocol.invalid_payload` naming the key; nothing is removed |
| Before init | `CodeUnregister` first | `protocol.init_not_completed` |
| Run survives removal | a run that already fetched `h` (a slow loop), then `h` is unregistered | the run's `Result` is correct; the next run of `h` → unknown |
| Teardown | a Session with entries; its last connection closes | `handles(client)` is empty, and other Sessions' entries stay |
| Late registration | `insert`/`code_register` for a Client ID closed (or never opened) | `protocol.init_not_completed`; `entry_count` is unchanged |
| Not an eviction | unregister and teardown removals | `evictions()` is unchanged |
| Config | `ast_cache_capacity = 0` / `-1` / `"x"` / absent | refused naming the field and its position / refused / refused / `10000` |

</intent-contract>

## Code Map

- `crates/hexput-script/src/lib.rs`:
  - `AstCache` (around lines 359-425) gains:
    - `with_capacity` (`moka::sync::Cache::builder().max_capacity(n).eviction_listener(…)`);
    - `open`, `close`, `remove(client_id, handle) -> bool`, `evictions`;
    - a `live: RwLock<HashSet<ClientId>>`;
    - an `AtomicU64` eviction counter (shared with the listener through an `Arc`);
    - an updated `Debug` impl.
  - `insert` becomes `Option<ScriptHandle>`.
  - `code_register`'s insert (around line 481, inside the blocking closure) maps `None` to the `init_not_completed` body.
  - Add `code_unregister`, which reuses `HANDLE`, `present`, `invalid_payload`, `unknown_handle` and `wire::bounded`.
  - *(Amended in review, loop 1.)* `cached_execution` fetches the handle's program when it is **called**, not when its future first runs: it becomes `pub fn cached_execution(payload, client_id, cache: &AstCache, registrations, settings, caller) -> impl Future<Output = Result<Value, Box<ErrorBody>>> + Send + 'static`, which scans the payload for its first well-formed `handle` (no decoding) and takes `cache.get` at once; the decode stays on the blocking pool and its errors still come first, and an accepted payload uses the fetched program (`unknown_script_handle` when there was none). `hexput-connection` calls it in the dispatch arm of the loop, before spawning the task, so a `CodeUnregister` (or eviction or teardown) the loop processes after a `CachedExecutionStart` cannot take the program from that earlier run: requests take effect in the order sent.
  - Rewrite the crate doc's "Not yet" paragraph (lines 86-87) and update `cached_execution`'s doc ("no longer cached" now has concrete causes).
- `crates/hexput-shared/src/wire.rs:71-122`: add `CodeUnregister` (with its doc, in `ALL` and in `as_str`) after `CachedExecutionStart`.
- `crates/hexput-port/src/error.rs:50-54`: extend the `UnknownScriptHandle` and `InvalidPayload` docs to cover `CodeUnregister` and eviction. The code itself is unchanged.
- `crates/hexput-session/src/lib.rs`:
  - The `Sessions` struct (line 73) gets the hook field. It derives `Debug` and `Default`, so implement `Debug` by hand, or wrap the hook.
  - `with_teardown`.
  - `detach` (line 155) passes `client_id` to `teardown` (line 254), which calls the hook last.
  - Update the module doc.
- `crates/hexput-connection/src/lib.rs`:
  - `answer` (around line 510): add `CodeUnregister` to the post-gate arm and answer it inline.
  - `init` (around line 744): call `connection.cache.open(client_id)` after `create`.
  - Update the module doc's message list.
- `crates/hexput-config/src/{lib.rs,file.rs}`:
  - `SystemConfig.ast_cache_capacity: u64` and `DEFAULT_AST_CACHE_CAPACITY`.
  - `RawFile.ast_cache_capacity: Option<Spanned<i64>>`, validated like `session_ttl` (file.rs line 138).
  - The lib.rs doc table (line 28 onwards).
- `crates/hexput-config/config.example.toml`: document `ast_cache_capacity`. `tests/config.rs::the_example_file_parses_and_documents_every_field` must still pass.
- `crates/hexput-daemon/src/lib.rs:233-235`: build `AstCache::with_capacity(loaded.config.ast_cache_capacity)` first, then `Sessions::new().with_teardown(move |id| cache.close(id))` with a cloned `Arc` (moved in review into one private `registry(&cache)` helper, pinned by an in-crate test).
- Tests:
  - `crates/hexput-tests/tests/script.rs`: the helpers `registered`/`registered_as` (around line 1030) must `open` the Client ID first.
  - `crates/hexput-tests/tests/connection.rs`: `run_timed_with_cache` (line 142) should wire `with_teardown` the way the Daemon does.
  - `crates/hexput-tests/tests/session.rs`: hook tests.
  - `crates/hexput-tests/tests/daemon.rs`: the template is around line 1392.
  - `crates/hexput-tests/tests/shared.rs`: the `MessageType` spelling list.
  - Every `AstCache::new()` call site (connection.rs and script.rs) becomes `with_capacity`.
- `scripts/check-crate-graph.py`: unchanged. `hexput-daemon` already reaches `hexput-session` and `hexput-script`.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/wire.rs`, `crates/hexput-port/src/error.rs`: add `CodeUnregister` and update the docs. This is the wire vocabulary.
- `crates/hexput-script/src/lib.rs`: give `AstCache` a bound, an eviction counter, `remove`, and `open`/`close` with insert gating, plus `code_unregister` and the doc updates. This covers the bound, unregister, and no orphaned entries.
- `crates/hexput-session/src/lib.rs`: add the teardown hook. This makes Session cleanup possible without a new edge.
- `crates/hexput-connection/src/lib.rs`: `open` on init and route `CodeUnregister`, so the cache is wired per Session.
- `crates/hexput-config/*`, `config.example.toml`: add `ast_cache_capacity`, making the capacity an operator setting (AD-7).
- `crates/hexput-daemon/src/lib.rs`: build the capacity-bounded cache and the hook. This is the composition root.
- `crates/hexput-tests/tests/{script,connection,session,config,daemon,shared}.rs`:
  - every matrix row;
  - the 4.2 deferred run-survives-removal test;
  - a hook-called-once-with-the-Client-ID test (and none while another connection is still attached);
  - a socket test: with capacity 1 configured, register two Scripts, and exactly one of them then runs while the other answers `unknown_script_handle`; `CodeUnregister` over the socket.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` and `.memlog.md`: add an `[Amended 2026-10-22, Epic 4 Story 4.3]` paragraph covering the `CodeUnregister` wire, `ast_cache_capacity`, the teardown hook, the open/close gate and the eviction counter, and update the `amended` frontmatter. SDKs and Epic 7 depend on these.
- `_bmad-output/implementation-artifacts/epic-4-context.md`: add a Technical Decisions bullet for Story 4.3's contract.
- `AGENTS.md`: Story 4.3 in Project Status, the test count, and the Next step line.

**Acceptance Criteria:**
- Given a configured `ast_cache_capacity`, when registrations exceed it, then the entry count stays within it, `evictions()` counts the evictions, and an evicted handle's `CachedExecutionStart` is `protocol.unknown_script_handle`.
- Given a `CodeUnregister` for a handle this Session owns, when it is processed, then the entry is gone before the reply is sent and the cache keeps no reference to its program.
- Given a Session whose last connection closes in the real Daemon wiring, when teardown completes, then none of its entries remain, and no registration still in flight can add one afterwards.
- Given the CI commands, when they run in their `--release` form, then all pass and `check-crate-graph.py` passes with no edge added.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/script.rs` (`the_cache_stays_within_its_capacity_and_counts_each_eviction`, `an_evicted_handle_is_unknown_to_a_cached_run_and_the_kept_one_still_runs`, `a_removed_or_evicted_program_is_released_at_once`, `a_removed_handle_is_unknown_and_its_program_is_released_at_once`, `unregistering_twice_or_another_sessions_or_a_never_issued_handle_is_unknown`, `a_malformed_unregister_names_its_key_and_removes_nothing`, `a_removal_is_not_an_eviction_and_a_teardown_is_not_one_either`, `a_session_teardown_releases_its_entries_and_no_other_sessions`, `a_registration_for_a_session_never_opened_or_already_ended_inserts_nothing`), `crates/hexput-tests/tests/connection.rs` (`code_unregister_before_init_is_init_not_completed`, `a_code_unregister_is_answered_with_an_empty_result_and_its_entry_is_gone_before_the_reply`, `a_malformed_code_unregister_is_refused_by_key_and_the_entry_survives`, `a_code_unregister_of_a_foreign_or_removed_handle_is_unknown_and_leaves_the_owner_alone`, `the_run_that_fetched_a_handle_survives_its_removal_and_the_next_run_is_unknown`, `a_capacity_eviction_is_answered_unknown_over_the_wire_and_the_kept_handle_runs`, `a_teardown_releases_its_session_and_leaves_another_sessions_entries`, `a_session_torn_down_by_a_connection_that_ran_out_of_script_leaves_no_entry` (renamed in review to `a_session_whose_only_connection_closes_cleanly_leaves_no_entry`), `an_eviction_is_logged_at_debug_and_each_one_is_counted`), `crates/hexput-tests/tests/session.rs` (`the_hook_is_called_once_with_the_client_id_when_the_last_connection_detaches`, `the_hook_runs_after_the_session_is_gone_and_outside_any_lock`, `a_registry_without_a_hook_tears_down_quietly`, `the_hook_is_called_for_each_session_that_ends_and_only_those`, `a_detach_of_an_unknown_session_does_not_call_the_hook`), `crates/hexput-tests/tests/config.rs` (`the_ast_cache_capacity_defaults_to_ten_thousand_entries`, `the_ast_cache_capacity_is_read_from_the_file`, `a_zero_or_negative_ast_cache_capacity_is_an_error_naming_the_field_and_position`, `an_ast_cache_capacity_of_the_wrong_type_is_rejected`, `the_example_file_documents_the_ast_cache_capacity`), `crates/hexput-tests/tests/daemon.rs` (`a_capacity_of_one_over_the_socket_keeps_one_script_and_code_unregister_removes_it`), and `crates/hexput-tests/tests/shared.rs` (`message_types_spell_pascal_case_and_parse_exactly`, extended with `CodeUnregister`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Test-harness deviations, recorded so the implementer is not surprised: every `AstCache::new()` in the test files became `AstCache::with_capacity(10_000)`; `registered_as` in `script.rs` now calls `open` first, and `register_unopened` is the path for the late-registration rows. `run_timed_with_cache` was not given the teardown hook: it receives an already-built `Arc<Sessions>`, and `with_teardown` consumes its registry, so the connection tests use a new `torn_down_by(&cache)` helper and `hosted_on`/`connect_on`, which take the shared cache, instead.
- Implementation (2026-10-22): one pre-written test did not compile — `a_malformed_code_unregister_is_refused_by_key_and_the_entry_survives` read the moved `cache` inside its `async move` closure; it now reads `probe` (the same cache), so the assertion is unchanged. moka 0.12.16 was verified to notify a declined admission as `RemovalCause::Size` (its own tests and `the_cache_stays_within_its_capacity_and_counts_each_eviction` both pin it), so no extra accounting was needed.
- The daemon-level teardown is not observable over the socket (a Client ID cannot be reattached before Epic 5), so no socket test asserts it; the connection-level teardown test runs the Daemon's wiring (`Sessions::with_teardown` calling `AstCache::close`).

## Spec Change Log

### 2026-10-22 — Review loop 1 (bad_spec)
- **Trigger:** edge-case-hunter finding — a `CachedExecutionStart(h)` followed by `CodeUnregister(h)` usually failed with `protocol.unknown_script_handle`, because the run was spawned with a lazy lookup on the blocking pool while the unregister was served inline first, so requests did not take effect in the order sent.
- **Amended:** the Code Map's `hexput-script` entry (`cached_execution` fetches at call time and is called at dispatch) and the `hexput-daemon` entry (one private `registry` helper). `<intent-contract>` untouched; its "a run that fetched its `Arc<Program>` keeps it" rule now covers every run dispatched before the removal.
- **Known-bad state avoided:** a pipelined run-then-unregister (the natural "run once and discard") failing nondeterministically; a lookup that depends on blocking-pool scheduling.
- **KEEP:** everything else in the reviewed tree survived the re-derivation unchanged — the capacity setting and its validation, `with_capacity` as the only constructor, the eviction counter, `CodeUnregister` and its decoder, the teardown hook, the open/close gate with the insert under the read lock, maintenance after every removal, and all pre-written tests. Re-derivation therefore reapplied the reviewed diff as it stood and changed only the amended points plus this pass's patches.

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 31 findings — high 0, medium 12, low 13, false 6, maybe-false 0
- findings:
  - `[low]` `[patch]` (blind) sprint-status moves 4.3 only to `ready-for-dev` — the entry is set to `review` at finalization, matching 4.1 and 4.2.
  - `[medium]` `[patch]` (blind; group with edge "one Session evicts others") the capacity is one pool shared by every Session, undocumented — documented in the `AstCache` doc, the Spine amendment, the memlog and the epic context; a per-Session quota is a design change, recorded in deferred-work.md.
  - `[low]` `[reject]` (blind) no upper bound on `ast_cache_capacity`, and `close` walks the cache on a runtime worker — the capacity is the operator's own choice, and the walk is linear in the entries actually held (about a millisecond at the 10 000 default, once per Session teardown); every fix (a maximum, a per-Client-ID index, `spawn_blocking`) is more than a direct correction for a case unlikely in everyday use.
  - `[low]` `[patch]` (blind; group with edge "insert holds the lock while running maintenance") the "no lock held while calling outside code" claim is false — `insert` now drops the open-set read guard after moka's insert and before `run_pending_tasks()` (ordering against `close` only needs the guard around the insert; `close` runs maintenance before walking); doc comments say so.
  - `[medium]` `[patch]` (blind; group with edge "eviction event has no Client ID") evictions are logged under another request's span — the listener's event is now a root event (`parent: None`) carrying `client_id` and `handle`; `an_eviction_is_logged_at_debug_and_each_one_is_counted` asserts both and that no span is attached.
  - `[medium]` `[patch]` (blind; group with edge "Sessions built without the hook", gap "Daemon wiring never exercised", intent §3.5) nothing ties `open` to the teardown hook and the Daemon's own wiring is untested — the wiring moved into one private `hexput_daemon::registry(&cache)`, used by `serve` and pinned by an in-crate test (`the_daemons_registry_releases_a_sessions_entries_when_it_is_torn_down`: teardown releases one Session's entries, keeps another's, and a late registration is `init_not_completed`); `rmpv` is the daemon's one dev-dependency for it.
  - `[medium]` `[defer]` (blind; group with gap "no test checks insert holds the read lock") the concurrent insert-vs-close race is untested — the code is correct as written; a deterministic test needs a seam inside `insert` the spec does not settle, and the gap layer filed it as defer. The misleading test name was corrected (`a_session_whose_only_connection_closes_cleanly_leaves_no_entry`). Recorded in `deferred` and deferred-work.md.
  - `[false]` `[reject]` (blind) `protocol.init_not_completed` would mislead a reattached Backend — a reply goes only to the connection that sent the request (AD-2), and a Session is torn down only once that connection has detached (a clean close writes its replies before detaching), so the reply can never reach a live Backend, with or without Epic 5's reconnect.
  - `[low]` `[patch]` (blind) the unregister failure log drops the handle — it now logs `handle` when the payload names a well-formed one, like a Cached Execution's failure.
  - `[low]` `[patch]` (blind) badly wrapped doc comments — rewrapped in `hexput-port/src/error.rs` (`InvalidPayload`), `cached_execution`'s `# Errors` and `hexput-connection`'s module doc.
  - `[low]` `[reject]` (blind) the Code Map names the field `live` and says `run_timed_with_cache` gets the hook — its fix is an edit of this build's spec; the Implementation Notes already record the actual helper.
  - `[low]` `[reject]` (blind) release-only verification compiles out `debug_assert!` in `teardown` — the user's own request requires `--release` for every cargo command on this disk-constrained machine, and the invariant holds by construction (`remove_if_mut` removes a Session only once its attachment set is empty); turning it into a production panic is not warranted.
  - `[false]` `[reject]` (blind) `evictions()` changes what the cache holds — `run_pending_tasks()` only applies operations already logged (every insert already ran maintenance, so no admission is pending from another caller's view beyond what that caller's own maintenance would apply moments later); it decides nothing a metrics read would not have decided anyway.
  - `[medium]` `[bad_spec]` (edge) a `CachedExecutionStart(h)` then `CodeUnregister(h)` fails the run — confirmed: the run looked the handle up on the blocking pool after spawning, while the unregister was served inline first. Amended the Code Map (Spec Change Log, loop 1): `cached_execution` now fetches the program when called and the connection calls it at dispatch; new test `a_cached_run_dispatched_before_an_unregister_of_its_handle_still_runs`.
  - `[medium]` `[patch]` (edge) Sessions built without `with_teardown` leak open IDs — grouped with the blind "nothing ties the open set to the hook" entry; fixed there.
  - `[low]` `[reject]` (edge) `close` runs on a Tokio worker — see the blind "no upper bound" row: linear in held entries, once per teardown; the fix is more than a direct correction.
  - `[medium]` `[patch]` (edge) the eviction listener records no Client ID and logs under the current span — grouped with the blind span entry; fixed there.
  - `[medium]` `[patch]` (edge) one Session can evict every other Session's entries — grouped with the blind shared-capacity entry; documented there, quota deferred.
  - `[low]` `[reject]` (edge) no upper bound on `ast_cache_capacity` — see the blind row: an operator setting; a maximum changes the spec's config contract for a case unlikely in everyday use.
  - `[low]` `[patch]` (edge, claim) `insert` runs maintenance under the open-set read lock — grouped with the blind lock-claim entry; fixed there.
  - `[low]` `[patch]` (edge, claim) the teardown test never asserts the registration succeeded — it now asserts the register reply is a `Result` with a handle before checking the cache is empty.
  - `[medium]` `[patch]` (verification-gap) the program's release after `remove`/`close` is only checked after calls that run maintenance — the `Weak` is now asserted immediately after `unregister` in `a_removed_handle_is_unknown_and_its_program_is_released_at_once` and immediately after `cache.close` in `a_session_teardown_releases_its_entries_and_no_other_sessions`.
  - `[medium]` `[patch]` (verification-gap) the Daemon's teardown hook wiring is never exercised — grouped with the blind open-set/hook entry; fixed there by the private `registry` helper and its in-crate test.
  - `[medium]` `[defer]` (verification-gap) no test checks `insert` holds the read lock while inserting — grouped with the blind in-flight race entry; deferred there.
  - `[false]` `[reject]` (intent) planning only vs implementation — "Halt after planning" governed the planning stage of this orchestrated run; this stage was directed to review, patch and commit the implementation, and the status disagreements are the sprint-status row patched above.
  - `[low]` `[reject]` (intent) "observable in metrics" is only an in-process counter and a log line — the epics place the metrics surface in Story 7.2, which reads `AstCache::evictions()`; the debug log is operator-visible now; nothing more can be exposed before that surface exists.
  - `[low]` `[reject]` (intent) the bound counts entries, not bytes — a defensible reading recorded as spec decision 1; the operator lowers the count to bound memory, and typical Scripts are small, so the worst case (a frame-sized source per entry) is unlikely in everyday use and a byte bound would be a redesign.
  - `[false]` `[reject]` (intent) "memory released" is shown only by a `Weak` proxy — a `Weak` that cannot upgrade proves the last strong reference dropped, which drops the `Program` and frees its allocation; that is the release itself, not a proxy.
  - `[medium]` `[patch]` (intent §3.5) Daemon teardown and the D2 race are tested only indirectly — the Daemon part is fixed by the private `registry` helper's in-crate test (grouped with the blind open-set/hook entry); the concurrent race part is the deferred entry above.
  - `[false]` `[reject]` (intent) additions beyond the AC (wire message, config key, new failure mode, Spine amendment) — each is a recorded spec decision implementing the AC (an unregister needs a request; a configured capacity needs a key) under the orchestrated run's direction; none contradicts the intent.
  - `[false]` `[reject]` (intent) the verification section uses `--release` — that is the user's own relayed request, which overrides the default commands; not a defect.

## Design Notes

**Golden exchange.**
```text
→ {id: 4, type: "CodeUnregister", payload: {handle: "0000000000000001"}}
← {id: 4, type: "Result", payload: {}}
→ {id: 5, type: "CachedExecutionStart", payload: {handle: "0000000000000001", variables: {}}}
← {id: 5, type: "Error", payload: {category: "protocol", code: "protocol.unknown_script_handle", …}}
```

**Why an open set rather than having the connection clean up after itself.** A connection whose stream is lost aborts its tasks, but a `spawn_blocking` registration closure still runs to completion. Only a check made inside the cache, ordered against `close` by the lock, guarantees that no entry outlives its Session. The open set also makes the hook alone sufficient for Story 5.8's delayed teardown.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes unchanged
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass

## Auto Run Result

**Summary.** Story 4.3 bounds the AST Cache by System Config's `ast_cache_capacity` (entries, at least 1, default 10 000) under moka's policy, counting and logging each capacity eviction; adds `CodeUnregister {handle}` (served inline, `Result {}` / `unknown_script_handle` / `invalid_payload`); releases a Session's entries through a `Sessions::with_teardown` hook the Daemon wires to `AstCache::close`; and refuses an insert for a Session not open, so no entry outlives its Session. Review loop 1 made a Cached Execution fetch its program at dispatch, so a run sent before an unregister of its handle still runs.

**Files changed.**
- `crates/hexput-shared/src/wire.rs` — `MessageType::CodeUnregister`.
- `crates/hexput-port/src/error.rs` — `UnknownScriptHandle`/`InvalidPayload` docs cover `CodeUnregister` and eviction.
- `crates/hexput-script/src/lib.rs` — bounded `AstCache` (`with_capacity`, `open`/`close`/`remove`, `evictions`, open-set gate), `code_unregister`, `cached_execution` fetching at call time.
- `crates/hexput-session/src/lib.rs` — `Sessions::with_teardown` and the hook call in `teardown`.
- `crates/hexput-connection/src/lib.rs` — `open` at init, `CodeUnregister` inline, cached runs fetched at dispatch.
- `crates/hexput-config/{src/lib.rs,src/file.rs,config.example.toml}` — `ast_cache_capacity`.
- `crates/hexput-daemon/{src/lib.rs,Cargo.toml}` — capacity-bounded cache, private `registry` helper and its in-crate test (`rmpv` dev-dependency); `Cargo.lock` follows.
- `crates/hexput-tests/tests/{script,connection,session,config,daemon,shared}.rs` — Story 4.3 tests and review additions.
- Spine, memlog, `epic-4-context.md`, `deferred-work.md`, `sprint-status.yaml`, `AGENTS.md` — contract records and status.

**Review findings breakdown.** 31 findings (medium 12, low 13, false 6). One bad_spec (dispatch-time fetch; loop 1). Patched entries: the shared-capacity documentation (medium), the eviction log's span and Client ID (medium), the Daemon registry helper and test (medium), the `Weak` release assertions (medium), the lock scope (low), the unregister log handle (low), doc rewrapping (low), the teardown test's registration assertion (low), the sprint-status row (low). Deferred: the concurrent insert-vs-close test (medium). Rejected: the capacity maximum and `close` on a runtime worker (low — operator setting, linear in held entries, fix more than a direct correction), the Code Map naming (its fix edits this spec), release-only `debug_assert!` (user mandate, invariant by construction), metrics surface (Story 7.2), entry-count bound (spec decision), and the six `false` findings with their refutations above.

**Follow-up review recommendation: `true`.** First pass with four medium entries patched (and a bad_spec loop). Unverified risk: `hexput_script::cached_execution` changed from an `async fn` to a function that fetches the program eagerly, a public-signature and request-ordering change; a second look should confirm that pinning programs at dispatch is harmless when many runs queue for blocking-pool threads (an unregistered program stays alive until every run dispatched before the unregister finishes) and that no future caller expects the lazy lookup.

**Verification.** In `--release` form, as the user requires: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (26 edges, 4 restricted names; no edge added); `cargo build --workspace --all-targets --locked --release` succeeded; `cargo test --workspace --locked --release` — 929 passing, 0 failing.

**Residual risks.** The capacity is shared across Sessions (documented; quota deferred). `close` walks the whole cache on the async exit path, linear in held entries. The insert-vs-close interleaving has no concurrent test (deferred). `debug_assert!`s are not exercised under release-only testing.

