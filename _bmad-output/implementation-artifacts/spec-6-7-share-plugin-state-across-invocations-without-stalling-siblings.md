---
title: 'Story 6.7: Share plugin state across invocations without stalling siblings'
type: 'feature'
created: '2026-10-23'
status: 'done'
baseline_revision: 'd2bac74c8c48b1972062c911ae76a3887133cd03'
review_loop_iteration: 1
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-6-context.md'
  - '{project-root}/_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md'
warnings: ['oversized']
deferred:
  - summary: >-
      A Plugin's Global Variable store has no size limit and is charged to no Resource Budget or quota, so `g[newKey] = …` across fires grows it without bound.
    evidence: |-
      Review finding (blind-hunter), medium. Each invocation is bounded by its memory and side-effect budgets and the Plugin's source is the Backend's own; the same Daemon-wide gap as the uncapped Plugin count (Story 6.2 deferral). The internal intent-map leak was patched. Settle with a per-Plugin or per-Session store quota in System Config.
    location: >-
      crates/hexput-globalvar/src/lib.rs (PluginStore::units)
    severity: medium
  - summary: >-
      Story 6.7 already implements Story 6.13's second acceptance criterion (hexput-session teardown and release_plugin tear down the Plugins' stores).
    evidence: |-
      Review finding (blind-hunter), low. Noted beside 6-13 in sprint-status.yaml and in deferred-work.md so Story 6.13's planning starts from that wiring.
    location: >-
      crates/hexput-session/src/lib.rs (teardown, release_plugin)
    severity: low
---

<intent-contract>

## Intent

**Problem:** Every handler invocation evaluates a Plugin's Global Variable initializers afresh (Story 6.4), so nothing a handler writes survives to the next invocation and FR-20's persistent, shared, per-key-locked state does not exist; `hexput-globalvar` is an empty stub.

**Approach:** Implement the per-Plugin concurrent Global Variable store in `hexput-globalvar` (generic over the stored value, `dashmap`, keyed by Global Variable and top-level key), initialize it once at registration, and have every handler invocation read and write Globals through it via a synchronous trait the interpreter defines and `hexput-exec` implements — short, synchronous critical sections, never across an `.await`, torn down only by `hexput-session`.

## Boundaries & Constraints

**Always:**
- The store lives in `hexput-globalvar` (depends on `hexput-shared` only, plus external `dashmap`), keyed by `PluginId`, independent of any actor or execution; `hexput_globalvar::teardown(plugin_id)` is its sole lifecycle exit, called by `hexput-session`'s private `teardown` (per Plugin of the Session) before anything drops — never `Drop` (AD-4).
- Globals are initialized exactly once per Plugin: after `prepare`, before the first `BackendRegisteredInit` handler; an invocation binds Globals from the store and never re-evaluates initializers. A registration that fails leaves no store behind.
- Lock unit is `(variable, top-level key)`: for a Global holding an object, each top-level property is its own unit; any other value (scalar, array) is one unit for the whole variable. Writes to different units never wait on each other.
- Every read/write is a short synchronous critical section on the blocking-pool thread; no lock or guard is ever held across an `.await` or a host call. Values cross the store detached (no heap handle survives an execution).
- Default strategy is `safe`. Under it, an assignment to a Global unit whose right-hand side contains no call (function, host, method) and no anonymous function holds that unit's writer-intent lock from the start of the assignment until its write, so concurrent `g.k = g.k + 1` loses no update. Otherwise the read and the write are separate atomic steps (last writer wins, never torn). Reads never take intent locks (no deadlock).
- Global access needs no capability, consumes no RPC call, and goes only through `hexput-exec` → `hexput-globalvar`; each write counts as one side effect against the Resource Budget (AD-3). `hexput-globalvar` never depends on `hexput-plugin`, `hexput-rpc` or `hexput-enforce` (already pinned by `check-crate-graph.py`).
- An invocation's writes stand even if it later fails (no rollback).

**Never:** `@Global` `locking`/`behavior` semantics (Stories 6.8–6.10 — annotations still parse and are ignored), the Backend's unsafe strategy, TTL, keyed/per-trigger isolation, any wire change, any new crate edge, any `unsafe` Rust, any async in `hexput-globalvar`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Persist | `let n = 0;` handler `n = n + 1; return n;`, fired 3 times | results 1, 2, 3 | none |
| Init seeds | init handler sets `g.ready = true`; later fire returns `g.ready` | `true` | none |
| Different keys concurrently | two fires (two connections) writing `g.a`, `g.b`, one stalled inside a host call | neither waits on the other; both writes visible | none |
| Same key concurrently | 8 concurrent fires each `g.c = g.c + 1` ×50 | final `g.c` = 400 | none |
| Failure after write | handler writes `n = 5` then errors | Error reply; next fire sees `n == 5` | handler error reported as today |
| Teardown | Session torn down (TTL / last detach without TTL) | store for its Plugins removed; `GlobalVariables` has no entry | none |
| Side effects | handler writing Globals beyond `budget.side_effects` | `budget.side_effects_exceeded` on that write | budget error, writes before it stand |

</intent-contract>

## Code Map

- `crates/hexput-globalvar/src/lib.rs` -- 8-line stub; becomes the store: `GlobalVariables<V>` registry (`PluginId → Arc<PluginStore<V>>`), `PluginStore` (DashMap per unit + per-unit intent `Mutex<()>`), `create`, `get`, `teardown(plugin_id)`.
- `crates/hexput-globalvar/Cargo.toml` -- add `dashmap = { workspace = true }` (already pinned 6.2.1 in the root workspace).
- `crates/hexput-interpreter/src/lib.rs:363` -- `Execution::handler` doc says "evaluated afresh … Nothing outlives it"; add a constructor/variant taking a `&dyn`/`Arc<dyn Globals>` (trait defined in the interpreter: `read(name, key)`, `write(name, key, value)`, `begin_update(name, key)` guard) and update the docs.
- `crates/hexput-interpreter/src/machine.rs:549` -- the top-level machine; route reads/writes of top-level `let` bindings (and `g.k` / `g["k"]` on them) through `Globals` instead of the heap when present.
- `crates/hexput-exec/src/lib.rs:426` (`invoke_handler`), `:448` (`drive`) -- accept the Plugin's store, implement `Globals` over `hexput-globalvar`, charge one side effect per write.
- `crates/hexput-plugin/src/lib.rs` -- `prepare`/`initialize`/`finish` (init, ~l.543–600) and fire (~l.763–800): create/seed the store before init handlers, pass it to every `invoke_handler`, drop it on a failed registration.
- `crates/hexput-session/src/lib.rs` -- private `teardown`: call `hexput_globalvar::teardown` for each of the Session's Plugin ids (`Sessions::plugins`); also on `release_plugin`.
- `crates/hexput-daemon/src/lib.rs` -- `registry(cache, plugins, ttl)`: share the one `GlobalVariables` with `Plugins`/connections.
- `crates/hexput-tests/tests/{globalvar,plugin,daemon}.rs` -- tests.
- `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` §9 -- dated decision bullet; `AGENTS.md` Project Status.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-globalvar/src/lib.rs` -- implement the store per Boundaries -- AD-4 store.
- `crates/hexput-interpreter/src/{lib,machine}.rs` -- `Globals` trait + Global-backed top-level bindings, intent lock per the safe rule (static RHS-has-call test on the AST) -- persistence without a new edge.
- `crates/hexput-exec/src/lib.rs` -- wire store into `invoke_handler`, side-effect charge -- AD-3.
- `crates/hexput-plugin/src/lib.rs`, `crates/hexput-connection/src/lib.rs` (pass the store into the plugin calls), `crates/hexput-daemon/src/lib.rs` -- seed once at registration, pass on every fire/init, clean on failure -- FR-20.
- `crates/hexput-session/src/lib.rs` -- teardown/release call `hexput_globalvar::teardown` -- AD-4.
- `crates/hexput-tests/tests/*.rs` -- one test per matrix row; a concurrency test with a Backend stalling a host call to prove non-blocking -- NFR6.
- LANGUAGE-REFERENCE §9, `AGENTS.md` -- record decisions, update status.

**Acceptance Criteria:**
- Given a registered Plugin, when Events are fired over several connections of its Session, then every handler sees the latest stored Global values (FR-20).
- Given the source, when a reviewer greps `hexput-globalvar`, then it has no async, no `.await`, and only `hexput-shared` as a workspace dependency; `python3 scripts/check-crate-graph.py` passes.

## Design Notes

Decision (unattended): "no update is lost" is read as statement-level atomicity of a call-free read-modify-write on one unit; a statement containing a call cannot hold a lock (it may suspend), so it degrades to atomic read and atomic write — recorded in LANGUAGE-REFERENCE §9. Intent locks are only ever held one at a time per thread (a call-free RHS cannot assign), so no deadlock.

Amendment (review pass 1, 2026-10-23): a whole write of a Global holding an object — `g = v`, or a nested write under a computed first key (`g[k].x = v`), which writes the whole Global — rewrites every one of its property units, so its writer intent overlaps all of them: `begin_update(name, None)` waits for, and holds off, every `begin_update(name, Some(key))` of the same variable, while property intents of distinct keys stay independent. Intents are kept per variable (never per key ever written). An invocation still holds at most one intent, so there is no deadlock. Catching a copy up with the store keeps an object's identity (its contents are replaced in place), so a receiver already evaluated still writes the Global.

## Verification

**Commands:**
- `cargo fmt --all --check` -- clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked --release` -- all pass

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/globalvar.rs` (store lifecycle, unit independence and the intent lock, no-lost-update under 8 threads, the crate's no-async and dependency pin) and `crates/hexput-tests/tests/daemon.rs` (`a_global_written_by_one_fire_is_read_by_the_next_so_state_persists_across_invocations`, `an_init_handler_seeds_a_global_that_a_later_fire_reads_on_any_connection`, `a_fire_on_one_connection_sees_the_latest_global_written_by_a_fire_on_another`, `two_fires_on_two_connections_touching_different_keys_neither_waits_on_the_other`, `eight_connections_each_firing_a_call_free_increment_fifty_times_lose_no_update`, `a_failure_after_a_write_keeps_the_write_and_the_next_fire_sees_it`, `a_global_access_needs_no_capability_and_consumes_no_rpc_call`, `each_global_write_is_one_side_effect_and_the_writes_before_the_refused_one_stand`, `a_global_initializer_runs_once_so_the_init_and_every_fire_build_on_one_run`, `a_failed_registration_leaves_no_store_and_a_same_name_registration_starts_fresh`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it. The store API the tests assume is the Code Map's: `GlobalVariables<V>::{new, create, get, teardown}` and `PluginStore<V>::{read, write, begin_update}` with `key: Option<&str>` (`None` for a whole variable). The Session-to-store teardown wiring (the Teardown row) is asserted at the store only; the implementer may add a Session-level observation if the wiring needs one.

Implementation decisions (unattended, recorded in LANGUAGE-REFERENCE §9 where they bind the language):

- A Global is read and written **by name**: a read of an unshadowed Global name syncs the invocation's copy first (only units whose store version changed are re-attached, in place); only an assignment whose target starts at the Global's name writes its unit back. A value read out of a Global is the invocation's copy — a write through an alias changes the copy only. A nested write under a computed first key (`g[k].x = v`) writes the whole Global.
- Initialization runs as its own seeding execution (`hexput_exec::seed_globals`, `Execution::seeding`: top-level `let`s only, nothing hoisted, so `features.callbacks` does not refuse it; other toggles and the budget apply) in `hexput_plugin::initialize`, before the first init handler. A seeding failure fails the registration with its diagnostic.
- The store is created unregistered by `prepare` and joins `Plugins::global_variables()` inside `Plugins::insert` under the registry lock, so a failed registration leaves none behind; `hexput-session` tears stores down after its teardown hook (which closes `Plugins`) and on `release_plugin`, through the `hexput_globalvar::Teardown` trait object the Daemon wires with `Sessions::with_global_variables`.
- The side-effect charge is a machine stop (`Outcome::GlobalWrite`) handled synchronously in the Executor's slice loop with the new `Budget::charge_side_effect`, so the budget stays the Executor's and no writer intent crosses an `.await`.
- `invoke_handler` gained a `globals: Option<Arc<GlobalStore>>` parameter (`None` keeps Story 6.4's per-invocation initializers); the two pre-existing exec tests pass `None`, and the exhaustive `Outcome` matches in tests/benches gained a `GlobalWrite` arm.

## Spec Change Log

### 2026-10-23 — Review pass 1 (bad_spec)
- **Trigger:** blind-hunter finding "whole-unit and property-unit intents don't block each other, so call-free updates can still lose writes": `g = g` or `g[k].x = 1` (whole intent) ran beside `g.a = g.a + 1` (property intent) and rewrote `a` from a stale copy.
- **Amended:** Design Notes — a whole-variable intent overlaps every property intent of its variable (and vice versa); intents are kept per variable; catching up keeps an object copy's identity.
- **Known-bad state avoided:** a call-free increment silently rolled back by a concurrent whole write of the same Global; the store test that pinned whole and property intents as independent.
- **KEEP:** everything else of the implementation — the store's units and versions, by-name reads and writes with per-unit write-back, the seeding execution, `Outcome::GlobalWrite` and its side-effect charge, store registration inside `Plugins::insert`, teardown through `hexput_globalvar::Teardown` — and every pre-written test except the one the amendment contradicts.
- **Re-derived:** `hexput_globalvar::PluginStore::begin_update` (per-variable `Holders`: a whole flag and a set of held properties, `notify_all` on release), `PluginGlobals::begin_update` without the racy kind check, and `tests/globalvar.rs`'s whole-vs-property test rewritten to assert the overlap both ways.

## Review Triage Log

### 2026-10-23 — Review pass
- verdicts: 35 findings — high 1, medium 10, low 20, false 4, maybe-false 0
- findings:
  - `[medium]` `[bad_spec]` Whole-unit and property-unit intents don't exclude each other, so `g = g` / `g[k].x = 1` beside `g.a = g.a + 1` loses the increment — verified: `PluginStore::begin_update` keyed intents by `(name, key)` and `write_whole` rewrote every property from the copy. Amended Design Notes (a whole intent overlaps every property intent of its variable); re-derived `begin_update` with per-variable `Holders`; new daemon test `a_whole_write_racing_property_increments_loses_none_of_them`; the store test pinning independence now asserts the overlap both ways.
  - `[low]` `[patch]` Waiting for an intent counts as CPU time and holds a blocking thread — real (the wait is inside the timed slice), but an intent is held only across a call-free expression, so waits are short. Documented in LANGUAGE-REFERENCE §9 (the wait counts toward the waiter's CPU time).
  - `[low]` `[patch]` A write through an alias, a parameter or a Backend modification changes the copy, not the store — real and the specified copy semantics (§9). Documented precisely in §9 and the interpreter's `globals` module doc: such a change reaches the store only when a later assignment by the Global's name writes the unit holding it.
  - `[medium]` `[patch]` `PluginGlobals::begin_update` checks the variable's kind and takes the intent in two steps — verified race. Removed the kind check: with overlapping whole/property intents a property intent excludes a concurrent whole write whatever kind the variable has.
  - `[medium]` `[patch]` The store has no size limit and the intents map never shrinks — verified: one `IntentLock` per unit ever written. Intents are now kept per variable (bounded by the Plugin's Global names) with the held property set shrinking on release; `variables` is bounded by Global names too. The store-size quota is deferred (frontmatter, deferred-work.md).
  - `[low]` `[patch]` A store stays registered forever if the registration task is abandoned during `finish` — verified: the guard released the name, then the still-running blocking `finish` registered the store under an id no Session lists. The `ReleaseOnDrop` guard now moves into the `finish` closure and is disarmed there only when the Plugin was stored.
  - `[low]` `[patch]` Teardown order contradicts AD-4 ("before anything drops") — verified: stores were torn down after the hook. `teardown` now tears the stores down first and again after the hook (which closes `Plugins`, catching a store that joined in between); docs updated.
  - `[low]` `[patch]` Story 6.13's second acceptance criterion is implemented here without a tracking note — verified. Noted beside 6-13 in sprint-status.yaml and in deferred-work.md.
  - `[low]` `[patch]` A property write is silently dropped when the Global was meanwhile replaced with a non-object, and a write is charged even when it changes nothing — verified, both by design (last whole write wins; side effects charged on attempt like host calls). Both stated in LANGUAGE-REFERENCE §9.
  - `[medium]` `[patch]` Interpreter-level Global behaviour has no direct tests — verified. Added daemon tests covering in-place property catch-up and whole catch-up (`a_handler_reads_what_another_invocation_wrote_while_it_was_suspended`), the object-to-object whole catch-up with a held receiver, the deferred `Frame::Intent`, shadowing, function/cycle refusal, seeding refusal under a disabled toggle and seeding that hoists no function.
  - `[low]` `[reject]` Seeding swallows a detach failure; `Changes::Whole` drops secrets — the first is unreachable (the parser's `syntax.invalid_global_initializer` forbids identifiers, calls and `fn` in an initializer, so it can produce neither a function nor a cycle), the second is correct by the reviewer's own analysis (scalar secrets ride per entry); what remains is a test-only gap, not worth a fixture today.
  - `[high]` `[patch]` A catch-up during an assignment rebinds the Global to a new object, so the receiver already on the stack is orphaned and the write is lost — verified (`g.a = stall() + g.a` after a concurrent `g = {a: 100, c: 1}` stored 100, not 101). `sync_global` now keeps an object copy's identity: properties in place, or `Heap::replace_contents` for a whole or key-adding change; test `a_global_replaced_whole_after_an_assignments_receiver_was_read_still_takes_the_write` fails without it.
  - `[medium]` `[patch]` A loop or alias holding the Global's object is orphaned by a whole rebind — same root cause as the previous row; fixed by the same in-place catch-up.
  - `[low]` `[patch]` Backend modifications to a Global's collection never reach the store — same root cause as the alias row (copy semantics); documented in §9.
  - `[medium]` `[patch]` Kind check and intent taken separately (TOCTOU) — same as the `begin_update` row above; fixed by removing the check.
  - `[medium]` `[patch]` The intents map grows with every distinct key — same as the size row; intents are now per variable.
  - `[low]` `[patch]` Intent wait counted as CPU time — same as the CPU row; documented.
  - `[low]` `[patch]` A property catch-up keeps a stale Value Secret when the stored value carries none — verified (`set_location_secret` never overwrites). The place's secret is now cleared (`Heap::clear_property_secret`) before the stored one is set; a whole catch-up re-declares the binding as a fresh place.
  - `[low]` `[patch]` The cyclic-Global message contains a run of spaces — verified at machine.rs; now a `\` continuation, pinned by the function/cycle test.
  - `[low]` `[reject]` Global writes reuse `type.function_result`/`type.cyclic_result` — the message names the Global, and a distinct code would add public surface (a new `Code::ALL` entry and wire code) for a rarely met case.
  - `[medium]` `[patch]` "Every handler sees the latest stored values" fails after a whole rebind during an assignment or a Backend modification — same root causes as the rebind row (fixed) and the copy-semantics row (documented).
  - `[low]` `[patch]` The Daemon's `registry` store wiring is never asserted — gap as filed. `the_daemons_registry_releases_a_sessions_entries_when_it_is_torn_down` now asserts both stores exist before expiry and only the kept Session's after.
  - `[low]` `[patch]` Shadowed Global names are not exercised — gap as filed. Added `a_binding_shadowing_a_global_is_local_so_its_writes_neither_reach_the_store_nor_count` (local `let` and same-named parameter, with `side_effects = 0`).
  - `[low]` `[patch]` Computed-key Global writes are untested — gap as filed. Added `eight_connections_each_firing_a_computed_key_increment_lose_no_update` (8 × 25 `g[k] = g[k] + 1`, sibling key untouched).
  - `[medium]` `[patch]` Mid-invocation catch-up is protected only by a timing-dependent test — gap as filed. Added the stall-driven `a_handler_reads_what_another_invocation_wrote_while_it_was_suspended` (property and whole paths) and the whole-object receiver test.
  - `[low]` `[patch]` Refusing a function or cyclic value stored into a Global has no test — gap as filed. Added `a_function_or_a_cycle_cannot_be_stored_in_a_global_and_the_stored_value_stays` (codes, exact messages, stored value unchanged).
  - `[low]` `[patch]` Global seeding at registration has no failure-path test — gap as filed. Added `a_global_initializer_that_fails_fails_the_registration_and_frees_the_name` and `seeding_hoists_no_function_so_disabled_callbacks_refuse_only_the_handlers`.
  - `[low]` `[patch]` (verification-gap other) Cyclic message spaces — same as the edge-case row; fixed.
  - `[low]` `[patch]` Tracking says `ready-for-dev` while the story is implemented — verified; finalization sets sprint-status to `review` and the spec to `done`. The orchestrated run plans then implements, so reading C is the run's scope.
  - `[false]` `[reject]` AC3 narrowed to call-free statements — the frozen intent's Always rule states exactly that a statement with a call has separate atomic read and write steps.
  - `[low]` `[reject]` AC2's end-to-end test does not prove two intent holders on different units never wait — an intent is held only across a call-free expression, which cannot be stalled from outside, so the property is observable only at the store, where `writes_to_different_units_never_wait_on_each_other` pins it.
  - `[medium]` `[patch]` AC4: no test that a handler observes a write made during its own suspension — same as the mid-invocation catch-up gap; fixed by the stall-driven test.
  - `[false]` `[reject]` AC5 holds only by construction — no Plugin actor exists; the store is owned by `GlobalVariables` and torn down only by `hexput-session`, which the session tests pin. Nothing to fix.
  - `[false]` `[reject]` Side-effect charging is beyond the ACs — the frozen intent's Always rule requires "each write counts as one side effect".
  - `[false]` `[reject]` Initializers-once and the untouched `hexput-connection` are outside the surface — the intent requires initialization exactly once; the Code Map's connection change proved unnecessary (stores ride `Plugins`), and the review patch now touches it only for the guard.

## Auto Run Result

**Summary.** Story 6.7 makes a Plugin's Global Variables persistent and shared: `hexput-globalvar` is the per-Plugin store of lock units (one per top-level property of an object Global, one for any other Global) with versions and writer intents; the interpreter reads and writes Globals by name through the `Globals` trait (catch-up on read, per-unit write-back, the `safe` intent rule); `hexput-exec` implements it and charges each write as one side effect; `hexput-plugin` seeds once at registration and passes the store to every invocation; `hexput-session` tears stores down (AD-4). The review pass re-derived the intent lock (a whole write's intent overlaps its variable's property intents, intents kept per variable), made catch-up keep an object copy's identity, and patched the teardown order, the registration guard and secret clearing.

**Files changed.**
- `crates/hexput-globalvar/src/lib.rs`, `Cargo.toml` — the store, registry, `Teardown` trait, overlapping per-variable writer intents.
- `crates/hexput-interpreter/src/{globals,lib,machine,heap}.rs` — `Globals`/`Changes`, `Outcome::GlobalWrite`, `Execution::with_globals`/`seeding`, by-name reads/writes, identity-keeping catch-up, `Heap::clear_property_secret`.
- `crates/hexput-exec/src/{globals,lib}.rs` — `Stored`, `PluginGlobals`, `seed_globals`, `invoke_handler(…, globals)`.
- `crates/hexput-enforce/src/lib.rs` — `Budget::charge_side_effect`.
- `crates/hexput-plugin/src/lib.rs` — store created by `prepare`, seeded by `initialize`, registered by `Plugins::insert`, passed by `run_fire`.
- `crates/hexput-session/src/lib.rs` — `with_global_variables`, store teardown before and after the hook, and on `release_plugin`.
- `crates/hexput-connection/src/lib.rs` — the registration guard decided inside the `finish` closure.
- `crates/hexput-daemon/src/lib.rs` — `registry` wires the stores; its test asserts them.
- `crates/hexput-tests/{tests/{globalvar,daemon,session,exec,interpreter}.rs,benches/cache.rs,Cargo.toml}` — tests.
- LANGUAGE-REFERENCE §9, AGENTS.md, sprint-status.yaml, deferred-work.md.

**Review findings.** 35 findings: 1 bad_spec loop (whole vs property intents), 26 patch rows (1 high, 8 medium, 17 low entries' members, several grouped), 2 deferred items (store-size quota; Story 6.13 overlap note), rejected: seeding detach/secret round trip (low; unreachable/correct), Global-specific error codes (low; new public surface), AC2 end-to-end intent independence (low; store-level only), and four `false` intent-alignment claims (AC3 narrowing, AC5, side-effect charge, initializers-once/connection surface) on the refutations logged above.

**Follow-up review recommended: true.** Patched by entry verdict: high 1 (stale receiver after a whole catch-up), medium 6 entries (TOCTOU, intent-map growth, missing interpreter tests, mid-invocation catch-up test, loop/alias orphaning, "latest values" claim), low the rest. Named risk: the concurrency of the new overlapping intent lock (`Holders` with `notify_all`) and of the identity-keeping catch-up (`replace_contents` during a statement) is exercised only by the new daemon tests' schedules; a second reviewer should check them for starvation and for a catch-up landing between an assignment's heap write and its write-back.

**Verification.** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 1347 passed, 0 failed. The stale-receiver test was confirmed to fail (100 vs 101) with the in-place catch-up disabled.

**Residual risks.** A whole write that replaces an object Global's own Value Secret leaves other invocations' copies with the old one (contents are replaced in place, the copy's own secret kept). A whole-variable intent can be starved by a continuous stream of property intents (no writer preference). The store has no size quota (deferred).
