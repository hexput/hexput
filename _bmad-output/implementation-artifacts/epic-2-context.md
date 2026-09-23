# Epic 2 Context: Run a script over a socket

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Turn the Epic 1 language into a running daemon: an operator starts it from one System Config file, a Backend connects over a Unix Domain Socket, hands over its per-backend Config and function registrations in a single init handshake, submits a one-shot script, and gets the result (or a structured error) back. No per-backend config file exists anywhere. This epic lays the structural spine every later epic extends: the `Port` boundary all transports adapt into, the Session/Connection split that reconnect and Plugins will rely on, the single shared `Executor` every execution mode must funnel through, independent async dispatch per execution, and Client-ID-tagged `tracing` that operability work builds on.

## Stories

- Story 2.1: Start the daemon from a System Config file
- Story 2.2: Frame requests and responses on the wire
- Story 2.3: Accept connections over a Unix Domain Socket
- Story 2.4: Complete the init handshake with inline config and registrations
- Story 2.5: Attach connections to a session that outlives them
- Story 2.6: Run a one-shot script and get the result back
- Story 2.7: Keep slow executions from blocking anything else
- Story 2.8: Trace every request back to its Client ID

## Requirements & Constraints

- **System Config** is file-based and carries only daemon operational settings: transport bind addresses/ports, TLS certificate paths, log level, default Session TTL. Its location resolves by one fixed precedence — CLI flag, then environment variable, then a fixed default OS path — identical for systemd, Docker, or anything else; packaging only sets the env var or mounts the default path. A missing file, a malformed file, or a missing required field exits non-zero naming the file and the problem; never fall back to an undocumented default for a required field. The resolved path is logged at startup.
- **Per-backend Config never touches disk** and never configures the daemon. Changing it never reads, writes, or reloads System Config.
- **Init handshake:** a fresh connection sends Config plus function registrations in one init message; the daemon creates a Session keyed by a newly issued Client ID, stores both, and returns the Client ID. An init missing either part is rejected naming what is missing, and no Session is created. Any execution request before init completes is rejected with a defined "init not completed" error and runs nothing. (Health/metrics will later be the only messages exempt from this gate — design the gate so that exemption fits, but health/metrics themselves are a later epic.)
- **Direct Execution** parses and evaluates a script with caller-supplied starting variables and returns the result on the issuing connection, creating no AST Cache entry. Parse and runtime failures return the Epic 1 structured diagnostic (category, code, severity, message, span) as a protocol error response.
- **Reliability:** a malformed, truncated, or unknown-type frame yields a defined protocol error response — never a panic or a silent drop. One script's failure, or one client disconnecting abruptly, must not disturb the daemon or any other connection's in-flight work.
- **Concurrency:** a slow execution never delays a fast one on the same or a different connection, and message processing continues throughout; one connection may have several executions in flight.
- **Logging:** every event emitted while handling a request carries the Client ID as a structured `tracing` span field, not interpolated text. Events before init still identify the connection and explicitly mark "no Client ID" rather than omitting the field. The subscriber honours System Config's log level and offers structured JSON output as a configured option.
- **Out of scope here:** TCP+TLS, WebSocket, Named Pipe, reconnect and its credential, Session TTL countdown, capability enforcement and Resource Budgets, runtime Config updates and per-execution overrides, the static-check mode, Cached Execution, Plugins, health/metrics.

## Technical Decisions

- **Hexagonal core (AD-1):** every transport is an adapter implementing one internal `Port`; the core never imports or branches on a transport type. Only `hexput-daemon` depends on `hexput-transport`. The UDS adapter creates the socket at the configured path and removes a stale socket file from an unclean shutdown.
- **Wire format:** one MessagePack envelope (`rmp-serde`/`serde`) carrying a correlation id, message type, and payload; responses match requests by correlation id and may arrive in any order. The envelope, Client ID newtype, and error shapes are defined once (`hexput-shared::wire`/`ids`, `hexput-port`) and reused by every future adapter — no adapter declares its own. Exact field layout is an implementation choice.
- **Session vs. Connection (AD-2):** a Session is its own entity keyed by Client ID holding Config and registrations; it has zero or more attached Connections (the type must not encode "exactly one"), and each Connection attaches to at most one Session and holds no state that outlives it. Responses go only to the issuing Connection — no broadcast to siblings. At this stage, when the last Connection detaches the Session is torn down, through one explicit teardown path never implied by `Drop`, so the later TTL work can delay that same path without relocating it.
- **Config ownership (AD-5):** `hexput-session` holds the single live copy of a Session's Config; nothing else snapshots it — readers go through it on every dispatch. `hexput-config` (System Config) and `hexput-session` have no dependency on each other.
- **One Executor (AD-3):** every execution path enters script evaluation through one `Executor` entry point in `hexput-exec`; there is no second path around it. Capability and budget enforcement will later live behind it (`hexput-enforce` reachable only via `hexput-exec`), so do not evaluate scripts from anywhere else. `hexput-script` owns Direct Execution and depends on parser, interpreter, check, and exec.
- **Non-blocking dispatch (AD-6):** each execution runs as an independent async task on the shared Tokio runtime; no per-connection serial queue exists anywhere in the path. No lock of any kind is held across an `.await`.
- **Crate edges are compiler-enforced:** follow the spine's crate dependency graph exactly and update `scripts/check-crate-graph.py` in the same change as any new edge. Daemon crates stay `lib`; `hexput-bin`'s `hexput-daemon.rs` is only a thin `main()` handing off to `hexput_daemon::run(...)`, and the daemon's `--config` flag uses `clap` like every other CLI.
- **Pinned stack:** tokio 1.53.1, serde 1.0.229, rmp-serde 1.3.1, tracing 0.1.44, clap 4.6.7, toml 1.1.6, tracing-subscriber 0.3.23 (Story 2.1), rmpv 1.3.1 with `with-serde` (Story 2.2), tokio's `net`/`io-util`/`macros`/`time`/`sync` features (Story 2.3) — pinned, with their feature sets, once in `[workspace.dependencies]`, inherited per crate.
- **Glossary names verbatim** in code: `Daemon`, `Backend`, `Session`, `Connection`, `Client ID`, `Config` vs. `System Config`, `Registered Function`, `Direct Execution`.
- **Result values:** a Script result cannot be or contain a function or a cycle (already an interpreter error); results and starting variables cross the wire as data only.

## Cross-Story Dependencies

- 2.1 (System Config) and 2.2 (envelope/Port) come first; 2.3's UDS adapter needs both. 2.4's handshake needs 2.2 and 2.3; 2.5's Session/Connection model is what 2.4 creates Sessions into, so they are built together in practice. 2.6 needs an initialized connection (2.4/2.5); 2.7 hardens 2.6's dispatch; 2.8 threads Client ID through all of them and uses 2.1's log level.
- Consumes Epic 1: parser, interpreter (`evaluate_with_variables` for starting variables), and the shared `Diagnostic` shape for error responses.
- Forward-facing: Epic 3 adds enforcement, budgets, and Config updates behind the same `Executor` and live Config copy; Epic 4 adds Cached Execution to `hexput-script`; Epic 5 adds three more adapters behind the same `Port`, reconnect, and the TTL delay on the 2.5 teardown path; Epic 7 builds health/metrics and FR-12 logging on 2.2's Port and 2.8's tracing.
