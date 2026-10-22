---
title: 'Story 5.6: Reconnect without resending configuration'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'd11afd3cd77296463552511d6f44784f00ed4582'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred:
  - summary: >-
      `Sessions::attach` is still `pub` and attaches a Connection without verifying a credential, so a production crate could bypass `Sessions::reconnect`.
    evidence: |-
      Pre-existing since Story 2.4; this story only reworded its doc. No production crate calls it today. Settle by gating it (doc(hidden), a test-only feature or moving its tests onto `reconnect`) and adding a RESTRICTED_NAMES entry to check-crate-graph.py.
    location: >-
      crates/hexput-session/src/lib.rs:213
    severity: low
---

<intent-contract>

## Intent

**Problem:** Story 5.5 issues a reconnect secret and can verify it, but nothing on the wire lets a new Connection attach to an existing Session: a Backend that opens another connection must `Init` again, resending Config and registrations and getting a different Session (FR-2, FR-13).

**Approach:** Add a `Reconnect` request, the alternative to `Init` on an unattached connection, carrying exactly `{client_id, reconnect_secret}`. `hexput-session` verifies the secret in constant time and attaches the Connection under one shard lock; the Connection then serves exactly as if it had sent the `Init`, against the Session's stored Config, registrations and AST Cache entries. A wrong secret and an unknown Client ID get one identical new error, `protocol.reconnect_denied`.

## Boundaries & Constraints

**Always:**
- Wire: `MessageType::Reconnect` (PascalCase `Reconnect`, added to `ALL` after `Init`), payload a map with exactly `client_id` (32 lowercase hex, `ClientId::from_str`) and `reconnect_secret` (64 lowercase hex, `ReconnectSecret::from_hex`). Success answers `Result {client_id}`: exactly that one key, never the secret.
- It bypasses the init gate like `Init`. On a connection already attached (by `Init` or `Reconnect`) it is `protocol.already_initialized` with nothing decoded or verified; an `Init` after a successful `Reconnect` is too.
- Payload decoding (`hexput-session`, by hand, `InitError`-style messages) comes before any registry lookup, so its outcome never depends on whether a Session exists. Any other key — `config` and `registrations` included — is `protocol.invalid_payload` naming the key and saying the stored Config and registrations are resumed (use `ConfigUpdate` after reconnecting). Absent/nil keys are missing, both named together; a repeated key, a non-map payload, a non-string or malformed value is refused. A message never echoes a presented secret or Client ID.
- Verification happens once, in `hexput-session`: new `Sessions::reconnect(client_id, &ReconnectSecret, ConnectionId) -> bool` verifies with the Session's `Credential::verify` (`ring::hmac::verify`, constant time) and inserts the Connection under the same `get_mut` shard lock, so a racing `detach` either tears the Session down first (→ denied) or sees the new attachment. An unknown Client ID runs the same black-boxed dummy verify as `verify_reconnect` and returns `false`.
- Denied: `protocol.reconnect_denied` (new `ProtocolCode::ReconnectDenied`, in `ALL`), with the one fixed message `no Session matches this Client ID and reconnect secret` for both unknown ID and wrong secret, the request id echoed; the connection stays open and unattached (it may retry or `Init`).
- On success `hexput-connection` sets `attached`, replaces the connection span with one carrying the Client ID (exactly as for `Init`) and logs "reconnect completed; attached to the Session" at `debug` as the new span's first event. It does not call `AstCache::open` (the Session is live, so its Client ID is already open). Every later request reads the Session afresh as today; the existing one exit path detaches it.
- Secret hygiene from Story 5.5 holds: the presented secret is never logged, never in a span/event field, error message or `Debug`; it is dropped after verification.
- No source or manifest of `hexput-session`, `hexput-connection`, `hexput-port` or `hexput-shared` names a transport (AD-1 guard scans comments).

**Never:**
- No TTL or delayed teardown (Story 5.8): detaching the last Connection still tears the Session down at once, so a reconnect only reaches a Session another Connection still holds. No new multi-connection semantics beyond attach (Story 5.7 tests them end to end).
- No reconnect-attempt limit, no closing a connection after denials, no secret rotation, no new crate edge, no new dependency.
- No validation of the credential in an adapter, `hexput-connection` or `hexput-port` beyond carrying it (AD-2).
- Never apply, merge or store a Config or registrations carried by a `Reconnect`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Reconnect | A inited (registrations, Config) and still attached; B sends `Reconnect {client_id, reconnect_secret}` | `Result {client_id}`; `attached(id) == 2`; B's executions use the stored registrations/Config and a host call's `Call` goes to B only; B can run A's cached handle | — |
| Wrong secret | live ID, a different valid 64-hex secret | `protocol.reconnect_denied`, fixed message; attachment count unchanged | connection stays open, unattached |
| Unknown ID | well-formed ID no Session has (or a torn-down one) | byte-identical error payload to the wrong-secret row | same |
| Carries Config | extra `config` or `registrations` key, even with a valid credential | `protocol.invalid_payload` naming the key; nothing attached; stored Config unchanged | — |
| Malformed | nil/non-map payload, missing key(s), repeated key, uppercase/short/non-string hex | `protocol.invalid_payload`, value not echoed | — |
| Already attached | `Reconnect` after `Init`/`Reconnect`; `Init` after `Reconnect` | `protocol.already_initialized`; Session untouched | — |
| Leaves | B closes; then A closes | B's close leaves A's Session live; A's close tears it down | — |

</intent-contract>

## Code Map

- `crates/hexput-session/src/lib.rs` -- `Sessions::attach` (~181; doc says nothing on the wire reaches it — reword: the wire path is `reconnect`, which verifies first; `attach` stays for tests), `detach` (~201, `remove_if_mut`), `verify_reconnect` (~215, dummy pattern to reuse). Add `reconnect`; module doc section "What Story 5.6 adds"; re-export the request type.
- `crates/hexput-session/src/credential.rs` -- `Credential::verify`, `Credential::dummy`, `ReconnectSecret::from_hex` (reuse; no change expected).
- `crates/hexput-session/src/init.rs` -- `InitError`, `describe_key`, `present`, `missing`, `Config::from_update_payload` (~45): the hand-decoding pattern to mirror. New `src/reconnect.rs` (a private module, its type re-exported): `ReconnectRequest { client_id, secret }` + `from_value(&Value) -> Result<Self, InitError>`.
- `crates/hexput-shared/src/ids.rs:62` -- `ClientId::from_str` (exactly 32 lowercase hex).
- `crates/hexput-shared/src/wire.rs:52-140` -- `MessageType` enum, `ALL`, `as_str`; document `Reconnect` (payload, `Result {client_id}`, denial).
- `crates/hexput-port/src/error.rs:18-105` -- `ProtocolCode`, `ALL`, `as_str`: add `ReconnectDenied` → `protocol.reconnect_denied`; extend `InvalidPayload`/`AlreadyInitialized` docs.
- `crates/hexput-connection/src/lib.rs` -- `answer` (~525: route `Reconnect` beside `Init` before the gate), `init` (~759: `already_initialized` check and reply pattern), `Answer::Initialized` handling in `exchange` (~286: span replacement + first event; generalize or add a sibling variant with the reconnect event text), `refuse` (~805), module docs (~8-60 request list, ~126 detach paragraph).
- `crates/hexput-tests/tests/session.rs:372-500` -- `create` helper and attach/detach tests to model on.
- `crates/hexput-tests/tests/connection.rs` -- `serve_with`/`message`/`init_reply_of` (95-260), `every_request_before_init_is_refused` (331; `Reconnect` is not gated), `connect_as` (1332), `wired_runtime` (1383), `an_update_through_one_connection_is_seen_by_every_attached_connection` (~2677, today `attach`es directly: switch it to a wire `Reconnect` where natural).
- `crates/hexput-tests/tests/daemon.rs:1190` -- `the_reconnect_secret_is_never_logged_even_at_trace_in_json`: extend to a second connection's reconnect (right and wrong secret).
- `crates/hexput-tests/tests/daemon/conformance.rs` -- `Case` type (~753), `run` (~756, one fresh client per case), `CASES` (~965), `init_result`/`begin` (~1045), `normalize` (690; already replaces `client_id`/`reconnect_secret` anywhere, sent envelopes included), `no_case_names_a_transport` (848).
- `crates/hexput-tests/tests/{port,shared}.rs` -- tests iterating `MessageType::ALL` / `ProtocolCode::ALL`; update any hand-written expected list.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- after the Story 5.5 amendment (~164).

## Tasks & Acceptance

**Execution:**
- `crates/hexput-shared/src/wire.rs` -- add `MessageType::Reconnect` (variant, `ALL`, `as_str`, doc) -- the request.
- `crates/hexput-port/src/error.rs` -- add `ProtocolCode::ReconnectDenied` and doc updates -- the one denial code.
- `crates/hexput-session/src/{reconnect.rs,lib.rs}` -- `ReconnectRequest::from_value` (payload rules above) and `Sessions::reconnect` (verify + attach under one lock); docs -- verification once, in the session crate (AD-2).
- `crates/hexput-connection/src/lib.rs` -- serve `Reconnect` on an unattached connection: decode → `invalid_payload`; `sessions.reconnect` false → `reconnect_denied` with the fixed message; true → `Result {client_id}`, attach, span replacement, debug event; docs -- the wire path.
- `crates/hexput-tests/tests/session.rs` -- `reconnect` true/false matrix (right, wrong, other Session's secret, unknown, torn-down; attachment counts), a racing detach-vs-reconnect thread test, `ReconnectRequest::from_value` rows (each error message free of the presented values).
- `crates/hexput-tests/tests/connection.rs` -- matrix rows over the in-memory Port with two connections on one `Sessions`: resumed registrations (blanket call → `Call` on B only, answered by B), resumed Config (a `budget.rpc_calls` of 0 refuses B's host call), denial payloads byte-identical for wrong secret vs unknown ID, `config` key refused with Config unchanged, already-attached refusals, B's close leaves A's Session, no reply on B carries the secret.
- `crates/hexput-tests/tests/daemon.rs` -- log test extension: neither the issued nor a wrong presented secret appears in trace JSON logs.
- `crates/hexput-tests/tests/daemon/conformance.rs` -- let a case open a second connection to the same Daemon (e.g. pass the case a transport-neutral `&dyn Fn() -> Box<dyn Client>`; existing cases ignore it), recorded in the same transcript; add a `reconnect` case after `init_twice`: A `begin`s; B sends unknown-ID and wrong-secret reconnects (both `reconnect_denied`), one with `config` (`invalid_payload`), the right one (`Result {client_id}` equal to A's), a second `Reconnect` (`already_initialized`), a `double` host call answered on B, then the sentinel on B -- one reconnect mechanism, identical on every transport (FR-13).
- `ARCHITECTURE-SPINE.md` -- `[Amended 2026-10-22, Epic 5 Story 5.6]` paragraph: message, reply, denial code and message, reject-not-ignore rule for Config, decode-before-lookup, atomic verify-and-attach, no attempt limit, dependence on Story 5.8 for reconnect after the last Connection drops.
- `AGENTS.md` -- Project Status: Story 5.6 landed; test count; next step 5.7.

**Acceptance Criteria:**
- Given a live Session with its Client ID and reconnect secret, when a new connection sends `Reconnect` carrying both over any transport the platform serves, then it is answered `Result {client_id}` and serves executions, host calls and cached handles under the stored Config and registrations, with nothing resent, and the conformance transcripts stay byte-identical across transports.
- Given a wrong secret for a live Client ID and an unknown Client ID, when each is processed, then both get `protocol.reconnect_denied` with byte-identical payloads apart from the echoed request id, and the registry is unchanged.
- Given the change, when `check-crate-graph.py` runs and the sources are inspected, then verification exists only in `hexput-session` (one `Credential::verify` per attempt, constant time), no crate edge or dependency changed, and no Session-side source names a transport.
- Given a `Reconnect` that carries Config or registrations, when it is processed, then it is refused as `protocol.invalid_payload` before any lookup and the stored Session Config remains authoritative.

## Design Notes

Decisions taken unattended (record in the Spine amendment):
- **Reject, not ignore, a carried Config.** The AC allows either; rejecting tells a Backend its Config was not applied, where ignoring would let it believe a policy is in force that is not — the same rule `Init` uses for unknown keys. Decoding first means the refusal reveals nothing about which Client IDs exist.
- **Malformed is `invalid_payload`, not denied.** A syntactically impossible ID or secret can never match, so saying so leaks nothing; only well-formed credentials reach the registry, and every miss there is the one denial.
- **Reply `Result {client_id}`.** Confirms which Session the Connection joined, mirrors `Init` minus the secret (sent once, at `Init` only).
- **No attempt limit.** A 256-bit secret makes guessing infeasible and the denial gives no oracle; the uncapped-connection and pre-init-deadline items already in deferred work cover the remaining resource exposure.
- **Reconnect needs a live Session.** Until Story 5.8, a Session is gone the moment its last Connection detaches, so tests keep the first Connection attached; 5.8 delays teardown without touching this path.

```rust
// hexput-session
pub fn reconnect(&self, id: ClientId, secret: &ReconnectSecret, c: ConnectionId) -> bool {
    match self.sessions.get_mut(&id) {
        Some(mut s) if s.credential.verify(secret) => { s.connections.insert(c); true }
        Some(_) => false,
        None => { /* black_box(Credential::dummy().verify(..)), as verify_reconnect */ false }
    }
}
```

## Verification

**Commands (release only — disk is scarce):**
- `cargo fmt --all --check` -- clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- no warnings
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo build --workspace --all-targets --locked --release` -- builds; `Cargo.lock` unchanged
- `cargo test --workspace --locked --release` -- all pass
- `scripts/check-windows.sh` -- clean, if the target is installed

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/session.rs` (`reconnect_attaches_a_second_connection_with_the_issued_secret`, `reconnect_refuses_a_wrong_secret_and_changes_no_attachment`, `reconnect_refuses_another_sessions_secret`, `reconnect_refuses_an_unknown_client_id_and_creates_nothing`, `reconnect_refuses_a_torn_down_session_like_an_unknown_one`, `a_reconnected_session_outlives_the_connection_that_created_it`, `a_reconnect_racing_the_last_detach_either_tears_down_first_or_attaches`, `reconnect_request_accepts_exactly_a_client_id_and_a_secret`, `reconnect_request_names_a_carried_config_or_registrations_and_says_what_resumes`, `reconnect_request_names_missing_parts_together`, `reconnect_request_refuses_a_non_map_a_repeated_key_and_malformed_values`, `reconnect_refusals_never_echo_the_presented_values`); `crates/hexput-tests/tests/connection.rs` (`a_reconnect_is_answered_with_its_client_id_alone_and_attaches_the_connection`, `a_reconnect_resumes_the_registrations_so_a_blanket_call_goes_to_the_reconnecting_connection`, `a_reconnect_resumes_the_config_so_its_limits_apply_to_the_reconnecting_connection`, `a_reconnect_resumes_the_cached_handles_of_the_session`, `a_wrong_secret_and_an_unknown_id_are_denied_with_identical_payloads_and_the_connection_stays_open`, `a_reconnect_after_the_session_is_torn_down_is_denied_exactly_like_an_unknown_id`, `a_reconnect_carrying_config_or_registrations_is_refused_before_any_lookup_and_changes_nothing`, `a_malformed_reconnect_is_invalid_payload_and_no_presented_value_is_echoed`, `a_second_init_or_reconnect_on_an_attached_connection_is_already_initialized_and_changes_nothing`, `a_reconnect_bypasses_the_init_gate_and_is_answered_as_a_denial_not_as_uninitialized`); `crates/hexput-tests/tests/port.rs` (`protocol_codes_are_stable_and_in_the_protocol_category` now expects `protocol.reconnect_denied` last, `a_reconnect_envelope_is_spelled_reconnect_on_the_wire_and_its_denial_is_not_fatal`); `crates/hexput-tests/tests/shared.rs` (`message_types_spell_pascal_case_and_parse_exactly`, `Reconnect` after `Init`); `crates/hexput-tests/tests/daemon.rs` (`neither_a_right_nor_a_wrong_reconnect_secret_is_logged_even_at_trace_in_json`); `crates/hexput-tests/tests/daemon/conformance.rs` (the `reconnect` case after `init_twice`, every case now taking a transport-neutral `open` for a second connection). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Implementation decisions (unattended): `hexput-connection`'s `Answer::Initialized` became `Answer::Attached(Attach::{Init, Reconnect}, …)` so both attachments share the one span-replacement path; the init gate's refusal now reads "send `Init` (or `Reconnect`) first" (no test pins the old text); `init.rs`'s `InitError::new`, `present`, `missing` and `describe_key` became `pub(crate)` for `reconnect.rs`; the optional switch of `an_update_through_one_connection_is_seen_by_every_attached_connection` from `attach` to a wire `Reconnect` was not made — it tests `ConfigUpdate` visibility, which the new reconnect tests already cover over the wire. No pre-written test was changed.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 31 findings — high 0, medium 3, low 20, false 8, maybe-false 0
- findings:
  - `[medium]` `[patch]` Blind: an unknown `Reconnect` key is echoed up to 64 characters, so a secret sent as a key comes back in the reply and the debug log — fixed: `reconnect.rs` names an unknown key only when it is a string of at most 16 characters (`describe_unknown_key`), otherwise "(a string too long to show)" / "(not a string)"; `reconnect_refusals_never_echo_the_presented_values` gained a secret-as-key and an ID-as-key payload; Spine amendment notes the rule.
  - `[low]` `[reject]` Blind: the shard write lock is held during the HMAC verify and attempts are unlimited — one HMAC-SHA256 verify is about a microsecond, the same order as any `ConfigUpdate` that takes the same lock; the spec requires verify-and-attach under one lock for the detach race, and a read-then-write split would add a re-check branch for no demonstrated harm.
  - `[low]` `[patch]` Blind: lookup work differs between unknown and live Client IDs; only the payload is identical — fixed by documenting it: `Sessions::reconnect`'s doc and the Spine amendment now say the outcome and reply are identical, the timing is not guaranteed (a live ID takes its shard's write lock), which is no oracle against 128 random bits.
  - `[low]` `[patch]` Blind: `sprint-status.yaml` moved 5.6 to `ready-for-dev` — fixed at finalization: `review`.
  - `[low]` `[reject]` Blind: an invalid `Reconnect` is logged twice and a denial only as the generic "refused a request" — mirrors `Init`'s existing pattern exactly; the denial's event carries `code = protocol.reconnect_denied`, which distinguishes it; the echo concern is fixed by the first row; operator-facing denial logging belongs to Epic 7 (already deferred).
  - `[low]` `[reject]` Blind: `ReconnectRequest::from_value` returns `InitError` and four helpers became `pub(crate)` — the doc names `Reconnect`; a neutral error type would add public surface for a naming nicety; `pub(crate)` is not public. The overlong doc line was rewrapped (see the `InvalidPayload` row).
  - `[low]` `[reject]` Blind: `config: nil` is refused rather than treated as absent — the spec says any key other than the two is refused; nil-as-missing applies to required keys; a Backend's `Reconnect` has no `config` field to serialize as null; changing it adds a branch.
  - `[low]` `[patch]` Blind: no test sends `Init` after a denial — fixed: `a_reconnect_after_the_session_is_torn_down_is_denied_exactly_like_an_unknown_id` now sends `Init` on the denied connection and checks a fresh, attached Session.
  - `[low]` `[patch]` Blind: the "reconnect completed" event is never asserted — grouped with the verification-gap row; fixed there.
  - `[low]` `[patch]` Blind: the daemon test reads the `Init` reply positionally — fixed: it reads `client_id`/`reconnect_secret` by key.
  - `[low]` `[reject]` Blind: conformance normalization hides which Client ID was sent — the case asserts in code that both denials are identical and the right one resumes A's ID; the transcript's job is cross-transport byte equality, which normalization is there to allow.
  - `[false]` `[reject]` Blind: the transcript does not record which connection was used — every `expect_*` after `open()` reads from `other`, so a reply on the wrong connection fails the case (it never arrives on `other`).
  - `[low]` `[reject]` Blind: the refused-`config` conformance row checks only the code — "Config unchanged and nothing attached" is pinned by `a_reconnect_carrying_config_or_registrations_is_refused_before_any_lookup_and_changes_nothing`; the conformance case proves transport-identical framing, not registry state.
  - `[low]` `[reject]` Blind: connection A is never re-checked after B attaches — connection tests pin A serving alongside B (`a_reconnect_resumes_the_registrations…`, the Leaves row); adding it to every transport adds cost for no new path.
  - `[low]` `[patch]` Blind: `ProtocolCode::InvalidPayload`'s doc was not rewrapped — fixed: rewrapped to 100 columns, as was `InitError`'s doc.
  - `[low]` `[reject]` Blind: the malformed `client_id` message does not name the type found — it deliberately echoes nothing about the presented value; naming the type adds a branch for a cosmetic gain.
  - `[low]` `[defer]` Blind: `Sessions::attach` stays `pub` with no verification — pre-existing since Story 2.4 (only its doc changed); no production caller; deferred with how to gate it.
  - `[low]` `[reject]` Blind: `ConfigUpdate` visibility across a reconnected Connection is untested — every execution reads the Session afresh by Client ID (`for_execution`), the same path whether attached by `attach` or `reconnect`, and the existing multi-connection `ConfigUpdate` test covers it.
  - `[medium]` `[patch]` Edge: unknown key used as credential is echoed (reply and debug log) — grouped with the first Blind row; fixed there.
  - `[medium]` `[patch]` Edge: the unknown-key refusal reaches the log through `%error` — same root cause as the first Blind row; fixed there (the message no longer carries a long key).
  - `[low]` `[patch]` Edge: an `Init` after a `Reconnect` is refused with "this connection already completed init" — fixed: `init`'s refusal now reads "this connection is already attached to a Session; that Session is unchanged", the same as `Reconnect`'s.
  - `[false]` `[reject]` Edge: the conformance case drops `other` without closing and races the next case — every case runs on its own Sessions (its own `begin`), and `other`'s detach only touches A's Session, which `client` still holds; the missing WebSocket Close frame is the already-deferred Story 5.2 behaviour, logged at `debug` and harmless.
  - `[low]` `[patch]` Edge: lock-contention timing tells known from unknown IDs — grouped with the third Blind row; documented there.
  - `[low]` `[patch]` Edge: sprint-status `ready-for-dev` — grouped with the fourth Blind row; fixed at finalization.
  - `[low]` `[patch]` Verification gap: the reconnect span replacement and "reconnect completed" event are never asserted — fixed: `neither_a_right_nor_a_wrong_reconnect_secret_is_logged_even_at_trace_in_json` now runs a failing execution on B and asserts the denial (request 2) carries `client_id = "none"`, "reconnect completed; attached to the Session" (request 3) is the first event naming the Client ID on B's connection, at `DEBUG`, and every later event there (the failed execution, request 4) carries it.
  - `[false]` `[reject]` Intent: the diff implements the story although the intent says "halt after planning" — this run's orchestrated task is the implement-and-review stage of the build, explicitly asking for the story to be finalized; the planning-only halt belonged to an earlier invocation.
  - `[false]` `[reject]` Intent: AC1's network-blip scenario is tested only with A still attached — the epics place the Session TTL in Story 5.8; until then the Session is gone when its last Connection detaches, which the spec's Never list records; the reconnect path is unchanged by 5.8.
  - `[false]` `[reject]` Intent: AC2's timing is not measured — the AC asks that the two cases be indistinguishable to the Backend, which the byte-identical payload pins; the timing caveat is now documented (third Blind row).
  - `[false]` `[reject]` Intent: no test asserts adapters do no validation — no adapter or `hexput-connection` source calls `verify`/`Credential`; verification is a single call in `Sessions::reconnect`, and the graph check's transport-name guard holds; the property is structural.
  - `[false]` `[reject]` Intent: AC4 chose reject over ignore — the AC allows either; the choice is recorded in the spec and the Spine.
  - `[false]` `[reject]` Intent: the bookkeeping contradicts itself (sprint status vs spec vs AGENTS.md) — the sprint-status row is the only wrong one, fixed (fourth Blind row); spec `in-review` and AGENTS.md "implemented" are consistent for a story under review.

## Auto Run Result

**Summary.** Story 5.6 adds the `Reconnect` request: an unattached connection presents `{client_id, reconnect_secret}`; `hexput-session` decodes it before any lookup, verifies the secret in constant time and attaches the Connection under one shard lock (`Sessions::reconnect`); the connection then serves against the Session's stored Config, registrations and cached handles. A wrong secret and an unknown Client ID get one byte-identical `protocol.reconnect_denied`; a carried `config`/`registrations` is `protocol.invalid_payload`.

**Files changed.**
- `crates/hexput-shared/src/wire.rs` -- `MessageType::Reconnect`.
- `crates/hexput-port/src/error.rs` -- `ProtocolCode::ReconnectDenied`; `InvalidPayload`/`AlreadyInitialized` docs.
- `crates/hexput-session/src/reconnect.rs` -- `ReconnectRequest::from_value` (bounded unknown-key echo after review).
- `crates/hexput-session/src/{lib.rs,init.rs}` -- `Sessions::reconnect`, module docs, crate-visible decoding helpers.
- `crates/hexput-connection/src/lib.rs` -- serves `Reconnect`; `Answer::Attached(Attach::{Init, Reconnect}, …)`; one already-attached message.
- `crates/hexput-tests/tests/{session,connection,daemon,port,shared}.rs`, `tests/daemon/conformance.rs` -- the reconnect matrix, the span/log assertions, the conformance `reconnect` case over every transport.
- `ARCHITECTURE-SPINE.md`, `epic-5-context.md`, `AGENTS.md`, `sprint-status.yaml`, `deferred-work.md` -- amendment, context, status, tracking, one deferral.

**Review findings.** 31 findings: 0 high, 3 medium, 20 low, 8 false. Patched (8 entries; by entry verdict 1 medium, 7 low): the unknown-key echo of a credential (medium; Blind + two Edge rows); the timing caveat documented; sprint status; `Init` after a denial tested; the reconnect span/event asserted in JSON logs (verification gap + Blind); the daemon test reads the reply by key; the `InvalidPayload`/`InitError` docs rewrapped; one already-attached message. Deferred (1): `Sessions::attach` remains `pub` and unverified (pre-existing). Rejected: 9 low and 8 false, each with its reason in the Review Triage Log (lock held during HMAC, double log line mirroring `Init`, `InitError` naming, `config: nil`, conformance normalization/row depth/A re-check, the type-naming message, `ConfigUpdate` over a reconnect; the transcript-connection, conformance-race and six intent-alignment claims refuted).

**Follow-up review recommended:** false -- one medium entry patched (the echo), with a direct test; no high.

**Verification.** All release-only: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` OK, `Cargo.lock` unchanged; `cargo test --workspace --locked --release` 1048 passing (three further full runs: two clean, one hit the known pre-existing flake below).

**Residual risks.** `tests/script.rs`'s Story 4.3 test `a_removed_handle_is_unknown_and_its_program_is_released_at_once` flaked once in four full runs (moka's write log), already recorded in deferred work; untouched by this story. A reconnect after the last Connection drops cannot succeed until Story 5.8. The Named Pipe conformance run remains compile-checked only on this machine.

