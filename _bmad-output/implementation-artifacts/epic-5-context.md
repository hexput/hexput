# Epic 5 Context: Connect from anywhere, survive disconnects

<!-- Generated from planning artifacts. Regenerate with compile-epic-context if planning docs change. -->

## Goal

A Backend reaches the Daemon over TCP+TLS, WebSocket or a Windows Named Pipe with protocol behaviour identical to the Unix Domain Socket path, and can reconnect on a previously issued Client ID — proving it owns the Session, not merely knowing the ID — resuming its stored Config and registrations without resending them, with several Connections live under one Client ID at once. A Session outlives its Connections and expires only after a TTL that starts when the last one leaves. This takes the Daemon from a local-only service to one remote Backends can depend on, and realizes the Session/Connection cardinality the architecture was built around.

## Stories

- Story 5.1: Connect remotely over TCP with mandatory TLS
- Story 5.2: Connect over WebSocket
- Story 5.3: Connect locally on Windows over a Named Pipe
- Story 5.4: Behave identically no matter how the backend connected
- Story 5.5: Issue a reconnect credential at first connect
- Story 5.6: Reconnect without resending configuration
- Story 5.7: Run several connections under one client ID
- Story 5.8: Expire a session only after the last connection is gone
- Story 5.9: Benchmark the remote path separately from the local one

## Requirements & Constraints

- Four transports — UDS, Named Pipe, TCP+TLS, WebSocket — with identical protocol behaviour: the same request produces the same result and the same error codes on each, modulo latency.
- TLS is mandatory, never optional or negotiable, for every non-local transport (TCP, WebSocket). UDS and Named Pipe are local and need no TLS. A plaintext attempt on a TLS listener is refused.
- Fixed default ports, used whenever System Config enables a transport without naming a port: **7476** TCP+TLS, **7478** WebSocket (7477 is reserved for the local playground of a later epic).
- System Config (file-based, read once at startup) carries bind addresses/ports and TLS certificate/key paths. The Daemon fails to start with a clear error naming the file and the problem when a certificate or key is missing, unreadable or malformed — it never silently listens without TLS.
- On a non-Windows host the Named Pipe adapter is cleanly absent rather than failing startup.
- Reconnect protection (decided): on first init the Daemon draws a reconnect secret of at least 256 bits from the OS CSPRNG and returns it with the Client ID exactly once; the Session stores only a salted hash, never the secret, and never logs it. A reconnect carries Client ID + secret; a wrong secret and an unknown Client ID are rejected with one identical reconnect-denied error, so IDs cannot be enumerated. Comparison is constant time. The secret lives as long as the Session; no rotation.
- A reconnect attaches the new Connection to the existing Session; Config and registrations are not resent, and the stored Config stays authoritative if a reconnect carries one.
- Several Connections may be attached to one Session concurrently. Each request's response returns only to the Connection that sent it (no cross-Connection broadcast); Session-owned state changes (Config updates, registrations) are visible to all of them.
- A Session's TTL (System Config `session_ttl_secs`) starts only when its last Connection detaches; a reconnect before it elapses stops the countdown with state intact; after expiry the Session, its Config, registrations and AST Cache entries are torn down, and a reconnect is rejected exactly like an unknown Client ID.
- Benchmarks report UDS and TCP/TLS figures separately — never merged — with each leg's transport and machine topology recorded.

## Technical Decisions

- **AD-1:** every transport is an adapter in `hexput-transport` implementing the one `Port` trait from `hexput-port` (`split()` into `Inbound`/`Outbound`); only `hexput-daemon` depends on `hexput-transport`, and no other crate names or branches on a transport. `hexput-transport`'s only workspace edge is `hexput-port`; `scripts/check-crate-graph.py` pins it.
- **AD-2:** the reconnect credential is validated once, in `hexput-session`, before a reconnect is accepted; adapters forward it unchanged and validate nothing. A Session has zero or more attached Connections, never exactly one.
- **AD-5/AD-7:** System Config (`hexput-config`) is the only source of operational settings, resolved by flag → `HEXPUT_CONFIG` → default path; per-backend Config never touches disk.
- Session teardown is the one private `teardown` path in `hexput-session` (with its teardown hook releasing AST Cache entries); the TTL delays that same path rather than adding a second one.
- Pinned stack relevant here: tokio 1.53.1, getrandom 0.4.3 (the one CSPRNG source, shared with Client IDs), rustls 0.23.45 with the `ring` crypto provider only (`ring`, `std`, `tls12`; no defaults — `aws-lc-rs` never appears in `Cargo.lock`), tokio-rustls 0.26.6, tokio-tungstenite 0.30.0 (`handshake` only — its TLS is our own tokio-rustls), futures-util 0.3.34 (`sink`, `std`). These are normal dependencies of `hexput-transport` only (plus dev-dependencies of `hexput-tests` for test clients). A new dependency or feature is pinned once in `[workspace.dependencies]` and recorded in the Spine's Stack table by dated amendment.
- TLS policy for network transports is fixed: mandatory on every TCP and WebSocket connection (no plaintext or `ws://` mode); rustls's safe default protocol versions; no client authentication (a Backend authenticates in-band via init/reconnect on every transport) and no ALPN; certificate and key read as PEM once at startup (a restart is the reload). The TLS handshake and any WebSocket upgrade run in the per-connection task, never in the accept loop, bounded by a 10 s handshake timeout. A `bind` naming an IP alone uses the transport's default port.
- Framing: UDS and TCP+TLS share one 4-byte length-prefixed framing implementation inside `hexput-transport`; WebSocket carries exactly one MessagePack envelope per binary message (text message → `protocol.malformed_frame`, not fatal). Every frame is bounded by `MAX_FRAME_LEN`.
- Named Pipe (Windows only): byte mode with the same 4-byte length-prefixed framing, first instance only (a name another process already serves fails startup), remote clients rejected (local only, no TLS), Windows' default security descriptor, no new dependency. Configured as `[transport.named_pipe] path = '\\.\pipe\<name>'`, validated on every platform. A client must retry `ERROR_PIPE_BUSY` (231) between instances.
- Platform rule: `[transport.uds]` is served only on Unix and `[transport.named_pipe]` only on Windows; TCP and WebSocket everywhere. A configured section the platform cannot serve is skipped with a `warn`; only a System Config with nothing servable fails startup. The Daemon's serving path is platform-independent apart from one local-transport module per platform.
- Verification beyond this Linux machine: a `windows` CI job lints the workspace and runs the tests whose names match its filters (today `named_pipe` and `conformance`); `scripts/check-windows.sh` is the local compile-only cross-check for `cfg(windows)` code. Every cargo command on this machine runs with `--release` (disk is scarce).
- Conformance: one shared suite in `hexput-tests` (`tests/daemon/conformance.rs`) runs every case end to end over every transport the platform serves, through a transport-neutral client; normalized transcripts (Client ID, Script handle and generated Reference ID nonces replaced, nothing else) must be byte-identical across transports. A new wire field that varies per run (an issued identifier or secret) is normalized there; a new adapter adds one `transports!` line and changes no case.
- AD-1 transport-name guard: `scripts/check-crate-graph.py` fails when any production crate other than `hexput-transport`, `hexput-daemon` and `hexput-config` names a transport anywhere in its sources or manifest, comments included (`uds`, `tcp`, `tls`, `wss` as identifier parts, WebSocket, Named Pipe, `rustls`, `tungstenite`, …). Session-side code describes credentials without naming any transport or TLS.
- Reconnect credential (landed with the first-connect story): `Sessions::create` issues a 256-bit `ReconnectSecret` (`getrandom`), the `Init` reply is `Result {client_id, reconnect_secret}` (64 lowercase hex), sent once and never logged; the Session keeps only a per-Session salt and `HMAC-SHA256(salt, secret)` via `ring` (0.17.14, a normal dependency of `hexput-session` only); `Sessions::verify_reconnect` compares in constant time and checks an unknown Client ID against a dummy credential. The secret type has a redacting `Debug`, no `Display`/`Clone`/serde; text only through `to_hex`/`from_hex`. No rotation. Reconnect, the reconnect-denied error and any attempt limit build on this.
- Reconnect (landed): an unattached connection sends `Reconnect {client_id, reconnect_secret}` (bypasses the init gate like `Init`); `hexput-session` decodes it before any lookup and `Sessions::reconnect` verifies in constant time and attaches under one shard lock. Success is `Result {client_id}` (never the secret) and the connection then serves exactly as after `Init`, against the Session's stored Config, registrations and AST Cache entries; every miss is one `protocol.reconnect_denied` with a fixed message; a carried `config`/`registrations` is `protocol.invalid_payload`; `Init`/`Reconnect` on an attached connection is `protocol.already_initialized`. No attempt limit.
- Per-connection state vs Session state: each connection has its own correlation (its own `Call`/`Authorize` id counter and pending table) and its own replies; a Session holds Config, registrations (set only at `Init`; no runtime function-registration request exists) and AST Cache entries keyed by Client ID, every execution reading them afresh at dispatch.
- Until the TTL story lands, detaching a Session's last Connection tears the Session down at once, so a reconnect can only reach a Session that still has another Connection attached.
- No lock is held across an `.await`; every execution remains its own task (AD-6); a failing or slow connection disturbs no other.
- Known open hardening items that grow in weight once the Daemon is network-reachable: no cap on concurrent connections, and decoded frame size bounded only by `MAX_FRAME_LEN`.

## Cross-Story Dependencies

- 5.1 and 5.2 add network adapters; 5.3 adds the Windows local adapter; 5.4's conformance suite runs over all four and needs them.
- 5.5 (credential issue) precedes 5.6 (reconnect), which 5.7 (several Connections) and 5.8 (TTL expiry) build on.
- 5.9 extends Epic 4's Criterion `cache` bench with a TCP+TLS leg and needs 5.1.
- Epic 6's Plugin lifetime (survives reconnect, torn down on TTL) and Epic 8's SDK reconnect depend on 5.5–5.8.
