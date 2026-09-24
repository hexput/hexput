---
name: 'Hexput v2'
type: architecture-spine
purpose: build-substrate
altitude: initiative
paradigm: 'Hexagonal (Ports & Adapters) at the transport boundary, Actor model for the runtime core'
scope: 'Hexput v2 daemon — the whole system covered by the v2 PRD'
status: final
created: '2026-09-18'
updated: '2026-09-24'
amended: '2026-09-18 — AD-8 and the check/ module added for FR-26 (optional static check); later the same day, Structural Seed rewritten from a single-crate module tree into a Cargo workspace, one crate per module plus language/tooling crates, all lib crates funneling into one hexput-bin crate for binaries. Both after this spine was first marked final. 2026-09-24 — edge hexput-connection → hexput-rpc and the Call/Value Secret wire contract added for FR-27/FR-28 (course correction). See .memlog.md.'
binds: [FR-1, FR-2, FR-3, FR-4, FR-5, FR-6, FR-7, FR-8, FR-9, FR-11, FR-12, FR-13, FR-16, FR-17, FR-18, FR-19, FR-20, FR-21, FR-22, FR-23, FR-24, FR-25, FR-26, FR-27, FR-28]
sources: ['_bmad-output/planning-artifacts/prds/prd-hexput-2026-09-18/prd.md', '_bmad-output/planning-artifacts/briefs/brief-hexput-2026-09-18/brief.md']
companions: []
---

# Architecture Spine — Hexput v2

## Design Paradigm

**Hexagonal (Ports & Adapters)** at the transport boundary, wrapping an **Actor model** runtime core.

- `hexput-transport` — one adapter per Transport (UDS, Named Pipe, TCP+TLS, WebSocket), each implementing a single internal `Port` interface. No transport-specific behavior crosses into the core.
- `hexput-session`, `hexput-connection`, `hexput-plugin` — the actor layer: a `Session` (durable, Client-ID-keyed) owns a transient `Connection` actor and zero or more `Plugin` actors. Ownership here is lifecycle ownership, not a per-operation consistency boundary: `Session` creates and tears down `Plugin`s (AD-4's `teardown` contract), but a `Plugin` — together with its Global Variable store — is its own consistency boundary for event dispatch, since `hexput-session` never mediates a single Event invocation.
- `hexput-exec`, `hexput-enforce` — the shared execution core every actor's work funnels through; this is where Capability and Resource Budget rules apply, once, for every mode.

## Invariants & Rules

```mermaid
graph TD
  subgraph Adapters
    UDS[UDS Adapter]
    NP[Named Pipe Adapter]
    TCP[TCP+TLS Adapter]
    WS[WebSocket Adapter]
  end
  subgraph Core
    Port[RPC Port]
    Session[Session Manager]
    Conn[Connection Actor]
    Plugin[Plugin Actor]
    GVStore[Global Variable Store]
    Executor[Shared Executor]
    Enforce[Capability + Budget Enforcement]
  end
  UDS --> Port
  NP --> Port
  TCP --> Port
  WS --> Port
  Port --> Session
  Session --> Conn
  Session --> Plugin
  Conn --> Executor
  Plugin --> Executor
  Plugin --> GVStore
  Executor --> Enforce
  Executor --> GVStore
```

Dependency direction: Adapters depend on the Core `Port`; the Core never imports or branches on a specific Adapter. Nothing depends back on Adapters.

### AD-1 — Transport-agnostic core [ADOPTED]

- **Binds:** FR-9, FR-11, all Transports
- **Prevents:** transport-specific behavior leaking into Session/execution semantics; adapters and core drifting on per-transport protocol behavior; health/metrics (FR-11) becoming a fifth, structurally separate listener
- **Rule:** every Transport is an adapter implementing one internal `Port` interface; the Core crate graph never imports or branches on a specific transport type — enforced by which crates list `hexput-transport` as a dependency (only `hexput-daemon` does). Health/metrics (FR-11) is served through that same `Port`, exempted from the init-handshake gate (FR-1) rather than given its own listener.

### AD-2 — Session outlives Connection, and may have several at once [ADOPTED]

- **Binds:** FR-1, FR-2, FR-3, FR-13, FR-21, FR-24
- **Prevents:** Plugin/Config/Registered-Function state being coupled to a single live socket, breaking reconnect (FR-2) and Plugin persistence (FR-21); a Session's connection cardinality being read differently by independently-built units
- **Rule:** `Session` is a distinct, owned entity keyed by Client ID, with a TTL sourced from System Config (AD-5). A Session may have zero or more concurrently attached `Connection` actors (FR-2) — never exactly one. Each `Connection` is attached to at most one `Session` at a time and holds no state that outlives it. Requests and their responses correlate to the specific `Connection` that issued them; there is no cross-Connection broadcast of results — Connections are isolated from each other even when they share a Session. The Session's TTL clock (FR-21, FR-24) only starts once the last attached Connection drops; while at least one Connection is attached, the Session is actively alive with no TTL countdown running. Session-owned state (Plugins, Config, Global Variables) is shared and consistent across every attached Connection. The reconnect credential (FR-13) is validated once, in `hexput-session`, before a reconnect request is accepted — every Transport adapter forwards the credential unchanged through the Port; no adapter performs its own validation, and no transport crate depends on `hexput-session` in a way that would let it try.

### AD-3 — One shared Executor enforces Capability + Resource Budget for every execution path, for its full duration [ADOPTED]

- **Binds:** FR-6, FR-7, FR-8, FR-19, FR-25
- **Prevents:** Direct Execution, Cached Execution, and Plugin Event handlers implementing capability/budget checks independently and drifting apart; async handler work (AD-6) silently escaping budget accounting once dispatch returns
- **Rule:** all three execution paths call into one `Executor` entry point; no path may reach a Registered Function or consume Resource Budget outside it. For any execution that continues past its dispatching call's return — every `async = true` Plugin handler (AD-6) — `Executor` hands the spawned task a live budget-accounting handle, not a closed one; every budget-relevant operation that task performs afterward, including Global Variable writes counted as side effects (AD-4), charges through that same handle. `hexput-globalvar` has no dependency on `hexput-enforce` or `hexput-rpc` at all — a second path into either is not merely discouraged, it does not compile. Global Variable access itself is intrinsic language state, not a Registered Function — exempt from FR-6/FR-7's capability-grant requirement, reached only through the direct `Executor`/`Plugin → GVStore` path (never through `hexput-rpc`). Both of Plugin's dispatch paths (the priority sequencing task and directly-spawned `async` handlers, AD-4) invoke the same single handler-invocation function in `hexput-exec` — capability checks, budget-handle setup, and Global Variable access all happen in that one shared function; the two paths differ only in *when* they're scheduled, never in *what* runs.

### AD-4 — Global Variable store is independent of the Plugin actor's mailbox [ADOPTED]

- **Binds:** FR-20, FR-22, FR-23, FR-25
- **Prevents:** `async` handlers (FR-23) being forced through a serialized mailbox (breaking their bypass-ordering requirement); `priority`-ordered handlers (FR-22) losing sequencing, or accidentally serializing *different* Events on the same Plugin; the store leaking on teardown; `keyed` variables (FR-25) serializing unrelated partitions
- **Rule:** a Plugin's Global Variables live in a shared concurrent per-Plugin store, independent of and reachable without the Plugin actor being alive. Locking is per-`(top-level-key, partition-key)` — for a non-`keyed` variable, partition-key is a constant, collapsing to plain per-top-level-key locking; for a `keyed` variable (FR-25), different Backend-supplied keys never contend for the same lock. A Global Variable's lock is never held across an `.await` point (e.g. a handler's mid-execution RPC call) — every read/write is a short, synchronous critical section against the store; a handler that needs the value across an await re-acquires it. The Backend-configurable "unsafe/lock-free" strategy (FR-20) is implemented entirely in safe Rust — no `unsafe` blocks — by using the store's finer-grained/optimistic access instead of the default per-key critical section; it trades logical race-safety (permits lost updates, stale reads) for latency, and is unrelated to the Security NFR's "no unsafe Rust in the parser/VM path" rule, which is about memory safety, not this concurrency-semantics choice. `hexput-globalvar` exposes `teardown(plugin_id)` as its sole store-lifecycle entry point; `hexput-session`'s TTL-expiry and explicit-unregister paths (AD-2, FR-21) are its only callers, invoked synchronously before the Plugin actor is dropped — teardown is never implied by actor `Drop`. The Plugin actor owns event routing and ordering *metadata* only (which handler runs at which priority) — it does not itself execute the ordered chain: `priority`-ordered dispatch for one Event is a per-(Plugin, Event) sequencing task spawned independently of the actor's mailbox, so an in-progress priority chain for one Event never blocks a different Event arriving concurrently for the same Plugin. Both that sequencing task and directly-spawned `async` handlers read/write the same Global Variable store.

### AD-5 — Per-backend Config and System Config are separate, non-overlapping surfaces [ADOPTED]

- **Binds:** FR-1, FR-3, FR-24
- **Prevents:** daemon operational settings (bind addresses, TLS paths, log level, Session TTL) being conflated with backend-supplied execution policy, or either surface silently absorbing the other's responsibility; a runtime Config update (FR-3) silently not reaching already-registered Plugins
- **Rule:** System Config (file-based) is the only source of the daemon's own operational settings. Per-backend Config never touches disk and never configures the daemon itself. `hexput-session` holds the single mutable, live copy of a Session's per-backend Config; `hexput-plugin`, `hexput-script`, and `hexput-exec` read through that copy on every dispatch — none of them snapshot or cache it at registration time — so an FR-3 runtime update is visible to Plugin Event dispatch exactly as it is to Direct/Cached Execution, with no separate propagation path to keep in sync.

### AD-6 — Non-blocking dispatch is structural, not incidental [ADOPTED]

- **Binds:** FR-16
- **Prevents:** a slow execution on one connection or Plugin stalling unrelated work — the head-of-line blocking FR-16 explicitly forbids
- **Rule:** every execution (Direct, Cached, Plugin handler) dispatches as an independent async task on the shared runtime. The only permitted serialization is a Plugin's own opted-in `priority` ordering (FR-22) among its own handlers for one Event — never across connections, Plugins, or Events (AD-4's per-(Plugin, Event) sequencing task is what makes this hold in practice).

### AD-7 — System Config discovery is one deployment-agnostic precedence, packaging doesn't change it [ADOPTED]

- **Binds:** FR-24
- **Prevents:** a systemd build and a Docker build independently inventing different config-discovery logic (different default paths, different override mechanisms) that silently diverge
- **Rule:** `hexput-config` resolves the System Config file's location via one fixed precedence regardless of packaging — explicit CLI flag, then environment variable, then a fixed default OS path. systemd units and Docker images both just set the environment variable or bind-mount the default path; neither changes `hexput-config`'s resolution logic.

### AD-8 — One check pass, one invocation point per submission path [ADOPTED]

- **Binds:** FR-26, FR-3, FR-5, FR-6
- **Prevents:** Direct submission, Cached registration, and Plugin registration each growing their own partial checker and disagreeing about what is a finding; the check drifting into a second, unenforced capability surface; the hot path paying for a check it already passed
- **Rule:** `hexput-check` exposes a single pure entry point taking a parsed AST plus what the environment provides — the callable-name set and the Script's starting-variable names — and the active policy, and returning findings — it never executes a script, never reaches `hexput-rpc` or `hexput-enforce`, and holds no state between calls. It is invoked from exactly one place per submission path: `hexput-script` on Direct Execution and on `CodeRegister` (never again on `CachedExecutionStart` — a registered script is checked once, at registration), and `hexput-plugin` at Plugin registration. The mode (`off`/`warn`/`error`) is read from the `hexput-session` live Config on every submission, never snapshotted (AD-5). Findings reuse the error shape defined once in `hexput-port` (Consistency Conventions), so the daemon response, the CLI, and the language server render them identically. The check is advisory by construction: it never grants, denies, or substitutes for a capability check or a budget charge — those remain `hexput-enforce`'s alone, reached only through the `Executor` (AD-3), and a script passing the check is not thereby authorized for anything.

## Consistency Conventions

| Concern | Convention |
| --- | --- |
| Naming (entities, files, interfaces, events) | Code identifiers match PRD §3 Glossary terms verbatim (`Session`, `Plugin`, `GlobalVariable`, `Capability`, ...) — no synonyms |
| Data & formats (ids, dates, error shapes, envelopes) | MessagePack (`rmp-serde`/`serde`) wire envelope for all Transports; Client ID and error shapes defined once in `hexput-port`, reused by every adapter |
| State & cross-cutting (mutation, errors, logging, config, auth) | All state mutation with security/budget consequences flows through `exec`/`enforce` (AD-3); structured logging via `tracing`, every entry tagged with Client ID (PRD FR-12); config split per AD-5 |
| Locking discipline | No lock (Global Variable or otherwise) is ever held across an `.await` point — critical sections stay short and synchronous; re-acquire after suspension instead |

## Stack

| Name | Version |
| --- | --- |
| Rust | 1.98.1 (2024 edition) |
| tokio | 1.53.1 |
| tokio-tungstenite | 0.30.0 |
| rustls | 0.23.45 |
| serde | 1.0.229 |
| rmp-serde | 1.3.1 |
| rmpv (`with-serde`) | 1.3.1 |
| dashmap | 6.2.1 |
| getrandom | 0.4.3 |
| moka | 0.12.16 |
| criterion | 0.8.2 |
| tracing | 0.1.44 |
| clap (`derive`) | 4.6.7 |
| toml (`std`, `parse`, `serde`; no default features) | 1.1.6 |
| tracing-subscriber (`fmt`, `std`, `json`; no default features) | 0.3.23 |
| serde_json (tests only) | 1.0.151 |

**[Amended 2026-09-22, Epic 1 Story 1.9 decision 1]** `clap` is added: `hexput-cli-core` needs an argument parser for the eval command, and Story 1.10's check command and the daemon's own `--config` flag (AD-7) then inherit the same one rather than each hand-rolling a parser. It is pinned here and in `[workspace.dependencies]` like every other row. Adding it to this table is deliberate: "nothing not listed is permitted" governs *crate edges* in the dependency graph below, while the Stack table is the one place a third-party version lives.

**[Amended 2026-09-23, Epic 2 Story 2.1 decision 1]** `toml` and `tracing-subscriber` are added. `hexput-config` parses the System Config file as TOML through `serde`, with the parser's serializer (`display`) left out because System Config has no write API. `hexput-daemon` emits its startup log — the resolved System Config path and the AD-7 source that supplied it — through a plain `tracing-subscriber` `fmt` subscriber filtered at the file's `log_level`; structured JSON output joins as a feature of the same pin with Story 2.8. From this story on, `[workspace.dependencies]` pins each crate's **feature set** alongside its version (`tokio`'s `rt-multi-thread` + `signal`, `serde`'s `derive`), so a member writes `workspace = true` and nothing else and two crates cannot diverge on what a dependency provides.

**[Amended 2026-09-23, Epic 2 Story 2.2 decision 5]** `rmpv` is added, with `with-serde`. `hexput-port` decodes a frame in two phases — bytes to an untyped MessagePack value through `rmp-serde` (whose nesting-depth limit bounds recursion), then the envelope map validated by hand so every failure gets its exact `protocol.*` code and the correlation id is recovered whenever it is readable. `rmpv::Value` is that untyped value and the Port's payload type; later stories convert a payload to its typed struct with `rmpv::ext::from_value`. It is the same project and release line as the already-pinned `rmp-serde`.

**[Amended 2026-09-23, Epic 2 Stories 2.4 + 2.5]** `getrandom` is added, with no features. `hexput-session` draws each Client ID's 128 bits from the OS CSPRNG through `getrandom::fill`, so a Client ID is unguessable rather than merely unique; OQ-2's reconnect secret (Epic 5) will draw from the same source rather than add a second one. A Daemon that cannot read the CSPRNG issues no Client ID at all. The same stories put the already-pinned `dashmap` to its first use: the Session registry, keyed by Client ID.

**[Amended 2026-09-23, Epic 2 Story 2.8]** `tracing-subscriber` gains its `json` feature, on the same 0.3.23 pin. System Config's `log_format = "json"` makes `hexput-daemon` write one JSON object per event, with the fields of every enclosing span — a connection's `connection` and `client_id`, a request's `id` — as JSON fields; plain text stays the default. The feature brings `serde_json` and `tracing-serde` in transitively; `serde_json` is also pinned — here and in `[workspace.dependencies]` — as a test-only dependency of `hexput-tests`, which parses those lines; no production crate depends on it directly. No crate edge changes.

## Structural Seed

**[Amended 2026-09-18, post-final, per user direction: "her modülü kendi crate'ine ayıralım... shared crate'in iç modül yapısı da olabildiğince parçalı olmalı... bütün crateler lib olmalı, binarylerin çıkacağı bin diye ayrı bir crate olmalı."]** What was one crate with an internal module tree is now a Cargo workspace: one crate per Structural Seed module, plus crates for the language (never named as a module above because it sits underneath `hexput-script`, `hexput-check`, and `hexput-plugin` rather than being one itself) and for developer tooling. This is not cosmetic — a crate boundary is a dependency edge Cargo enforces at compile time, where a module boundary was only convention. Several ADs below (AD-1, AD-3, AD-4, AD-5, AD-8) state a rule of the shape "only X may reach Y directly" — under the module tree that rule lived in code review; under the crate graph, the crate that must not reach Y simply does not list it as a dependency, and doing so anyway is a compile error, not a review finding.

**Every crate is a `lib` crate.** Exactly one crate in the workspace, `hexput-bin`, is allowed to produce a binary; every other crate — including `hexput-daemon` — exposes a `run()`-shaped entry point and nothing calls `std::process::exit` or owns a `fn main()`. `hexput-bin` itself stays systematically thin: one file per binary under `src/bin/`, each doing nothing but argument parsing hand-off and a call into the one library crate that actually implements that binary. No logic lives in `hexput-bin` that isn't about being an operating-system entry point.

```text
Cargo.toml                    # [workspace], members below, pinned deps hoisted to [workspace.dependencies]
crates/
  hexput-shared/               # cross-cutting types every other crate may depend on; internally split, never a grab-bag
    src/
      diagnostics.rs           # Category, Code, Diagnostic, Span — the one error/finding shape (§7 LANGUAGE-REFERENCE, hexput-port errors)
      wire.rs                  # MessagePack envelope, correlation id, message-type enum (Consistency Conventions)
      ids.rs                   # ClientId, SessionId, PluginId — newtypes, never bare strings/uuids past the boundary
      budget.rs                # the six Resource Budget dimensions (FR-8) as one enum, shared by hexput-enforce, hexput-check and metrics
      lib.rs

  # — language: no execution, no I/O, no host access —
  hexput-ast/                  # AST node types + Span — data only, no lexing/parsing/eval logic
  hexput-lexer/                # tokenizer (Epic 1 Story 1.2) — depends on hexput-shared only
  hexput-parser/                # parser (Stories 1.3-1.5) — depends on hexput-lexer, hexput-ast
  hexput-interpreter/          # tree-walking evaluator (Stories 1.6-1.7) — depends on hexput-ast only, no host reach
  hexput-check/                # static check (FR-26, AD-8) — depends on hexput-ast only; NOT hexput-interpreter, NOT hexput-rpc, NOT hexput-enforce, by construction
  hexput-cli-core/             # eval + check command logic (Stories 1.9-1.10) — depends on lexer/parser/interpreter/check

  # — daemon: one crate per Structural Seed module —
  hexput-transport/            # adapters: uds.rs, named_pipe.rs, tcp_tls.rs, websocket.rs — one Port impl each
  hexput-port/                  # wire envelope + request/response + event correlation, built on hexput-shared::wire
  hexput-session/                # Client ID -> Session, TTL (from System Config), reconnect + FR-13 protection
  hexput-connection/            # transient per-connection actor, attached to a Session
  hexput-script/                # Direct/Cached Execution: AST cache (moka), parse/interpret entry points
  hexput-plugin/                # Plugin actor: registration, event routing, priority/async dispatch (AD-4)
  hexput-globalvar/              # Global Variable store: per-Plugin concurrent map (dashmap), locking + behavior strategies
  hexput-exec/                   # the one shared Executor all three modes funnel through (AD-3) — the only crate depending on hexput-enforce
  hexput-enforce/                # Capability checks + Resource Budget enforcement — reachable only via hexput-exec
  hexput-rpc/                    # host-function registry, capability grants, outbound RPC dispatch to Backend
  hexput-config/                 # System Config (file) parsing — separate crate from hexput-session's per-backend Config
  hexput-daemon/                  # wiring root: composes every daemon crate above into one runnable `run(SystemConfig)`

  # — developer tooling (FR-14, FR-15), independent of the daemon —
  hexput-grammar/               # tree-sitter grammar
  hexput-lsp-core/               # language server logic — depends on lexer/parser/check only, never interpreter or any daemon crate

  hexput-bin/                    # the ONLY crate that produces binaries
    src/bin/
      hexput-daemon.rs          # thin main() -> hexput_daemon::run()
      hexput.rs                 # thin main() -> hexput_cli_core::run() (eval + check)
      hexput-lsp.rs              # thin main() -> hexput_lsp_core::run_server()
```

### Crate dependency graph

Direction matters more than the list — an edge only appears if the AD it enforces requires it. Nothing not listed is permitted.

```mermaid
graph TD
  shared[hexput-shared]

  ast[hexput-ast] --> shared
  lexer[hexput-lexer] --> shared
  parser[hexput-parser] --> lexer
  parser --> ast
  interp[hexput-interpreter] --> ast
  check[hexput-check] --> ast
  check --> shared
  clicore[hexput-cli-core] --> lexer
  clicore --> parser
  clicore --> interp
  clicore --> check

  transport[hexput-transport] --> port[hexput-port]
  port --> shared
  session[hexput-session] --> port
  session --> shared
  conn[hexput-connection] --> session
  conn --> port
  conn --> script
  conn --> rpc
  session --> globalvar
  script[hexput-script] --> parser
  script --> interp
  script --> check
  script --> exec[hexput-exec]
  script --> port
  plugin[hexput-plugin] --> parser
  plugin --> globalvar[hexput-globalvar]
  plugin --> exec
  plugin --> check
  globalvar --> shared
  exec --> enforce[hexput-enforce]
  exec --> interp
  exec --> rpc[hexput-rpc]
  exec --> globalvar
  enforce --> shared
  rpc --> port
  config[hexput-config] --> shared

  daemon[hexput-daemon] --> transport
  daemon --> session
  daemon --> conn
  daemon --> script
  daemon --> plugin
  daemon --> config
  daemon --> exec

  grammar[hexput-grammar]
  lspcore[hexput-lsp-core] --> lexer
  lspcore --> parser
  lspcore --> check

  bin[hexput-bin] --> daemon
  bin --> clicore
  bin --> lspcore
```

What this graph makes a compile error rather than a review comment:

- **AD-1:** only `hexput-daemon` depends on `hexput-transport`. `hexput-session`, `hexput-script`, and `hexput-plugin` do not — so none of them can branch on a transport type even by accident.
- **AD-3:** `hexput-enforce` has exactly one dependent, `hexput-exec`. `hexput-rpc`, `hexput-plugin`, and `hexput-script` cannot reach it directly; a capability or budget check literally has to go through `hexput-exec`.
- **AD-4:** `hexput-globalvar` depends on neither `hexput-plugin` (the actor) nor `hexput-rpc` — the store is reachable with the actor dead, and never reachable through RPC, because those import edges don't exist. The reverse edge `hexput-session -> hexput-globalvar` is required and present: AD-4 makes `hexput-session` the sole caller of `teardown(plugin_id)`, which it cannot be without depending on the store. (This edge was missing when the graph was first derived from the module tree, making AD-4's rule unimplementable; corrected 2026-09-18.)
- **AD-5:** `hexput-config` (System Config) and `hexput-session` (per-backend Config) are separate crates with no dependency between them — the two surfaces cannot accidentally merge into one type.
- **AD-8:** `hexput-check` depends on `hexput-ast` alone — not `hexput-interpreter`, not `hexput-rpc`, not `hexput-enforce`. It cannot execute a script or touch the host even by mistake; the capability to do so was never compiled in.

**[Amended 2026-09-22, Epic 1 Story 1.10 decision 2]** AD-8's entry point takes one more thing the environment provides: the Script's **starting-variable names**. A Script written for a Backend reads inputs its own source never declares, so a pass that does not know them reports every such read as undeclared and is useless on exactly the Scripts it exists for. Only names are taken — a static check needs to know a name exists, never what it holds, and taking a value would put an evaluation on a path whose whole promise is that nothing runs. The `hexput-ast`-alone edge is unchanged and is now asserted by `scripts/check-crate-graph.py` rather than only stated here; the `hexput-shared` edge the crate was scaffolded with in Story 1.1 is gone, since `hexput-ast` re-exports the diagnostics shape and the graph above lists `check --> ast` only.

**[Amended 2026-09-23, Epic 2 Story 2.3]** The edge `hexput-connection --> hexput-port` is added. The connection actor is what drives a `Port` — it reads envelopes from an accepted connection and writes responses back — so it needs the `Port` trait and the envelope types, which live in `hexput-port`. Routing them through a `hexput-session` re-export would hide the real dependency behind a crate that has no reason to carry it. The edge grants no reach AD-1 forbids: `hexput-port` holds no transport, and `hexput-connection` still cannot name one, because only `hexput-daemon` depends on `hexput-transport`. `scripts/check-crate-graph.py` now pins the exact sets of `hexput-transport` (`hexput-port` alone), `hexput-connection` and `hexput-daemon`. The same story adds `tokio`'s `net`, `io-util`, `macros`, `time` and `sync` features to the pinned feature set.

**[Amended 2026-09-23, Epic 2 Story 2.6]** Two edges are added: `hexput-script --> hexput-port` and `hexput-connection --> hexput-script`. Direct Execution is served in `hexput-script`, which owns the `ExecutionStart` payload, the conversion between wire values and Hexput values, and the `ErrorBody` a failure becomes — all of which are `hexput-port` types, so the script crate reaches the codec directly instead of inventing a second error or value shape. `hexput-connection` only routes an initialized `ExecutionStart` to `hexput_script::direct_execution` and writes the reply on the same connection. Neither edge grants a reach an AD forbids: `hexput-port` holds no transport (AD-1), and `hexput-script` still reaches evaluation only through `hexput_exec::execute`, the one Executor (AD-3) — `hexput-enforce` keeps `hexput-exec` as its sole dependent. `scripts/check-crate-graph.py` now pins the exact sets of `hexput-script` and `hexput-exec`, and `hexput-connection`'s set gains `hexput-script`.

**[Amended 2026-09-24, Epic 3 Story 3.1 decision 3 / course correction]** The edge `hexput-connection --> hexput-rpc` is added. A host call (FR-6, FR-27) is an outbound request the Daemon originates on the connection that submitted the execution, so its reply arrives on that connection's `Inbound` half and its request must be written by that connection's single writer. `hexput-rpc` owns the correlation — Daemon-issued call ids, the pending-call table, the `Call` payload and the decoding of its reply — and the connection actor holds one per connection: it writes the `Call`s handed to it and routes every Backend `Result`/`Error` naming a pending call back to it. Re-exporting `hexput-rpc`'s types through `hexput-exec` and `hexput-script` would hide the real dependency behind two crates that do not own it. The edge grants no reach an AD forbids: `hexput-rpc` does not depend on `hexput-enforce`, which keeps `hexput-exec` as its sole dependent (AD-3), and the capability and budget decisions for a call are still taken inside the Executor, not by the connection. `scripts/check-crate-graph.py` pins the exact sets of `hexput-rpc` and `hexput-enforce`, and `hexput-connection`'s set gains `hexput-rpc`.

**[Amended 2026-09-24, course correction — FR-27/FR-28 wire contract]** Two shapes are fixed here because the Daemon, both SDKs (Epic 8) and every Backend would otherwise diverge on them. (1) Every host call — a Registered Function or a Registered Method — is one generic `Call` message, `{name, arguments}` plus `receiver` for a method; its reply is the ordinary `Result {value}` (optionally `modifications`, a list of `{ref, value}`) or `Error`; an execution's own `Result` likewise carries `modifications` for the referenced locations the script wrote, so changes flow both ways by Reference ID. The id space is per direction: a Backend's `Result`/`Error` is always a reply to a Daemon `Call`, and the Daemon's `Result`/`Error` always a reply to a Backend request, so Daemon-issued call ids never collide with the Backend's. (2) A value carrying a Value Secret travels as a holder map with exactly the keys `__secret` and `value` — in Rust terms `ValueHolder { key: Option<String>, ref: String, value, rest }`, where `rest` keeps every further Backend field — and a value without one travels plain, so Story 2.6's wire values stay valid. The Value Secret lives in `hexput-interpreter`'s value model as hidden per-value metadata; it adds no crate edge.

`[workspace.dependencies]` in the root `Cargo.toml` pins every version from the Stack table above exactly once; member crates inherit with `workspace = true` rather than re-pinning.

```mermaid
erDiagram
  SESSION ||--o{ CONNECTION : "attached to (0..N, transient)"
  SESSION ||--o{ PLUGIN : owns
  SESSION ||--o| CONFIG : "has (per-backend, in-protocol)"
  SESSION ||--o{ REGISTERED_FUNCTION : has
  PLUGIN ||--o{ GLOBAL_VARIABLE : declares
  PLUGIN ||--o{ EVENT_HANDLER : registers
```

## Capability → Architecture Map

| Capability / Area | Lives in (crate) | Governed by |
| --- | --- | --- |
| Connection & Session lifecycle (FR-1, FR-2, FR-3, FR-13, FR-21, FR-24) | `hexput-session`, `hexput-connection`, `hexput-config` | AD-2, AD-5 |
| Script execution (FR-4, FR-5, FR-16) | `hexput-script`, `hexput-exec` | AD-3, AD-6 |
| Static check (FR-26) | `hexput-check`, invoked from `hexput-script` and `hexput-plugin` | AD-8 |
| Capability-based RPC (FR-6, FR-7, FR-27) | `hexput-rpc`, `hexput-enforce`, driven by `hexput-exec` and written by `hexput-connection` | AD-3, AD-6 |
| Value Secret, Reference IDs, host-side modifications (FR-28) | `hexput-interpreter` (hidden value metadata), `hexput-exec` (wire conversion, applying modifications) | AD-3 |
| Resource Budgeting (FR-8) | `hexput-enforce` | AD-3 |
| Transport layer (FR-9) | `hexput-transport`, `hexput-port` | AD-1 |
| Plugin Registration & Events (FR-17…FR-23, FR-25) | `hexput-plugin`, `hexput-globalvar`, `hexput-exec` | AD-3, AD-4, AD-6 |
| Health/metrics (FR-11) | `hexput-port` | AD-1 |
| Structured logging (FR-12) | cross-cutting via `tracing`, no dedicated crate | Consistency Conventions |
| Hexput language (Epic 1, feeds FR-4/5/14/15/19/26) | `hexput-ast`, `hexput-lexer`, `hexput-parser`, `hexput-interpreter` | none — pre-dates any FR, consumed by the rows above |

## Deferred

- Exact wire message schema/field layout inside `hexput-port` — implementation detail once `rmp-serde`/`serde` types exist; not an invariant two independently-built units would diverge on given AD-1.
- TLS certificate reload mechanism (hot-reload vs. restart-required) — operational detail, not structural.
- Priority tie-breaking convention, default handler order absent `priority`, and `async`+`priority` combination precedence — already flagged as [ASSUMPTION]s in PRD OQ-11; low risk, revisit before `hexput-plugin` ordering logic is implemented.
- Master-slave horizontal scaling — explicitly future work (PRD §6.2, brief Scope) — no AD here; `hexput-session` and `hexput-plugin` ownership models above are designed to not preclude it, not to implement it.
- OS-level sandboxing (seccomp/cgroups/namespaces) — permanently out per PRD/brief; not part of this spine's trust model.
- Tree-sitter grammar / LSP (FR-14, FR-15) — `hexput-grammar` and `hexput-lsp-core`, separate deliverables/tools, not part of the daemon's own crate graph; no AD here. `hexput-lsp-core` depends on `hexput-check` (AD-8) as a library for its diagnostics, which is why that crate is pure and has no dependency on `hexput-daemon` or any crate that does.
- Client SDKs (FR-10) — external, per-language libraries that speak the `hexput-port` wire protocol; not part of the daemon's own architecture, so out of this spine entirely, not merely unmentioned.
- Benchmark harness structure (PRD SM-1, brief addendum) — thesis-deliverable detail, not a system invariant.
- Exact `keyed` Global Variable storage layout (e.g. nested map keyed by Backend-supplied key) — implementation detail within `hexput-globalvar`, owned by the code once it exists.
- Deployment & environments: systemd-unit vs. Docker-image as the packaging artifact (or both), environment/config profiles (dev/staging/prod), process-supervision and restart policy, upgrade/rollback story. System Config *discovery* is structural and covered by AD-7; everything else here is packaging-time and doesn't change `hexput-config`'s logic or any other module's structure, so no further AD is needed — this is a deliberate defer, not a silent gap.
