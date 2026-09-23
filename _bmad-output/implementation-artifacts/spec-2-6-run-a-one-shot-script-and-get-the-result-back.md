---
title: 'Story 2.6: Run a one-shot script and get the result back'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: 'cb2f5ec4c6d6bdf46083dd7717e03d488304e1ff'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** An initialized connection still cannot run anything: `ExecutionStart` is answered `protocol.not_implemented`, `hexput-exec` and `hexput-script` are empty, so no Backend can hand the Daemon a Script and get its result (FR-4).

**Approach:** `hexput-exec` gets the one Executor entry point (evaluate a parsed Program with starting variables; enforcement joins it in Epic 3). `hexput-script` serves Direct Execution: it decodes the `ExecutionStart` payload `{source, variables}`, parses without caching, runs through the Executor, and turns the result into a `Result` payload `{value}` or the Story 1.8 diagnostic into an `ErrorBody`. `hexput-connection` routes an initialized `ExecutionStart` to it and replies on the same connection; `protocol.not_implemented` is removed.

## Boundaries & Constraints

**Always:** Every evaluation in the Daemon enters through `hexput_exec::execute`; `hexput-script` never calls the interpreter's `evaluate*` itself. Direct Execution creates no cached AST. A parse or runtime failure is an `Error` response built by `ErrorBody::from(&Diagnostic)` (category, code, severity, message, span), echoing the request id; the connection and every other connection keep serving. An `ExecutionStart` payload is a map with exactly `source` (string) and `variables` (map); either absent/nil is `protocol.invalid_payload` naming the missing key(s) together, and `variables: {}` is valid (empty is not missing, as for `Init`). A variable name must be a §2 identifier and appear once. Wire → value: nil/bool/string/array/string-keyed map map to their §3 types; every integer and float becomes a `number`, but an integer outside ±2^53, a NaN or an infinity, a binary, an ext, a non-string map key, a duplicate object key or invalid UTF-8 is `protocol.invalid_payload` naming the path (e.g. `variables.user.tags[2]`) — never silently lossy. A result the Daemon's own decoder would reject for nesting is `protocol.result_too_deep`, detected by an iterative walk before any recursive conversion. A reply that cannot be framed (`InvalidInput` from `send`) is answered `protocol.response_too_large` with the request id — no reply is ever silently dropped. Refusal messages stay bounded (truncate echoed names/paths, as in 2.4). The init gate stays one check. No lock across an `.await`. Crate edges follow the decision below and `check-crate-graph.py` pins them.

**Decisions (2026-09-23, Erdem):**
1. *Crate edges* — add `hexput-script --> hexput-port` and `hexput-connection --> hexput-script`, recorded as a dated Spine amendment. `hexput-script` owns the `ExecutionStart` payload, wire↔value conversion and the `ErrorBody`; `hexput-connection` only routes.
2. *Numbers on the wire* — a finite whole number within ±2^53 is encoded as a MessagePack integer (`-0` as `0`); every other number as float64.
3. *Spec size* — kept whole (~2300 tokens, over the 1600 guideline), since `deferred-work.md` assigns the depth and frame-size guards to 2.6.

**Never:** No concurrent or spawned dispatch (2.7) — execution runs inline in the connection's loop for now; no Client ID tracing span (2.8); no static check invocation (no check mode exists in Config until Epic 3); no AST Cache (Epic 4); no capability, budget or Registered Function calls (Epic 3); no second evaluation path.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Run | initialized; `{source: "return a + 1;", variables: {a: 2}}` | `Result {value: 3}` on this connection, same id | N/A |
| No inputs | `{source: "return [1, \"x\", null];", variables: {}}` | `Result {value: [1, "x", nil]}` | N/A |
| Objects round-trip | `variables: {o: {k: [true]}}`, `return o;` | `Result {value: {k: [true]}}`, key order kept | N/A |
| Parse failure | `source: "let = ;"` | `Error` with the parser's diagnostic and span | category `syntax` |
| Runtime failure | `source: "return 1 / 0;"`-style failure | `Error` with the interpreter's diagnostic | its §7 code |
| Name clash | `variables: {a: 1}`, source declares `let a` | `Error` `syntax.duplicate_declaration` | from interpreter |
| Missing / malformed payload | no `source`; `variables` not a map; extra key; bad name `"user-id"`; NaN; binary; 2^60 | `Error` naming the key or path; nothing runs | `protocol.invalid_payload` |
| Too deep | result nested past the decoder's limit | `Error`; connection keeps serving | `protocol.result_too_deep` |
| Too large | result whose frame exceeds `MAX_FRAME_LEN` | `Error` with the request id | `protocol.response_too_large` |
| Before init | `ExecutionStart` first | refused, nothing runs | `protocol.init_not_completed` |
| Isolation | A's script fails, B runs concurrently over the socket | B gets its result; the Daemon keeps running | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-exec/{Cargo.toml,src/lib.rs}` -- empty. Add `pub fn execute(program: &Program, variables: Vec<(Arc<str>, Value)>) -> Result<Value, Diagnostic>` over `hexput_interpreter::evaluate_with_variables`; doc it as the one Executor entry point Epic 3 wraps with enforcement. `Program` reaches it through a new `pub use` in `hexput-interpreter/src/lib.rs:58` (exec has no `hexput-ast` edge). Existing deps (enforce, rpc, globalvar) stay, unused.
- `crates/hexput-script/{Cargo.toml,src/lib.rs}` (+ `src/wire.rs`) -- empty. Add `direct_execution(payload: &rmpv::Value) -> Result<rmpv::Value, ErrorBody>` (or a small typed error the connection maps to codes): decode payload by hand (copy the style of `crates/hexput-session/src/init.rs`), `hexput_parser::parse`, `hexput_exec::execute`, depth walk, value → wire. No `moka`.
- `crates/hexput-parser/src/lib.rs` -- gains `pub fn is_identifier(&str) -> bool`, moved from `crates/hexput-cli-core/src/lib.rs:343-355` (the lexer's judgement, so reserved words live once); cli-core calls the parser's.
- `crates/hexput-port/src/error.rs` -- remove `NotImplemented`; add non-fatal `ResultTooDeep` (`protocol.result_too_deep`) and `ResponseTooLarge` (`protocol.response_too_large`); widen `InvalidPayload`'s doc to `ExecutionStart`; keep `ALL` complete. `MAX_NESTING_DEPTH` in `src/codec.rs` is the bound to derive the result limit from (account for envelope + `{value}` wrapper levels).
- `crates/hexput-connection/{Cargo.toml,src/lib.rs}` -- after the gate (line ~120), call `hexput_script::direct_execution(&request.payload)` and wrap `Result`/`Error` with the request id; on `send` returning `InvalidInput` (line ~76), send `protocol.response_too_large` with the id instead of only logging. Rewrite the module doc's "What a connection is answered today".
- `scripts/check-crate-graph.py` -- pin `hexput-script` and `hexput-exec` exact sets; update `hexput-connection`'s; keep enforce's sole dependent `hexput-exec`.
- `ARCHITECTURE-SPINE.md` graph (line ~216) -- dated amendment adding `script --> port` and `conn --> script` (Decision 1).
- `crates/hexput-tests/Cargo.toml` -- dev-deps `hexput-exec`, `hexput-script`. New `tests/exec.rs`, `tests/script.rs` (payload decoding and conversion rows, depth boundary); `tests/connection.rs:443` and `tests/daemon.rs:663` expect `not_implemented` and must change; `tests/port.rs:471` stable-string list; add a daemon-socket run and the isolation row.
- `AGENTS.md` Project Status; `deferred-work.md` lines 138-151 (mark the frame-size, depth and `not_implemented` entries resolved by this spec) -- keep honest.
- Do not touch: `hexput-session` (no Config read needed yet), `hexput-transport`, `hexput-enforce`, `hexput-rpc`.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-interpreter/src/lib.rs`, `crates/hexput-parser/src/lib.rs`, `crates/hexput-cli-core/src/lib.rs` -- re-export `Program`; move `is_identifier`.
- [x] `crates/hexput-exec/**` -- the Executor entry point.
- [x] `crates/hexput-port/src/error.rs` -- codes.
- [x] `crates/hexput-script/**` -- Direct Execution, payload decoding, wire↔value, depth check.
- [x] `crates/hexput-connection/**` -- route `ExecutionStart`; `response_too_large`.
- [x] `scripts/check-crate-graph.py`, `ARCHITECTURE-SPINE.md` -- edges and amendment.
- [x] `crates/hexput-tests/**` -- every matrix row.
- [x] `AGENTS.md`, `deferred-work.md` -- status.

**Acceptance Criteria:**
- Given the five CI commands, when run, then all pass.
- Given the workspace, when searched for calls to `evaluate`/`evaluate_with_variables`, then the only daemon-side caller is `hexput_exec::execute` (the CLI's local `hexput eval` is not a Daemon execution path) (AD-3).
- Given `ProtocolCode`, when listed, then `NotImplemented` is gone and nothing refers to it.

## Design Notes

- No AST Cache exists yet, so "no cache entry" holds by construction; Epic 4 adds the cache and the test that Direct Execution leaves it empty.
- Running inline blocks this connection's loop and a runtime worker while a Script runs, and an infinite loop pins it (no budget until Epic 3). 2.7 moves dispatch to independent tasks; this spec keeps the call in one place so that move is local.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green.

## Implementation Notes

- Implemented by a concurrent session in this working tree; this session took over the finished state at the maintainer's direction, verified it against the spec and ran the five CI commands (all green, 443 tests).
- `direct_execution` returns `Result<Value, Box<ErrorBody>>` — boxed so every `Result` it returns is not as large as the error payload.
- `MAX_RESULT_DEPTH = MAX_NESTING_DEPTH - 2`: the envelope map and the `{value}` payload map take two of the decoder's levels. A test encodes a result at exactly that depth and decodes it back through `hexput_port::decode`.
- The result walk is iterative and also computes a lower bound on the encoded size, refusing a result certain to exceed `MAX_FRAME_LEN` as `protocol.response_too_large` before conversion. That bound also stops a result sharing one collection exponentially (`[x, x]` nested) without expanding it. The connection's `InvalidInput` fallback covers whatever the bound misses.
- `Array::iter`/`Object::iter` (borrowed) were added to `hexput-interpreter` so the walk and the conversion do not copy the result.
- An invalid-UTF-8 MessagePack str can arrive as `Binary` from `hexput-port`'s codec; the refusal message names both possibilities.
- `Value` is `#[non_exhaustive]`: a kind the walk does not know is refused as `type.function_result` (the interpreter already makes that unreachable).

- `direct_execution` returns `Result<Value, Box<ErrorBody>>`: clippy's `result_large_err` rejects an unboxed 136-byte `ErrorBody`, and the workspace has no lint suppressions to follow.
- The result walk also bounds size, not only depth: every value encodes to at least one byte (a string or key to at least its length), so a result whose lower bound passes `MAX_FRAME_LEN` is `protocol.response_too_large` before conversion. This stops a result sharing one collection exponentially (`a = [a, a]` sixty times) from being expanded into memory; the `send`-side `InvalidInput` path still covers everything the bound misses.
- The interpreter's `Value` is `#[non_exhaustive]`; a variant the walk does not know is refused as `type.function_result` (no wire representation) rather than dropped. None exists today.
- `Array::iter`/`Object::iter` were added to `hexput-interpreter` so the walk and conversion borrow the result instead of cloning each level.
- `hexput-cli-core` no longer uses `hexput-lexer` after `is_identifier` moved; the manifest edge is left, since the graph check pins that set.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind, edge | A non-terminating Script pins a runtime worker and hangs shutdown | medium | Verified: `answer` calls `direct_execution` synchronously; `abort_all` in `hexput-daemon` cannot interrupt a task that never awaits. Inline execution is the frozen intent until 2.7, and no budget exists until Epic 3 | defer |
| 2 | blind | `deliver` returns `true` when even the refusal cannot be framed, so "never left unanswered" overclaims | low | Real only for a broken adapter (a refusal is a few hundred bytes); direct correction of the doc wording | patch — module doc now says "never left unanswered for its reply's size" |
| 3 | blind | The `InvalidInput` fallback is only tested with an injected failure, not a real result under the lower bound but over the frame | low | The connection test drives the exact branch; a real case needs a ~70 MB result per test run | reject |
| 4 | blind | The size bound limits wire bytes, not memory (~30x amplification into `rmpv::Value`) | medium | Verified: one byte per scalar is counted; each `rmpv::Value` is ~32 bytes. Budgets are excluded by the frozen Never | defer |
| 5 | blind | `to_wire`'s `_ => Nil` arm could silently send `nil` if it drifts from `check_result` | low | Unreachable today: `check_result` refuses every unknown kind before `to_wire` runs, and `Value` has no other variant; a future variant is a compile-visible change to both | reject |
| 6 | blind, edge | `Unrepresentable` becomes `type.function_result` with a fabricated span `(0,0,1,1)` | low | Unreachable today (the interpreter raises `type.function_result` with the real span first); only a future `Value` variant reaches it | reject |
| 7 | blind | AD-3 is not graph-enforced: `hexput-script` keeps the Spine's `script --> interp` edge | low | Real; the edge predates this story (Spine graph) and closing it needs re-exports plus an amendment | defer |
| 8 | blind, edge | `hexput-cli-core` still depends on `hexput-lexer` after `is_identifier` moved; `///` on a `pub(crate) use` | low | Verified: no `hexput_lexer` use left in `hexput-cli-core/src`; direct deletion | patch — edge, crate doc and graph pin removed; `//` comment |
| 9 | blind | A non-UTF-8 `source` is reported as "not a string" | low | Real but rare; distinguishing it adds a branch | reject |
| 10 | blind | No test of a starting variable clashing with a top-level `fn` | low | Pre-existing interpreter behaviour (`evaluate_with_variables` checks `StatementKind::Function`); `hexput_exec::execute` is a pass-through | reject |
| 11 | blind | No test that `return [f]` / `return {k: f}` reach the interpreter's `type.function_result` | low | Covered by the interpreter's own tests (Story 1.7); this change only forwards the diagnostic | reject |
| 12 | blind | The socket isolation test's comment claims concurrency it does not show | low | Verified; both Scripts finish at once. Direct correction | patch — comment reworded, independence left to 2.7 |
| 13 | blind, verification-gap | Nested objects are never tested against the depth limit | medium | Pre-verified: dropping `nested(depth)?` from the Object arm passes every test | patch — `nested_objects_count_toward_the_depth_limit_like_arrays` |
| 14 | blind | `MAX_RESULT_DEPTH` choice untested for the error envelope's own depth | false | An `Error` body is a flat map (span one level deeper): depth 3, far from the limit | reject |
| 15 | blind | `sent[1..]` in a connection test skips the Init reply unchecked | low | Cosmetic test robustness; a failed init would fail the following assertions anyway | reject |
| 16 | edge | An `EncodeError::Serialize` would also be reported as `protocol.response_too_large` | low | `rmp_serde` serializing an `rmpv::Value` has no failing path in practice; distinguishing adds a branch across the adapter | reject |
| 17 | edge | Truncating the rendered path at `PATH_LIMIT` can cut inside a quoted key | low | Cosmetic; the message stays bounded, which is its purpose | reject |
| 18 | edge | A key containing a backtick breaks the backtick-delimited path | low | Cosmetic, rare | reject |
| 19 | verification-gap | Nothing shows a large result that fits is still sent | medium | Pre-verified: an over-counting estimate would pass every test | patch — `a_result_just_under_the_frame_is_sent` (a `MAX_FRAME_LEN - 1024` string encodes and frames) |
