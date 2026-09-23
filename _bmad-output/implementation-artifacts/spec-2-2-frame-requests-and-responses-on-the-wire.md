---
title: 'Story 2.2: Frame requests and responses on the wire'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: 'd42f5dfa60156d5a9c68646106a57a3e0fbf23ce'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** There is no wire protocol. `hexput-shared::wire`/`ids` and `hexput-port` are doc-comment stubs, so no transport adapter (2.3 onward) can exchange a single message, and nothing defines how a Backend correlates responses with requests or what a broken frame gets back.

**Approach:** Define the one MessagePack envelope (`id`, `type`, `payload`) and the Client ID newtype in `hexput-shared`, and build a sans-IO codec in `hexput-port`: length-prefixed stream framing, a decoder that turns bytes into an envelope or a defined protocol error response, and the one error-payload shape shared by protocol errors and Epic 1 diagnostics.

## Boundaries & Constraints

**Always:** Every adapter reuses these types and this codec — nothing transport-specific lives here (AD-1). Decoding never panics, never recurses unboundedly, and never allocates in proportion to a length a frame merely *claims*. Every failure maps to one `protocol.*` code; the error response echoes the request's correlation id whenever the bytes let it be read, and carries a nil id otherwise. Responses carry their request's id verbatim; the daemon treats ids as opaque and never reorders or deduplicates them. Versions (and feature sets) are pinned once in `[workspace.dependencies]` and recorded in the Spine's Stack table with a dated amendment. `scripts/check-crate-graph.py` pins `hexput-port`'s exact dependency set.

**Never:** No sockets, tasks, or async (2.3). No init gate, Session, or typed `Init`/`ExecutionStart` payloads (2.4/2.6). No Client ID generation or reconnect secret (2.4/Epic 5). No health/metrics messages (Epic 7). No change to the language's `Category`/`Code` or §7.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Round trip | envelope with id, known type, arbitrary nested payload | encode → frame → decode yields an equal envelope | N/A |
| Out-of-order responses | requests 1 and 2; responses encoded 2 then 1 | each decoded response matches its request by id | N/A |
| Split / coalesced stream | frames fed byte-by-byte, or several in one chunk | decoder yields exactly the frames sent, in order | N/A |
| Not MessagePack / trailing bytes / empty frame | garbage, a value plus extra bytes, zero length | `protocol.malformed_frame`, nil id | error response |
| Truncated content | frame whose MessagePack value ends early, incl. an array header claiming 2³² elements | `protocol.truncated_frame`, no large allocation | error response |
| Bad envelope | not a map; missing/non-integer `id`; missing/non-string `type`; unknown field | `protocol.invalid_envelope`, id echoed if readable | error response |
| Unknown type | `type: "Frobnicate"` with valid id | `protocol.unknown_message_type`, id echoed, names the type | error response |
| Deep nesting | payload nested past the depth limit | `protocol.malformed_frame`, no stack overflow | error response |
| Oversized frame | length prefix above `MAX_FRAME_LEN` | `protocol.frame_too_large`; decoder is poisoned (stream cannot resync) | error response, then adapter closes |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/wire.rs` -- stub; becomes `CorrelationId(u64)`, `MessageType`, `Envelope<P>`.
- `crates/hexput-shared/src/ids.rs` -- stub; becomes `ClientId`. `SessionId`/`PluginId` stay unwritten (their stories).
- `crates/hexput-shared/src/diagnostics.rs` -- read-only here: `Diagnostic`, `Span`, `Category::as_str`, `Code::as_str`, `Severity::as_str` feed `ErrorBody::from(&Diagnostic)`. Do not add serde or variants to it.
- `crates/hexput-port/src/lib.rs` (+ modules) -- stub; the codec. Depends on `hexput-shared` only.
- `crates/hexput-transport/src/lib.rs` -- untouched (2.3).
- `Cargo.toml` `[workspace.dependencies]` -- add `rmpv` (`with-serde`); `rmp-serde`/`serde` already pinned.
- `scripts/check-crate-graph.py` `EXACT_DEPENDENCIES` -- add `"hexput-port": {"hexput-shared"}`.
- `crates/hexput-tests/Cargo.toml`, `tests/port.rs`, `tests/shared.rs` -- dev-dependency on `hexput-port`; new port tests; ClientId/envelope tests in shared.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- the Story 1.2 item "`Code` wraps `&'static str` and no diagnostics type derives serde" is resolved by Decision 4.

## Tasks & Acceptance

**Execution:**
- [x] `Cargo.toml`, `ARCHITECTURE-SPINE.md` -- pin `rmpv` with `with-serde`; Stack table row + dated amendment.
- [x] `crates/hexput-shared/{Cargo.toml,src/wire.rs,src/ids.rs}` -- serde dependency; envelope, id and message-type types per Decisions 1–3.
- [x] `crates/hexput-port/{Cargo.toml,src/**}` -- `encode_frame`, `FrameDecoder` (push bytes, pull frames), `decode(&[u8]) -> Result<Envelope<Value>, ProtocolFailure>`, `encode(&Envelope<Value>)`, `ProtocolError` with its codes, `ErrorBody` + `error_response(id, body)` -- AC 1–3.
- [x] `scripts/check-crate-graph.py` -- pin `hexput-port`'s set.
- [x] `crates/hexput-tests/tests/{port,shared}.rs` -- every matrix row; `ErrorBody` from a real parser `Diagnostic` round-trips with its span; `ClientId` string round trip and rejection of malformed ids.
- [x] `AGENTS.md` Project Status, `deferred-work.md`, `epic-2-context.md` pinned-stack line -- keep honest.

**Acceptance Criteria:**
- Given the five CI commands, when run, then all pass.
- Given `hexput-port`, when its public API is inspected, then it performs no I/O and has no dependency but `hexput-shared` among workspace crates.

## Design Notes

1. **Envelope.** A MessagePack map with exactly `id` (uint64, or nil only on an error response whose request id was unreadable), `type` (string) and `payload` (any value; absent reads as nil). Encoded with field names (`rmp_serde::to_vec_named`) so any-language SDKs see a self-describing map. `Envelope<P>` is generic in `hexput-shared`; the Port instantiates `P = rmpv::Value`, and later stories convert payloads to typed structs with `rmpv::ext::from_value`.
2. **Message types** are PascalCase, matching the PRD's `ExecutionStart`, `CodeRegister`, `HealthCheck`. The `#[non_exhaustive]` enum holds this epic's vocabulary: requests `Init`, `ExecutionStart`; responses `Result`, `Error`. A response is not per-request-type — the client knows what `Result` answers from the id.
3. **Client ID** is 128 bits, `Display`/`FromStr`/serde as 32 lowercase hex characters (a string: portable to JavaScript, whose numbers cannot hold it). Constructed from `[u8; 16]`; generation is 2.4's.
4. **Error payload.** `ErrorBody { severity, category, code, message, span: Option<Span-as-map> }` with owned strings — the single error shape on the wire, built from a `Diagnostic` (span present) or a `ProtocolError` (category `protocol`, severity `error`, no span). `protocol` is a wire-only category: it is never a language failure, so `Category` and §7 stay untouched, and `Diagnostic` needs no serde.
5. **Framing.** A 4-byte big-endian length, then that many bytes of one MessagePack value. `MAX_FRAME_LEN` = 16 MiB, a documented constant. WebSocket (Epic 5) skips the prefix and feeds each message to `decode` directly. Decode is two-phase: bytes → `rmpv::Value` through `rmp-serde` (its depth limit guards recursion), then the map is validated by hand so each failure gets its exact code and the id is recovered whenever readable.
6. **The async `Port` trait** (what a connection adapter implements) arrives with its first adapter in 2.3; 2.2 ships the sans-IO codec it will wrap, so both are defined once.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green.

## Implementation Notes

- `MAX_NESTING_DEPTH` = 128 container levels, counting the envelope map, so a payload may nest 127 deep. rmp-serde's counter fails on reaching zero, so the deserializer gets a budget of 129.
- Decoding reads through a shrinking `&mut &[u8]` (rmp-serde's owned-reader path) so trailing bytes are visible afterwards. That path reads string/binary bodies through `take(len).read_to_end`, which grows with the bytes present, never the length claimed; rmpv's visitors use `Vec::new()`, never a size hint.
- Validation order: map shape and unknown/repeated/non-string keys first (id echoed if readable; a repeated `id` is ambiguous and never echoed), then `id` missing or non-uint64 (nil id), then `type` (missing/non-string is `invalid_envelope`, unknown is `unknown_message_type`, both echo the id), then nil `id` on a non-`Error` type.
- `Envelope<P>` derives `Serialize` only: the Port decodes by hand, and typed payloads come from `rmpv::ext::from_value` on the payload.
- `ErrorBody::to_value` builds the payload map by hand (infallible); `span` is always present on the wire, nil when absent.
- `ProtocolError::is_fatal` is true only for `frame_too_large`; `FrameDecoder` discards pushed bytes once poisoned.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind, edge | Phase-1 failures (trailing bytes, truncation after `id`, depth) return a nil id though `id` may be readable | low | The frozen matrix itself pins trailing bytes to a nil id, so "readable" means "after the frame parses" (Design Note 5's phase 2) — the one reading consistent with all frozen text; only a buggy SDK sends such frames, and recovery needs a second partial decoder | reject |
| 2 | blind, edge | `decode` has no size cap; a message-oriented adapter (WebSocket) calling it directly never gets `frame_too_large` | low | Real, but no such adapter exists until Epic 5, whose `tokio-tungstenite` has its own max-message-size setting; the fix adds a guard and redefines `is_fatal` per transport | reject |
| 3 | blind | `encode` never checks `MAX_FRAME_LEN`; an oversized Script result has no defined response | medium | Only `encode_frame` refuses it; no result producer exists until 2.6, which must turn that refusal into an `Error` response carrying the request id | defer |
| 4 | blind, edge | Decoded `rmpv::Value` tree amplifies a 16 MiB frame of 1-byte values ~30x (≈0.5 GiB per frame, per connection) | medium | Confirmed: ~32 bytes per `Value`; the claimed-length rule holds but actual decoded size is unbounded. Per-connection memory bounds are Resource Budget work, out of Epic 2's scope | defer |
| 5 | blind | `FrameDecoder` keeps a large buffer's capacity forever and copies each body with `to_vec` | low | Real, but only after an unusually large frame; the fix adds branches | reject |
| 6 | edge | Adapter pushing without draining grows the buffer unboundedly | false | The decoder's contract is push-then-drain; a drained buffer never holds more than one partial frame plus one chunk. An adapter violating that is the adapter's bug (2.3) | reject |
| 7 | blind | `ParseClientIdError` says "(got 32 bytes)" when the length is right and a character is wrong, and counts bytes not characters | low | Confirmed in `ids.rs`; the message misleads about the actual problem; fix is a direct correction of the text | patch |
| 8 | blind | `ErrorBody` carries one diagnostic, but FR-26 findings are many | low | A findings response composes a list of `ErrorBody`; the static-check mode is out of Epic 2's scope, and `From<&Diagnostic>` preserves severity correctly | reject |
| 9 | blind, edge | An invalid-UTF-8 MessagePack `str` decodes as `Binary`, making the "`type` must be valid UTF-8" branch dead | low | Confirmed: rmp-serde 1.3.1 `decode.rs:352-368` falls back to `visit_bytes`; the branch is unreachable, and the resulting `Binary` `type` is still `invalid_envelope`. Fix is deleting the dead branch | patch |
| 10 | blind | Spec status `in-review` vs sprint `in-progress` | false | Sprint sync is step 5's job | reject |
| 11 | blind | `MessageType` serde spelling vs `as_str` never checked for every variant | low | Encode uses the derive, decode uses `as_str`; a divergence would break round trip for that variant, yet only one variant is round-tripped; fix is a test | patch |
| 12 | blind | Truncation inside bin/ext/float bodies and a `0xFFFFFFFF` prefix untested | maybe-false | If misclassified it would be `malformed_frame` instead of `truncated_frame` (low); settled by cutting a payload containing those types | reject |
| 13 | blind | `ErrorBody` has a hand-built `to_value` and a derived `Serialize` that could drift | low | Field names are already tied by the round-trip tests through `Deserialize`; no caller uses the derive's positional form | reject |
| 14 | blind | `Envelope.id` optional for every type; decode accepts `Result`/`Error` from a Backend | false | Deliberate: the codec is direction-agnostic because Epic 3's outbound RPC makes a Backend send `Result`; dispatch (2.4/2.6) decides what a request path accepts | reject |
| 15 | edge | `encode` has no depth bound; a deeply nested Script result would overflow the stack serializing, or emit frames peers reject | high | The interpreter builds arbitrarily deep values without host recursion, so 2.6's result conversion and `encode` are the first recursive consumers; no producer exists yet | defer |
| 16 | edge | `encode` accepts a nil id on a non-`Error` envelope | low | Only reachable by hand-building an `Envelope` with pub fields; `Envelope::new` always sets an id; the fix adds a guard and an error variant | reject |
| 17 | verification-gap | Nil `span` key on a protocol error payload never checked on the raw map | low | Pre-verified; a non-Rust SDK relies on the fixed key set | patch |
| 18 | verification-gap | `ErrorBody` from a warning `Diagnostic` never tested | low | Pre-verified; `hexput-check` is already a dev-dependency, so the test is cheap now | patch |
| 19 | verification-gap | `encode_frame` at exactly `MAX_FRAME_LEN` untested | low | Pre-verified; an off-by-one would split encoder and decoder limits | patch |
