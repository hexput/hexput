---
title: 'Story 5.2: Connect over WebSocket'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '055ec5e8370553e01c2488f46169bed41368d865'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred: []
---

<intent-contract>

## Intent

**Problem:** The Daemon refuses a System Config with `[transport.websocket]` at startup ("not supported yet"). A Backend behind HTTP-only proxies and gateways therefore has no way in. FR-9 requires WebSocket with TLS, and FR-29 fixes 7478 as its default port. Today's System Config also lets WebSocket run without TLS, which FR-9 forbids.

**Approach:**
- Add a WebSocket adapter, `hexput-transport::websocket`, implementing the existing `Port`.
- It reuses Story 5.1's `TlsIdentity` and its handshake-in-the-connection-task shape. A TLS handshake is followed by the WebSocket upgrade (tokio-tungstenite), with one timeout over both.
- Each binary WebSocket message carries exactly one MessagePack envelope, with no length prefix. This is Story 2.2's recorded framing decision.
- `[transport.websocket]` must give `tls_cert` and `tls_key`. Its `bind` may omit the port, which then defaults to 7478.
- The Daemon serves UDS, TCP and WebSocket from one accept loop into the same `hexput_connection::serve`.

## Boundaries & Constraints

**Always:**
- **TLS on every WebSocket connection, exactly as on TCP.**
  - Every accepted connection completes a rustls server handshake through the same `TlsIdentity`/`ServerConfig` path as `tcp_tls`, and only then the HTTP upgrade.
  - There is no `ws://` mode and no option to disable TLS.
- **The handshake runs in the connection's own task**, never in the accept loop.
  - `WebSocketListener::accept()` returns a `WebSocketHandshake` right after the TCP accept, with `TCP_NODELAY` set.
  - `WebSocketHandshake::finish()` runs TLS and then the upgrade, together bounded by `tcp_tls::HANDSHAKE_TIMEOUT` (10 s; `with_handshake_timeout` overrides it). A timeout is `io::ErrorKind::TimedOut`.
  - Any failure closes only that socket. The Daemon logs it at `debug` with the peer address: `"WebSocket handshake failed"`.
- **Upgrade policy.**
  - Any request path is accepted.
  - No subprotocol is negotiated.
  - No `Origin` check: the Backend authenticates through init/reconnect, never through cookies.
  - tokio-tungstenite is configured with `max_message_size = max_frame_size = Some(hexput_port::MAX_FRAME_LEN)`.
- **Messages → `Received`** (`WebSocketInbound::recv`, which must stay cancel-safe):

  | Peer sends | `recv` yields |
  |---|---|
  | Binary message | `decode(&bytes)`, so an empty or invalid message is that decode's `Malformed`, which is not fatal |
  | Text message | `Malformed` with `protocol.malformed_frame`, id `None`, message `"a WebSocket text message; Hexput frames are binary messages"`. Not fatal: reading continues |
  | Ping / Pong | Nothing. tungstenite answers a Ping |
  | A message over the limit | `Malformed` with `protocol.frame_too_large`, id `None` (fatal). The half is `Closed` from then on |
  | A Close frame | The stream is driven until tungstenite reports it closed, so its close reply is flushed. Then `Closed(None)` |
  | End of TCP without a Close frame, `ResetWithoutClosingHandshake`, or `UnexpectedEof` | `Closed(None)`. This matches Story 5.1, where a Backend that stops sending is still answered |
  | Any other error | `Closed(Some(io::Error))` |

  After a `Closed`, the half yields `Closed(None)` forever.
- **Outbound.** `send` writes one binary message of `encode(&envelope)` and flushes.
  - An encoded body longer than `MAX_FRAME_LEN` is `io::ErrorKind::InvalidInput`, and nothing is written.
  - The halves come from `futures_util::StreamExt::split`.
- **System Config (`hexput-config`).**
  - Add `pub const DEFAULT_WEBSOCKET_PORT: u16 = 7478`. `transport.websocket.bind` accepts the same forms as TCP's and defaults to that port.
  - `tls_cert` and `tls_key` are required, so leaving one out is serde's missing-field error with its position. The half-pair check and its message are removed.
  - `WebSocketTransport.tls` becomes `Tls`, no longer `Option<Tls>`.
- **Daemon startup order.**
  1. Load the TCP identity.
  2. Load the WebSocket identity. It is loaded separately even if it names the same files.
  3. Bind UDS.
  4. Bind TCP.
  5. Bind WebSocket.
- **Startup failures.**
  - A failure exits `1` with one stderr line, `error: <message>`.
  - A WebSocket bind failure reads `error: <addr>: cannot listen for WebSocket: <os error>`.
  - Any failure after a listener is bound closes every listener bound before it. No socket file is left behind.
- **Logging and shutdown.**
  - Once bound, the Daemon logs `info` `"listening on WebSocket with TLS"` with field `address` = the listener's actual local address.
  - Accept failures carry `transport = "websocket"`.
  - Shutdown stops every listener.
- **Dependency pins** (in `[workspace.dependencies]`):
  - `tokio-tungstenite = { version = "0.30.0", default-features = false, features = ["handshake"] }`. The TLS is our own tokio-rustls, so it takes neither tungstenite's TLS features nor `connect`.
  - `futures-util = { version = "0.3.34", default-features = false, features = ["sink", "std"] }`, the version already in `Cargo.lock`.
  - `hexput-transport` gains both. `hexput-tests` gains both as dev-dependencies.
- **Lock file and graph.**
  - `rustls` stays 0.23.45, and no `aws-lc` package appears in `Cargo.lock`.
  - No workspace crate edge changes, and `check-crate-graph.py` stays as is.
- **Disk and build:** this machine is short on disk, so every cargo command runs with `--release`.
  - Resolve the new packages once with `cargo build --workspace --all-targets --release`, without `--locked`.
  - Then confirm that `git diff Cargo.lock` only adds packages, then use `--locked`.

**Never:**
- Plaintext WebSocket in any form, even on loopback.
- A length prefix inside WebSocket messages, or text-message envelopes.
- A change to the `Port`/`Inbound`/`Outbound` traits. This includes a close hook: the Daemon sends no Close frame of its own (see Design Notes).
- Session, credential or execution logic in `hexput-transport`.
- Any crate other than `hexput-daemon` naming the adapter.
- HTTP routing, Origin or subprotocol checks, or permessage-deflate.
- Named Pipe or non-Unix serving (Story 5.3). The `cfg(not(unix))` Daemon path stays as it is.
- A connection cap, a pre-init deadline, or a Session TTL (Story 5.8).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| WSS round trip | Daemon with `[transport.websocket]` and the fixture cert/key. A tokio-tungstenite client over a tokio-rustls stream that trusts `ca.pem` (`wss://localhost:<port>/`) sends `Init`, then `ExecutionStart {source:"return 1 + 2;", variables:{}}` as binary messages | `Result {client_id}`, then `Result {value: 3}`, each one binary message. The replies are identical to UDS and TCP | — |
| Default port | `bind = "127.0.0.1"` | listens on `127.0.0.1:7478`, and the logged `address` ends in `:7478`. The test skips when 7478 is taken, like 5.1's 7476 test | — |
| Plain `ws://` probe | a raw TCP client sends an HTTP upgrade request in cleartext | no upgrade response and no frame. The socket is closed, and a WSS client on the same Daemon still works | `debug` "WebSocket handshake failed" |
| TLS but no upgrade | a TLS client sends `GET / HTTP/1.1` with no upgrade headers, or a length-prefixed frame | no Hexput frame. The connection ends | `debug` "WebSocket handshake failed" |
| Silent peer | the client connects and sends nothing | `finish` fails `TimedOut` after the short test timeout | the socket is dropped |
| Text message | `"hello"` sent as text, then a valid binary request | `Error` `protocol.malformed_frame` with a nil id. The binary request is then answered normally | — |
| Oversized message | a binary message over `MAX_FRAME_LEN` (adapter test: a server half with the limit in force) | `Malformed` `protocol.frame_too_large`, then `Closed` | the connection is closed |
| Peer Close frame | the client sends Close after its last reply | `Closed(None)`, and the client receives the close reply with no error | — |
| Intermediary drop | after Init, the client's TCP stream is dropped with no Close frame and no `close_notify` | `Closed(None)`. The Daemon logs `"connection detached from its Session"`, and a new WSS client and a UDS client are still served | — |
| WebSocket config without TLS | `[transport.websocket]` missing `tls_cert` or `tls_key` | a config error naming the missing field with its line and column. Exit 1 | — |
| Bad WS cert/key | missing, malformed, or a mismatched key | exit 1, `error: <path>: …`, as for TCP. Nothing is bound, no socket file | — |
| WS address in use | a test holds the address | exit 1, `error: <addr>: cannot listen for WebSocket: …`. The UDS file is removed and TCP is unbound | — |

</intent-contract>

## Code Map

- `Cargo.toml:43` -- `tokio-tungstenite = "0.30.0"` is pinned with default features and has no consumer yet. Set its features and add `futures-util`, each with a Story comment.
- `crates/hexput-transport/src/lib.rs` -- the module doc lists the adapters. Add `pub mod websocket;` (no cfg) and update the doc.
- `crates/hexput-transport/src/tcp_tls.rs` -- has `TlsIdentity { config: Arc<ServerConfig> }`, the `TcpTlsListener`/`TlsHandshake::finish` pattern (`tokio::time::timeout` around `TlsAcceptor::accept`, the `TimedOut` message) and `HANDSHAKE_TIMEOUT`.
  - Expose the config to the crate (`pub(crate)` accessor) so `websocket` builds its `TlsAcceptor` from it.
  - Change no public behaviour.
- `crates/hexput-transport/src/framed.rs` -- the reference semantics for `recv`: the closed flag, `UnexpectedEof` as a clean close, a fatal oversized frame. The WebSocket adapter mirrors these semantics but does not use this module.
- `crates/hexput-port/src/{codec.rs,error.rs,port.rs}` -- read-only.
  - `ProtocolFailure { id, error }` and `ProtocolError::new(code, msg)` are public, and so is `ProtocolCode::{MalformedFrame, FrameTooLarge}`.
  - `decode`, `encode`, `MAX_FRAME_LEN`.
  - `hexput-port`'s lib doc line 9 already says WebSocket skips the prefix.
- `crates/hexput-config/src/file.rs` -- `RawWebSocket` (optional TLS), `Validator::websocket`, `half_tls_pair` (to be deleted), and `bind(…, None)`, which becomes `Some(DEFAULT_WEBSOCKET_PORT)`.
- `crates/hexput-config/src/lib.rs` -- the module-doc TOML sample and its prose ("TLS optional"), `WebSocketTransport`, and `DEFAULT_TCP_PORT`, beside which the new constant goes.
- `crates/hexput-config/config.example.toml:66-74` -- the "not served yet" and "TLS optional" notes need rewriting.
- `crates/hexput-daemon/src/lib.rs` -- several places change:
  - `listen()` refuses websocket; replace that with the identity load and bind steps.
  - `Listeners { uds, tcp }`: add `websocket`.
  - The accept `select!` gets a third branch built like the TCP one.
  - `Listeners::close`.
  - The module doc's Contract and Serving sections.
- `crates/hexput-tests/tests/transport.rs:383-797` -- the TLS adapter test helpers (`fixture`, `identity`, `trusting_client_config`, `tls_connect`) to reuse for the WebSocket client.
- `crates/hexput-tests/tests/daemon.rs:1063-1080` -- the startup-failure table's `[transport.websocket] not supported yet` row, which this story removes.
  - `mod tls_serving` (2631+): `Daemon` harness, `fixture`, `tcp_section`, the blocking `tls()` client, `frames`, the port-7476 skip pattern, and the address-in-use test, all of which the new WebSocket tests copy.
- `crates/hexput-tests/tests/config.rs` -- tests that flip:
  - 85-115: the full parse with websocket on `[::1]:7401`.
  - 156 `a_websocket_transport_may_omit_tls` becomes a refusal.
  - 412 `half_a_websocket_tls_pair_…` becomes a missing-field test.
  - 781 `a_bare_ip_websocket_bind_still_needs_a_port` now gets 7478.
  - The 492/509 bind-error tests need TLS keys added.
- `scripts/check-crate-graph.py` -- `hexput-transport` is `{hexput-port}`, and SOLE_DEPENDENTS stays unchanged.

## Tasks & Acceptance

**Execution:**
- `Cargo.toml`, `crates/hexput-transport/Cargo.toml`, `crates/hexput-tests/Cargo.toml` -- add the pins and wire them in.
  - Resolve the lock once without `--locked`.
  - Verify `rustls` 0.23.45 and that there is no `aws-lc`.
- `crates/hexput-transport/src/websocket.rs` (new) -- define:
  - `WebSocketListener::bind(addr, TlsIdentity)` (async), `with_handshake_timeout`, `local_addr`, and `accept() -> io::Result<WebSocketHandshake>` (cancel-safe, nodelay);
  - `WebSocketHandshake::{peer_addr, finish}`;
  - `WebSocketPort: Port`, with `WebSocketInbound`/`WebSocketOutbound` (public structs).

  The module doc states the TLS, upgrade and message policy. This is FR-9 and AD-1.
- `crates/hexput-transport/src/tcp_tls.rs` -- add a crate-private access to the identity's `Arc<ServerConfig>`.
- `crates/hexput-config/src/{lib.rs,file.rs}`, `config.example.toml` -- add `DEFAULT_WEBSOCKET_PORT`, make the TLS required with the `Tls` type, make the bind port optional, and update the docs and the example per Boundaries.
- `crates/hexput-daemon/src/lib.rs` -- implement the startup order, the error lines, the `"listening on WebSocket with TLS"` log and the third accept branch. The handshake runs in the spawned task with the `debug` failure log. Also update `close` and the module doc, and delete the websocket refusal.
- `crates/hexput-tests/tests/transport.rs` -- adapter tests for these matrix rows:
  - round trip;
  - text message;
  - malformed binary;
  - plaintext probe;
  - TLS without upgrade;
  - silent-peer `TimedOut`;
  - peer Close;
  - drop without Close;
  - oversized message;
  - an unframable outbound envelope (`InvalidInput`, nothing sent).
- `crates/hexput-tests/tests/config.rs` -- the flipped tests above, plus the bare-IP forms getting 7478 and the missing `tls_cert`/`tls_key` error naming the field.
- `crates/hexput-tests/tests/daemon.rs` -- add a WSS client helper (a current-thread tokio runtime driving tokio-tungstenite over tokio-rustls) beside `tls_serving`, and these tests:
  - the WSS round trip, identical to UDS;
  - UDS, TCP and WebSocket served by one Daemon, each answering;
  - the default port 7478, skipping when taken;
  - the plaintext probe;
  - the intermediary drop;
  - the WebSocket startup failures (identity, address in use, and the config without TLS).

  Delete the "not supported yet" row. Read the ephemeral port from the `address=` field of the "listening on WebSocket with TLS" line.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- make two edits:
  - **Stack:** tokio-tungstenite gets its feature note (`handshake` only), and add a `futures-util` 0.3.34 row.
  - **Dated amendment (2026-10-22, Story 5.2):**
    - a binary message is one envelope, with no prefix;
    - TLS is mandatory and Story 2.1's optional WebSocket TLS is withdrawn;
    - the default port is 7478;
    - the handshake and upgrade run in the connection task under one 10 s bound;
    - no Origin or subprotocol checks;
    - no edge change.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- update the `close_notify` item to say that Story 5.2 also sends no Close frame, and why.
- `AGENTS.md` -- Project Status: add Story 5.2 to the Epic 5 paragraph, update the "Next step", and update the test count.

**Acceptance Criteria:**
- Given a System Config enabling WebSocket with valid TLS, when a client opens a WSS connection via tokio-tungstenite and sends a binary message holding an envelope, then the Daemon answers through the same `Port` and `serve` with the same MessagePack envelope as UDS and TCP.
- Given `[transport.websocket] bind = "127.0.0.1"` with no port, when the Daemon starts, then it listens on 7478.
- Given any WebSocket connection, when it is established, then a TLS handshake completed first. A cleartext client is never upgraded, and a WebSocket section without `tls_cert`/`tls_key` is refused at startup.
- Given a WebSocket connection dropped without a Close frame, when the drop is detected, then the adapter reports a clean close. `serve` detaches the Connection through its one exit path, exactly as on every other transport, and the Daemon keeps serving others.
- Given the workspace, when `python3 scripts/check-crate-graph.py` runs, then it passes, with `hexput-transport` still depending on `hexput-port` alone.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/transport.rs` (a_binary_message_crosses_tls_and_the_reply_comes_back_as_one_binary_message, a_text_message_is_malformed_and_reading_continues, a_malformed_binary_message_is_not_fatal_and_the_next_one_still_arrives, pings_are_answered_by_the_transport_and_yield_nothing, a_cleartext_upgrade_is_never_answered_and_its_socket_is_closed, a_tls_client_that_sends_no_upgrade_gets_no_session, a_silent_websocket_peer_times_out_with_timed_out, a_silent_websocket_peer_never_stalls_the_next_accept, the_handshake_reports_the_peers_address, a_peer_close_frame_is_a_clean_close_and_the_reply_it_is_owed_is_delivered, a_websocket_dropped_without_a_close_frame_is_a_clean_close, an_oversized_message_is_fatal_and_the_half_stays_closed, an_envelope_too_large_to_frame_is_refused_without_writing), `crates/hexput-tests/tests/config.rs` (a_websocket_transport_must_give_tls_and_a_missing_pair_is_refused, a_websocket_section_without_its_tls_pair_is_rejected_with_the_field_and_its_position, every_field_of_a_present_transport_is_required, a_bare_ip_websocket_bind_listens_on_the_default_port_7478, the_example_file_parses_and_documents_every_field), and `crates/hexput-tests/tests/daemon.rs` (a_wss_client_gets_the_same_replies_as_a_uds_client, uds_tcp_and_websocket_are_served_by_one_daemon_each_answering, a_bare_ip_websocket_bind_listens_on_the_default_port_7478, a_plaintext_websocket_probe_is_never_upgraded_is_logged_at_debug_and_disturbs_no_wss_client, an_intermediary_drop_detaches_the_connection_and_the_daemon_keeps_serving, shutdown_stops_the_websocket_listener_too, every_websocket_startup_failure_exits_one_naming_the_file_and_leaves_nothing_behind, a_websocket_section_without_its_tls_pair_exits_one_naming_the_field, a_websocket_address_in_use_exits_one_and_unbinds_the_listeners_before_it). The `daemon.rs` "not supported yet" startup row was removed, as the spec's Tasks section requires. `crates/hexput-tests/Cargo.toml` gained `tokio-tungstenite` and `futures-util` as dev-dependencies; the `futures-util` workspace pin is not yet in the root `Cargo.toml`, so the implementer adds it. The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Implementation decisions (unattended):
  - Two pre-written tests were corrected. The WebSocket adapter's `an_envelope_too_large_to_frame_is_refused_without_writing` clashed with the TCP test of the same name in `transport.rs` and became `a_websocket_envelope_too_large_to_frame_is_refused_without_writing`, unchanged otherwise. `a_peer_close_frame_is_a_clean_close_and_the_reply_it_is_owed_is_delivered` read the client to end of stream after the close reply; that end only comes when the core drops the Port, and then without a TLS `close_notify` (deferred-work, Design Note 4), so it hung while the test held the halves and would see `UnexpectedEof` once dropped. It now asserts that the next client read (bounded at 10 s) is the close reply with no error, which is the matrix row exactly.
  - Added `a_text_message_is_answered_with_a_malformed_frame_error_and_reading_continues` in `daemon.rs` (and `Wss::send_text`) so the "Text message" matrix row is covered at the wire, as the nil-id `Error` envelope, not only at the adapter.
  - `TlsIdentity::server_config(self) -> Arc<ServerConfig>` is the crate-private accessor; `hexput-config`'s `bind` now takes the default port as a plain `u16`, since every listener has one.
  - The lock was resolved with `cargo metadata` (no build needed); `git diff Cargo.lock` only adds 20 packages, `rustls` stays 0.23.45, no `aws-lc`.
  - A full-suite run once failed `script::a_removed_handle_is_unknown_and_its_program_is_released_at_once` (moka maintenance timing under load; no file of this story touches it). It passed five isolated reruns and the next full run: 991 passing.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 27 findings — high 0, medium 5, low 14, false 8, maybe-false 0
- findings:
  - `[medium]` `[patch]` (group A: WebSocket replies owed when the peer stops) Replies still owed after a peer's Close are lost; the module doc claimed "a Backend that stops sending is still answered" — confirmed in tungstenite 0.30 (`ClosedByPeer` → `SendAfterClosing` on write). Fixed the `websocket` module doc (new "Replies still owed when the peer stops" section), added `a_reply_still_owed_after_the_peers_close_frame_cannot_be_sent`, recorded the limitation in deferred-work.md and AGENTS.md.
  - `[low]` `[patch]` `sprint-status.yaml` sets 5-2 to `ready-for-dev` — set to `review` at finalization.
  - `[low]` `[patch]` The `Closed(Some(_))` path has no test — added `a_text_message_that_is_not_utf8_fails_the_connection` (invalid UTF-8 text frame → `Closed(Some(_))`, then `Closed(None)`).
  - `[low]` `[patch]` Limit boundaries untested — added `a_message_of_exactly_the_limit_is_read_and_reading_continues` (a `MAX_FRAME_LEN` message reaches `decode`, non-fatal, next message arrives); the outbound check is a visible `body.len() > MAX_FRAME_LEN`, so no outbound boundary test was added.
  - `[low]` `[reject]` No Daemon-level oversized-message test — `serve`'s fatal-`Malformed` handling is transport-generic and covered by the connection tests; the adapter test pins the mapping. Unlikely to matter; the test would add a 16 MiB wire test for no new code path.
  - `[low]` `[reject]` The Closing state has no time limit — after the close reply flushes, tungstenite (server role) terminates and the next read returns `ConnectionClosed`; it only waits while the peer's receive buffer is full, the same stall as any non-reading peer (deferred bounded-write and pre-init/idle items). A timeout would add a guard for a rare case.
  - `[low]` `[patch]` Example-file test barely checks WebSocket — now asserts `bind == 0.0.0.0:7478` and the exact cert/key paths.
  - `[low]` `[reject]` Missing-TLS config case tested three times — redundancy costs a few milliseconds and harms no caller; consolidating is churn without a named defect.
  - `[low]` `[reject]` The WebSocket accept branch copies the TCP one — real duplication, but the fix is a generic refactor (more than a direct correction); Story 5.3 can extract the helper when a third handshake type arrives.
  - `[false]` `[reject]` WebSocket certificate errors don't name the transport — the message names the offending file path, which is what the operator fixes; when both sections name the same file, the same file is broken for both. The spec's matrix row mandates `error: <path>: …` as for TCP.
  - `[false]` `[reject]` A known flaky test was not recorded — deferred-work.md already records `a_removed_handle_is_unknown_and_its_program_is_released_at_once` (from Story 4.5's review, severity medium unverified).
  - `[low]` `[reject]` Port races in the address-in-use and shutdown tests — the same pattern as Story 5.1's TCP tests; a collision needs another parallel test to be handed exactly that freed ephemeral port in a millisecond window. Restructuring adds complexity for a rare flake.
  - `[low]` `[reject]` Spec build steps vs. what was done (`cargo metadata` instead of a release build to resolve the lock) — the fix is editing this build's spec; the lock was verified (only additions, rustls 0.23.45, no aws-lc) and every later command ran `--release --locked`.
  - `[low]` `[patch]` A doc line past 100 columns in `hexput-config/src/lib.rs` — re-wrapped (and `websocket.rs` line 11, also 101 columns).
  - `[medium]` `[patch]` (group A) A clean TCP/TLS end (`close_notify` then EOF) sets tungstenite's state to `Terminated`, so later writes fail `AlreadyClosed` — confirmed (`read_message_frame` replaces the state). Documented in the module doc; deferred-work records how it could be fixed (a stream wrapper). An end without `close_notify` (`UnexpectedEof`) leaves the state `Active` and the writer usable.
  - `[medium]` `[patch]` (group A) Peer Close while requests execute drops every pending reply — same root cause and fix as above.
  - `[false]` `[reject]` Invalid UTF-8 text maps to `Closed(Some)` rather than a non-fatal `malformed_frame` — the spec's "Any other error → `Closed(Some(io::Error))`" row covers it, and RFC 6455 §8.1 requires failing the connection on invalid UTF-8 text; now pinned by a test.
  - `[false]` `[reject]` A peer protocol violation maps to `Closed(Some)` with no protocol error sent — per the spec's "Any other error" row, and RFC 6455 requires failing the WebSocket connection; a peer that violates framing cannot be trusted to parse a reply.
  - `[low]` `[reject]` The Closing loop can wait forever on a peer that stops reading — same evidence as the blind-hunter's Closing finding above.
  - `[medium]` `[patch]` (group A) No test covers a reply owed after the peer's Close — added `a_reply_still_owed_after_the_peers_close_frame_cannot_be_sent`.
  - `[medium]` `[patch]` Oversized-message test passes with the adapter's message limit removed (verification-gap) — added `a_fragmented_message_over_the_limit_is_fatal` (four `MAX_FRAME_LEN/4` frames plus one byte); confirmed it fails with `.max_message_size` removed and passes with it.
  - `[false]` `[reject]` The diff implements more than "Halt after planning" — this bmad-build-auto run includes step-03 (implementation) before this review; the relayed user request only constrains builds to `--release`, which was honoured.
  - `[low]` `[patch]` The status markers disagree (`ready-for-dev` / `in-review` / implemented) — sprint-status set to `review`, spec to `done` at finalization.
  - `[false]` `[reject]` AC3's "remote" is only tested over loopback — the adapter never branches on the peer address, so loopback runs exactly the path a remote peer takes; TLS is enforced for every connection.
  - `[false]` `[reject]` AC4's "the Session survives" is not observed — the spec's Design Note 3 settles it: the drop takes the same detach path as every transport, a Session with another attached Connection survives by that path, and survival past the last Connection is Story 5.8's TTL.
  - `[low]` `[reject]` The 7478 daemon test skips when the port is taken — the config test still pins 7478 and the same pattern was accepted for 5.1's 7476; CI normally has the port free.
  - `[false]` `[reject]` The diff goes beyond the ACs (required TLS, no Origin/subprotocol, no Close frame) — FR-9 requires TLS on every non-local transport; the rest are recorded design decisions (Design Notes 2, 4, 5) and the Spine amendment, not defects.

## Design Notes

These decisions were taken unattended, as the most defensible readings:

1. **One binary message holds one envelope.** WebSocket frames messages itself, so a length prefix would be redundant and alien to browser clients. Story 2.2 already recorded this ("WebSocket skips the prefix and feeds each message to `decode` directly"). `MAX_FRAME_LEN` bounds a message, as it bounds a frame.
2. **TLS is required, even though Story 2.1 made it optional.** FR-9 and the epic say that TLS is mandatory for every non-local transport, and the AC says that TLS is "enforced exactly as for TCP". The "proxies in between" story is about HTTP-friendliness, not about terminating TLS elsewhere. A loopback exemption would be a second, transport-specific policy.
3. **"The Session survives" is read as "the same detach path as any transport".**
   - Today, when the last Connection detaches, the Session is torn down at once on every transport.
   - Survival past the last Connection is Story 5.8's TTL.
   - Here, the adapter reports the drop as a clean close, and `serve`'s single exit path detaches. Nothing WebSocket-specific touches the Session (AD-1, AD-2).
4. **No Close frame from the Daemon.** Sending one needs an `Outbound` close hook, which is a `hexput-port` trait change with a bounded-write question on stalled peers. 5.1 rejected the same hook for `close_notify`.
   - A peer's own Close is still answered, because tungstenite queues the reply and `recv` flushes it.
   - On Daemon shutdown or a fatal frame, clients see an abnormal closure (1006) after their last reply.
   - Every reply is a whole message, so nothing is lost. The deferred item records this.
5. **No Origin check.** WebSocket's Origin defence protects cookie-authenticated endpoints. A Hexput Backend proves itself in-band (Init/reconnect), so a hostile page gains nothing it could not get with any TCP client.

Golden shape of `finish`:

```rust
let accepted = async {
    let tls = TlsAcceptor::from(self.config).accept(self.stream).await?;
    tokio_tungstenite::accept_async_with_config(tls, Some(ws_config())).await.map_err(to_io)
};
match tokio::time::timeout(self.timeout, accepted).await { Ok(r) => r.map(WebSocketPort::new), Err(_) => Err(timed_out(self.timeout)) }
```

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes unchanged
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass, including the new transport, config and daemon tests
- `grep -c 'name = "aws-lc' Cargo.lock` -- expected: 0

## Auto Run Result

- **Summary:** Story 5.2 serves WebSocket with mandatory TLS: `hexput_transport::websocket` (TLS then upgrade in the connection task, one binary message per envelope, `MAX_FRAME_LEN` bounds), required `tls_cert`/`tls_key` and a default port 7478 in System Config, and a third accept branch in the Daemon. The review pass documented and pinned that a WebSocket ends both directions together (replies owed after a peer's Close or a clean TLS end are lost), and added boundary and fragmentation tests.
- **Files changed:**
  - `Cargo.toml`, `Cargo.lock` — tokio-tungstenite (`handshake` only) and futures-util pins.
  - `crates/hexput-transport/{Cargo.toml,src/lib.rs,src/tcp_tls.rs,src/websocket.rs}` — the WebSocket adapter and the crate-private `TlsIdentity::server_config`.
  - `crates/hexput-config/{src/lib.rs,src/file.rs,config.example.toml}` — required TLS, `DEFAULT_WEBSOCKET_PORT`, docs.
  - `crates/hexput-daemon/src/lib.rs` — startup order, WebSocket bind/log/accept branch, close.
  - `crates/hexput-tests/{Cargo.toml,tests/transport.rs,tests/config.rs,tests/daemon.rs}` — adapter, config and daemon tests.
  - `ARCHITECTURE-SPINE.md`, `deferred-work.md`, `sprint-status.yaml`, `AGENTS.md` — amendment, deferrals, status.
- **Review findings:** 27 — patches applied for 12 rows (5 medium, 7 low: group A reply-loss doc + test, fragmented-limit test, exact-limit test, invalid-UTF-8 test, example-file assertions, two doc re-wraps, sprint status), 0 deferred to frontmatter (the reply-loss limitation is also recorded in deferred-work.md as a follow-up), 15 rejected with the reasons in the Review Triage Log (8 false, 7 low).
- **Follow-up review recommendation:** `false`. Patched by verdict: high 0, medium 2 entries (group A, the fragmented-limit gap), low 6 entries. Although two medium entries were patched, no unverified risk remains to name: group A's behaviour is pinned by a test and documented, and the fragmented-limit test was mutation-checked (fails without `max_message_size`).
- **Verification:** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK (26 edges, 4 restricted names); `cargo build --workspace --all-targets --locked --release` success; `cargo test --workspace --locked --release` 995 passed, 0 failed; `grep -c 'name = "aws-lc' Cargo.lock` = 0.
- **Residual risks:** a Backend that closes (Close frame or clean TLS end) while expecting replies loses them (documented; deferred-work); the Closing state waits on a peer whose receive buffer is full; the 7478 test skips when the port is taken.
