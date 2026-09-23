---
title: 'Story 2.8: Trace every request back to its Client ID'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: '65bc86bcb81a8114c8c7850c1dd997f906abe769'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** The Daemon's log events carry no Client ID and no connection identity, so nothing an operator reads can be attributed to the Backend that caused it — FR-12 (and Epic 7's audit work) would have to be retrofitted. The subscriber also only writes plain text.

**Approach:** Every connection runs inside a `tracing` span naming the connection and its Client ID (an explicit "no Client ID" marker before init), and every request inside a child span naming its correlation id, so every event emitted while handling it — including on the blocking thread that runs the Script and while writing its reply — carries both as structured span fields, never message text. System Config gains an option selecting JSON log output.

## Boundaries & Constraints

**Always:** The Client ID is a span field, never interpolated into a message. Before init the connection span still has the field, set to the marker, and a connection identifier. Spans are created at `ERROR` level so no configured `log_level` can disable them and strip their fields from the events that do pass. The span lives in `hexput-connection` (transport-agnostic, AD-1); the Daemon only configures the subscriber. The connection identifier is the same value the Session registry uses for this Connection's attachment — one identity per Connection per Daemon run. Plain text stays the default output; `log_level` keeps working unchanged for both formats. No lock held across an `.await`.

**Decisions (2026-09-23, Erdem):**
1. *JSON output key* — top-level `log_format = "text" | "json"` in System Config, default `"text"`, validated like `log_level`.
2. *No-Client-ID marker* — `client_id = "none"`.

**Never:** No new message types, no health/metrics or audit events (Epic 7), no log file/rotation settings, no `EnvFilter`/per-target filtering, no change to the wire format, `hexput-script` or `hexput-exec`, no new workspace crate edge.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Pre-init refusal | `ExecutionStart` before `Init`, `log_level = "debug"` | the refusal's events carry `connection` and the no-Client-ID marker | N/A |
| Init | valid `Init` | "init completed" and every later event on that connection carry the issued Client ID | N/A |
| Execution | initialized, a failing Script | its events (incl. those on the blocking thread) carry the Client ID and the request id | N/A |
| Two connections | A and B each init | each connection's events carry only its own Client ID | N/A |
| JSON | `log_format = "json"` | every log line is one JSON object; span fields appear as JSON fields | N/A |
| Quiet level | `log_level = "warn"` | spans still enabled; any emitted `warn`/`error` event keeps its span fields | N/A |
| Bad format | `log_format = "xml"` | exit `1` naming the file, the field, its position and the accepted values | as other invalid values |

</frozen-after-approval>

## Code Map

- `crates/hexput-connection/src/lib.rs` -- `serve` (~line 82) creates the connection span; `exchange` (~line 103) owns it as a local `Span`, instruments `inbound.recv()`, `deliver` and the drain with it (`tracing::Instrument`) and runs `answer` in scope. A `Span` cannot re-record a field in text output without duplicating it (`fmt` appends on `record`), so on a successful init the loop **replaces** the connection span with a new one carrying the Client ID; "init completed" is logged inside the new one. Per request, a child `request{id}` span wraps `answer`, and the existing `Span::current()` capture at the `spawn_blocking` call (line ~128) then carries connection + request into the execution; delivering a finished execution's reply enters a `request{id}` span too. The daemon's `"connection accepted"` debug line moves here (`"connection opened"`), inside the span. Update the module doc (new "# Logging" section).
- `crates/hexput-session/src/lib.rs` -- `ConnectionId` is issued inside `create`/`attach` (line ~84/~107). Add `Sessions::connect(&self) -> ConnectionId` (issued when a connection opens, before init); `create(init, connection)` and `attach(client_id, connection)` take it instead of issuing one. Update `ConnectionId`'s doc (a Connection's id for the run, also its attachment). Add `Display` (the bare number) for logs.
- `crates/hexput-config/src/{lib.rs,file.rs}` -- add `LogFormat { Text, Json }` (`ALL`, `as_str`, `Display`, mirroring `LogLevel`), `SystemConfig::log_format` (default `Text`), raw field + validator mirroring `log_level`; module doc block and `config.example.toml` document it.
- `crates/hexput-daemon/src/lib.rs` -- `logging` (~line 318) takes the format; JSON uses `fmt().json()` with the current span and span list, so fields appear in the object. Blocking-pool threads already get the dispatch via `on_thread_start`. Drop the "Structured JSON output is Story 2.8's" note.
- `Cargo.toml` -- add `json` to the `tracing-subscriber` pin; pin `serde_json` (tests parse JSON lines). Spine Stack table: amend `tracing-subscriber`'s feature list (dated amendment).
- `crates/hexput-tests/tests/{daemon.rs,config.rs,session.rs,connection.rs}` -- daemon tests over the real socket with `log_level = "debug"` + `log_format = "json"` parse every log line and assert the matrix rows; one text-format test asserts `client_id=<hex>` appears in span context, not in a message. Config tests for the new field. Session/connection tests follow the new `create`/`attach`/`connect` signatures. Add `serde_json` to hexput-tests' dev-dependencies.
- Do not touch: `hexput-script`, `hexput-exec`, `hexput-port`, `hexput-transport`, `scripts/check-crate-graph.py` (no workspace edge changes).

## Tasks & Acceptance

**Execution:**
- [x] `Cargo.toml`, spine Stack table -- `json` feature, `serde_json` pin, amendment note.
- [x] `crates/hexput-config/{src/lib.rs,src/file.rs,config.example.toml}` -- `log_format`.
- [x] `crates/hexput-session/src/lib.rs` -- `connect`; ids passed into `create`/`attach`; `Display`.
- [x] `crates/hexput-connection/src/lib.rs` -- connection and request spans; module doc.
- [x] `crates/hexput-daemon/src/lib.rs` -- format-aware subscriber; accept log moved.
- [x] `crates/hexput-tests/{Cargo.toml,tests/*.rs}` -- matrix rows; signature updates.
- [x] `AGENTS.md` Project Status (2.8 done, Epic 2 code-complete, next step), `deferred-work.md` if anything is deferred.

**Acceptance Criteria:**
- Given the five CI commands, when run, then all pass.
- Given any event emitted while a connection is served, when read in either format, then it carries `connection` and `client_id` as span fields, and events for a request also carry its `id`.
- Given `hexput-connection`, when read, then no message string interpolates a Client ID.

## Design Notes

Replacing the span on init, rather than `record`ing into a `client_id = Empty` field, is deliberate: `Empty` is omitted from output (violating "mark, don't omit"), and re-recording a set field duplicates it in text output. JSON line shape expected:

```json
{"timestamp":"…","level":"DEBUG","fields":{"message":"a Direct Execution failed","code":"runtime.…"},
 "span":{"id":7,"name":"request"},
 "spans":[{"client_id":"3f…9a","connection":4,"name":"connection"},{"id":7,"name":"request"}]}
```

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green (the first build may need network for `tracing-serde`; then run with `--locked`).

## Implementation Notes

- Markers use `%` (Display): plain text shows `client_id=none` and `client_id=<hex>` alike; JSON shows both as strings. A request with no readable id gets `id = none` in its span; a malformed frame whose id is unreadable is logged in the connection span alone.
- `refuse` now logs `refused a request` with its `code` at `debug`, so the pre-init refusal (and every other refusal) has an event carrying the span fields.
- `Sessions::attach` returns `bool` (whether the Session exists) now that the caller supplies the `ConnectionId`.
- The quiet-level row is tested in `tests/connection.rs` (an Outbound whose writes are all unframable triggers the connection's one `warn`), because nothing over the real socket can make the Daemon emit a `warn`/`error` on demand. `tracing` caches callsite interest process-wide; a callsite first hit with no subscriber while another test installs one can be cached as disabled (observed: ~30% flake), so every `serve` in that file runs under a discarding subscriber.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind | Spec `in-review` vs sprint-status `in-progress`; AGENTS.md says stories are in review | false | Step 5 syncs sprint status at presentation; mid-review mismatch is the workflow's own ordering | reject |
| 2 | blind | An invalid `Init` logs two events (detail line + `refused a request`) | low | Real, but the two carry different facts (the decode error vs the code); merging needs `refuse` to take the detail | reject |
| 3 | blind, edge | Request `id` is a JSON number when readable, the string `"none"` otherwise | low | Verified in `request_span`; a typed log pipeline sees a mapping conflict; one-token correction | patch — always a string |
| 4 | blind | `ConnectionId`'s new `Display` ("as logs show it") is unused in production | low | Verified: the span records `id.get()`; only a session test calls it; direct deletion | patch — removed |
| 5 | blind | `serde_json` pinned with no Stack-table row | low | Verified against the spine's clap amendment ("the one place a third-party version lives") | patch — row added |
| 6 | blind | Two-connection test accepts `"none"` after init | medium | Verified: the `all(... == "none" || == own)` assertion passes if post-init events keep the old span | patch — ordering asserted |
| 7 | blind | Quiet-level row not tested through the Daemon's own `logging()` | low | Nothing over the socket can make the Daemon emit a `warn` on demand; the connection test uses the same `json()`+max-level shape | reject |
| 8 | blind | Discarding-subscriber doc claims "every `serve`", but the quiet test installs its own | false | The quiet test also runs `serve` under a live subscriber, which is the property the doc needs | reject |
| 9 | blind | One request appears under several span instances (answer vs reply vs blocking thread) | low | Real; fields are identical, only span identity differs, which matters to no current layer; handing spans through `JoinSet` adds structure | reject |
| 10 | blind | `ERROR`-level spans' hot-path cost unmeasured, misleading to future layers | low | Rationale recorded in the spec; a per-layer filter is a larger change with no current consumer | reject |
| 11 | blind | Client IDs logged in plaintext despite anti-enumeration stance | false | The ACs mandate logging the Client ID; OQ-2 makes reconnect need a separate secret, so a Client ID alone is not a credential | reject |
| 12 | blind | Pre-init reply write / close events not checked for `client_id=none` | false | The JSON test's first loop asserts both fields on every event inside a connection span, which covers them | reject |
| 13 | blind | Accept failures unattributed; allow-list of Daemon-level events fragile | low | No connection exists when accept fails; allow-list failing on a new event is the intended tripwire | reject |
| 14 | edge | `create`/`attach` accept a caller-supplied `ConnectionId`; uniqueness is now a caller convention | low | Real; only `serve` calls them, once per connection; a runtime guard adds complexity, a doc precondition is direct | patch — precondition documented |
| 15 | edge | A panicked execution is logged in the connection span without its request id | low | Real; pre-existing 2.7 path (already deferred); needs `join_next_with_id` or `catch_unwind` | defer |
| 16 | edge | Debug-level daemon tests could flake on callsite interest caching | maybe-false | 40 clean runs by the verification layer; every daemon-test `serve` runs under a dispatcher; would be medium if real | defer (unverified) |
| 17 | edge | An unframable reply now logs two debug events | low | Real and harmless (the cause line and the refusal code) | reject |
| 18 | verification-gap | Request span around delivering a finished execution's reply is untested | medium | Pre-verified: the quiet test never reaches `Finished(Ok)` | patch — execution added to the quiet test |
| 19 | verification-gap | Which span a malformed frame is logged in is untested | medium | Pre-verified: every malformed-frame test uses a discarding subscriber | patch — JSON daemon test covers both cases |
