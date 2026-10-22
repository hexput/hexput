---
title: 'Story 5.8: Expire a session only after the last connection is gone'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '2a33d48d608fb27fd0c5738ccf47aa91a57414b1'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred:
  - summary: >-
      An orphaned Session now lives for session_ttl_secs (default 300 s), so a connect, Init, CodeRegister, disconnect loop can hold about one TTL's worth of Sessions with no cap.
    evidence: |-
      Required FR-2/FR-21 behaviour lengthens the pre-existing uncapped-registry exposure ("The Session registry is unbounded" in deferred-work.md assumed a Session lasts only while connected). Settle with the deferred Session/connection caps, counting orphaned Sessions.
    location: >-
      crates/hexput-session/src/lib.rs (Sessions::detach)
    severity: medium
  - summary: >-
      A panic inside serve still skips the detach, so the TTL never starts and the Session leaks until the Daemon stops; the Story 2.4 deferral that pointed at Story 5.8 stays open.
    evidence: |-
      The countdown starts only from detach on serve's one exit path; Story 5.8 added no supervisor. Nothing in serve is known to panic on peer input. Settle with a Daemon-side supervisor that detaches a panicked connection and runs its Expiry.
    location: >-
      crates/hexput-connection/src/lib.rs (serve)
    severity: medium
---

<intent-contract>

## Intent

**Problem:** Detaching a Session's last Connection tears it down at once, so a Backend that drops briefly loses its Config, registrations and AST Cache entries and can never reconnect; System Config's `session_ttl_secs` is parsed but unused (FR-2, FR-21, FR-24, AD-2).

**Approach:** Delay the one existing private `teardown` path behind the TTL. The registry (still synchronous) marks a Session whose last Connection left as *expiring* with a fresh countdown generation and hands the caller an `Expiry`; `hexput-connection::serve` sleeps the TTL on a Tokio task and then asks the registry to expire it, which tears down only if the Session is still unattached under that same generation. A reconnect (or attach) before then clears the countdown under the shard lock. The Daemon builds its registry with System Config's TTL.

## Boundaries & Constraints

**Always:**
- The countdown starts only when the *last* Connection detaches; while any Connection is attached none runs (detaching a non-last Connection returns no `Expiry`).
- Expiry tears down through the one private `teardown` fn — Config, credential, registrations, then the teardown hook (AST Cache `close`) — exactly as today; no second teardown path, nothing implied by `Drop` (AD-4).
- `Sessions::reconnect` (and `attach`) on an expiring Session attaches and stops the countdown in the same shard lock as the credential verify; state is untouched (same Config, registrations, cache handles, same reconnect secret).
- Expiry vs reconnect race: `expire` removes the Session only via `remove_if_mut` when it is unattached *and* its countdown generation equals the `Expiry`'s; a stale `Expiry` (from an earlier orphaning, the countdown since stopped and maybe restarted) does nothing. Each orphaning draws a new generation.
- After expiry a reconnect with the old Client ID and secret is `protocol.reconnect_denied`, byte-identical to an unknown Client ID (already true once the entry is gone); its cache handles are gone.
- A registry with no TTL set (`Sessions::new()`, TTL `Duration::ZERO`) tears down on the last detach, as now — the same path with zero delay. The Daemon always sets System Config's TTL (validated ≥ 1 s), so in the Daemon the immediate teardown is replaced.
- `hexput-session` stays synchronous and adds no dependency; the timer lives in `hexput-connection` (already on tokio with `time`). No lock held across an `.await`.
- Logging (debug, fields not message text, no secret): after the existing "connection detached from its Session", when an `Expiry` was returned, "last connection detached; the Session expires after its TTL unless a Connection reconnects" with `ttl_secs`; when `expire` returns true, "Session expired after its TTL; torn down" with `client_id`.
- Every cargo command runs with `--release`.

**Never:**
- No new wire message, field or error code; no crate edge or new dependency; no transport name in a production crate outside the AD-1 set.
- No attempt limit, no connection cap, no per-Session TTL set by the Backend (TTL is System Config only).
- No reaper/sweeper loop; no tearing down Sessions one by one on Daemon shutdown (the registry drops with the run; pending timers are cancelled with the runtime).
- Do not weaken an existing test; tests on `Sessions::new()` keep immediate teardown.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Busy Session | TTL T; A and B attached; time > T passes | Session live, `attached == 2`; no `Expiry` was ever issued | — |
| Non-last leaves | A, B attached; B detaches | `detach` returns `None`; Session live | — |
| Last leaves, no reconnect | A detaches (TTL T > 0) | `Some(Expiry)`; `contains` true, `attached == Some(0)` until expired; then gone, hook called once, cache handles released | — |
| Reconnect in time | last left; valid `Reconnect` before T | `Result {client_id}`; Config, registrations and cache handles intact; the old `Expiry` later does nothing | — |
| Stale expiry | orphan (e1), reconnect, detach again (e2) | `expire(e1)` false, Session live; `expire(e2)` true | — |
| Reconnect after expiry | torn down by TTL | `protocol.reconnect_denied`, payload identical to unknown Client ID; `Init` then works fresh | not fatal |
| Wrong secret during countdown | expiring Session, wrong secret | `protocol.reconnect_denied`; countdown keeps running; Session still expires | — |
| Zero TTL | `Sessions::new()` | last detach tears down at once, returns `None` | — |

</intent-contract>

## Code Map

- `crates/hexput-session/src/lib.rs` -- `Session` struct (~98; add the countdown generation, e.g. `expiring: Option<u64>`), `Sessions` (~109; add `ttl: Duration`, generation counter), `with_teardown` (~140, model `with_ttl` on it), `attach` (~213), `detach` (~223, `remove_if_mut` → now marks expiring and returns `Option<Expiry>` when TTL > 0), `reconnect` (~254, clears `expiring` on success), `attached` (~325), private `teardown` (~400, doc says 5.8 delays the call, not moves it). Module doc (~1-77): add "What Story 5.8 adds"; fix the 2.4/2.5 and 4.3 paragraphs' "Story 5.8 will" wording.
- `crates/hexput-connection/src/lib.rs` -- `serve` (~211-237): the one detach call site; spawn the TTL timer task there with `Arc<Sessions>` (`tokio::spawn`, `tokio::time::sleep(expiry.ttl())`), log as above. Module doc (~151-158, "detaching the last Connection tears the Session down") to update.
- `crates/hexput-daemon/src/lib.rs` -- `registry(cache)` (~232) → `registry(cache, ttl)` using `loaded.config.session_ttl` at the call in `serve` (~262); shutdown comment (~337) still holds; in-crate test `the_daemons_registry_releases_a_sessions_entries_when_it_is_torn_down` (~670) detaches and expects immediate release — pass a TTL and call `expire` on the returned `Expiry`, asserting nothing is released before it. Module doc "Serving" paragraph: mention the TTL.
- `crates/hexput-config/src/lib.rs` (`session_ttl`, ~140, `DEFAULT_SESSION_TTL` ~99) -- read-only; already validated ≥ 1.
- `crates/hexput-tests/tests/session.rs` -- registry tests near `detaching_the_last_connection_tears_the_session_down` (430), helper `create` (379), `concurrent_creates_and_detaches_leave_nothing_behind` (488); reconnect tests further down (~750-1030) for the credential pattern.
- `crates/hexput-tests/tests/connection.rs` -- `a_reconnect_after_the_session_is_torn_down_is_denied_exactly_like_an_unknown_id` (~5609, model), helpers `issued`/`open_bare`/`refused_with`/`reconnect`/`init_reply_of`/`on_runtime`/`finished`/`assert_silent`, `cache_registry`-style teardown wiring (~1317), `register_with` (~4117), `cached_run` (~4559). Existing tests use `Sessions::new()` (immediate) and must stay green.
- `crates/hexput-tests/tests/daemon/conformance.rs` -- `Daemon::start` (137), `log_with`/`log_count` (~175-200), `reconnect` case (1246) for the wire pattern; `served()` transports.
- `crates/hexput-tests/tests/daemon.rs` -- `Sandbox::valid` (121); test-level JSON log helpers (~1300).
- `ARCHITECTURE-SPINE.md` -- after the Story 5.7 amendment (~168).

## Tasks & Acceptance

**Execution:**
- `crates/hexput-session/src/lib.rs` -- add `Sessions::with_ttl(Duration)` (chainable with `with_teardown`), a public `Expiry` (`client_id()`, `ttl()`, `Debug`; its generation private, so it is constructible only by `detach`), `detach` returning `Option<Expiry>`, `Sessions::expire(&Expiry) -> bool`, and `Sessions::expiring(client_id) -> Option<bool>` (whether a countdown runs; `None` when absent); clear the countdown in `reconnect`/`attach`; update docs -- the TTL lives in the registry, the one teardown path is delayed, not duplicated.
- `crates/hexput-connection/src/lib.rs` -- in `serve`, on `Some(expiry)` from `detach`, log and spawn the timer task calling `sessions.expire(&expiry)` after `expiry.ttl()`; update the module doc -- the countdown runs only after the last Connection leaves.
- `crates/hexput-daemon/src/lib.rs` -- wire `loaded.config.session_ttl` into the registry; update the in-crate test and module doc.
- `crates/hexput-tests/tests/session.rs` -- deterministic registry tests (no sleeping) for every matrix row except the wire-only ones: busy/non-last returns `None`; last returns an `Expiry` with the TTL and Client ID, `expiring == Some(true)`, `attached == Some(0)`; `expire` tears down once (hook count 1), a second `expire` is false; reconnect during countdown → `expiring == Some(false)`, stale `Expiry` false, state (settings, registrations) intact; wrong secret leaves the countdown; zero TTL immediate; a concurrent detach/reconnect/expire stress leaving no Session torn down while attached.
- `crates/hexput-tests/tests/connection.rs` -- in-memory with a short TTL (e.g. 200 ms) and teardown wired to the AST Cache: last close then reconnect within TTL runs a host call and a previously registered handle; two attached Connections survive > 3×TTL; last close with no reconnect → after TTL the Session and cache handles are gone and a reconnect is denied with the unknown-ID payload, then `Init` fresh works.
- `crates/hexput-tests/tests/daemon.rs` (or the conformance module, outside the case section) -- Daemon with `session_ttl_secs = 1`, `log_level = "debug"`: Init, register a Script, drop; reconnect on a new connection before 1 s → the handle still runs; drop again; wait for "Session expired after its TTL; torn down" via `log_with`; reconnect → `protocol.reconnect_denied`; exit `0`.
- `ARCHITECTURE-SPINE.md` -- `[Amended 2026-10-22, Epic 5 Story 5.8]` paragraph: TTL from System Config starts at the last detach, `Expiry` + generation, reconnect stops it under the shard lock, the timer in `hexput-connection` (registry stays sync), zero TTL = immediate (registry default, never the Daemon), expiry indistinguishable from an unknown ID, no reaper, shutdown drops pending timers; note the 5.6 amendment's "until Story 5.8" sentence is superseded.
- `AGENTS.md` -- Project Status: Story 5.8 landed, test count, next step 5.9.

**Acceptance Criteria:**
- Given a Daemon whose System Config sets `session_ttl_secs`, when a Session has at least one attached Connection for longer than that TTL, then it stays live and serves requests.
- Given a Session whose last Connection dropped, when the TTL elapses with no reconnect, then the Session, its Config, registrations and AST Cache entries are gone, through the same teardown path (hook called exactly once).
- Given a Session whose last Connection dropped, when a valid `Reconnect` arrives before the TTL elapses, then it succeeds and the Session's Config, registrations and cached Scripts are unchanged, and the earlier countdown never tears it down.
- Given a Session torn down by TTL expiry, when a reconnect with its Client ID and secret arrives, then it is `protocol.reconnect_denied` exactly like an unknown Client ID, and an `Init` on that connection creates a fresh Session.
- Given the change, when `python3 scripts/check-crate-graph.py` runs, then it passes with no new edge or dependency.

## Design Notes

Decisions taken unattended (record in the Spine amendment):
- **Timer placement.** `hexput-session` is deliberately synchronous ("nothing here is async"); adding tokio there would be a new dependency and make `detach` need a runtime. The registry therefore owns *what* (TTL, generation, the atomic expire), and `serve` — the one production detach site, already on tokio — owns *when*. A detached `tokio::spawn` per orphaning is enough: it holds only an `Arc<Sessions>` and an `Expiry`, and the runtime cancels it at shutdown.
- **Zero TTL means now.** Keeps `Sessions::new()` (used by ~100 tests) meaning what it meant, and is the same path with no delay — not a second path. The Daemon can never configure zero.
- **Generation, not deadline check.** `expire` trusts the caller's timing and guards only identity of the countdown; this makes the registry tests deterministic (call `expire` directly) and kills the reconnect-then-drop-again race.

```rust
pub fn detach(&self, client_id: ClientId, connection: ConnectionId) -> Option<Expiry>;
pub fn expire(&self, expiry: &Expiry) -> bool; // true = torn down now
```

## Verification

**Commands (release only — disk is scarce):**
- `cargo fmt --all --check` -- clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- no warnings
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo build --workspace --all-targets --locked --release` -- builds; `Cargo.lock` unchanged
- `cargo test --workspace --locked --release` -- all pass
- `scripts/check-windows.sh` -- clean, if the target is installed

## Auto Run Result

Status: done
Blocking condition: none.

**Implemented:** a Session outlives its last Connection by System Config's `session_ttl_secs`. `hexput-session` (still synchronous) gained `with_ttl`, `Expiry`, `detach -> Option<Expiry>`, `expire` (generation-guarded, `remove_if`, the one teardown path) and `expiring`; `reconnect`/`attach` stop a countdown under the shard lock. `hexput-connection::serve` sleeps the TTL on a Tokio task and calls `expire`; the Daemon wires the TTL into its registry. A registry with no TTL still tears down at once.

**Files changed:**
- `crates/hexput-session/src/lib.rs` -- TTL, `Expiry`, countdown generation, `expire`/`expiring`, docs (5.6/5.7 race wording fixed in review, `detach` documents a dropped `Expiry`).
- `crates/hexput-connection/src/lib.rs` -- the timer task in `serve`, its logging, module doc; `serve` documents that a TTL registry needs timers.
- `crates/hexput-daemon/src/lib.rs` -- `registry(cache, ttl)` from System Config; in-crate test; shutdown comment (countdowns hold the registry until the run's runtime drops).
- `crates/hexput-tests/tests/session.rs` -- deterministic registry tests, plus (review) attach stops the countdown and a detach-vs-stale-expire stress.
- `crates/hexput-tests/tests/connection.rs` -- wired TTL tests (1 s TTL where a test acts inside the countdown), a host call after reconnect, and a JSON-log test pinning `ttl_secs` and `client_id`.
- `crates/hexput-tests/tests/daemon.rs` -- socket-level expiry/resume test (2 s TTL, asserts the first countdown never fires while attached and the expiry is logged once, after the second start).
- `ARCHITECTURE-SPINE.md`, `epic-5-context.md`, `deferred-work.md`, `sprint-status.yaml`, `AGENTS.md` -- amendment, landed bullet, two deferrals, status `review`, Project Status.

**Review findings:** 25 findings (high 0, medium 10, low 10, false 5). Patched: 18 rows (B1, B4–B10, B12, E1, E3–E7, V1 and the tracking rows). Deferred: 2 (unbounded orphaned Sessions; panic in `serve` still leaks). Rejected: B11 (log traceability -- low; the span already carries the Client ID, now pinned by a test; a new field adds log surface for no requirement), E2 (false -- the Daemon's runtime is built in `run` and dropped right after `serve` returns), I1 (false -- this run's stage implements; the planning halt was the earlier stage's), I2 (low -- busy Session covered at registry and in-memory levels; a 300 s Daemon test is not worth its cost), I3/I4 (false -- spec-sanctioned), I5 (false -- process rule, verified by the release-only runs).

**Follow-up review recommended: true** (first pass, 5 medium entries patched, 0 high). Specific unverified risk: the countdown tests still run on wall-clock time (1–2 s windows rather than Tokio's paused clock); a follow-up should confirm they are stable under CI load.

**Verification:** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` OK, `Cargo.lock` unchanged; `cargo test --workspace --locked --release` 1082 passed, 0 failed; `scripts/check-windows.sh --release` clean.

**Residual risks:** timing-based tests (above); the two deferrals.

## Implementation Notes


- Tests pre-written by the test agent in `crates/hexput-tests/tests/session.rs` (`a_detach_with_another_connection_still_attached_starts_no_countdown`, `the_last_detach_returns_an_expiry_and_starts_the_countdown_without_tearing_down`, `expire_tears_the_session_down_once_through_the_hook_and_a_second_expire_is_false`, `a_reconnect_during_the_countdown_attaches_stops_it_and_keeps_the_state`, `a_stale_expiry_from_an_earlier_orphaning_does_nothing_and_the_newer_one_expires`, `a_wrong_secret_during_the_countdown_is_refused_and_the_countdown_keeps_running`, `a_session_expired_by_its_ttl_is_refused_a_reconnect_exactly_like_an_unknown_one`, `a_zero_ttl_tears_down_on_the_last_detach_at_once_as_before`, `with_ttl_and_with_teardown_chain_in_either_order`, `concurrent_reconnects_and_expiries_never_tear_down_an_attached_session`), `crates/hexput-tests/tests/connection.rs` (`a_reconnect_within_the_ttl_resumes_the_session_and_its_cached_handles_and_the_countdown_never_fires`, `two_attached_connections_keep_their_session_live_past_several_ttls`, `the_last_close_with_no_reconnect_expires_the_session_and_its_handles_then_a_reconnect_is_denied`, `a_wrong_secret_during_the_countdown_is_denied_and_the_session_still_expires`) and `crates/hexput-tests/tests/daemon.rs` (`a_session_expires_one_ttl_after_its_last_connection_and_a_reconnect_before_that_resumes_it`; `Serving` gained a readable `log` and `log_with`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Implementation (decisions taken unattended): `Expiry` derives `Clone`/`PartialEq`/`Eq` besides `Debug`; `detach` is not `#[must_use]` (tests detach as a statement). `expire` uses `DashMap::remove_if`. The timer task in `serve` is instrumented with the connection span (which carries `client_id`), so "Session expired after its TTL; torn down" names the Client ID through the span rather than a duplicate event field; `ttl_secs` is logged as an `f64` so sub-second test TTLs are not shown as `0`.
- Test fixes (not weakening): `connection.rs`'s `a_reconnect_within_the_ttl_…` moved `cache` into its `async move` block and then read it outside; it now reads a cloned `Arc`. `daemon.rs`'s new `Duration`/`Instant` import is `#[cfg(unix)]`, since only unix code uses it (`scripts/check-windows.sh --release` flagged it).
- Verification: the five CI commands (release forms) pass, 1079 tests on Linux; `scripts/check-windows.sh --release` clean; `Cargo.lock` unchanged.

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 25 findings — high 0, medium 10, low 10, false 5, maybe-false 0
- findings:
  - `[low]` `[patch]` B1 `detach` not `#[must_use]`; a dropped `Expiry` leaks the Session — documented the hazard on `detach` (must_use would force `let _ =` on ~100 zero-TTL call sites that never get one).
  - `[medium]` `[defer]` B2 orphaned Sessions pile up for a TTL with no cap — required TTL behaviour lengthening the pre-existing uncapped registry; deferred (frontmatter + deferred-work.md).
  - `[medium]` `[defer]` B3 panic in `serve` still skips detach; Story 2.4 deferral pointed at 5.8 — pre-existing, no supervisor in scope; deferral recorded saying why it stays open.
  - `[low]` `[patch]` B4 stale 5.6/5.7 race docs in `hexput-session` — module doc and `reconnect` doc now name `expire` and the zero-TTL detach as what removes a Session.
  - `[low]` `[patch]` B5 stale epic-5-context bullet, AGENTS.md 5.6 text, Spine 5.6 amendment — bullet rewritten as "(landed)", AGENTS.md phrase corrected with the Project Status update, Spine sentence marked superseded inline.
  - `[low]` `[patch]` B6 sprint-status `ready-for-dev` and the planning-only Auto Run Result contradict the implementation — sprint-status set to `review`; Auto Run Result rewritten at finalization.
  - `[medium]` `[patch]` B7 in-memory reconnect test makes no host call — added `getUser(5)` round trip after the reconnect.
  - `[medium]` `[patch]` B8 wired tests depend on a 200 ms wall-clock window — tests acting inside the countdown use `RESUMABLE_TTL` (1 s); `SHORT_TTL` kept only where nothing happens inside it.
  - `[medium]` `[patch]` B9 Daemon test never checks the first countdown did nothing — sleeps past the first TTL while attached, asserts no expiry logged, then exactly one expiry after the second start.
  - `[low]` `[patch]` B10 no test that `attach` stops a countdown; stress races only reconnect/expire — added `an_attach_during_the_countdown_stops_it_too` and `concurrent_detaches_and_stale_expiries_never_tear_down_by_the_old_countdown`.
  - `[low]` `[reject]` B11 expiry events hard to trace (old connection span, no cancel log) — the span carries the Client ID FR-12 requires (now pinned); new log fields add surface no requirement asks for.
  - `[low]` `[patch]` B12 "registry drops with the run" while timers hold it — Daemon shutdown comment now says countdowns hold it until the run's runtime drops right after `serve` returns.
  - `[low]` `[patch]` E1 TTL registry on a runtime without timers panics the timer task — `serve` doc states the timers requirement (the Daemon's runtime has them).
  - `[false]` `[reject]` E2 Daemon stops serving while the runtime keeps running — `run` builds the runtime and drops it right after `block_on(serve)` returns, so no timer outlives the run.
  - `[medium]` `[patch]` E3 reconnect test's 200 ms window flaky — grouped with B8; 1 s TTL.
  - `[medium]` `[patch]` E4 "still held" assertion's 200 ms window flaky — grouped with B8; 1 s TTL.
  - `[medium]` `[patch]` E5 Daemon reconnect must land within 1 s — `session_ttl_secs = 2`.
  - `[medium]` `[patch]` E6 host call after reconnect not tested — grouped with B7.
  - `[low]` `[patch]` E7 stress does not race a detach — grouped with B10.
  - `[medium]` `[patch]` V1 `ttl_secs`/`client_id` on the expiry events untested — added `the_countdown_and_the_expiry_are_logged_at_debug_with_the_ttl_and_the_client_id` (JSON log; `fields.ttl_secs`, `span.client_id` on both, expiry exactly once at DEBUG).
  - `[false]` `[reject]` I1 diff implements code while intent said halt after planning — this run's stage is the build's implementation/review; the planning halt applied to the earlier invocation; status contradictions are B6's rows.
  - `[low]` `[reject]` I2 busy-Session criterion not exercised at Daemon level or with the 300 s default — covered by registry and wired tests with the Daemon's own TTL wiring covered separately; a 300 s test is not worth its cost.
  - `[false]` `[reject]` I3 immediate teardown survives as the registry default — the spec's Boundaries sanction it (same path, zero delay); the Daemon always sets a TTL ≥ 1 s.
  - `[false]` `[reject]` I4 AST Cache release only through the hook — the spec requires the hook path (AD-4); the Daemon's wiring is tested in-crate and over the socket.
  - `[false]` `[reject]` I5 `--release` rule not observable in the diff — a process rule, satisfied by every verification run.
