---
title: 'Stories 2.4 + 2.5: Init handshake, and Sessions that Connections attach to'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: '988a56ef73a297d8321702817d73338260336573'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** A connection can reach the Daemon but can never complete init: `Init` is answered `protocol.not_implemented`, `hexput-session` is an empty stub, and no Client ID is ever issued — so no Backend can hand over its Config and registrations, and there is no Session for later stories to execute against, reconnect to, or expire.

**Approach:** Build Stories 2.4 and 2.5 together (Decision 1). `hexput-session` gets a Session registry keyed by a freshly generated `ClientId`; a Session holds the single live copy of the Backend's Config, its Registered Functions, and the set of Connections attached to it (zero or more). `hexput-connection::serve` decodes an `Init` payload, creates the Session with itself attached, answers `Result { client_id }`, and detaches when the connection ends; detaching the last Connection tears the Session down through one explicit teardown path. `hexput-daemon` owns one registry and hands it to every connection.

## Boundaries & Constraints

**Always:** An `Init` payload is a map with exactly the keys `config` and `registrations`; either absent (or nil) is rejected naming the missing key(s), and no Session is created on any rejection. `hexput-session` holds the only copy of a Session's Config (AD-5) — `hexput-connection` holds the Session's `ClientId` and its own attachment id, never a copied `Config`. Client IDs come from the OS CSPRNG; a collision with a live Session regenerates rather than overwrites. The Session type holds a *set* of attached Connections — nothing encodes "exactly one" — and a Connection is attached to at most one Session. A reply goes only to the Connection that sent the request; there is no broadcast. Detaching the last Connection removes the Session atomically with the detach (no window where a racing attach finds a half-torn-down Session) and then calls one explicit `teardown` — never implied by `Drop` — so Story 5.8 can delay that same call behind a TTL. Every reply echoes the request id. `ExecutionStart` before init keeps `protocol.init_not_completed`; the gate is one check a later health/metrics exemption can bypass without restructuring. No lock is held across an `.await`. Nothing is written to disk. New third-party crates are pinned once in `[workspace.dependencies]` and recorded as a dated Spine Stack amendment; `check-crate-graph.py` pins `hexput-session`'s exact set.

**Decisions (2026-09-23, Erdem):**
1. *Scope* — Stories 2.4 and 2.5 are one spec: Session creation, attach/detach, and last-detach teardown land together, so no interim build leaks a Session per connection.
2. *Config contents before Epic 3* — `config` must be a map, and until Epic 3 defines its keys it must be **empty**: any key is rejected naming it, as System Config rejects unknown keys. A Backend is never allowed to believe a policy is in force that nothing enforces.
3. *`ExecutionStart` after init, before 2.6* — answered `protocol.not_implemented`, now that code's only use; its removal moves from 2.4 to 2.6 in `deferred-work.md`.
4. *Spec size* — kept whole at ~2000 tokens, over the 1600 guideline, by choice.

**Never:** No execution (2.6), no reconnect or reconnect secret (5.x), no TTL (5.8), no Config update message (3.8), no per-Config-key semantics (3.7/3.9/3.10), no capability grants on registrations (3.2), no Client-ID tracing span (2.8), no second init or teardown path.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Init | fresh connection, `{config: {}, registrations: [{name: "getUser"}]}` | Session created with this Connection attached; `Result` payload `{client_id: "<32 lowercase hex>"}` | N/A |
| No functions | `{config: {}, registrations: []}` | valid init — empty is not missing | N/A |
| Missing part | `{registrations: [...]}` / `{config: {}}` / `{}` / nil payload | `Error` naming `config`, `registrations`, or both; no Session | `protocol.invalid_payload` |
| Malformed part | `config` not a map or non-empty; `registrations` not an array; an entry not `{name: <string>}`; empty name; extra envelope key | `Error` naming the offending key or index; no Session | `protocol.invalid_payload` |
| Duplicate registration | two entries named `getUser` | `Error` naming `getUser`; no Session | `protocol.invalid_payload` |
| Execution before init | `ExecutionStart` first | refused, nothing runs | `protocol.init_not_completed` |
| Execution after init | `ExecutionStart` on an initialized connection | refused until 2.6 | `protocol.not_implemented` |
| Second init | `Init` on an initialized connection | refused; the existing Session is untouched | `protocol.already_initialized` |
| Two connections | A and B each send `Init` | two Sessions, two distinct Client IDs; each reply only on its own connection | N/A |
| Last detach | the only attached Connection closes (clean, error, or fatal frame) | Session removed from the registry and torn down; its Client ID no longer resolves | N/A |
| Not last detach | a Session with two attachments (registry-level test; no reconnect exists to produce it on the wire) | one detaches: Session, Config and registrations remain | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-session/{Cargo.toml,src/lib.rs}` -- stub today. Add: `Sessions` registry over `dashmap::DashMap<ClientId, Session>`; `create(InitRequest) -> (ClientId, ConnectionId)` (session born with its creator attached); `detach(ClientId, ConnectionId)` that, when the set empties, removes the entry under the map's own entry lock (`remove_if` / entry API) and then calls `teardown`; read accessors for tests (`contains`, attached count, registration names). `Session { config: Config, registrations: Vec<RegisteredFunction>, connections: HashSet<ConnectionId> }`. `ConnectionId` is a crate-local `u64` newtype from an `AtomicU64` counter (not a wire id, so not in `hexput-shared::ids`). `Config` is a unit-like struct with no fields yet (Decision 2). `InitRequest::from_value(&Value) -> Result<InitRequest, InitError>`. `teardown(Session)` is the sole teardown fn — releases Config and registrations; documents that Plugins' `hexput_globalvar::teardown` joins here in Epic 6. Client IDs: `getrandom::fill` into `ClientId::from_bytes`. Deps stay `hexput-port`, `hexput-shared`, `hexput-globalvar` (AD-4) plus `dashmap`, `getrandom`.
- `crates/hexput-shared/src/ids.rs:10-15` -- `ClientId` doc says generation is 2.4's; point it at `hexput-session`.
- `crates/hexput-port/src/error.rs` -- `ProtocolCode` gains `InvalidPayload` (`protocol.invalid_payload`) and `AlreadyInitialized` (`protocol.already_initialized`), both non-fatal; keep `ALL` complete; `NotImplemented`'s doc now names post-init `ExecutionStart` and 2.6. Stable-string test lives in `crates/hexput-tests/tests/port.rs`.
- `crates/hexput-connection/src/lib.rs` -- `serve<P: Port>(port, sessions: Arc<Sessions>)`; per-connection `Option<(ClientId, ConnectionId)>`; `answer` becomes init-aware (gate in one place); every return path of `serve` detaches if attached (explicit calls, not a `Drop` guard). Rewrite the module doc's "What a connection is answered today".
- `crates/hexput-daemon/src/lib.rs:210-255` -- build one `Arc<Sessions>` in `serve`; pass a clone to each spawned `hexput_connection::serve`. On shutdown, aborted connection tasks don't detach; the registry is dropped with the run — state that in a comment.
- `Cargo.toml` -- pin `getrandom = "0.4.3"`; `dashmap` 6.2.1 is already pinned.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` Stack table (after line 142) -- dated amendment adding `getrandom` (CSPRNG for Client IDs; OQ-2's reconnect secret will reuse it).
- `scripts/check-crate-graph.py` -- `EXACT_DEPENDENCIES["hexput-session"] = {"hexput-port", "hexput-shared", "hexput-globalvar"}`.
- `crates/hexput-tests/Cargo.toml` -- add `hexput-session` dev-dep. `tests/session.rs` (new): payload decoding rows, registry create/detach/teardown, ID uniqueness. `tests/connection.rs`: reuse the in-memory `Scripted` Port and `message`/`code_of` helpers; `every_request_before_init_is_refused_with_its_id_echoed` (line ~126) expects `not_implemented` for `Init` and must change; add init, gate, second-init, detach-on-close tests. `tests/daemon.rs`: one init over the real socket returning a 32-hex Client ID.
- `AGENTS.md` Project Status, `_bmad-output/implementation-artifacts/deferred-work.md` (line ~150, the `not_implemented` note → 2.6) -- keep honest.
- Do not touch: `hexput-transport`, `hexput-config` (AD-5: no edge to `hexput-session`), `hexput-rpc`, `hexput-globalvar`.

## Tasks & Acceptance

**Execution:**
- [x] `Cargo.toml`, `ARCHITECTURE-SPINE.md` -- pin `getrandom`; Stack amendment.
- [x] `crates/hexput-port/src/error.rs` -- two new codes; `NotImplemented` doc.
- [x] `crates/hexput-session/**`, `crates/hexput-shared/src/ids.rs` -- registry, Session, Config, RegisteredFunction, ConnectionId, Init decoding, ID generation, attach/detach, teardown.
- [x] `crates/hexput-connection/src/lib.rs` -- serve `Init`, init-aware gate, detach on every exit.
- [x] `crates/hexput-daemon/src/lib.rs` -- one registry per Daemon run.
- [x] `scripts/check-crate-graph.py` -- pin `hexput-session`.
- [x] `crates/hexput-tests/**` -- every matrix row.
- [x] `AGENTS.md`, `deferred-work.md` -- status; move the `not_implemented` note to 2.6.

**Acceptance Criteria:**
- Given the five CI commands, when run, then all pass.
- Given the workspace, when searched for per-backend `Config` storage, then only `hexput-session` holds one, and nothing writes it to disk (AD-5).
- Given `hexput-session`, when searched for `impl Drop`, then none performs teardown; `teardown` has exactly one caller, `Sessions::detach` (AD-4).

## Design Notes

- **Payload decoding by hand, like the envelope.** `rmpv::ext::from_value` yields serde's messages, which don't reliably name the missing key; walking the `Value` map gives exact messages (`missing \`config\` and \`registrations\``, `\`registrations[2].name\` is not a string`) under one code. Both missing keys are reported together.
- **Empty is not missing** — the check pass's own rule ("a missing callable-name list is not an empty one"): `registrations: []` is a Backend exposing nothing; an absent key is a broken init.
- **Registry lock discipline.** DashMap shard locks are synchronous and always released before `serve` awaits `send`; nothing in `hexput-session` is `async`. `teardown` runs after the entry is out of the map, so it never runs under a shard lock.
- **Why detach is explicit in `serve`.** A `Drop` guard would make teardown implied by `Drop` one step removed — the thing AD-4 forbids. A panic or abort inside `serve` therefore skips detach; that leak is recorded in `deferred-work.md` rather than hidden behind a guard.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green.

## Implementation Notes

- `Init` decoding lives in `hexput-session/src/init.rs`; duplicate detection uses a name -> index map so a large registration list is not scanned quadratically, and per-entry messages are formatted only on failure.
- `serve` splits into `serve` (owns the attachment, one detach after the loop) and `exchange` (the loop); every exit of `exchange` — clean close, lost stream, failed write, fatal frame — reaches the single detach. No `Drop` guard.
- `Sessions::create` keeps the spec's infallible signature and panics if the OS CSPRNG is unreadable (documented under `# Panics`); a Daemon that cannot issue unguessable ids issues none. The panic is confined to the connection's task.
- `Sessions::attach` is public so the "not last detach" row is testable at registry level; nothing on the wire reaches it before reconnect.
- `deferred-work.md` gained two entries: the panic-skips-detach leak (by design) and the unbounded Session registry.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind, edge | Refusal messages echo untrusted input unbounded (every config key listed, full key/name text) | medium | Verified: a config of ~1M fixint keys (2 bytes each on the wire) yields ~18 bytes of message per key; the reply exceeds `MAX_FRAME_LEN`, `send` returns `InvalidInput`, and `serve` only logs — the Backend never gets an answer to its `Init`, and a single pre-init connection forces a ~150 MB allocation | patch — cap listed keys, truncate echoed keys and names |
| 2 | edge | An `InvalidInput` reply to `Init` is dropped silently, leaving the Backend waiting | medium | Same root cause as #1: after this change only an oversized init refusal can reach that branch; bounding the message removes the path. The generic case is 2.3's triage row 4 (2.6's job) | patch (with #1) |
| 3 | blind, edge | Registration names are not checked against identifier rules or length | low | Real: `"get user"` registers but is uncallable. Unlikely in practice; a check needs identifier rules `hexput-session` cannot reach (no lexer edge) — more than a direct correction | reject |
| 4 | edge | An invalid-UTF-8 MessagePack string name is reported as "not a string" | low | Real but cosmetic and rare; the fix adds a branch | reject |
| 5 | blind | Story 2.5 AC1 ("Session continues to exist") contradicted by last-detach teardown | false | AC4 settles "at this epic's stage" that the last detach tears down; the frozen matrix's "Last detach" and "Not last detach" rows implement both, and the latter is tested | reject |
| 6 | blind | The spec file is untracked | false | Expected before commit; it is committed with the story | reject |
| 7 | blind | No `serve`-level test of two Connections on one Session answering only the issuer | low | That state is unreachable on the wire until reconnect (Epic 5); `serve` writes only to its own `Outbound`, so no broadcast path exists; covering it needs a new seam | reject |
| 8 | blind | Shutdown drops the registry without routing Sessions through `teardown` | low | Real in principle, harmless today (nothing to release) and the daemon comment at the `abort_all` site says so, where Epic 6 will meet it; the fix adds public surface | reject |
| 9 | blind | `AGENTS.md` says 2.4–2.5 implemented while sprint status says in-progress | false | Sprint sync is step 5's job | reject |
| 10 | blind | Daemon socket test does not check the reply's key or entry count | low | Verified; direct correction | patch |
| 11 | blind | `Session`'s public getters are unreachable — `Sessions` never hands out a `Session` | low | Verified: no method returns `&Session`; dead public API that 2.6 would otherwise design around; direct deletion | patch |
| 12 | blind, verification-gap | Client ID collision retry (and the CSPRNG panic) is untested; no seam to inject ids | medium | Pre-verified by the verification-gap layer: replacing the loop with an overwriting `insert` passes every test | defer |
| 13 | blind | `protocol.invalid_payload` is documented as `Init`-only despite its generic name | false | The generic name is deliberate so 2.6's `ExecutionStart` payload errors reuse it; 2.6 widens the doc | reject |
| 14 | blind | Double blank line before the first new `deferred-work.md` entry | low | Verified; direct correction | patch |
