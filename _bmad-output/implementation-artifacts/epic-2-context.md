# Epic 2 Context: Run a script over a socket

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Turn the Epic 1 language into a running daemon. An operator starts it from one System Config file. A Backend connects over a Unix Domain Socket, sends its per-backend Config and function registrations in a single init handshake, submits a one-shot script, and gets back the result or a structured error. No per-backend config file exists anywhere. This epic also lays the structure every later epic builds on: the `Port` boundary every transport adapts into, the Session/Connection split that reconnect and Plugins rely on, the single shared `Executor` every execution mode goes through, independent async dispatch per execution, and `tracing` tagged with the Client ID, which operability work builds on.

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

- **System Config** is a file holding only the daemon's operational settings: transport bind addresses, TLS certificate paths, log level, default Session TTL. Its location is resolved by one fixed precedence — CLI flag, then environment variable, then a fixed default OS path — identical under systemd, Docker, or anything else. A missing, malformed, or incomplete file makes the daemon exit non-zero naming the file and the problem; it never falls back to an undocumented default for a required field. The resolved path is logged at startup.
- **Per-backend Config never touches disk** and never configures the daemon. Changing it never reads, writes, or reloads System Config.
- **Init handshake:** a fresh connection sends Config and registrations together in one init message; the daemon creates a Session keyed by a newly issued Client ID, stores both on it, and returns the Client ID. An init missing either part is rejected naming what is missing, and no Session is created. Any execution request before init is rejected with a defined "init not completed" error and nothing runs. The gate must allow health/metrics to be exempted later (a later epic builds them).
- **Direct Execution** parses and evaluates a script with caller-supplied starting variables and returns the result on the connection that sent the request. It creates no AST Cache entry. A parse or runtime failure returns as a protocol error response carrying the Epic 1 structured diagnostic (category, code, severity, message, span).
- **Reliability:** a malformed, truncated, or unknown-type frame gets a defined protocol error response — never a panic or a silent drop. A failing script or an abruptly disconnecting client must not affect the daemon or any other connection's in-flight work.
- **Concurrency:** a slow execution never delays a fast one, on the same connection or a different one, and message processing continues throughout. One connection may have several executions in flight at once.
- **Logging:** every event emitted while handling a request carries the Client ID as a structured `tracing` span field, not message text. Events before init still identify the connection and explicitly mark "no Client ID" rather than omitting the field. The subscriber honours System Config's log level; structured JSON output is available as a configured option.
- **Out of scope:** TCP+TLS, WebSocket, Named Pipe, reconnect and its credential, the Session TTL countdown, capability enforcement and Resource Budgets, runtime Config updates and per-execution overrides, the static-check mode, Cached Execution, Plugins, health/metrics.

## Technical Decisions

- **Hexagonal core (AD-1):** every transport is an adapter implementing one internal `Port`; the core never imports or branches on a transport type. Only `hexput-daemon` depends on `hexput-transport`, which depends on `hexput-port` alone. The UDS adapter creates the socket at the configured path and removes a stale socket left by an unclean shutdown.
- **Wire format:** one MessagePack envelope carrying correlation id, message type and payload; responses match requests by correlation id and may arrive in any order. Envelope, Client ID representation and error shapes are defined once and reused by every adapter. Decoding is two-phase: `rmp-serde` to an untyped `rmpv::Value` (its nesting limit bounds recursion), then the envelope validated by hand so each failure gets its exact `protocol.*` code and the correlation id is recovered whenever readable. Payloads convert to typed structs later. Field layout beyond this is left to the implementation.
- **Session vs. Connection (AD-2):** a Session is its own entity keyed by Client ID, holding Config and registrations, with zero or more attached Connections — its type must not encode "exactly one". Each Connection attaches to at most one Session and holds no state that outlives it. A response goes only to the Connection that issued the request. In this epic a Session is torn down when its last Connection detaches, through one explicit teardown path — never `Drop` — so the Epic 5 TTL can delay that same path without moving it.
- **Client IDs are unguessable:** `hexput-session` draws 128 bits per Client ID from the OS CSPRNG via `getrandom`; a Daemon that cannot read the CSPRNG issues none. Epic 5's reconnect secret reuses this source. The Session registry is a `dashmap` keyed by Client ID.
- **Config ownership (AD-5):** `hexput-session` holds the single live copy of a Session's Config; nothing snapshots it, and readers go through it on every dispatch. `hexput-config` and `hexput-session` do not depend on each other.
- **One Executor (AD-3):** every execution path reaches evaluation through one `Executor` entry point in `hexput-exec`, with no path around it; enforcement later sits behind it (`hexput-enforce` is reachable only through `hexput-exec`). `hexput-script` owns Direct Execution — the `ExecutionStart` payload, wire-value/Hexput-value conversion, and the error body a failure becomes, all as `hexput-port` types rather than a second shape. `hexput-connection` only routes an initialized request to `hexput-script` and writes the reply on the same connection. Once the check mode exists, `hexput-script` is the one place that invokes the check on Direct Execution, reading the mode from the live Config.
- **Non-blocking dispatch (AD-6):** each execution runs as an independent async task on the shared Tokio runtime; no per-connection serial queue anywhere in the path. No lock of any kind is held across an `.await`.
- **Crate edges are compiler-enforced:** follow the spine's crate dependency graph exactly, including its dated amendments — `hexput-connection` depends on `hexput-session`, `hexput-port` and `hexput-script`; `hexput-script` on the parser, interpreter, `hexput-check`, `hexput-exec` and `hexput-port`. None of these edges grants transport reach or a path around the Executor. Any new edge updates `scripts/check-crate-graph.py` in the same change; it pins exact dependency sets. Daemon crates stay `lib`; `hexput-bin`'s `hexput-daemon.rs` only hands off to `hexput-daemon`, whose `run` returns an exit code. The daemon's flags use `clap`.
- **Pinned stack, feature sets included:** tokio 1.53.1, serde 1.0.229, rmp-serde 1.3.1, rmpv 1.3.1 (`with-serde`), dashmap 6.2.1, getrandom 0.4.3 (no features), tracing 0.1.44, clap 4.6.7 (`derive`), toml 1.1.6 (`std`/`parse`/`serde`, no serializer), tracing-subscriber 0.3.23 (`fmt`/`std`; Story 2.8 adds JSON output as a feature of the same pin). `[workspace.dependencies]` pins each version with its feature set; members write only `workspace = true`.
- **Glossary names verbatim** in code: `Daemon`, `Backend`, `Session`, `Connection`, `Client ID`, `Config` vs. `System Config`, `Registered Function`, `Direct Execution`.
- **Result values** cross the wire as data only; a Script result cannot be or contain a function or a cycle; the interpreter already rejects these.

## Cross-Story Dependencies

- 2.1 (System Config) and 2.2 (envelope, Port) come first; 2.3's UDS adapter needs both. 2.4's handshake needs 2.2/2.3, and 2.5's Session/Connection model is where 2.4 creates Sessions, so they are built together. 2.6 needs an initialized connection. 2.7 moves 2.6's dispatch out of the connection loop into independent tasks. 2.8 threads the Client ID through all of them and uses 2.1's log level.
- From Epic 1: the parser, the interpreter (`evaluate_with_variables` for starting variables), and the shared `Diagnostic` shape for error responses.
- Later epics: Epic 3 adds enforcement, budgets, Config keys and Config updates behind the same `Executor` and live Config copy; Epic 4 adds Cached Execution to `hexput-script`; Epic 5 adds three adapters behind the same `Port`, reconnect (reusing the CSPRNG source), and the TTL delay on 2.5's teardown path; Epic 7 builds health/metrics and full logging on 2.2's Port and 2.8's tracing.
