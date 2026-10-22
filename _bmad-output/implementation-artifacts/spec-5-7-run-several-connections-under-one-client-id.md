---
title: 'Story 5.7: Run several connections under one client ID'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'fe7a48669fa7ac22e9726dcbf558f34bff93af32'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred:
  - summary: >-
      AGENTS.md's Build, Lint, Test section lists only debug-profile cargo commands, while the maintainer requires every build on this machine to use --release.
    evidence: |-
      Review finding (blind-hunter). The user asked for every build to use --release because disk is scarce; only this spec's Always list and Verification section record it, so the next story's agent following AGENTS.md builds in debug. The fix edits an agent-context file, which triage routes to defer. Settle by adding a machine-local note (or an AGENTS.md line saying local runs use the --release form while CI keeps its own profile).
    location: >-
      AGENTS.md (Build, Lint, Test)
    severity: medium
---

<intent-contract>

## Intent

**Problem:** Story 5.6 lets a second connection attach to a live Session with `Reconnect`, but nothing proves end to end that several Connections under one Client ID run side by side: that each execution's replies, `Call`s and `Authorize` questions stay with the Connection that issued it while the others work concurrently, that Session-owned state changed through one is seen by all, and that this holds whatever transport each Connection uses (FR-2, AD-2, AD-5).

**Approach:** Mostly proof, not new machinery: the registry already holds a set of Connections and each connection already owns its correlation. Add tests at three levels — registry, in-memory connection, and the real Daemon (a conformance case over every transport plus one Session spanning every transport the platform serves at once) — fix any single-connection assumption they expose, and record the multi-Connection contract in docs and a Spine amendment.

## Boundaries & Constraints

**Always:**
- Any number of Connections may be attached to one Session at once (no cap), each attached by `Init` (the first) or `Reconnect`; they may use different transports.
- Per-Connection (never shared): request ids, replies, the `Call`/`Authorize` id counter and pending table. A request id reused on two Connections is two independent requests. A `Call`/`Authorize` for an execution goes only to the Connection that started it, and only that Connection's reply completes it; a `Result`/`Error` on another Connection naming that id is answered there as today (`protocol.unexpected_message`) and leaves the other call pending.
- Per-Session (shared, read afresh at dispatch): Config (`ConfigUpdate` from any attached Connection; its `Result {}` goes to the updater only), the registrations made at `Init`, and AST Cache entries (`CodeRegister` on one, `CachedExecutionStart`/`CodeUnregister` on any). No broadcast: no Connection is told of another's change.
- A Connection leaving (clean close, lost stream, fatal frame) detaches it alone; other Connections' in-flight executions and the Session continue. The Session is torn down only when the last one detaches (TTL is Story 5.8).
- AD-1 guard: no production source outside `hexput-transport`/`hexput-daemon`/`hexput-config` names a transport; transport names in tests stay outside the conformance `// BEGIN CASES` section.
- Every cargo command runs with `--release`.

**Never:**
- No new wire message, field, error code, crate edge or dependency. No runtime function-registration request (see Design Notes).
- No cross-Connection broadcast or fan-out of any reply, `Call` or event; no Session-level request ordering across Connections (AD-6).
- No TTL, delayed teardown or reconnect after the last Connection left (Story 5.8). No connection cap or attempt limit.
- Do not weaken an existing test.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Both live | A `Init`s; B `Reconnect`s | `attached == 2`; each serves executions | — |
| Same id, concurrent host calls | A and B each send `ExecutionStart` id 2 calling `double`, both pending at once | each gets exactly one `Call` on its own Connection; B answered first, then A; each `Result` id 2 carries its own value; nothing else on either | — |
| Reply on the wrong Connection | B, with no call of its own pending under that id, answers A's pending `Call` id | B: `protocol.unexpected_message`; A's call stays pending and A's own answer completes it | not fatal |
| Per-call handler | A runs a non-blanket `ask` | `Authorize` reaches A only | — |
| Config shared | B `ConfigUpdate`s `budget.rpc_calls = 0` | `Result {}` on B only; A's next host call `budget.rpc_calls_exceeded` | — |
| Cache shared | A `CodeRegister`s; B runs the handle; B `CodeUnregister`s | B gets the value; A's later run `protocol.unknown_script_handle` | — |
| One leaves mid-flight | B closes (or sends a fatal frame) while A's execution waits on a `Call` | A's execution completes normally; Session live with A; A's close tears it down | — |
| Three, mixed transports | one Daemon serving every platform transport; one Connection per transport on one Client ID | each transport's Connection gets only its own replies; a `ConfigUpdate` on one is in force on the others; closing all but one keeps the Session | — |

</intent-contract>

## Code Map

- `crates/hexput-session/src/lib.rs` -- `Sessions::create`/`attach` (~213)/`reconnect`/`detach` (~223, `remove_if_mut`)/`attached` (~325)/`for_execution`; module doc (~13-55, "zero or more"). Expected: doc only ("What Story 5.7 adds" paragraph).
- `crates/hexput-connection/src/lib.rs` -- `serve` (200), `exchange` (260; `Calls::new()` per connection at 265), `answer` (554; `ConfigUpdate`/`CodeRegister`/`CodeUnregister` routing 595-599), reply routing for `Result`/`Error` naming a pending call, module doc (~8-140). Expected: doc only, unless a test exposes a single-connection assumption.
- `crates/hexput-rpc` -- `Calls`/`Caller`: the per-connection id counter and pending table (read-only evidence for the per-Connection rule).
- `crates/hexput-tests/tests/session.rs` -- `detaching_one_of_two_connections_keeps_the_session` (449) and the reconnect tests to model on.
- `crates/hexput-tests/tests/connection.rs` -- helpers: `open_bare`/`issued`/`on_runtime`/`refused_with`/`resumed`/`reconnect` (~5300-5420), `Backend::{reply,call,question}` (1187-1210), `execution` (711), `init_payload`/`init_configured` (213), `budget_of` (2262), `config_update` (2508), `register_with` (4117), `cached_run` (4559), `unregister_request` (4848), `finished`, `value_of`, `code_of`. Existing `an_update_through_one_connection_is_seen_by_every_attached_connection` (2677) uses `attach` directly.
- `crates/hexput-tests/tests/daemon/conformance.rs` -- `Open`/`Case` (~756), `run` (760), `quiet` (801), `CASES` (970), `reconnect` case (1225, the credential/reconnect pattern to reuse), `expect_request`/`answer` (1162-1200), `served()` from `transports!` (589-630), `Transport::section`/`connect`, harness `Daemon::start` (134). Module doc (1-50) describes the case contract.
- `crates/hexput-tests/tests/daemon.rs` -- `Sandbox::valid` (121, the UDS section), `mod conformance` (20).
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- after the Story 5.6 amendment (~166).

## Tasks & Acceptance

**Execution:**
- `crates/hexput-tests/tests/session.rs` -- three Connections on one Session (one `create`, two `reconnect`): `attached` counts up and down, detaching in any order keeps the Session until the last; registrations/settings identical from every Connection's `for_execution` -- the registry's cardinality (AD-2).
- `crates/hexput-tests/tests/connection.rs` -- in-memory rows of the matrix with two (where useful three) wire-attached Connections: same-id concurrent host calls answered out of order; reply on the wrong Connection; `Authorize` to the issuer only; `ConfigUpdate` on B in force on A's next execution with no message on A; `CodeRegister`/`CodeUnregister` across Connections; B closing and B sending a fatal frame while A's execution waits; after each, `try_recv` shows nothing stray on the other Connection; `sessions.is_empty()` at the end -- per-Connection vs per-Session rules at the core loop.
- `crates/hexput-tests/tests/daemon/conformance.rs` -- add case `several_connections` after `reconnect` (A `begin`s, B reconnects via `open`, then the same-id concurrent `double` calls, B's `ConfigUpdate` refusing A's next host call, A's `CodeRegister` run then unregistered on B and unknown on A, then `quiet(B)`); the runner's `quiet(A)` closes it -- identical behaviour on every transport, normalized transcripts byte-identical. Outside the case section, add `one_session_spans_every_transport_this_platform_serves`: one Daemon whose System Config concatenates every `served()` transport's section; A `Init`s on the first, each other transport `Reconnect`s; interleaved executions with host calls on each return only to their issuer; a `ConfigUpdate` on the last is in force on the first; closing all but the first leaves it serving; Daemon exits `0`. Update the module doc's case contract (a case may open further connections through `open`).
- `crates/hexput-session/src/lib.rs`, `crates/hexput-connection/src/lib.rs` -- doc paragraphs stating the per-Connection/per-Session split above; fix any defect the tests expose -- the contract stated where it lives.
- `ARCHITECTURE-SPINE.md` -- `[Amended 2026-10-22, Epic 5 Story 5.7]` paragraph: the per-Connection/per-Session split, no broadcast (incl. `ConfigUpdate`'s reply), wrong-Connection reply handling, no connection cap, mixed transports under one Client ID, and the registration reading below.
- `AGENTS.md` -- Project Status: Story 5.7 landed, test count, next step 5.8.

**Acceptance Criteria:**
- Given a live Session with one attached Connection, when a second connection reconnects with the Client ID and secret over any served transport (the same or a different one), then both are attached and serve requests simultaneously.
- Given two Connections on one Session, when each submits an execution (even with the same request id, each making host calls), then every reply, `Call` and `Authorize` reaches only the Connection that issued the execution, and the other Connection receives nothing.
- Given two Connections on one Session, when one updates the Config or registers (or unregisters) a Script, then the other's next request runs under that change, without any notification message.
- Given the change, when `python3 scripts/check-crate-graph.py` runs, then it passes with no new edge, dependency or transport name in a production crate.

## Design Notes

Decisions taken unattended (record them in the Spine amendment):
- **"Registers a function" reads as the Session-owned registrations that exist.** Function registrations are made only at `Init` (FR-1); neither PRD, Spine nor epics define a runtime function-registration request, and inventing one would be a wire change no planning artifact specifies. The AC is met by (a) `Init` registrations made through one Connection serving every reconnected Connection, and (b) the one registration a Connection can make after attaching, `CodeRegister` (and `CodeUnregister`), visible to all. If a later epic adds runtime registration, it lands in `Session` state and inherits this.
- **A wrong-Connection reply is not routed across.** Correlation is per Connection (each has its own id space, so ids collide across Connections by design); routing by id across a Session would let one Connection complete another's call.
- **No cap on attached Connections** in this story: the uncapped-connection item already in deferred work covers resource exposure.
- Concurrency in the conformance case must be deterministic per Connection: send both requests, then read each Connection in a fixed order (each stream is FIFO), so transcripts stay byte-identical.

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

**Summary.** Story 5.7 proves several Connections live under one Client ID with no new machinery: tests at the registry (`hexput-tests/tests/session.rs`), in-memory connection (`tests/connection.rs`) and real Daemon levels (conformance case `several_connections` on every transport, and `one_session_spans_every_transport_this_platform_serves`), plus the per-Connection/per-Session contract documented in `hexput-session`, `hexput-connection` and a dated Spine amendment. No production code, wire field, crate edge or dependency changed; no test exposed a single-connection assumption.

**Files changed.**
- `crates/hexput-session/src/lib.rs` -- module doc: "What Story 5.7 adds" (the multi-Connection contract).
- `crates/hexput-connection/src/lib.rs` -- module doc: several Connections on one Session.
- `crates/hexput-tests/tests/session.rs` -- four registry tests (three Connections, every detach order, shared state outliving the creator, a foreign secret).
- `crates/hexput-tests/tests/connection.rs` -- eleven in-memory tests and a sentinel-based `assert_silent`.
- `crates/hexput-tests/tests/daemon/conformance.rs` -- case `several_connections`, the spanning test, `Daemon::log_count`, module/`Open` docs.
- `ARCHITECTURE-SPINE.md` -- `[Amended 2026-10-22, Epic 5 Story 5.7]`.
- `epic-5-context.md`, `sprint-status.yaml` (`review`), `AGENTS.md` (Project Status, test count).

**Review findings.** 34 findings: 0 high, 16 medium, 12 low, 6 false. Patched entries (grouped by root cause): 6 medium (the session test that could not fail; no test of a leaving Connection with its own call pending; lost stream untested; the spanning test not waiting for detaches; the spanning test with no stray sentinel; `assert_silent` as a one-moment `try_recv`) and 8 low (sprint status, colliding call ids unasserted, unused transcript undocumented, no transport-count assert, an unused registration, the conformance module doc, `Call` names unasserted, no stray check in the basic test, the stale `Open` doc). Deferred: 1 (release-only builds belong in AGENTS.md). Rejected: the stale Auto Run Result (fix edits this spec; rewritten here at finalization), wrong-Connection reply/`Authorize` over real transports (`serve` is transport-generic, AD-1; covered in memory), a panic before `daemon.stop()` (same pattern as the suite's `run`; the failure is still reported), and the six `false` findings with their refutations in the triage log.

**Follow-up review recommended: true** (first pass, six medium entries patched). Specific unverified risk: the spanning test now waits on the Daemon's `debug` "connection detached from its Session" log line and runs with `log_level = "debug"`; that path has run on Linux only — on Windows (Named Pipe + TCP+TLS + WebSocket) it is compile-checked, never executed, here.

**Verification (all release, as the user directed).** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (26 AD edges, 4 restricted names, transport-name guard held); `cargo build --workspace --all-targets --locked --release` built, `Cargo.lock` unchanged; `cargo test --workspace --locked --release` 1064 passed, 0 failed. `scripts/check-windows.sh` not run (no Windows target needed for test-only changes; see risk above).

**Residual risks.** The silence checks are now sentinel-ordered per Connection, which proves nothing arrived before the sentinel's reply; a write that would arrive only after a Connection's last read is still unobservable by construction. Runtime function registration remains unspecified (Design Notes).

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/session.rs` (`three_connections_on_one_session_count_up_and_down_in_any_order`, `a_session_stays_live_until_its_last_connection_leaves_in_every_order`, `the_state_every_connection_reads_is_the_sessions_and_outlives_its_creator` (renamed in review), `a_connection_of_another_session_never_attaches_here`), `crates/hexput-tests/tests/connection.rs` (`two_connections_on_one_session_are_both_attached_and_serve_at_once`, `the_same_request_id_on_two_connections_is_two_requests_each_answered_on_its_own`, `a_reply_naming_another_connections_pending_call_is_unexpected_and_leaves_that_call_pending`, `a_per_call_question_reaches_only_the_connection_that_issued_the_execution`, `a_config_update_on_one_connection_is_in_force_on_the_others_next_request_with_no_message_on_it`, `a_script_registered_on_one_connection_runs_on_another_until_unregistered_there`, `a_connection_closing_mid_flight_leaves_the_others_execution_to_complete`, `a_connection_sending_a_fatal_frame_mid_flight_leaves_the_others_execution_to_complete`, `three_connections_on_one_session_each_get_only_their_own_replies_and_closing_two_keeps_it`, plus in review `a_connection_closing_with_its_own_call_pending_fails_only_that_call` and `a_connection_whose_stream_is_lost_mid_flight_abandons_only_its_own_execution`), and `crates/hexput-tests/tests/daemon/conformance.rs` (case `several_connections`, run by `uds`/`tcp_tls`/`websocket`; `one_session_spans_every_transport_this_platform_serves`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 34 findings — high 0, medium 16, low 12, false 6, maybe-false 0
- findings:
  - `[low]` `[patch]` sprint-status.yaml moves 5.7 to `ready-for-dev`, not `review` — set to `review` at finalization.
  - `[low]` `[reject]` The spec's Auto Run Result still says `ready-for-dev`/halted after planning — the fix edits this build's spec; finalization rewrote the section.
  - `[medium]` `[patch]` `every_connection_of_one_session_reads_the_same_registrations_and_settings` compares one read with itself — replaced by `the_state_every_connection_reads_is_the_sessions_and_outlives_its_creator`: detaches the creator, updates the Config, detaches another, and asserts the remaining Connection reads the `Init` registrations and the update.
  - `[medium]` `[patch]` No test closes a Connection while it has its own `Call` pending under the same id as another's — added `a_connection_closing_with_its_own_call_pending_fails_only_that_call` (B gets `host.no_reply`, A's colliding call completes).
  - `[medium]` `[patch]` The lost-stream way of leaving is untested — added `a_connection_whose_stream_is_lost_mid_flight_abandons_only_its_own_execution` (`Received::Closed(Some(ConnectionReset))` on B, nothing written to B, A completes).
  - `[low]` `[patch]` Same-id tests never assert the call ids collide — added `a_call == b_call` (two- and three-Connection tests, both leave tests), `from_a.id == from_b.id` in `several_connections`, and every call id equal in the spanning test.
  - `[low]` `[patch]` The spanning test's `Transcript` is never compared — commented that it is recorded only because the shared helpers write through one.
  - `[medium]` `[patch]` The spanning test truncates clients without waiting for the Daemon to detach them — the Daemon runs at `log_level = "debug"` and the test waits (`Daemon::log_count`) for one "connection detached from its Session" per dropped Connection before its next request.
  - `[medium]` `[patch]` The spanning test has no stray-envelope sentinel — `quiet` runs on every Connection before the others are dropped.
  - `[low]` `[patch]` The spanning test does not check it got several transports — asserts `transports.len() >= 2`.
  - `[low]` `[reject]` Wrong-Connection reply and `Authorize`-to-issuer are tested only in memory — `serve` is transport-generic (AD-1, no transport branch; the graph check's guard held), so a real-transport repeat adds a test, not coverage of a reachable defect.
  - `[false]` `[reject]` Dispatch-time behaviour (`ConfigUpdate`/`CodeUnregister` while an execution is in flight) is untested — `an_execution_in_flight_keeps_its_limits_and_the_next_one_gets_the_update` and `a_cached_run_dispatched_before_an_unregister_of_its_handle_still_runs` in `tests/connection.rs` test it; the state is keyed by Client ID, so which Connection sends the update does not enter the read.
  - `[medium]` `[patch]` `assert_silent` (a single `try_recv`) cannot catch a late stray write — now sends a sentinel execution and requires its `Result` to be the next envelope (fails on a stray write and on a dead Connection).
  - `[medium]` `[defer]` The release-build requirement is recorded only in this spec, not AGENTS.md — the fix edits an agent-context file; deferred (frontmatter and deferred-work.md).
  - `[low]` `[patch]` `two_connections_on_one_session_are_both_attached_and_serve_at_once` registers `getOrder` and never calls it — registration dropped. The second half (no wire test of an `Init` registration serving a reconnected Connection) does not hold: B calls `double`, registered only in A's `Init`, in the same-id tests.
  - `[low]` `[patch]` The conformance module doc says each case closes its extra Connection — now says the case leaves it quiet and drops it when it returns.
  - `[medium]` `[patch]` (edge) Truncated clients dropped before their detaches are observed — grouped with the detach-wait row above; same fix.
  - `[medium]` `[patch]` (edge) Middle transports' Connections never read after the `ConfigUpdate` — grouped with the sentinel row above; `quiet` on every Connection.
  - `[low]` `[reject]` (edge) A panic before `daemon.stop()` leaves the Daemon thread running — the suite's `run` has the same shape; the failure is still reported and the process ends with the test binary; a guard adds machinery for no reachable harm.
  - `[low]` `[patch]` (edge) `several_connections` never asserts the `Call` name — asserts both are `double` (and `call.name` in the spanning test).
  - `[medium]` `[patch]` (edge) `assert_silent(B)` in the per-call handler test is a one-moment check — grouped with the `assert_silent` row; same fix.
  - `[medium]` `[patch]` (edge) `try_recv` returning `Disconnected` passes the silence check — grouped with the `assert_silent` row; the sentinel's `next()` panics on a closed Connection.
  - `[low]` `[patch]` (edge) The basic two-Connection test has no stray check — `assert_silent` on A and B after both values.
  - `[medium]` `[patch]` (edge) The registry test's claim is asserted, not proven — grouped with the session-test row; same fix.
  - `[medium]` `[patch]` (edge) Mixed-transport partial detach is not verified end to end — grouped with the detach-wait row; same fix.
  - `[false]` `[reject]` (edge) The `hexput-session` doc says every further Connection attaches by `reconnect`, but `attach` is `pub` and unverified — `attach`'s own doc says nothing on the wire reaches it and it stays for tests; no production crate calls it; its gating is already deferred from Story 5.6.
  - `[medium]` `[patch]` (other) The spanning test checks survival only by timing — grouped with the detach-wait row; same fix.
  - `[medium]` `[patch]` (other) The session test cannot fail — grouped with the session-test row; same fix.
  - `[low]` `[patch]` (other) `type Open` doc says only `reconnect` uses it — now names `reconnect` and `several_connections`.
  - `[false]` `[reject]` (intent) The diff goes past "halt after planning" — that was the planning stage's directive, recorded in the old Auto Run Result; this run's task is to implement, review and finalize. The status mismatches are the sprint-status and Auto Run Result rows above.
  - `[false]` `[reject]` (intent) AC3 "registers a function" takes a non-literal reading — the intent contract's Never list excludes a runtime function-registration request and the Design Notes record the reading; `Init` registrations serving a reconnected Connection are exercised on the wire directly (B calls `double` registered in A's `Init`), and the registry test is fixed above.
  - `[false]` `[reject]` (intent) AC2's tests go wider than "result" — wider coverage that contradicts nothing is not a defect.
  - `[medium]` `[patch]` (intent) No-broadcast is checked only at chosen moments — grouped with the `assert_silent` row; same fix (and `quiet` in the spanning test).
  - `[false]` `[reject]` (intent) The `--release` constraint is stated but not exercised — all five CI commands ran in their `--release` form in this pass (1064 tests passing); see Auto Run Result.

