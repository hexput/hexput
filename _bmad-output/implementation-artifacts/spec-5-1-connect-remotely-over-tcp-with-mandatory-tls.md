---
title: 'Story 5.1: Connect remotely over TCP with mandatory TLS'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '75124cf974b36e6e7f9aeeb7ad6be8722310ac9f'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred:
  - summary: >-
      TCP+TLS is still refused on non-Unix platforms although the tcp_tls adapter is platform-independent.
    evidence: |-
      hexput-daemon imports both adapters under cfg(unix) and its cfg(not(unix)) serve refuses to start; the intent-contract's Never list keeps that path for Story 5.3. Recorded in deferred-work.md.
    location: >-
      crates/hexput-daemon/src/lib.rs (cfg(not(unix)) serve)
    severity: medium
  - summary: >-
      TlsIdentity::load does not check the leaf's validity window or log the served identity; several keys, key-file permissions and encrypted keys are not diagnosed.
    evidence: |-
      with_single_cert checks key consistency only; an expired leaf starts a Daemon whose every handshake fails at debug. Needs an X.509 parser (a Stack decision). Recorded in deferred-work.md.
    location: >-
      crates/hexput-transport/src/tcp_tls.rs (TlsIdentity::load)
    severity: medium
  - summary: >-
      Handshake failures are debug-only, so a systematic misconfiguration is invisible at the default info level.
    evidence: |-
      Chosen by the intent-contract (debug logging) and design note 4; an operator signal (rate-limited warn or a counter) belongs to Epic 7's operator logging. Recorded in deferred-work.md.
    location: >-
      crates/hexput-daemon/src/lib.rs (TCP accept branch)
    severity: medium
  - summary: >-
      A remote peer that completes the handshake but never sends Init holds its connection task and socket forever.
    evidence: |-
      Pre-existing: hexput_connection::serve has no pre-init or idle deadline on any transport; TCP makes the peer remote. Recorded in deferred-work.md beside the connection cap.
    location: >-
      crates/hexput-connection/src/lib.rs (serve)
    severity: medium
---

<intent-contract>

## Intent

**Problem:** The Daemon serves only a Unix Domain Socket. A System Config with `[transport.tcp]` is refused at startup ("not supported yet"), so a Backend on another machine cannot reach the Daemon. FR-9 requires TCP, and requires TLS on it without exception. FR-29 fixes 7476 as TCP's default port.

**Approach:**
- Add a TCP+TLS adapter, `hexput-transport::tcp_tls`, implementing the existing `Port` trait. It uses rustls through `tokio-rustls` with the `ring` provider.
- The adapter shares one length-prefixed framing implementation with the UDS adapter.
- The Daemon loads the certificate and key before it binds anything, then serves UDS and TCP listeners together into the same `hexput_connection::serve`.
- `[transport.tcp] bind` may omit the port, which then defaults to 7476.

## Boundaries & Constraints

**Always:**
- **TLS on every TCP connection.**
  - Every accepted TCP connection completes a rustls server handshake before a single byte reaches the framing.
  - Plaintext is never accepted, at any point.
  - A failed handshake (plaintext, a wrong protocol, a client that does not trust the certificate) or a timed-out one closes only that socket. It is logged at `debug` with the peer address (`"TLS handshake failed"`). The accept loop keeps serving.
- **Handshakes off the accept loop.**
  - The handshake runs inside the per-connection task the Daemon spawns, never in the accept loop, so a slow peer never stalls other accepts.
  - It is bounded by `hexput_transport::tcp_tls::HANDSHAKE_TIMEOUT` (10 s). A builder method overrides it for tests.
- **Accepted sockets** get `TCP_NODELAY`.
- **TLS setup.**
  - Built with `ServerConfig::builder_with_provider(ring provider)` and rustls's safe default protocol versions (TLS 1.2 + 1.3).
  - No client authentication. Backend identity is the init/reconnect protocol (Story 5.5), not mTLS.
  - No ALPN.
- **Loading the identity.**
  - `TlsIdentity::load(cert, key)` reads PEM through `rustls::pki_types::pem::PemObject`. Do not add `rustls-pemfile`.
  - The cert file must contain at least one `CERTIFICATE`. The chain is kept in file order.
  - The key file must contain a private key (PKCS#8, PKCS#1 or SEC1).
  - The key must match the leaf. Rely on rustls's `with_single_cert` consistency check; if this rustls version lacks one, compare the public keys explicitly.
  - Every failure is a `TlsError` whose `Display` starts with the offending file's path and states the problem: not found, unreadable (with the OS error), contains no certificate, contains no private key, malformed PEM, key does not match the certificate, or a key rustls cannot use.
- **System Config (`hexput-config`).**
  - `transport.tcp.bind` accepts either `IP:port` or a bare IP: `0.0.0.0`, `::`, `::1` or `[::1]`.
  - A bare IP gets `pub const DEFAULT_TCP_PORT: u16 = 7476`. Port `0` (an ephemeral port) stays valid.
  - An unparsable bind is still an `InvalidValue` naming the field, and its message now says the port is optional.
  - `[transport.websocket] bind` still requires a port. Its default, 7478, is Story 5.2's.
- **Daemon startup order.**
  1. Load the TLS identity.
  2. Bind the UDS (if configured).
  3. Bind the TCP listener.
- **Startup failures.**
  - Any failure exits `1` with one stderr line, `error: <message>`, that names the cert/key path or the bind address.
  - A failure after the UDS is bound closes it again, so no socket file is left behind.
  - A TCP bind failure reads `error: <addr>: cannot listen for TCP: <os error>`.
- **Logging and shutdown.**
  - Once bound, the Daemon logs `info` `"listening on TCP with TLS"` with field `address` = the listener's actual local address.
  - On shutdown it stops accepting on every listener.
- **Shared framing.**
  - UDS and TCP+TLS share one framing implementation: a private generic module in `hexput-transport`, generic over `AsyncRead`/`AsyncWrite` halves.
  - UDS keeps its owned `into_split` halves; TLS uses `tokio::io::split`.
  - `UdsPort`/`UdsInbound`/`UdsOutbound` keep their public names (type aliases are fine) and their exact behaviour. `tests/transport.rs` passes unchanged.
  - `Inbound::recv` stays cancel-safe.
- **Dependency pins** (in `[workspace.dependencies]`):
  - `rustls = { version = "0.23.45", default-features = false, features = ["ring", "std", "tls12"] }`, resolved at exactly 0.23.45 (`cargo update -p rustls --precise 0.23.45`). 0.23.46 is 8 days old.
  - `tokio-rustls = { version = "0.26.6", default-features = false, features = ["ring", "tls12"] }`. 0.26.6 was released 2026-09-28.
  - `aws-lc-rs` must not appear in `Cargo.lock`.
  - `hexput-transport` gains `rustls` and `tokio-rustls`. `hexput-tests` gains both as dev-dependencies, for its TLS client.
  - No workspace crate edge changes, and `check-crate-graph.py` stays as is.
- **Disk and build:** this machine is short on disk. Every cargo command runs with `--release`. Update `Cargo.lock` once with `cargo update -p rustls --precise 0.23.45` (after the manifests change), then use `--locked`.

**Never:**
- An option to disable TLS on TCP.
- Any session, credential or execution logic in `hexput-transport`.
- Any crate other than `hexput-daemon` naming the TCP adapter.
- WebSocket serving or its 7478 default (Story 5.2). `[transport.websocket]` stays refused at startup.
- Named Pipe or any non-Unix serving (Story 5.3). The `#[cfg(not(unix))]` Daemon path stays as it is, though `tcp_tls` itself is platform-independent code.
- Certificate hot-reload: restarting the Daemon is the reload, per the Spine.
- A connection cap, or a bound on decoded frame size. Both are already recorded in `deferred-work.md`.
- Adding `rcgen` or `rustls-pemfile`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| TLS round trip | Daemon with `[transport.tcp]` and fixture cert/key; client trusting the fixture CA sends `Init`, then `ExecutionStart {source:"return 1 + 2;", variables:{}}` | `Result {client_id}`, then `Result {value: 3}`, identical to UDS | — |
| Default port | `bind = "127.0.0.1"` | listens on `127.0.0.1:7476`; logged `address` ends `:7476`; a TLS client connects there | — |
| Plaintext probe | raw TCP client writes a framed `Init` | no frame is ever answered; the socket is closed; a TLS client on the same Daemon still works afterwards | `debug` "TLS handshake failed" |
| Silent peer | client connects and sends nothing | the adapter's handshake ends with `TimedOut` after the configured timeout | socket dropped |
| Missing / unreadable cert or key | path absent or a directory | exit 1: `error: <path>: …` naming the file and the problem; no socket file left | — |
| Cert without certificate / key without key / malformed PEM | fixture text written by the test | exit 1 naming that file and the problem | — |
| Key not matching certificate | `other.key` fixture | exit 1 naming the key file: does not match the certificate | — |
| Address in use | a test holds a `TcpListener` on the address | exit 1: `error: <addr>: cannot listen for TCP: …`; UDS socket file removed | — |
| WebSocket configured | `[transport.websocket]` | still exit 1, "not supported yet" | — |

</intent-contract>

## Code Map

- `Cargo.toml` -- `[workspace.dependencies]`: `rustls` exists with no features (no consumer yet); add the features and `tokio-rustls`. Each pin carries its Story comment.
- `crates/hexput-transport/src/lib.rs` -- module doc says TCP+TLS arrives with Epic 5; add `pub mod tcp_tls;` (no cfg) and the private shared framing module.
- `crates/hexput-transport/src/uds.rs` -- `UdsPort::split`, `UdsInbound::recv` (decoder loop: oversized prefix → fatal and closed; `Ok(0)` → `Closed(None)`; `Interrupted` retried) and `UdsOutbound::send` (encode + `encode_frame`, `InvalidInput` when unframable). Move these into the generic module and keep the behaviour exactly.
- `crates/hexput-transport/Cargo.toml` -- add `rustls` and `tokio-rustls`. The comment about `hexput-port` being the only workspace edge stays true.
- `crates/hexput-port/src/port.rs:21-53` -- the `Port`/`Inbound`/`Outbound` contract (`Send + 'static`, `Send` futures). Read-only.
- `crates/hexput-config/src/file.rs` -- `RawTcp`; `Validator::bind` (shared with websocket) → add a default-port parameter (`Some(DEFAULT_TCP_PORT)` for tcp, `None` for websocket); `transports()`.
- `crates/hexput-config/src/lib.rs` -- module-doc TOML sample (`bind = "0.0.0.0:7400"`); `TcpTransport`. Add `DEFAULT_TCP_PORT` beside `DEFAULT_SESSION_TTL`.
- `crates/hexput-config/config.example.toml` -- the TCP section says "not served yet" and uses port 7400. Document 7476 as the default, the optional port, and that TLS is mandatory. Leave the websocket note.
- `crates/hexput-daemon/src/lib.rs` -- `serve` (unix), `listen` (refuses tcp/websocket), the module doc "TCP+TLS and WebSocket have no adapter until Epic 5", and the accept loop's `select!` over one `listener.accept()`. Turn it into a set of listeners: a UDS option and a TCP option, where an absent listener is never ready. TCP accepts spawn `async { handshake.finish().await → serve(port, …) }` into the same `JoinSet`. `registry`/AST Cache wiring is unchanged.
- `crates/hexput-tests/tests/transport.rs` -- UDS adapter tests: the pattern for the new TLS adapter tests (`pair`, `framed`, `request`).
- `crates/hexput-tests/tests/daemon.rs:476-590` -- `Serving` harness (thread + `run_with` + oneshot stop) and the `wire` module over a std `UnixStream`. Lines 1016-1100: the startup-failure table that asserts `[transport.tcp] not supported yet`; it must now expect TLS/bind failures instead, and keep the websocket rows.
- `crates/hexput-tests/tests/config.rs:80-115,380-400` -- a full-file parse asserting `tcp.bind` and missing-field cases. Extend it with the bind forms.
- `scripts/check-crate-graph.py:81` -- `"hexput-transport": {"hexput-port"}`. Unchanged; it must still pass.
- `_bmad-output/implementation-artifacts/deferred-work.md:15` -- the open item "rustls's crypto provider". Resolve it here.

## Tasks & Acceptance

**Execution:**
- `Cargo.toml`, `crates/hexput-transport/Cargo.toml`, `crates/hexput-tests/Cargo.toml` -- pin `rustls` and `tokio-rustls` as in Boundaries and wire them in. Then run `cargo update -p rustls --precise 0.23.45` and confirm that `Cargo.lock` has no `aws-lc` package.
- `crates/hexput-transport/src/framed.rs` (new, private) -- the generic `FramedInbound<R>`/`FramedOutbound<W>` extracted from `uds.rs`, then `uds.rs` rewritten on top of it -- so one framing serves every byte-stream transport.
- `crates/hexput-transport/src/tcp_tls.rs` (new) -- define:
  - `TlsIdentity::load(cert: &Path, key: &Path) -> Result<TlsIdentity, TlsError>`, building the `Arc<ServerConfig>`;
  - `TcpTlsListener::bind(addr: SocketAddr, identity: TlsIdentity) -> io::Result<Self>` (async, tokio `TcpListener`);
  - `local_addr()`, `with_handshake_timeout(Duration)`;
  - `accept() -> io::Result<TlsHandshake>`, cancel-safe, setting nodelay;
  - `TlsHandshake::peer_addr()`, and `TlsHandshake::finish(self) -> io::Result<TcpTlsPort>`, timed out with `io::ErrorKind::TimedOut`;
  - `TcpTlsPort: Port`, with `TcpTlsInbound`/`TcpTlsOutbound`;
  - `HANDSHAKE_TIMEOUT`.
  
  Module docs state the TLS policy. This is FR-9 and AD-1.
- `crates/hexput-config/src/{lib.rs,file.rs}`, `config.example.toml` -- add `DEFAULT_TCP_PORT` and the optional TCP port, and update the docs and the example, per Boundaries.
- `crates/hexput-daemon/src/lib.rs` -- implement the startup order, the error lines, the `"listening on TCP with TLS"` log, the multi-listener accept loop, and handshakes in spawned tasks. Update the module-doc contract (serving section, exit `1` causes). Websocket is still refused, with the message changed to say that `[transport.uds]` and `[transport.tcp]` are served.
- `crates/hexput-tests/fixtures/tls/` (new) -- check in PEM fixtures generated once with the system `openssl`:
  - `ca.pem`: a self-signed CA, ECDSA P-256, `basicConstraints=critical,CA:true`;
  - `server.pem`: the leaf signed by that CA. It must not be a CA, with `subjectAltName=DNS:localhost,IP:127.0.0.1` and `extendedKeyUsage=serverAuth`;
  - `server.key`: the leaf key, PKCS#8;
  - `other.key`: an unrelated PKCS#8 P-256 key.
  
  All certificates are valid for 36500 days. Put the exact openssl commands in a doc comment of the test helper that loads the fixtures. Tests locate the fixtures through `env!("CARGO_MANIFEST_DIR")`.
- `crates/hexput-tests/tests/transport.rs` -- add these tests:
  - every `TlsIdentity::load` failure kind, each naming its path;
  - a framed round trip over TLS (client: `tokio_rustls::TlsConnector` with a root store holding `ca.pem`, server name `localhost`);
  - a plaintext client, where `finish` errs and the client reads no frame;
  - a silent peer, which hits a short `with_handshake_timeout` and returns `TimedOut`;
  - a malformed frame over TLS, which is reported like on UDS.
- `crates/hexput-tests/tests/config.rs` -- cover the bind forms (`0.0.0.0`, `::`, `[::1]`, `::1`, an explicit port, `:0`, and garbage) and that websocket still needs a port.
- `crates/hexput-tests/tests/daemon.rs` -- add a TLS client helper next to `Serving` (a tokio current-thread runtime or a blocking wrapper, whichever is simpler), and tests for:
  - the TLS round trip and the plaintext probe, from the I/O matrix;
  - UDS and TCP served together by one Daemon, each answering;
  - the default port 7476 on `127.0.0.1`;
  - every startup-failure row, replacing the `[transport.tcp] not supported yet` rows.
  
  Read the ephemeral port from the `address=` field of the `"listening on TCP with TLS"` log line (config `bind = "127.0.0.1:0"`).
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- make two edits:
  - **Stack table:** add `tokio-rustls` 0.26.6 and note that rustls uses the `ring` provider.
  - **Dated amendment (2026-10-22, Story 5.1):** default port 7476 when `bind` omits a port; TLS mandatory, handshake in the per-connection task, bounded at 10 s; no client auth and no ALPN; PEM through `pki-types`; and no edge change.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- mark the rustls crypto-provider item resolved by this spec.
- `AGENTS.md` -- Project Status: add an Epic 5 paragraph for Story 5.1, update "Next step", and update the test count.

**Acceptance Criteria:**
- Given a System Config with `[transport.tcp]` naming a bind address and valid cert/key, when the Daemon starts, then it logs `"listening on TCP with TLS"` with the bound address, and a TLS client completes `Init` and a Direct Execution with replies identical to UDS.
- Given `[transport.tcp] bind = "127.0.0.1"` with no port, when the Daemon starts, then it listens on port 7476.
- Given a plaintext TCP client, when it sends a framed request, then it never receives a Hexput frame, its socket is closed, and other connections are unaffected.
- Given a missing, unreadable or malformed certificate or key, or a key that does not match the certificate, when the Daemon starts, then it exits 1 with an error naming the file and the problem, and nothing is left listening or on disk.
- Given the workspace, when `python3 scripts/check-crate-graph.py` runs, then it passes with `hexput-transport` still depending on `hexput-port` alone, and no crate other than `hexput-daemon` (and the dev-only `hexput-tests`) references `tcp_tls`.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/transport.rs` (`every_identity_failure_names_its_file_and_the_problem`, `a_key_rustls_cannot_use_names_its_file`, `a_complete_identity_loads_and_a_chain_is_accepted_in_file_order`, `a_framed_message_crosses_tls_and_a_reply_comes_back_over_it`, `a_malformed_frame_over_tls_is_reported_and_the_next_one_still_arrives`, `a_plaintext_client_is_never_answered_and_its_socket_is_closed`, `a_client_that_does_not_trust_the_certificate_gets_no_session`, `a_silent_peer_times_out_with_timed_out_and_nothing_is_read`, `a_silent_peer_never_stalls_the_next_accept`, `the_handshake_timeout_is_ten_seconds`), `crates/hexput-tests/tests/config.rs` (`the_default_tcp_port_is_7476`, `a_bare_ip_bind_gets_the_default_tcp_port`, `an_explicit_tcp_port_is_kept_and_port_zero_stays_valid`, `an_unparsable_tcp_bind_names_the_field_and_says_the_port_is_optional`, `a_bare_ip_websocket_bind_still_needs_a_port`), and `crates/hexput-tests/tests/daemon.rs` (`a_tls_client_gets_the_same_replies_as_a_uds_client`, `a_bare_ip_bind_listens_on_the_default_port_7476`, `a_plaintext_probe_is_never_answered_is_logged_at_debug_and_disturbs_no_tls_client`, `shutdown_stops_every_listener_and_removes_the_socket`, `every_tls_startup_failure_exits_one_naming_the_file_and_leaves_nothing_behind`, `a_tcp_address_in_use_exits_one_and_removes_the_socket_bound_before_it`; the startup-failure table lost its `[transport.tcp]` row). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 35 findings — high 0, medium 14, low 15, false 6, maybe-false 0
- findings:
  - `[low]` `reject` blind: the server never sends a TLS `close_notify` — real (`Outbound` has no shutdown hook), but every frame is self-delimiting so no reply is lost; only a client reading past the Daemon's close (shutdown or a fatal frame) sees a truncation error, and the fix is a `hexput-port` trait change. Noted in deferred-work.md for Story 5.2, which needs the same hook for its close frame.
  - `[medium]` `patch` blind: a TLS peer's bare TCP FIN reads as a lost stream, unlike UDS — confirmed in rustls 0.23.45 (`UnexpectedEof` without `close_notify`). Fixed: `FramedInbound::recv` maps `UnexpectedEof` to `Closed(None)`; tests `a_tls_client_closing_with_close_notify_is_a_clean_close`, `a_tls_client_ending_tcp_without_close_notify_is_a_clean_close_too` (fails without the fix) and the Daemon test `a_tls_client_that_stops_sending_still_gets_its_replies`.
  - `[low]` `reject` blind: the peer address never reaches the `connection` span — true, but FR-12 traces requests to the Client ID, which the span carries; the peer is logged on handshake failure. Carrying it needs a new `serve` parameter (public surface).
  - `[low]` `patch` blind: an accept error's inline backoff couples the listeners and the warn has no transport field — the sleep is 100 ms per failing accept and shutdown is still seen on the next turn (EMFILE starves both listeners anyway), so only the log is fixed: each "cannot accept a connection" now carries `transport = "uds"|"tcp"`.
  - `[medium]` `defer` blind: TCP+TLS refused on non-Unix — the intent-contract's Never list keeps the `cfg(not(unix))` path for Story 5.3; recorded in deferred-work.md and the spec's `deferred`.
  - `[medium]` `patch` blind: a Daemon test binds the fixed port 7476 and fails when anything holds it — fixed: the test probes `127.0.0.1:7476` first and skips with a note when it is taken. The sub-point (an ephemeral port re-bound by a parallel test between shutdown and the connect assertion) is negligible: the kernel does not hand a just-freed ephemeral port to the next bind.
  - `[low]` `patch` blind: the bracketed-IPv4 rule is untested and the WebSocket message hard-codes 7400 — added `[127.0.0.1]`/`[0.0.0.0]` (refused) and `[::]` (→ `[::]:7476`) to the config tests; the WebSocket example in the message now says 7478.
  - `[low]` `patch` blind: `config.example.toml` WebSocket example uses 7401 — now `0.0.0.0:7478` with a note that 7478 is its fixed port (also in `hexput-config`'s module doc).
  - `[low]` `reject` blind: a protocol-version failure is reported as a certificate error — `with_safe_default_protocol_versions` cannot fail with the `ring` provider, which supports both versions; the message still names the cert file, which is where every chain problem lies.
  - `[medium]` `defer` blind: `TlsIdentity::load` lacks expiry, multiple-key, permission and identity-logging checks — beyond the story's acceptance criteria and needs an X.509 parser (a Stack decision); recorded in deferred-work.md and `deferred`.
  - `[medium]` `defer` blind: handshake failures are debug-only, so a systematic one is invisible — the intent-contract fixes `debug` (design note 4); an operator signal belongs to Epic 7; recorded in deferred-work.md and `deferred`.
  - `[low]` `patch` blind: bookkeeping is stale (sprint-status `ready-for-dev`, epic-5 `backlog`) — finalization moves the story to `review` and epic-5 to `in-progress`. The empty triage log and the pre-review follow-up flag are not defects: they are filled at this step.
  - `[low]` `reject` blind: `FramedInbound`/`FramedOutbound` are reachable only through aliases — the aliases carry their own docs and every caller names the alias; re-exporting adds public surface for no named caller.
  - `[medium]` `patch` blind: TLS write behaviour (flush, frames over one record, back-to-back frames) untested — added `frames_larger_than_a_tls_record_and_back_to_back_cross_tls_whole` (a 100 KiB frame plus two more, both directions, each read with a timeout so a missing flush fails).
  - `[medium]` `patch` edge: TLS EOF without `close_notify` → `Closed(Some(UnexpectedEof))` — same root cause as the blind finding above; fixed by the same patch.
  - `[low]` `reject` edge: outbound never sends `close_notify` — same as the blind finding; rejected on the same grounds.
  - `[low]` `reject` edge: no cap on concurrent handshakes — the intent-contract's Never list excludes a connection cap (already in deferred-work.md); each stalled handshake is bounded at 10 s.
  - `[medium]` `defer` edge: a peer that never sends Init holds the connection forever — pre-existing (`serve` has no pre-init deadline on any transport); recorded in deferred-work.md and `deferred`.
  - `[low]` `patch` edge: TCP accept errors sleep inside the select arm — same root as the blind backoff finding; the transport field was added, the 100 ms backoff kept.
  - `[medium]` `defer` edge: non-Unix with only `[transport.tcp]` cannot start — same as the blind finding; deferred to Story 5.3.
  - `[low]` `reject` edge: an encrypted PKCS#8 key reads as "contains no private key" — the message already lists the expected formats; a dedicated variant adds public surface. Noted with the identity checks in deferred-work.md.
  - `[medium]` `defer` edge: an expired or not-yet-valid leaf loads silently — same as the blind identity-checks finding.
  - `[low]` `reject` edge: a bare IPv6 with a scope id (`fe80::1%2`) needs an explicit port — rare (link-local listeners), and `[fe80::1%2]:7476` works; an extra parse branch is not worth it.
  - `[medium]` `patch` edge (claim): TCP abandons executions UDS would answer, contradicting "byte for byte alike" — same root cause as the EOF finding; fixed by the same patch.
  - `[medium]` `patch` verification-gap: no Daemon test shows the handshake stays off the accept loop — added `a_silent_tcp_peer_stalls_no_accept_on_either_transport` (a silent peer stays connected while a TLS and a UDS client each complete Init and a run within 5 s; awaiting the handshake in the loop would cost 10 s).
  - `[medium]` `patch` verification-gap: a TLS peer's clean close takes the lost-stream path, untested — same root cause as the EOF finding; covered by its three tests.
  - `[low]` `reject` verification-gap: removing `TCP_NODELAY` would go unnoticed — latency only, and a test needs a stream accessor on `TlsHandshake` (new public surface).
  - `[medium]` `patch` verification-gap (other): `a_bare_ip_bind_listens_on_the_default_port_7476` binds the fixed port — same as the blind finding; the test skips when the port is taken.
  - `[low]` `patch` verification-gap (other): the bracketed-IPv4 rule is unexercised — same as the blind finding; covered.
  - `[false]` `reject` intent: the diff implements more than planning ("halt after planning") — that directive bound the planning stage of this run; the orchestrator dispatched implementation, review and finalization for this spec as separate stages.
  - `[false]` `reject` intent: the artifacts disagree about state — they were mid-run snapshots; finalization sets spec `done`, sprint-status `review` and AGENTS.md "awaiting human review", which agree.
  - `[false]` `reject` intent: reading B2 (`[transport.tcp]` without `bind`) is untested — design note 1 settles reading B1 and the spec's acceptance criterion names `bind = "127.0.0.1"`; B2 is not the specified behaviour.
  - `[false]` `reject` intent: no restricted-name assertion pins that only `hexput-daemon` names `tcp_tls` — `check-crate-graph.py`'s SOLE_DEPENDENTS already allows only `hexput-daemon` to depend on `hexput-transport`, so no other production crate can name the module.
  - `[false]` `reject` intent: plaintext refusal checks no explicit TLS alert — the acceptance criterion asks that no Hexput frame is received and the socket closes, which the tests assert; an alert is optional TLS behaviour.
  - `[false]` `reject` intent: the release-build constraint shows only in the spec text — every cargo command in this run used `--release` (see Verification); the constraint is per-machine, so AGENTS.md's CI commands stay as they are.

## Design Notes

These decisions were taken unattended, as the most defensible readings:

1. **"Enabling TCP without a port" means `bind` holds an IP without a port.** The field is not made optional. Which interface a network listener exposes is a security choice the operator must state, while the port has a fixed, documented default.
2. **The `ring` provider over `aws-lc-rs`.** It is a smaller build (aws-lc needs cmake and a large C build, and this machine is short on disk) and has no FIPS ambitions. This also closes the open deferred item.
3. **The handshake belongs to the connection, not to the accept.** `accept()` returns quickly with a `TlsHandshake`. The Daemon finishes the handshake inside the spawned task, so the accept loop stays non-blocking (AD-6 in spirit) and a stalled client costs one task for at most 10 s.
4. **Failed handshakes are logged at `debug`.** Internet-facing listeners see constant probes, and logging each one at `warn` would drown the log. Epic 7 owns operator-facing denial and violation logging.
5. **No mTLS.** Story 5.5's reconnect secret is the Backend's credential (OQ-2). Adding client certificates would create a second, transport-specific authentication path, which AD-2 forbids.

Golden shape of the Daemon's TCP accept branch:

```rust
accepted = tcp.accept() => match accepted {
    Ok(handshake) => { let (s, c) = (Arc::clone(&sessions), Arc::clone(&cache));
        connections.spawn(async move { match handshake.finish().await {
            Ok(port) => hexput_connection::serve(port, s, c).await,
            Err(error) => tracing::debug!(%error, "TLS handshake failed"),
        }}); }
    Err(error) => { tracing::warn!(%error, "cannot accept a connection"); sleep(ACCEPT_BACKOFF).await; }
},
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

**Summary.** Story 5.1 adds TCP with mandatory TLS. `hexput-transport::tcp_tls` (rustls 0.23.45 with `ring`, tokio-rustls 0.26.6) loads a PEM identity, accepts without handshaking, and finishes the handshake in the connection's task (10 s bound). It shares one generic framing with the UDS adapter. `[transport.tcp] bind` may omit its port, which defaults to 7476. The Daemon loads the identity, binds UDS, then TCP, and serves both from one accept loop. The review pass made a TLS end without `close_notify` a clean close, so in-flight executions are answered as on UDS. It also added the missing Daemon- and adapter-level tests, made the fixed-port test skip when 7476 is taken, tagged accept failures with their transport, and aligned the WebSocket example port with 7478.

**Files changed**
- `Cargo.toml`, `Cargo.lock`, `crates/hexput-transport/Cargo.toml`, `crates/hexput-tests/Cargo.toml` — rustls (`ring`) and tokio-rustls pins; no `aws-lc`.
- `crates/hexput-transport/src/framed.rs` — the shared length-prefixed framing; `UnexpectedEof` reads as a clean close.
- `crates/hexput-transport/src/tcp_tls.rs` — `TlsIdentity`, `TcpTlsListener`, `TlsHandshake`, `TcpTlsPort`, `HANDSHAKE_TIMEOUT`.
- `crates/hexput-transport/src/uds.rs`, `lib.rs` — UDS rebuilt on the shared framing; module docs.
- `crates/hexput-config/src/{lib.rs,file.rs}`, `config.example.toml` — `DEFAULT_TCP_PORT`, an optional TCP port, docs and the example (WebSocket example on 7478).
- `crates/hexput-daemon/src/lib.rs` — startup order, the TCP listener, the multi-listener accept loop, handshakes in spawned tasks, and a `transport` field on accept failures.
- `crates/hexput-tests/fixtures/tls/*` — CA, leaf, leaf key, unrelated key.
- `crates/hexput-tests/tests/{transport,config,daemon}.rs` — adapter, bind-form and Daemon tests, including the review's close, large-frame and silent-peer tests.
- Spine, `deferred-work.md`, `sprint-status.yaml`, `AGENTS.md`, `epic-5-context.md` — the amendment, deferrals, tracking and status.

**Review findings breakdown**
- 35 findings in total: 14 medium, 15 low, 6 false.
- **Patched**, as 7 root-cause entries:
  - 3 medium entries:
    - TLS EOF as a clean close;
    - a Daemon test that the handshake stays off the accept loop;
    - the 7476 test skipping when the port is taken.
  - 1 medium entry: TLS write tests (flush, multi-record and back-to-back frames).
  - 3 low entries:
    - the accept-failure `transport` field;
    - the bracketed-bind tests and the 7478 example in the message;
    - the WebSocket example port and the bookkeeping.
- **Deferred**, 4 entries, all in `deferred-work.md` and in `deferred`:
  - non-Unix TCP (Story 5.3);
  - identity expiry and related checks;
  - handshake-failure visibility (Epic 7);
  - a pre-init deadline for remote peers.
- **Rejected**, each with its reason in the triage log:
  - Server `close_notify`: low, needs a Port trait change. Noted in `deferred-work.md` for Story 5.2.
  - Peer address in the connection span: new `serve` parameter.
  - Protocol-version error attribution: unreachable with `ring`.
  - Alias-only framing types: the aliases are documented.
  - Handshake cap: excluded by the intent.
  - Encrypted-key message: new public surface.
  - Scoped IPv6 without a port: rare, and the explicit form works.
  - `TCP_NODELAY` test: new public surface, latency only.
  - Six intent-alignment items, all `false`.

**Follow-up review recommended: true.** Four medium entries were patched (a first pass with two or more), with no high. The named unverified risk is that the shared framing now reads a TLS stream's `UnexpectedEof` as `Closed(None)`. So an unauthenticated TCP FIN, or one injected on the path, ends a TLS connection as a clean close: the Daemon keeps writing replies to in-flight executions instead of abandoning them. Truncation cannot forge data, but a reviewer should confirm that this trade is acceptable for an internet-facing listener.

**Verification**, all run on the patched tree:
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings`: no warnings.
- `python3 scripts/check-crate-graph.py`: OK, 26 edges and 4 restricted names.
- `cargo build --workspace --all-targets --locked --release`: success.
- `cargo test --workspace --locked --release`: 968 passed, 0 failed.
- `grep -c 'name = "aws-lc' Cargo.lock`: 0.
- The new no-`close_notify` test was run once against the unpatched framing and failed there.

**Residual risks**
- No server `close_notify`.
- No pre-init or idle deadline and no connection cap for remote peers.
- An expired certificate shows only at `debug`.
- TCP is not served on non-Unix platforms.

All of these are recorded in `deferred-work.md`.
