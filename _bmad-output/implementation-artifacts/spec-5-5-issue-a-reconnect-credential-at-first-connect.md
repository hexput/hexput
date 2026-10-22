---
title: 'Story 5.5: Issue a reconnect credential at first connect'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '735896da95367e4dec95c4de3f9f238a43961f8b'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred: []
---

<intent-contract>

## Intent

**Problem:** A Client ID is the only thing a Backend gets at init, so once Story 5.6 adds reconnect, knowing (or leaking) an ID would be enough to take over its Session (FR-13). Nothing proves ownership yet.

**Approach:** At every successful `Init`, `hexput-session` draws a 256-bit reconnect secret from the OS CSPRNG (the same `getrandom` source as Client IDs), keeps only a salted hash of it in the Session, and the Init `Result` returns it once, as `{client_id, reconnect_secret}`. Verifying a presented secret against the stored hash is defined here, in `hexput-session` alone, in constant time; Story 5.6 wires it to a reconnect message.

## Boundaries & Constraints

**Always:**
- Secret = 32 bytes from `getrandom::fill`, spelled on the wire as 64 lowercase hex characters under `reconnect_secret`, after `client_id`. A CSPRNG failure panics exactly as Client ID issue does (no secret, no Session).
- Stored form: a per-Session 32-byte random salt and `HMAC-SHA256(key = salt, message = secret bytes)` via `ring::hmac`; verification is `ring::hmac::verify` (constant time). The secret itself is never stored in the Session, never logged, never in any later response.
- The secret type has a redacting `Debug`, no `Display`, no `Clone`/`Copy`, no serde; its text form comes only from one explicitly named method. The stored credential's `Debug` shows neither salt nor digest.
- Defined once in `hexput-session`, identical on every transport: adapters and `hexput-connection` only carry the string; nothing validates elsewhere (AD-1, AD-2).
- Session-side sources and manifest never name a transport or TLS (the AD-1 transport-name guard scans comments too).
- `ring` is pinned once in `[workspace.dependencies]` at `0.17.14` with default features (exactly the feature set rustls already unifies, so nothing new is built or downloaded) and recorded in the Spine's Stack table by a dated amendment.

**Never:**
- No reconnect message, no `Sessions::attach` change, no reconnect-denied error code, no TTL work (Stories 5.6–5.8).
- No secret rotation, no password-hashing KDF (the secret is 256 bits of entropy), no `zeroize`/`subtle`/`sha2` dependency, no new crate edge.
- Never put the secret in a span, event field, error message or `Debug` output.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| First init | valid `Init` on any transport | `Result {client_id: <32 hex>, reconnect_secret: <64 hex>}`, keys in that order | No error |
| Two inits | two connections init | different Client IDs and different secrets | No error |
| Later replies | `ExecutionStart`, `ConfigUpdate`, `CodeRegister`, second `Init` on the same connection | no reply contains the secret's hex | — |
| Invalid init | malformed `Init` payload | `protocol.invalid_payload`, no Session, no secret drawn | unchanged |
| Verify right secret | the issued secret, its Client ID | `verify_reconnect` → `true` | — |
| Verify wrong secret / other Session's secret / unknown ID | — | `false`; unknown ID still runs one HMAC verify against a fixed dummy credential | — |
| Parse | `from_hex` of 64 lowercase hex | `Some`; uppercase, 63/65 chars, non-hex → `None` | — |

</intent-contract>

## Code Map

- `crates/hexput-session/src/lib.rs` -- `Session` (~70) gains a `credential` field; `Sessions::create` (~145) returns `(ClientId, ReconnectSecret)`; `generate_client_id` (~275) is the CSPRNG pattern to mirror; `teardown` (~291) destructures `Session` (add the field). Module docs: add a "What Story 5.5 adds" section.
- `crates/hexput-session/src/credential.rs` (new) -- `ReconnectSecret`, `Credential`.
- `crates/hexput-session/Cargo.toml` -- add `ring = { workspace = true }` with a comment that names no transport.
- `Cargo.toml` (root `[workspace.dependencies]`) -- `ring = "0.17.14"` with a comment (may mention rustls: the root manifest is not a production crate).
- `crates/hexput-connection/src/lib.rs:778-790` -- the Init reply builds `{client_id}`; add `reconnect_secret`. The `Answer::Initialized(client_id, …)` path and the "init completed; Session created" event (~292) must not gain the secret.
- `crates/hexput-shared/src/wire.rs:57` -- `MessageType::Init` doc: state the `Result {client_id, reconnect_secret}` answer.
- `crates/hexput-daemon/src/lib.rs:701` -- in-crate test calling `sessions.create`; take `.0`.
- Tests asserting the exact one-key Init reply: `crates/hexput-tests/tests/connection.rs:237-256` (`client_id_of`), `crates/hexput-tests/tests/daemon.rs:645-662`, `~2630-2640`, `~3550-3560`; `crates/hexput-tests/tests/daemon/conformance.rs` `normalize` (~685, add `reconnect_secret` → assert 64 hex → `<reconnect-secret>`), `init_result` (~1037), `assert_keys(&payload, &["client_id"])` (~1186, ~1193).
- `crates/hexput-tests/tests/session.rs:372-390` -- `create` helper and a direct `sessions.create`.
- `crates/hexput-tests/tests/daemon.rs:1158-1320` -- `json_events`/`JsonEvent` and the JSON-log daemon test: the model for a log-capture test.
- `crates/hexput-tests/benches/cache/rhai.rs:205` -- the Rhai comparator server's own Init reply; leave it (init is not timed, it is not the Daemon).
- `scripts/check-crate-graph.py` -- unchanged (external deps are not workspace edges); it must stay green.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md:132-160` -- Stack table and dated amendments.

## Tasks & Acceptance

**Execution:**
- `Cargo.toml`, `crates/hexput-session/Cargo.toml` -- pin and add `ring` -- the HMAC and its constant-time verify without a new build.
- `crates/hexput-session/src/credential.rs` -- `pub struct ReconnectSecret([u8; 32])` with `generate()` (crate-private), `to_hex(&self) -> String`, `from_hex(&str) -> Option<Self>` (exactly 64 lowercase hex), redacting `Debug`; `pub(crate) struct Credential { salt, digest }` with `issue() -> (Credential, ReconnectSecret)`, `verify(&self, &ReconnectSecret) -> bool`, redacting `Debug`, and a crate-private dummy for unknown IDs; `#[cfg(test)]` unit test (private items, say why) that hashing one secret under two salts gives different digests and both verify -- the one credential definition.
- `crates/hexput-session/src/lib.rs` -- store the `Credential` in `Session`; `create` returns `(ClientId, ReconnectSecret)` (the credential issued once, before the id loop); add `Sessions::verify_reconnect(&self, client_id, &ReconnectSecret) -> bool` (unknown ID verifies against the dummy, returns `false`); re-export `ReconnectSecret`; docs -- the Session holds only the hash.
- `crates/hexput-connection/src/lib.rs` -- reply `{client_id, reconnect_secret: secret.to_hex()}`; drop the secret after building the reply -- the one place it leaves the Daemon.
- `crates/hexput-shared/src/wire.rs` -- Init doc.
- `crates/hexput-daemon/src/lib.rs` -- adapt the test call.
- `crates/hexput-tests/tests/session.rs` -- adapt helpers; tests: distinct secrets, 64-hex form, `verify_reconnect` true/false matrix rows, `from_hex` rows, `Debug` of `Sessions` and of the secret contains no secret hex.
- `crates/hexput-tests/tests/connection.rs` -- adapt `client_id_of` to exactly two keys in order; test that later replies on the connection (an execution, a `ConfigUpdate`, a second `Init`) never contain the secret hex.
- `crates/hexput-tests/tests/daemon.rs` -- adapt the three exact-shape asserts; new test: daemon at `log_level = "trace"`, `log_format = "json"`, init then run a Script, the log contains the Client ID and never the secret hex.
- `crates/hexput-tests/tests/daemon/conformance.rs` -- normalize `reconnect_secret`, expect keys `["client_id", "reconnect_secret"]` -- the credential is uniform on every transport.
- `ARCHITECTURE-SPINE.md` -- Stack row `ring | 0.17.14` and an `[Amended 2026-10-22, Epic 5 Story 5.5]` paragraph (wire shape, HMAC-SHA256 salted hash, constant-time verify, decisions below).
- `AGENTS.md` -- Project Status: Story 5.5 landed; test count; next step 5.6.

**Acceptance Criteria:**
- Given a fresh connection completing init over any transport the platform serves, when the Daemon answers, then the `Result` carries `client_id` and a 64-lowercase-hex `reconnect_secret` (256 bits from the OS CSPRNG), and the conformance transcripts stay byte-identical across transports.
- Given an issued secret, when the Session stores it, then only the salt and HMAC digest are kept, `verify_reconnect` accepts exactly that secret for exactly that Client ID, and no later reply, log line (at `trace`, JSON) or `Debug` output contains it.
- Given the credential mechanism, when the crate graph and the transport-name guard run, then the credential lives in `hexput-session` alone, no crate edge changed, and `check-crate-graph.py` passes.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/session.rs` (`a_created_session_issues_a_reconnect_secret_of_64_lowercase_hex_characters`, `two_sessions_get_different_client_ids_and_different_secrets`, `verify_reconnect_accepts_exactly_the_issued_secret_for_its_client_id`, `verify_reconnect_refuses_a_secret_that_differs_in_one_character`, `verify_reconnect_refuses_another_sessions_secret`, `verify_reconnect_refuses_an_unknown_client_id_for_any_secret`, `a_torn_down_session_no_longer_verifies_its_secret`, `a_secret_parses_only_as_exactly_64_lowercase_hex_characters`, `the_secret_debug_and_the_sessions_debug_never_show_the_secret`), `crates/hexput-tests/tests/connection.rs` (`a_valid_init_answers_client_id_then_reconnect_secret_in_that_order`, `two_connections_initing_get_different_client_ids_and_different_secrets`, `no_reply_after_init_on_that_connection_carries_the_reconnect_secret`, `an_invalid_init_answers_no_reconnect_secret_and_creates_no_session`), `crates/hexput-tests/tests/daemon.rs` (`the_reconnect_secret_is_never_logged_even_at_trace_in_json`, plus the adapted `init_over_the_socket_answers_a_client_id`, TLS and named-pipe Init helpers) and `crates/hexput-tests/tests/daemon/conformance.rs` (`normalize`, `init_result`, `init`, `uninitialized`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

- Implementation decision (unattended): `no_reply_after_init_on_that_connection_carries_the_reconnect_secret` was strengthened to also send a `CodeRegister` (the matrix's "Later replies" row names it) and to match the later replies by content, since executions answer in completion order. Nothing was weakened.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 19 findings — high 0, medium 3, low 10, false 6, maybe-false 0
- findings:
  - `[low]` `[patch]` sprint-status moved 5-5 only to `ready-for-dev` while the spec and AGENTS.md say implemented — set to `review`, like 5-1…5-4.
  - `[medium]` `[patch]` no test checks that the secret on the wire is the one the Session stored — `a_valid_init_answers_client_id_then_reconnect_secret_in_that_order` now probes while the connection is attached: `verify_reconnect(client_id, from_hex(wire))` is `true` and a one-character change is `false`.
  - `[low]` `[patch]` the Rhai bench server's `Init` reply now differs from the Daemon's without saying so — the Rhai server answers `{client_id, reconnect_secret}` (a dummy 64-hex value), so the bench's one stated wire difference stays true.
  - `[low]` `[patch]` the `hexput-connection` comment promised the secret goes out of scope as soon as the reply is built — reworded: the value is dropped but its hex lives in the reply until sent, and neither is zeroed.
  - `[low]` `[patch]` the Spine amendment's reason for no `zeroize`/`subtle` (no new build) does not hold, as rustls already builds both — the amendment now gives the real reasons (ring's verify is constant time; zeroization is outside FR-13's leaked-Client-ID threat model).
  - `[medium]` `[patch]` nothing tests that the stored credential's `Debug` hides salt and digest — grouped with the verification-gap finding; new in-crate test `the_credential_debug_shows_neither_salt_nor_digest`.
  - `[low]` `[patch]` "same work" for an unknown Client ID overstated (shard guard, lookup not constant time) — docs in `hexput-session` (module, `verify_reconnect`, `Credential::dummy`) and the Spine now say "one HMAC verify either way" and that the lookup is not constant time; a reconnect-attempt limit is named as Story 5.6's.
  - `[low]` `[patch]` the in-crate unit test copied `issue`'s sign-and-copy steps — private `Credential::for_secret(salt, &secret)` now used by both.
  - `[false]` `[reject]` the JSON trace-log test drives few paths — the secret exists only in the `Init` reply built in `init()`; a refused second `Init`, `ConfigUpdate`, `CodeRegister` and `Call` never hold it, so they cannot log it, and the test drives the one path that does (plus the shared writer).
  - `[low]` `[patch]` `teardown` doc line over 100 columns and `Binds:` missing FR-13 — re-wrapped; `Binds: AD-2, AD-4, AD-5, FR-13`.
  - `[low]` `[reject]` `epic-5-context.md` gains two Story 5.4 bullets in this change — they are accurate and the clause about normalizing per-run wire fields is what 5.5 relies on; called out here instead of split, so the 5.4 text is visibly part of this review.
  - `[low]` `[patch]` the dummy verify's discarded result could in principle be elided by the optimizer — wrapped in `core::hint::black_box` (input and result).
  - `[medium]` `[patch]` (verification-gap) the stored credential's `Debug` redaction is never checked — grouped with the `Debug` finding above; the in-crate test asserts neither salt nor digest appears (hex or byte-array form) and the exact redacted text.
  - `[false]` `[reject]` (intent-alignment) halt boundary: the diff goes past planning — this run is a build-auto iteration whose stage implements the story; the conflicting status markers are the sprint-status finding above, patched.
  - `[false]` `[reject]` (intent-alignment) AC1 "exactly once" not exercised across connections — `Sessions::create`, called only from `init()`, is the one issuer; a second `Init` is refused before it, and no reconnect exists yet (5.6), so no other path can send it.
  - `[false]` `[reject]` (intent-alignment) "never logged" only tested over UDS — logging lives in transport-generic `hexput-connection`, which adapters cannot change; the transport choice cannot alter what is logged (AD-1, transport-name guard).
  - `[low]` `[reject]` (intent-alignment) secret not zeroized in process memory — real but outside FR-13's threat model (a leaked Client ID); the fix adds a dependency and wipe logic, which the spec rules out; the comment and Spine now state it plainly.
  - `[false]` `[reject]` (intent-alignment) the Named Pipe is not run locally — the reply is built in transport-generic code, the Named Pipe conformance helper compiles (`scripts/check-windows.sh`) and the `windows` CI job runs it.
  - `[false]` `[reject]` (intent-alignment) `verify_reconnect` belongs to 5.6 — the intent's Approach itself says verification is defined here, in constant time; 5.6 wires it to a reconnect message.

## Design Notes

Decisions taken unattended (record in the Spine amendment):
- **Wire name/shape** `reconnect_secret`, 64 lowercase hex: mirrors the Client ID's hex text, safe in any SDK and in JSON logs of Backends; MessagePack binary would be refused by nothing but complicates SDKs.
- **HMAC-SHA256 keyed by a random salt** is the salted hash: the input is 256 random bits, so a slow KDF adds nothing; `ring::hmac::verify` gives the constant-time comparison 5.6 requires, so no `subtle`. `ring` is already compiled for rustls with these features.
- **`verify_reconnect` lands now** so the stored hash is testable through the public API; 5.6 builds verify-and-attach on `Credential::verify` and owns the reconnect-denied error.

```rust
let (credential, secret) = Credential::issue();      // salt + HMAC(salt, secret)
// Init reply: [("client_id", id.to_string()), ("reconnect_secret", secret.to_hex())]
assert!(sessions.verify_reconnect(id, &secret));
```

## Verification

**Commands (release only — disk is scarce):**
- `cargo fmt --all --check` -- clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- no warnings
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo build --workspace --all-targets --locked --release` -- builds; `git diff Cargo.lock` adds only `ring` to `hexput-session`'s dependency list
- `cargo test --workspace --locked --release` -- all pass
- `scripts/check-windows.sh` -- clean, if the target is installed

## Auto Run Result

**Summary.** Every successful `Init` issues a 256-bit reconnect secret (OS CSPRNG) returned once as `Result {client_id, reconnect_secret}` (64 lowercase hex); the Session keeps only a random salt and `HMAC-SHA256(salt, secret)` via `ring`; `Sessions::verify_reconnect` checks in constant time (unknown IDs against a dummy credential). The conformance suite normalizes the new field on every transport.

**Files changed.**
- `Cargo.toml`, `Cargo.lock`, `crates/hexput-session/Cargo.toml` — `ring` 0.17.14 pinned and added to `hexput-session`.
- `crates/hexput-session/src/credential.rs` — new: `ReconnectSecret`, `Credential` (issue/for_secret/verify/dummy), redacting `Debug`s, in-crate tests.
- `crates/hexput-session/src/lib.rs` — Session stores the credential; `create` returns the secret; `verify_reconnect`; docs.
- `crates/hexput-connection/src/lib.rs` — the Init reply carries `reconnect_secret`.
- `crates/hexput-shared/src/wire.rs` — `Init` doc.
- `crates/hexput-daemon/src/lib.rs` — in-crate test adapted.
- `crates/hexput-tests/tests/{session,connection,daemon}.rs`, `tests/daemon/conformance.rs` — new and adapted tests.
- `crates/hexput-tests/benches/cache/rhai.rs` — the comparator's Init reply matches the Daemon's shape.
- `ARCHITECTURE-SPINE.md` — Stack row and the Story 5.5 amendment; `epic-5-context.md`, `sprint-status.yaml`, `AGENTS.md` — status and context.

**Review findings.** 19 findings: 11 patched (entries: 2 medium, 9 low — the two `Debug` findings are one entry), 0 deferred, 8 rejected (6 false, 2 low; reasons in the triage log above).

**Follow-up review recommendation:** `false`. Two medium entries were patched, but both were test gaps now closed by passing tests (wire secret verifies against the stored hash; credential `Debug` redaction); no specific unverified risk remains to name.

**Verification (all release, disk-scarce machine).** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` built; `cargo test --workspace --locked --release` 1024 passed, 0 failed.

**Residual risks.** The Named Pipe path runs only in the `windows` CI job; the secret's memory is dropped, not wiped; the registry lookup in `verify_reconnect` is not constant time and no attempt limit exists yet (Story 5.6).
