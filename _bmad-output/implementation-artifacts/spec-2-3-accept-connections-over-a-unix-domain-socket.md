---
title: 'Story 2.3: Accept connections over a Unix Domain Socket'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: 'ed9653d1da94c4307b47392b222aee504177de02'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** The daemon starts and listens nowhere: `hexput-transport`, `hexput-connection` and the `Port` trait are stubs, so no Backend can reach the runtime, and 2.2's codec has nothing feeding it bytes.

**Approach:** Define the async `Port` trait in `hexput-port`, implement it once for a Unix Domain Socket in `hexput-transport` (bind, stale-socket recovery, accept, framed read/write over 2.2's codec), serve each accepted `Port` from a transport-generic connection loop in `hexput-connection`, and have `hexput-daemon` bind the configured socket at startup, spawn one task per connection, and remove the socket on clean shutdown.

## Boundaries & Constraints

**Always:** The core is generic over `Port` and never names a transport type (AD-1); only `hexput-daemon` depends on `hexput-transport` (already asserted). Every accepted connection is its own Tokio task; a read error, an abrupt close, or a panic in one connection never ends the listener or another connection (NFR4). A malformed frame gets 2.2's protocol error response and the connection keeps reading, except a fatal (`frame_too_large`) failure, after which the response is sent and the connection closes. Stale-socket recovery deletes only a *socket* file nobody is listening on; a live socket or a non-socket file at the path is a startup error naming the path, never deleted. Bind failure exits `1` naming the path, before "daemon started" is logged. On clean shutdown the daemon stops accepting and removes the socket file it created. No lock is held across an `.await`. New tokio features are added once, to the workspace pin. `check-crate-graph.py` pins `hexput-transport`, `hexput-connection` and `hexput-daemon`'s exact sets in the same change.

**Decisions (2026-09-23, Erdem):**
1. *Well-formed messages* — 2.4's gate is pulled forward: `ExecutionStart` gets the permanent `protocol.init_not_completed` error; a `Result` or `Error` sent by the Backend gets `protocol.unexpected_message`; `Init` gets a temporary `protocol.not_implemented` error that 2.4 replaces (and removes the code). Every reply echoes the request id.
2. *Unimplemented transports* — a System Config with `[transport.tcp]` or `[transport.websocket]` makes the daemon exit `1` naming the transport that has no adapter yet; it never starts with a partial set.
3. *Socket permissions* — `[transport.uds]` gains an optional `mode` (a string of 3–4 octal digits, at most `0777`, e.g. `"0660"`); absent means the process umask decides. An invalid `mode` is a `ConfigError` naming the file and field. The mode is in force before any peer can connect by path.

**Never:** No Session, Client ID issuance, or execution (2.4–2.6). No TCP/WebSocket/Named Pipe adapter (Epic 5). No per-connection concurrent dispatch or writer task (2.7 — but the trait must not prevent it). No Client-ID tracing fields (2.8). No socket ownership (`chown`) management.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Start | `[transport.uds] path` in a writable dir, nothing there | socket created, "listening" logged with the path | N/A |
| Stale socket | socket file at the path, no listener | removed, rebound, removal logged | N/A |
| Live socket | another process listening at the path | exit `1`, "already in use", file untouched | startup error |
| Not a socket | regular file / directory at the path | exit `1` naming the path, file untouched | startup error |
| Missing parent / path too long | parent dir absent; path > platform `sun_path` limit | exit `1` naming the path and the OS reason | startup error |
| Framed message | valid envelope | handed to the core through `Port`; reply on the same connection per Decision 1 | error response |
| Socket mode | `mode = "0660"` | socket file has exactly `0660` when the first peer can connect | invalid mode: exit `1`, `ConfigError` |
| TCP/WS configured | `[transport.tcp]` present | exit `1` naming `tcp` as not yet supported | startup error |
| Malformed frame | garbage frame, then a valid one | protocol error for the first, the second still processed | error response |
| Oversized prefix | length > `MAX_FRAME_LEN` | `frame_too_large` response, connection closed | error response, close |
| Abrupt disconnect | 3 clients; one closes mid-frame / resets | the other two still get replies; new clients still accepted | logged at debug |
| Shutdown | SIGINT/SIGTERM | listener closed, socket file removed, exit `0` | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-port/src/lib.rs` (+ new `port.rs`) -- add the `Port`/`Inbound`/`Outbound` traits and `Received`; crate stays tokio-free and I/O-free (traits only). Lib doc already promises the trait "with Story 2.3".
- `crates/hexput-port/src/frame.rs` -- reuse `FrameDecoder` (push/`next_frame`/`is_poisoned`), `encode_frame`, `MAX_FRAME_LEN`; `codec.rs` -- `decode`, `encode`, `ProtocolFailure::to_response`; `error.rs` -- `ErrorBody`, `error_response`; `ProtocolCode` gains `InitNotCompleted`, `UnexpectedMessage`, `NotImplemented` (Decision 1; keep `ALL` complete, `is_fatal` false for all three).
- `crates/hexput-config/src/{lib.rs,file.rs}`, `config.example.toml` -- `UdsTransport` gains `mode: Option<u32>` parsed from the `mode` string (Decision 3); tests in `tests/config.rs`.
- `crates/hexput-transport/{Cargo.toml,src/lib.rs,src/uds.rs}` -- `#[cfg(unix)] mod uds`: `UdsListener::bind(path)`, `accept() -> UdsPort`, `UdsPort: Port` (reader half owns a `FrameDecoder`; writer half writes `encode_frame(encode(..))`), removal on drop-free explicit `close()`. Depends on `hexput-port` + `tokio` (`net`, `io-util`) + `tracing`.
- `crates/hexput-connection/{Cargo.toml,src/lib.rs}` -- `pub async fn serve<P: Port>(port: P)`: recv loop, protocol-error replies, fatal close, message hand-off. Needs a new `hexput-connection -> hexput-port` edge (see Design Notes).
- `crates/hexput-daemon/src/lib.rs` -- `serve()` binds the UDS listener before "daemon started", `select!`s accept vs shutdown, spawns `hexput_connection::serve`, logs accept errors and continues, removes the socket on exit; bind errors become exit `1`. Update the crate doc's "No transport is started yet".
- `Cargo.toml` -- tokio gains `net`, `io-util`, `macros`, `sync`; `deferred-work.md` line 15 (tokio features) updated.
- `scripts/check-crate-graph.py` -- `EXACT_DEPENDENCIES` for `hexput-transport` {port}, `hexput-connection` {session, port}, `hexput-daemon` (current set).
- `ARCHITECTURE-SPINE.md` -- dated amendment adding the `conn --> port` edge to the graph.
- `crates/hexput-tests/{Cargo.toml,tests/transport.rs,tests/connection.rs,tests/daemon.rs}` -- dev-deps `hexput-transport`, `hexput-connection`, `tokio`; `daemon.rs`'s `VALID` (`/run/hexput.sock`) must move to a sandbox socket path or every existing daemon test fails to bind.
- `crates/hexput-config/config.example.toml` -- document `mode` and the socket's lifecycle (stale removal, removal on shutdown).
- Do not touch: `hexput-session`, `hexput-shared`.

## Tasks & Acceptance

**Execution:**
- [x] `Cargo.toml`, `ARCHITECTURE-SPINE.md` -- tokio features; `conn --> port` edge amendment.
- [x] `crates/hexput-config/**` -- optional `mode` (Decision 3).
- [x] `crates/hexput-port/src/{lib.rs,port.rs,error.rs}` -- `Port` traits per Design Notes 1; three new `ProtocolCode`s.
- [x] `crates/hexput-transport/**` -- UDS adapter per Design Notes 2.
- [x] `crates/hexput-connection/**` -- transport-generic `serve` per Design Notes 3.
- [x] `crates/hexput-daemon/src/lib.rs` -- refuse unimplemented transports (Decision 2), bind, accept loop, shutdown cleanup.
- [x] `scripts/check-crate-graph.py` -- pin the three sets.
- [x] `crates/hexput-tests/**` -- every matrix row: adapter tests over a temp-dir socket; `serve` tested with an in-memory `Port` (proves AD-1: no socket involved); daemon end-to-end over a real socket with a test-controlled shutdown future; existing daemon tests moved to sandbox socket paths.
- [x] `AGENTS.md` Project Status, `deferred-work.md`, `config.example.toml` -- keep honest.

**Acceptance Criteria:**
- Given the five CI commands, when run, then all pass.
- Given `hexput-connection`, when its source is inspected, then it names no transport type and is exercised in tests by a non-socket `Port`.

## Design Notes

1. **Port shape.** Split halves so 2.7 can read and write concurrently without a lock: `trait Port: Send + 'static { type Inbound: Inbound; type Outbound: Outbound; fn split(self) -> (Self::Inbound, Self::Outbound); }`; `Inbound::recv(&mut self) -> impl Future<Output = Received> + Send`; `Outbound::send(&mut self, Envelope<Value>) -> impl Future<Output = io::Result<()>> + Send`. `enum Received { Message(Envelope<Value>), Malformed(ProtocolFailure), Closed(Option<io::Error>) }` — the adapter owns bytes and framing, the core sees only envelopes. Native `async fn`-in-trait style (RPITIT), no `async-trait` crate; generic, not `dyn`.
2. **UDS adapter.** Bind: if the path exists and is a socket, try connecting — refused means stale (remove, rebind), success means live (error). Anything else at the path is an error. With a `mode`, bind at a temporary sibling name, `chmod`, then `rename` onto the path — connecting by path is impossible until the mode is set, so no peer slips in between bind and chmod. `UdsListener` remembers the bound file's (dev, inode) and removes it on `close()` only if it is still that file. EOF with a partial frame buffered is `Closed`, not `truncated_frame` — a peer that left gets no reply.
3. **Connection loop.** `serve` loops on `recv`: `Message` → reply per Decision 1, `Malformed` → send `to_response()`, close if fatal; `Closed` → return. A failed `send` ends the loop. Errors log at `debug` (a peer leaving is routine).
4. **Graph edge.** The Spine's graph lets `hexput-connection` reach `hexput-port` only through `hexput-session`. The connection actor *is* what drives the Port, so a direct edge is the honest one; recorded as a dated Spine amendment rather than routed through a session re-export.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green.

## Implementation Notes

- Implemented directly in the main session (no implementation subagent), from this spec.
- `Received::Closed(Option<io::Error>)`: `None` for a clean close *including* EOF mid-frame (a peer that left is owed no reply), `Some` for a read error. After `Closed` or a fatal `Malformed` the UDS inbound half yields `Closed(None)` forever.
- `UdsOutbound::send` maps an envelope too large to frame to `io::ErrorKind::InvalidInput` with nothing written; the connection stays usable (tested). Turning that into an `Error` response carrying the request id is still 2.6's job (2.2 triage row 3).
- `mode` is a TOML *string* (`"0660"`), not an integer: a bare `660` would silently be decimal. Temporary bind name is `.hexput-<pid>-<n>.sock` in the socket's directory; it is removed if `chmod`/`rename` fails.
- `UdsListener::close` compares `(dev, inode)` so it never removes a successor's socket. Bind is synchronous (tokio's `UnixListener::bind` is) and must run inside the runtime; the stale probe is a blocking local connect.
- Daemon: `serve` returns `Result<(), String>`; the startup error goes to stderr as `error: …` like config errors. The accept loop is a `biased` `select!` over shutdown, reaping the connection `JoinSet` (a panicked task is logged at `error`), and accept; a failed accept logs a warning and backs off 100 ms. A non-Unix build has a `serve` that refuses to start (no adapter for that platform until the Named Pipe).
- tokio gained `time` beyond the four features the Code Map listed, for that back-off.
- Review patch: `bind_with_mode` binds inside a private `0700` directory beside the path and `hard_link`s onto it (an `AlreadyExists` link is `InUse`), removing the temporary socket and directory either way; `bind` checks `SocketAddr::from_pathname(path)` first so an overlong path fails with and without a mode. This replaces Design Note 2's "temporary sibling name, then `rename`".
- Existing daemon tests used a fixed `/run/hexput.sock`; they now use a per-sandbox socket via `Sandbox::valid()`. The real-binary signal test also asserts the socket is removed.
- New deferred items: remove `protocol.not_implemented` in 2.4; no bound on concurrent connections.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind | A peer can connect to the guessable temporary socket before its mode is set | medium | Real: the sibling name is predictable and, under a permissive umask, connectable between `bind` and `chmod`, contradicting Decision 3 ("in force before any peer can connect by path") | patch — bind inside a private `0700` directory |
| 2 | blind, edge | `rename` replaces whatever appeared at the path after `clear_stale` checked it | low | Real TOCTOU (two daemons starting on one path both "succeed"); fix is a direct substitution | patch — `hard_link` (fails with `EEXIST` → `InUse`) then remove the temporary |
| 3 | edge, verification-gap | An overlong path with a `mode` binds at the short temporary name and links onto an unreachable path; the daemon reports it is listening | high | Reproduced by the reviewer; the matrix row requires exit `1` | patch — `SocketAddr::from_pathname(path)` check up front; test with `Some(mode)` |
| 4 | blind, edge, verification-gap | `serve` closes the connection on any send error, contrary to `Outbound`'s `InvalidInput` = still usable | low | Real contract/consumer disagreement; no reply is large enough to hit it yet | patch — `InvalidInput` is logged and the connection stays open; test added |
| 5 | blind | Port traits say nothing about cancel safety | low | Real doc gap that 2.7's `select!` will depend on; doc-only fix | patch |
| 6 | blind | AGENTS.md still says "No transport listens yet. Every other daemon crate is still a stub." | low | Confirmed contradiction introduced by this story's status update | patch — AGENTS.md's own rule ("keep this section honest") overrides the agent-context defer route |
| 7 | blind | Test-helper doc for `Sent` claims it counts reads | low | Confirmed; direct correction | patch |
| 8 | blind | Transport tests hard-code `/tmp` while daemon tests use `temp_dir()` | false | The helper's comment already states why (socket path length limit); the difference is deliberate | reject |
| 9 | blind | Sprint status `in-progress` vs spec `in-review` | false | Sprint sync is step 5's job | reject |
| 10 | blind, edge | Blocking stale-socket probe hangs startup when a live listener's backlog is full | low | Real on Linux (blocking connect waits for backlog space), but needs a saturated live daemon at the same path; fix needs a non-blocking/timeout probe | reject |
| 11 | blind, edge | Answering a Backend's `Error` with an `Error` can ping-pong | low | Decision 1 (frozen) explicitly answers `Result`/`Error` with `unexpected_message`; a loop needs a Backend that also answers errors with errors | reject — settled by intent |
| 12 | blind | `config.example.toml`'s uncommented TCP/WebSocket sections make a copied example fail to start | low | Real, but the failure is exit `1` with a message naming the unserved section, and the file says so; commenting them out means reworking the example test | reject |
| 13 | blind, edge | A crash between bind and placement leaves the temporary socket behind | low | Microsecond window; after the patch the leftover is a `.hexput-<pid>-<n>` directory, harmless | reject |
| 14 | blind, edge | Accept back-off sleep delays shutdown by up to 100 ms per failed accept | low | Real but bounded and only under persistent `EMFILE`; fix adds a nested `select!` | reject |
| 15 | blind | Panic isolation of connection tasks untested | low | No seam to inject a panic into `serve`; isolation rests on `JoinSet`/`tokio::spawn` semantics | reject |
| 16 | edge | Temporary name can exceed `sun_path` when the configured path fits | low | After the patch the temporary is `<parent>/.hexput-<pid>-<n>/s`, ~20 bytes longer; only paths within that of the limit are affected, and they fail with an error naming the path | reject |
| 17 | edge | `symlink_metadata` failing after bind leaves the socket behind | low | Requires the file to vanish or become unreadable in the microseconds after bind | reject |
| 18 | edge | A mode without the owner write bit (`"0444"`, `"000"`) locks out every non-root Backend | low | An explicit operator choice of permission bits; documented as such | reject |
| 19 | edge | No cap on concurrent connections; a peer that never reads blocks its task's `write_all` forever | medium | Real resource exhaustion by a local peer; per-connection isolation (NFR4) holds. Resource bounds are Epic 3 / Epic 5 work | defer (connection cap already recorded this story; write timeout appended) |
| 20 | verification-gap | Temporary-socket cleanup on `chmod`/link failure never runs in a test | low | Pre-verified; no deterministic way to fail placement after `clear_stale` without a fault-injection seam | defer |
