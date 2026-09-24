---
stepsCompleted: [1, 2, 3, 4]
inputDocuments:
  - _bmad-output/planning-artifacts/prds/prd-hexput-2026-09-18/prd.md
  - _bmad-output/planning-artifacts/prds/prd-hexput-2026-09-18/addendum.md
  - _bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md
  - _bmad-output/planning-artifacts/briefs/brief-hexput-2026-09-18/brief.md
  - _bmad-output/planning-artifacts/briefs/brief-hexput-2026-09-18/addendum.md
  - _bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md
---

# hexput - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for hexput, decomposing the requirements from the PRD, UX Design if it exists, and Architecture requirements into implementable stories.

## Requirements Inventory

### Functional Requirements

*FR ids are taken verbatim from the PRD (§4). They are stable references assigned in finalization order, not document order — FR-13 and FR-24 sit in §4.1 despite their numbers.*

**Connection & registration lifecycle (PRD §4.1)**

FR-1: A Backend can establish a new connection and supply its Config and function registrations in the same init handshake, with no static per-backend config file; execution requests on a connection that has not completed init are rejected.
FR-2: A Backend can reconnect with a previously issued Client ID and resume without resending Config; a Client ID is not limited to one concurrent connection.
FR-3: An already-connected Backend can update its stored Config at runtime, and can override specific Config values (resource budgets, language-feature toggles such as disabling loops or if/else) for a single execution without mutating stored Config; a disabled-construct failure is a distinct error from a budget violation or capability denial.
FR-13: Reconnecting via a Client ID requires proof of authorization, not merely knowledge of the ID; one mechanism defined once, applied uniformly across all four Transports.
FR-24: The daemon reads a file-based System Config at startup (transport bind addresses/ports, TLS certificate paths, log level, default Session TTL) and fails to start with a clear error when it is missing or malformed; changing per-backend Config never requires touching System Config.

FR-26: A Backend can have submitted scripts and Plugins statically checked before execution, in one of three modes — `off` (default), `warn`, or `error` — set in Config and overridable per execution; the check reports undeclared identifiers, arity mismatches, calls to names that are neither local functions nor Registered Functions on that Session, literal-operand type errors, unreachable code, disabled-construct usage, and Plugin-only declaration mismatches, each carrying the same category/code/span as any other error. **[Added 2026-09-18 during sprint planning, after the PRD and Spine were first marked final. Now traced in all three: PRD §4.2 FR-26 and Glossary "Static Check", Architecture Spine AD-8 plus the `hexput-check` crate in the Structural Seed, and LANGUAGE-REFERENCE.md §10.]**

**Script execution model (PRD §4.2)**

FR-4: A Backend can submit a script for one-shot Direct Execution without creating an AST Cache entry.
FR-5: A Backend can register a script once (`CodeRegister`) and trigger repeated Cached Executions (`CachedExecutionStart`) with different variables served from the AST Cache, with no state bleed between runs.
FR-16: Execution requests are processed asynchronously; a slow execution never blocks message processing or another execution request, on the same connection or a different one, and one connection may have multiple executions in flight.

**Capability-based RPC (PRD §4.3)**

FR-6: A Backend can register a function as callable independently of the logic deciding whether a specific call is allowed — `context.allow()` at registration grants blanket access; otherwise a per-call handler must return `true` or the call is capability-denied.
FR-7: A script cannot reach the filesystem, network, or host process memory except through an explicitly granted Registered Function; any other capability reference fails with a defined capability-denied error, not a host-level exception.
FR-27: A Backend can register a function as a method bound to an object key (`registerMethod(objKey, fn)`); a script calls it as `value.name(args)` on a value whose Value Secret carries that key, the call reaching the Backend as the same generic `Call` message with the receiver itself attached; a registered method wins over an own property of the same name, an unregistered method name is a capability denial, and argument/receiver nesting is capped by a Config limit (default 12). **[Added 2026-09-24 by course correction — sprint-change-proposal-2026-09-24.md.]**
FR-28: Every value crossing to the Backend carries a hidden Value Secret (`__secret`: Reference ID `ref`, optional `key`, further Backend fields) that the Backend can read and edit and a script can never observe or change — reading yields `null`, writing is silently ignored, iteration skips it; modifications flow both ways by Reference ID — changes the Backend makes during a call are reported in the call's reply and applied before the script resumes, and referenced locations the script writes are reported in the execution's result; a Reference ID names a collection itself or the location a scalar or string arrived in, never a copy; a script can never override a Registered Method (`capability.method_override`, also a static-check finding). **[Added 2026-09-24 by course correction.]**

**Resource budgeting (PRD §4.4)**

FR-8: Every execution is bounded independently across CPU time, memory, allocation count, RPC call count, output size, and side-effect count; exceeding any one dimension terminates the execution with an error naming that dimension, and limits are configurable per Config and overridable per execution.

**Transport layer (PRD §4.5)**

FR-9: A Backend can connect over Unix Domain Socket, Named Pipe, TCP+TLS, or WebSocket with identical protocol behavior; TLS is mandatory for every non-local transport, and Named Pipe is the Windows-native local equivalent of UDS.

**Client SDKs (PRD §4.6)**

FR-10: Client SDKs ship in two phases — Phase 1 (launch): JavaScript and Python, each covering the full FR-1…FR-5 + FR-13 surface; Phase 2 (post-launch, not a launch blocker): Node.js, Rust, Go.

**Observability & operations (PRD §4.7)**

FR-11: The daemon exposes a health check usable without an authenticated Backend connection, and execution metrics (counts, latency, budget-violation rate) scrapable by standard tooling, with per-budget-dimension violation counters rather than one aggregate.
FR-12: The daemon emits structured logs for connection lifecycle events, capability denials, and budget violations, each independently identifiable and tagged with the originating Client ID.

**Developer tooling (PRD §4.8)**

FR-14: A tree-sitter grammar for the Hexput language parses all language constructs (variables, callbacks, loops, conditionals, objects/arrays) into a tree usable for editor highlighting and folding.
FR-15: A basic language server provides syntax-error diagnostics at correct locations and basic completion over LSP, usable from any LSP-compatible editor.

**Plugin registration & events (PRD §4.9)**

FR-17: Plugin registration is a protocol-level distinct mode from Direct/Cached Execution; one connection may use both concurrently, and one Client ID may hold multiple Plugins whose `name` is unique per Client ID rather than daemon-wide.
FR-18: On Plugin registration the daemon invokes every `@Event(BackendRegisteredInit)` handler exactly once, to completion (or failing registration), before the Plugin accepts any other Event; the name is reserved.
FR-19: A Backend declares the full set of firable Event names and each Event's return shape at Plugin registration time; undeclared Event names are rejected, non-conforming handler results are a defined error, and firing a declared Event with no matching handler is a defined no-op. Handler invocations carry the same Resource Budget and Capability rules as Script execution.
FR-20: A Plugin's Global Variables persist and are shared across all its Event invocations with per-top-level-key locking by default; a Backend may configure an unsafe/lock-free strategy at registration, Plugin code may override per-variable where the Backend permits, and `async = true` handlers default to unsafe/lock-free unless the Backend opts them into the safe strategy.
FR-25: A Global Variable's lifetime is configurable per-variable and independent of its locking strategy: `forever` (default), `ttl: <duration>`, `separate_each_trigger`, or `keyed` (partitioned by a Backend-supplied key declared per-Event; firing a key-required Event without a key is a defined error).
FR-21: A Plugin's registration and Global Variable state live for the Session's lifetime, surviving reconnect and torn down on Session TTL expiry or explicit unregister; the TTL clock starts only when the last attached Connection drops; an in-flight Event invocation on disconnect runs to completion locally and is cancelled only when it next attempts an RPC back to the gone Backend.
FR-22: Plugin code can order handlers on one Event via `@Event(<name>, priority = N)` — ascending order, lower value first — permitted only when the Backend's Config allows Plugin-level ordering control; otherwise `priority` is ignored or rejected and registration order applies.
FR-23: `@Event(<name>, async = true)` runs a handler concurrently without waiting its turn in the Event's ordering; `async` defaults to `false`, implies unsafe/lock-free Global Variable mutation unless the Backend configures otherwise, and takes precedence when combined with `priority`.

### NonFunctional Requirements

NFR1 (Security — trust boundary): The capability model (FR-6, FR-7) is the entire security boundary; no secondary enforcement layer and no OS-level sandbox exists or will be added. Every Registered Function call must be attributable to a Client ID.
NFR2 (Security — memory safety): No `unsafe` Rust ships in the parser/VM execution path without explicit review; memory safety here is a security property. The Backend-facing "unsafe/lock-free" Global Variable strategy is a concurrency-semantics choice implemented in entirely safe Rust and is unrelated to this rule.
NFR3 (Performance): Steady-state Cached Execution latency and throughput must be benchmarkable against Rhai as the primary comparator, reporting UDS and TCP/TLS numbers separately. No numeric target is fixed pre-benchmark — the harness is in scope, the target number is not.
NFR4 (Reliability): A single script's failure — panic, budget violation, malformed input — must not take down the daemon or disturb other connections' in-flight executions.
NFR5 (Concurrency — execution): Message handling and execution are asynchronous end to end; no execution request may block message processing or another execution request, on the same connection or a different one.
NFR6 (Concurrency — Global Variables): Global Variable locking is per-top-level-key, not per-Plugin, so one handler mutating one key must not stall a concurrent handler on a different key of the same variable.
NFR7 (Operability): The daemon runs as standard infrastructure (systemd unit or Docker container) and is operable — health, metrics, logs — without a per-backend config file existing anywhere.

### Additional Requirements

*From the Architecture Spine (AD-1…AD-7, Consistency Conventions, Stack, Structural Seed) and the PRD addendum. These are binding structural constraints on how stories are implemented, not separate features.*

**Starter template:** NONE. The Architecture specifies no starter/greenfield template — Hexput v2 is a from-scratch Rust workspace with a prescribed crate graph. Epic 1 Story 1 must therefore create the root `Cargo.toml` and every member crate's skeleton from the Spine's Structural Seed rather than instantiate a template. There is no v1 code in this repository to port or reference.

- **AD-1 (transport-agnostic core):** every Transport is an adapter implementing one internal `Port` interface; the Core crate graph never imports or branches on a transport type — only `hexput-daemon` depends on `hexput-transport`. Health/metrics (FR-11) is served through that same `Port`, exempted from the init-handshake gate — never a fifth listener.
- **AD-2 (Session outlives Connection):** `Session` is keyed by Client ID with a TTL from System Config; it may have zero or more concurrently attached `Connection` actors — never exactly one. Connections hold no state that outlives them, requests/responses correlate to the issuing Connection with no cross-Connection broadcast, and the FR-13 reconnect credential is validated once in `hexput-session` — never in an adapter.
- **AD-3 (one shared Executor):** Direct Execution, Cached Execution, and Plugin Event handlers all call one `Executor` entry point; no path reaches a Registered Function or consumes budget outside it. `async = true` handlers receive a live budget-accounting handle that stays live past dispatch return. Global Variable access is intrinsic language state — exempt from capability grants, reached only via `Executor`/`Plugin → GVStore`, never via `hexput-rpc` — `hexput-globalvar` does not depend on it. Both Plugin dispatch paths invoke the same single handler-invocation function in `hexput-exec`.
- **AD-4 (Global Variable store independence):** the store is a shared concurrent per-Plugin structure reachable without the Plugin actor being alive. Locking is per-`(top-level-key, partition-key)`; `keyed` partitions never contend. No Global Variable lock is held across an `.await`. `hexput_globalvar::teardown(plugin_id)` is the sole store-lifecycle entry point, called only by `hexput-session`, synchronously, before the actor drops — never implied by `Drop`. The Plugin actor owns routing/ordering metadata only; `priority` chains run in a per-(Plugin, Event) sequencing task spawned outside the actor mailbox.
- **AD-5 (Config surfaces are disjoint):** System Config is file-based and daemon-only; per-backend Config never touches disk — `hexput-config` and `hexput-session` are separate crates with no dependency on each other. `hexput-session` holds the single mutable live copy; `hexput-plugin`, `hexput-script`, and `hexput-exec` read through it on every dispatch and never snapshot it at registration time.
- **AD-6 (non-blocking dispatch is structural):** every execution dispatches as an independent async task; the only permitted serialization is a Plugin's own opted-in `priority` ordering among its own handlers for one Event.
- **AD-7 (System Config discovery):** one fixed precedence regardless of packaging — CLI flag, then environment variable, then a fixed default OS path. systemd units and Docker images set the env var or bind-mount the default path; they never change resolution logic.
- **Locking discipline (Consistency Convention):** no lock of any kind is ever held across an `.await` point; critical sections stay short and synchronous, re-acquiring after suspension.
- **Naming (Consistency Convention):** code identifiers match PRD §3 Glossary terms verbatim (`Session`, `Plugin`, `GlobalVariable`, `Capability`, `Client ID`, `Registered Function`, `Resource Budget`) — no synonyms.
- **Wire format (Consistency Convention):** MessagePack via `rmp-serde`/`serde` for all Transports; Client ID and error shapes defined once in `hexput-shared`/`hexput-port` and reused by every adapter.
- **Logging (Consistency Convention):** structured `tracing` throughout, every entry tagged with Client ID.
- **Crate graph (Structural Seed):** a Cargo workspace, one crate per module — `hexput-transport`, `hexput-port`, `hexput-session`, `hexput-connection`, `hexput-script`, `hexput-check`, `hexput-plugin`, `hexput-globalvar`, `hexput-exec`, `hexput-enforce`, `hexput-rpc`, `hexput-config`, `hexput-daemon` — plus `hexput-shared` and the language crates (`hexput-ast`, `hexput-lexer`, `hexput-parser`, `hexput-interpreter`) that sit underneath them, and `hexput-grammar`/`hexput-lsp-core` for tooling. Every crate is a `lib`; only `hexput-bin` produces binaries. A crate boundary is a compiler-enforced dependency edge, not a convention — an AD's "only X reaches Y" rule is literal: the crate that must not reach Y does not depend on it, and cannot.
- **AD-8 (one check pass):** `hexput-check` is a pure entry point — parsed AST plus callable-name set plus policy in, findings out. It has no dependency on `hexput-interpreter`, `hexput-rpc`, or `hexput-enforce`, so it cannot execute or reach the host even by mistake. It is invoked once per submission path: Direct Execution and `CodeRegister` in `hexput-script`, Plugin registration in `hexput-plugin` — never on `CachedExecutionStart`. Passing the check grants nothing; enforcement stays `hexput-enforce`'s alone via `hexput-exec`.
- **Pinned stack:** Rust 1.98.1 (2024 edition), tokio 1.53.1, tokio-tungstenite 0.30.0, rustls 0.23.45, serde 1.0.229, rmp-serde 1.3.1, dashmap 6.2.1, moka 0.12.16, criterion 0.8.2, tracing 0.1.44.
- **Benchmark harness (SM-1):** a Criterion-based harness measuring steady-state Cached Execution p50/p99 and throughput against Rhai, reporting UDS and TCP/TLS separately, plus cold-parse vs. cache-hit cost. The harness ships; the comparative numbers are a thesis deliverable, not a launch gate.
- **Deployment packaging:** systemd unit and/or Docker image; environment profiles, supervision, and upgrade/rollback are explicitly deferred by the Spine and must not be invented inside a story.
- **Deliberately out of scope:** OS-level sandboxing (permanent), transactional/rollback RPC side effects, master-slave horizontal scaling, LSP semantic features, Phase 2 SDKs, published benchmark results.

### Resolved Open Questions (decided during epic breakdown, 2026-09-18)

*PRD §9 left these open. Resolved here with user authority — OQ-1 decided by the user directly, OQ-2/OQ-3/OQ-11 delegated to the PM role ("solve it yourself"). These are binding on story acceptance criteria; the PRD's [ASSUMPTION] tags on FR-22/FR-23 are now settled, not assumed.*

- **OQ-1 → RESOLVED (user): health and metrics are RPC message types, not an HTTP endpoint.** `HealthCheck` and `MetricsScrape` are ordinary `hexput-port` message types served pre-init-gate (no completed handshake, no authenticated Backend required), consistent with AD-1's prohibition on a structurally separate listener. `MetricsScrape` returns a Prometheus text-exposition-format payload as its response body so standard tooling can consume it through a thin bridge; the daemon itself never opens an HTTP listener. Per-budget-dimension violation counters are separate series (FR-8, FR-11).
- **OQ-2 → RESOLVED (PM): reconnect protection is a daemon-issued reconnect secret paired with the Client ID.** On first init the daemon generates a high-entropy (>=256-bit) secret from a CSPRNG and returns it alongside the Client ID exactly once; it persists only a salted hash of that secret in the `Session`, never the secret itself. A reconnect message carries Client ID + secret; `hexput-session` verifies it in constant time and rejects mismatches with a defined reconnect-denied error that is indistinguishable between "unknown Client ID" and "wrong secret". The secret lives for the Session's lifetime; rotation is out of scope for v2. Validation happens once in `hexput-session` for all four Transports (AD-2) — no adapter validates anything.
- **OQ-3 → RESOLVED (PM): the execution-policy feature toggles are a fixed, closed set.** Togglable constructs (each independently on/off, all defaulting to on, settable in Config and overridable per execution per FR-3): `loops` (for/while), `conditionals` (if/else), `callbacks` (user-defined and anonymous function definition and invocation), `object_literals`, `array_literals`, and `rpc_calls` (a blanket switch disabling all Registered Function invocation regardless of Capability grants). Always-on and never togglable: variable declaration and assignment, scalar literals, arithmetic/comparison/logical operators, property and index access on existing values, `return`, and Plugin Global Variable access — disabling any of these would leave the language unable to express or report anything. Using a disabled construct raises the FR-3 "construct disabled by policy" error naming the toggle, distinct from a budget violation (FR-8) or capability denial (FR-6/FR-7).
- **OQ-11 → RESOLVED (PM): ordering conventions confirmed as the PRD's assumed defaults.** `priority` is ascending — lower value runs first. Absent `priority`, handlers for one Event run in source-declaration order within the Plugin. `async = true` combined with `priority` resolves in favor of `async`: the handler is dispatched concurrently and its `priority` value is ignored (not rejected), since a handler that never waits its turn has no ordering position to occupy.
- **OQ-5 → DEFERRED (user): JavaScript vs. Node.js SDK differentiation is not resolved now.** The user's call: "not much actually, we will see it while building." FR-10 Phase 1 therefore proceeds as *one* JavaScript SDK targeting the Phase 1 bar; whether a separate Node.js SDK is warranted, and on what axis (transport reach vs. API shape), is settled during SDK implementation and is not a launch gate.
- **OQ-6 (scope-pruning cadence) and the §4.7 observability delegation note carry forward unresolved** — neither blocks epic or story construction.

### UX Design Requirements

_None. Hexput v2 is a headless daemon with no user interface, and no UX design contract exists in the planning artifacts. The developer-facing tooling requirements (FR-14 tree-sitter grammar, FR-15 LSP) are captured as functional requirements above, not UX requirements._

### FR Coverage Map

FR-1: Epic 2 - Init handshake carrying inline Config + function registrations; no per-backend config file
FR-2: Epic 5 - Reconnect on an existing Client ID without resending Config; multiple concurrent connections per ID
FR-3: Epic 3 - Runtime Config edit, per-execution override, and the closed set of language feature toggles
FR-4: Epic 2 - Direct Execution: one-shot parse-and-run with no AST Cache entry
FR-5: Epic 4 - Cached Execution: CodeRegister once, CachedExecutionStart many with fresh variables
FR-6: Epic 3 - Function registration separate from per-call allow decision (context.allow / return true)
FR-7: Epic 3 - No ambient host access; every reach outside the script is a granted Registered Function
FR-27: Epic 3 (Daemon side) and Epic 8 (SDK `registerMethod`) - Registered Methods on keyed objects over the generic `Call` message
FR-28: Epic 3 (Daemon side) and Epic 8 (SDK handling of Value Secrets and modifications) - Value Secret, Reference IDs, host-side modifications
FR-8: Epic 3 - Six-dimension Resource Budget, independently enforced, configurable and overridable
FR-9: Epic 5 - UDS, Named Pipe, TCP+TLS, WebSocket adapters with identical protocol semantics
FR-10: Epic 8 - Phase 1 client SDKs: JavaScript and Python
FR-11: Epic 7 - HealthCheck and MetricsScrape RPC messages, pre-init-gate, per-dimension counters
FR-12: Epic 7 - Structured logs for connection lifecycle, capability denials, budget violations, tagged by Client ID
FR-13: Epic 5 - Reconnect protection: daemon-issued secret validated once in `hexput-session`
FR-14: Epic 9 - Tree-sitter grammar for the Hexput language
FR-15: Epic 9 - Basic LSP: syntax diagnostics and completion
FR-16: Epic 2 - Non-blocking concurrent execution; independent async task per execution
FR-17: Epic 6 - Plugin registration as a protocol-distinct mode; multiple Plugins per Client ID
FR-18: Epic 6 - Reserved BackendRegisteredInit fired exactly once before the Plugin accepts other Events
FR-19: Epic 6 - Backend-declared Event names and return shapes, declared upfront at registration
FR-20: Epic 6 - Global Variable state with per-top-level-key locking and configurable strategies
FR-21: Epic 6 - Plugin lifecycle tied to Session TTL, surviving reconnect; disconnect/in-flight semantics
FR-22: Epic 6 - Handler ordering via priority, gated by Backend Config
FR-23: Epic 6 - async = true handlers bypass ordering and dispatch concurrently
FR-24: Epic 2 - File-based System Config with CLI > env > default-path discovery
FR-25: Epic 6 - Global Variable behavior strategies: forever, ttl, separate_each_trigger, keyed
FR-26: Epic 1 (the check pass itself) and Epic 3 (its Config mode and per-execution override)

**Coverage check:** all 28 FRs (FR-1...FR-28) are mapped to an owning epic; FR-26, FR-27 and FR-28 are split across two, since the check pass and its policy surface belong to different layers, and the Daemon and SDK sides of a wire feature ship in different epics. Epic 1 owns no FR directly — it is the enabling substrate the PRD assumes but never states as a requirement (the Hexput language itself), and is consumed by FR-4, FR-5, FR-14, FR-15, and FR-19.


## Epic List

*9 epics. The Architecture Spine is final and its crate boundaries are fixed, so epics are few and large, split only at genuine risk boundaries — an epic groups stories by user value, not 1:1 by crate, so most epics touch several crates in one coherent slice. Epics that would otherwise churn the same crates are consolidated: `hexput-transport`+`hexput-port`+`hexput-session` into Epic 5, `hexput-plugin`+`hexput-globalvar` into Epic 6. Dependencies flow strictly forward — no epic requires a later epic to function.*

### Epic 1: Hexput language core

A script author can write Hexput source and see it evaluated correctly — variables, conditionals, loops, callbacks, objects, and arrays — running it locally through a CLI eval harness with no daemon, socket, or backend involved. This makes the language itself testable and fuzzable on its own, ahead of any I/O, and gives Epics 4, 6, and 9 a parser and AST to build on rather than invent.
**FRs covered:** FR-26 (the check pass itself; its policy surface is Epic 3). Otherwise enabling substrate consumed by FR-4, FR-5, FR-14, FR-15, FR-19.

### Epic 2: Run a script over a socket

A Backend can start the daemon from a System Config file, connect over a Unix Domain Socket, hand over its Config and function registrations in a single init handshake, and receive the result of a one-shot script — without a per-backend config file existing anywhere. Establishes the `Port` boundary (AD-1), the single shared `Executor` every mode funnels through (AD-3), independent async dispatch per execution (AD-6), System Config discovery precedence (AD-7), and the `tracing` scaffolding Epic 7 later builds FR-12 on.
**FRs covered:** FR-24, FR-1, FR-4, FR-16

### Epic 3: Expose the host safely

A Backend can register the host functions — and, on keyed objects, the methods — a script may call, grant them either blanket at registration (`context.allow()`) or per call (`return true`), hand values across with hidden Value Secrets through which it tracks and changes them, and bound every execution independently across CPU time, memory, allocations, RPC calls, output size, and side-effect count — tuning those limits and the language feature toggles per Config or per execution. This epic is the entire trust boundary of the system; there is no layer behind it.
**FRs covered:** FR-6, FR-7, FR-8, FR-3, FR-26 (mode and override), FR-27, FR-28 (Daemon side)

### Epic 4: Execute hot logic fast

A Backend can register a script once and re-execute it many times with fresh variables served from the AST Cache, with no state bleeding between runs — and can prove the hot path is fast, via a Criterion harness measuring steady-state p50/p99 and throughput against Rhai with UDS and TCP/TLS reported separately (SM-1, NFR3).
**FRs covered:** FR-5

### Epic 5: Connect from anywhere, survive disconnects

A Backend reaches the daemon over TCP+TLS, WebSocket, or Named Pipe with semantics identical to UDS, and reconnects on a previously issued Client ID — proving authorization, not merely knowing the ID — resuming its stored Config and registrations, with several connections live concurrently under one ID. Realizes AD-2's Session/Connection cardinality end to end.
**FRs covered:** FR-9, FR-2, FR-13

### Epic 6: Stateful Plugins driven by events

A Backend can register a Plugin — metadata block, top-level Global Variables, `@Event`-annotated handlers — declaring upfront every Event name it may fire and each Event's return shape, then fire those Events to drive handlers that share persistent state across invocations under the configured locking and lifetime strategies, ordered by `priority` or bypassing ordering with `async`. Plugin state survives reconnect and is torn down on Session TTL expiry or explicit unregister.
**FRs covered:** FR-17, FR-18, FR-19, FR-20, FR-25, FR-21, FR-22, FR-23

### Epic 7: Operate the daemon in production

An operator can confirm the daemon is alive and serving, scrape execution metrics with per-budget-dimension violation counters through the same RPC Port without completing an init handshake, and reconstruct what any given Client ID did — capability denials and budget violations each independently identifiable — from structured logs alone, with no separate audit subsystem. Also delivers the systemd unit and container image the MVP scope commits to.
**FRs covered:** FR-11, FR-12 (plus PRD §6.1 packaging, which carries no FR id)

### Epic 8: Connect from JavaScript and Python

A backend engineer working in JavaScript or Python can install a thin client SDK and perform the full Phase 1 surface idiomatically — connect, send init Config and registrations, run Direct and Cached Executions, receive results, and reconnect via Client ID with its reconnect credential — without hand-writing MessagePack frames.
**FRs covered:** FR-10, FR-27 and FR-28 (SDK side)

### Epic 9: Author Hexput scripts in an editor

A script author — often not the backend engineer running the daemon — gets syntax highlighting and folding from a tree-sitter grammar, and syntax-error diagnostics plus basic completion from a language server, in any LSP-compatible editor. Both reuse Epic 1's parser rather than reimplementing the grammar.
**FRs covered:** FR-14, FR-15


---

## Epic 1: Hexput language core

A script author can write Hexput source and see it evaluated correctly — variables, conditionals, loops, callbacks, objects, and arrays — running it locally through a CLI eval harness with no daemon, socket, or backend involved. This makes the language testable on its own, ahead of any I/O, and gives Epics 4, 6, and 9 a parser and AST to build on rather than invent.

*Normative definition: [LANGUAGE-REFERENCE.md](language/LANGUAGE-REFERENCE.md). Every story below implements that document — where a story and the reference disagree, the reference wins. Its `[DECISION]` markers are the choices made during the readiness gate that no earlier planning artifact recorded; confirm them before Story 1.2 starts.*

*Crate landing (Story 1.1 creates all of them empty): AST types → `hexput-ast` (1.3-1.5); lexer → `hexput-lexer` (1.2); parser → `hexput-parser` (1.3-1.5); evaluator → `hexput-interpreter` (1.6-1.7); diagnostics shape → `hexput-shared::diagnostics` (1.8); check pass → `hexput-check` (1.10); CLI → `hexput-cli-core` + the `hexput` binary in `hexput-bin` (1.9-1.10). `hexput-check` depends only on `hexput-ast` — it is built here but never executes, by construction (AD-8).*

### Story 1.1: Project scaffold and pinned toolchain

As a Hexput contributor,
I want a buildable Cargo workspace with the Architecture Spine's full crate graph and pinned dependency versions in place,
So that every later story lands in its architecturally correct crate instead of inventing a layout, and a dependency an Architecture Decision forbids is a compile error rather than something a reviewer has to catch.

**Acceptance Criteria:**

**Given** a clone of the repository at the v2 branch with no `Cargo.toml`
**When** I run `cargo build` and `cargo test` at the workspace root
**Then** both succeed on Rust 1.98.1 with edition 2024, and every member crate listed in the Spine's Structural Seed exists: `hexput-shared`, `hexput-ast`, `hexput-lexer`, `hexput-parser`, `hexput-interpreter`, `hexput-check`, `hexput-cli-core`, `hexput-transport`, `hexput-port`, `hexput-session`, `hexput-connection`, `hexput-script`, `hexput-plugin`, `hexput-globalvar`, `hexput-exec`, `hexput-enforce`, `hexput-rpc`, `hexput-config`, `hexput-daemon`, `hexput-grammar`, `hexput-lsp-core`, and `hexput-bin`

**Given** the workspace's `Cargo.toml`
**When** it is inspected
**Then** `[workspace.dependencies]` pins tokio 1.53.1, tokio-tungstenite 0.30.0, rustls 0.23.45, serde 1.0.229, rmp-serde 1.3.1, dashmap 6.2.1, moka 0.12.16, criterion 0.8.2, and tracing 0.1.44 exactly once, and every member crate that uses one of them inherits it with `workspace = true` rather than re-pinning a version

**Given** every crate but `hexput-bin`
**When** its `Cargo.toml` is inspected
**Then** it declares `[lib]` and no `[[bin]]` target — `hexput-bin` is the only crate in the workspace producing a binary, and it does so only via `src/bin/hexput-daemon.rs`, `src/bin/hexput.rs`, and `src/bin/hexput-lsp.rs`, each a thin `main()` that parses arguments and calls into `hexput-daemon`, `hexput-cli-core`, or `hexput-lsp-core` respectively, with no other logic in `hexput-bin`

**Given** the crate dependency graph the Spine defines
**When** each crate's `Cargo.toml` `[dependencies]` is inspected
**Then** it matches the Spine's graph exactly for the five AD-enforcing edges: `hexput-enforce` is a dependency of `hexput-exec` and of no other crate; `hexput-globalvar` depends on neither `hexput-plugin` nor `hexput-rpc`; `hexput-check` depends on `hexput-ast` but not `hexput-interpreter`, `hexput-rpc`, or `hexput-enforce`; `hexput-session` and `hexput-config` do not depend on each other; and only `hexput-daemon` depends on `hexput-transport` (AD-1, AD-3, AD-4, AD-5, AD-8)
**And** attempting to add any of those forbidden edges fails `cargo build` with an unresolved-import error, not merely a lint — the boundary is structural, not advisory

**Given** the workspace
**When** CI runs
**Then** `rustfmt` and `clippy` run clean with warnings denied across every member crate
**And** CI fails on any `unsafe` block introduced in `hexput-lexer`, `hexput-parser`, `hexput-interpreter`, or `hexput-exec` unless it carries a reviewed `SAFETY:` justification, so the memory-safety property NFR2 depends on is enforced mechanically rather than by reviewer memory (NFR2)

**Given** every crate in the workspace
**When** a reviewer inspects its `lib.rs` doc comment
**Then** it states that crate's responsibility in the Spine's own words and names the ADs binding it

### Story 1.2: Tokenize Hexput source

As a script author,
I want my source text turned into a token stream that records where each token came from,
So that later parse errors can point at the exact line and column of my mistake.

**Acceptance Criteria:**

**Given** a source string containing identifiers, number/string/boolean/null literals, operators, punctuation, and comments
**When** the lexer runs
**Then** it produces a token stream where every token carries a byte offset, line, and column
**And** comments and insignificant whitespace are discarded without shifting the recorded positions of surrounding tokens

**Given** a source string containing an unterminated string literal or an unrecognized character
**When** the lexer runs
**Then** it returns a lexical error naming the offending position, rather than panicking or silently skipping the input

**Given** an empty source string
**When** the lexer runs
**Then** it returns an empty token stream and no error

### Story 1.3: Parse expressions, declarations, and member access

As a script author,
I want variable declarations, assignments, literals, operators, and property/index access parsed into an AST,
So that the smallest useful Hexput program has a structural representation to evaluate.

**Acceptance Criteria:**

**Given** source declaring variables with `let`, assigning to them, and combining values with arithmetic, comparison, and logical operators
**When** the parser runs
**Then** it produces an AST whose operator nodes respect standard precedence and associativity
**And** every AST node carries the source span of the text it came from

**Given** source reading a property with dot notation and an element with index notation, including chained access and the optional forms `a?.b` and `a?.[b]`
**When** the parser runs
**Then** the AST represents the access chain in evaluation order and marks each link as optional or not

**Given** source with a syntax error such as a missing semicolon, an unclosed parenthesis, or a dangling operator
**When** the parser runs
**Then** it returns a parse error identifying the span and what was expected, and does not panic

### Story 1.4: Parse conditionals and loops

As a script author,
I want `if`/`else` branching and loop constructs parsed,
So that I can express the branching business rules Hexput exists to run.

**Acceptance Criteria:**

**Given** source containing an `if` statement with and without an `else` branch, including nested and chained forms
**When** the parser runs
**Then** each branch's body is represented as its own block node with correct nesting

**Given** source containing the language's loop constructs, including a loop body that declares its own variables
**When** the parser runs
**Then** the AST distinguishes the loop's condition/iteration clause from its body

**Given** a malformed conditional or loop, such as a missing condition or an unclosed body
**When** the parser runs
**Then** a parse error naming the construct and span is returned

### Story 1.5: Parse functions, callbacks, objects, and arrays

As a script author,
I want to define functions and callbacks and write object and array literals,
So that I can structure logic and data rather than writing one flat expression.

**Acceptance Criteria:**

**Given** source defining a named function and an anonymous callback, each with parameters and a `return`
**When** the parser runs
**Then** the AST records each function's parameter list, body block, and whether it is named or anonymous

**Given** source containing a nested object literal and an array literal, including an object holding arrays and vice versa
**When** the parser runs
**Then** the AST preserves key order for object literals and element order for array literals at every depth

**Given** source passing a callback as an argument to another call
**When** the parser runs
**Then** the callback is represented as a value expression in the argument position, not as a statement

### Story 1.6: Evaluate expressions and variable scope

As a script author,
I want my declarations, operators, and member accesses to produce correct values at runtime,
So that a Hexput script computes the answer I intended.

**Acceptance Criteria:**

**Given** a parsed script declaring variables and combining them with arithmetic, comparison, and logical operators
**When** the interpreter evaluates it
**Then** the resulting value matches LANGUAGE-REFERENCE.md §4 for each operator, including short-circuit behavior and the fact that `&&` and `||` return an operand rather than a `bool`

**Given** expressions mixing types — `"Total: " + 5`, `"10" - 1`, `true * 3`, `0 == false`, `[] == false`
**When** the interpreter evaluates them
**Then** each result matches LANGUAGE-REFERENCE.md §4.2 and §4.3 exactly, with the cross-type equality cases evaluating to `false` rather than JavaScript's coerced results

**Given** a conversion §4.2 does not perform — `"abc" * 2`, a collection in a numeric operand, or stringifying an array — or a division by zero
**When** the interpreter evaluates it
**Then** it raises the `type` or `arithmetic` error from LANGUAGE-REFERENCE.md §7 rather than yielding `NaN`, infinity, or a placeholder string

**Given** a script where an inner block declares a variable shadowing an outer one
**When** the interpreter evaluates it
**Then** the inner binding applies only within its block and the outer binding is intact afterward

**Given** a script reading an undeclared variable, or accessing a property of `null` without `?.`
**When** the interpreter evaluates it
**Then** each returns a defined `reference` error naming the offending span, and the interpreter itself does not panic

**Given** a script reading an absent object key, reading an array index outside its range, and assigning to an absent key
**When** the interpreter evaluates them
**Then** both reads yield `null` and the write creates the key, per LANGUAGE-REFERENCE.md §7 — absent data is `null`, while the two cases above are script errors

**Given** expressions using optional access — `order.customer?.name` where `customer` is `null`, and `a?.b.c.d` where `a` is `null`
**When** the interpreter evaluates them
**Then** each yields `null`, the rest of the chain after a short-circuited `?.` is never evaluated, and `?.` suppresses only `null` access — never a `type` error (LANGUAGE-REFERENCE.md §4.4)

### Story 1.7: Execute control flow, functions, and callbacks

As a script author,
I want branches, loops, and function calls to actually run,
So that a complete rule — not just an expression — can be evaluated.

**Acceptance Criteria:**

**Given** a script whose `if`/`else` branches assign different values
**When** the interpreter evaluates it
**Then** only the taken branch's effects are observable

**Given** conditions holding each falsy value — `null`, `false`, `0`, `""`, an empty array, an empty object — and truthy counterparts of each type
**When** `if` and `while` evaluate them
**Then** the branch is taken exactly per LANGUAGE-REFERENCE.md §4.1, with empty collections falsy

**Given** a script with a loop that accumulates into a variable and one that exits early
**When** the interpreter evaluates it
**Then** the accumulated value and the iteration count match the expected semantics

**Given** a script defining a function and calling it with arguments, including recursion and a callback invoked by another function
**When** the interpreter evaluates it
**Then** parameters bind per invocation with no leakage between calls, and `return` yields control to the caller with the returned value

**Given** a script with unbounded recursion
**When** the interpreter evaluates it
**Then** it terminates with a defined runtime error rather than overflowing the host stack

### Story 1.8: Report errors with precise source locations

As a script author,
I want lexical, parse, and runtime errors reported with the line, column, and offending snippet,
So that I can fix my script without guessing where it broke.

**Acceptance Criteria:**

**Given** any lexical, parse, or runtime error produced by Stories 1.2 through 1.7
**When** the error is rendered
**Then** it carries a machine-readable category, a stable code, a human message, and a source span with line and column

**Given** an error span
**When** it is rendered for a terminal
**Then** the output shows the offending source line with the span marked, and multi-line spans are rendered without truncating the location information

**Given** an error produced by the language core
**When** a caller inspects it programmatically
**Then** the span and category are accessible as structured fields, not only as formatted text, so Epic 9's language server can reuse them

### Story 1.9: Evaluate a script from the command line

As a script author,
I want to run a Hexput file from the terminal and see its result,
So that I can develop and check my logic without a daemon, a socket, or a backend application.

**Acceptance Criteria:**

**Given** a file containing a valid Hexput script
**When** I run the CLI eval command against that file
**Then** the script's result value is printed and the process exits zero

**Given** a file containing a script with a syntax or runtime error
**When** I run the CLI eval command against it
**Then** the Story 1.8 error rendering is printed to stderr and the process exits non-zero

**Given** a script that expects input variables
**When** I supply them on the command line
**Then** they are bound as the script's starting variables before evaluation

### Story 1.10: Check a script without running it

As a script author,
I want my script inspected for mistakes before it is ever executed,
So that a typo'd variable or a wrong argument count is caught at the moment I submit it, not halfway through a rule that has already called back into the host.

**Acceptance Criteria:**

**Given** a parsed AST
**When** the check pass runs
**Then** it reports undeclared identifier reads and assignments, duplicate `let` in one block, `break`/`continue` outside a loop, wrong argument counts against functions declared in the same script, literal-operand type errors such as `"abc" * 2`, and code unreachable after `return`/`break`/`continue` (FR-26, LANGUAGE-REFERENCE.md §10)

**Given** an unused local variable
**When** the check pass runs
**Then** it is reported as a warning and never as an error, so a warning-only finding can never reject a script

**Given** the check pass and a list of names the caller says are callable — Registered Function names when the daemon runs it, empty when the CLI does
**When** a script calls a name that is neither a local function nor in that list
**Then** the call is reported as a finding, and the caller supplying no list means the finding is simply not raised rather than every host call being flagged

**Given** any finding
**When** it is rendered
**Then** it carries the same category, stable code, message, and source span as any runtime error (Story 1.8), so the CLI, a Backend response, and a language server can render it identically

**Given** the check pass's entry point
**When** a reviewer inspects it
**Then** it is a single pure function taking a parsed AST, a callable-name set, and the active policy, returning findings — it never executes the script, `hexput-check` has no dependency on `hexput-rpc` or `hexput-enforce` so it cannot reach them, and it holds no state between calls (AD-8)

**Given** a script with no findings, and one whose findings are all warnings
**When** the pass completes
**Then** it reports success in both cases and distinguishes "clean" from "warnings only" in its result

**Given** the CLI from Story 1.9
**When** I run its check command against a file
**Then** findings are printed with the Story 1.8 rendering, the exit code is non-zero only when an error-severity finding exists, and no part of the script is executed

---

## Epic 2: Run a script over a socket

A Backend can start the daemon from a System Config file, connect over a Unix Domain Socket, hand over its Config and function registrations in a single init handshake, and receive the result of a one-shot script — without a per-backend config file existing anywhere. Establishes the `Port` boundary (AD-1), the single shared `Executor` (AD-3), independent async dispatch (AD-6), and System Config discovery precedence (AD-7).

### Story 2.1: Start the daemon from a System Config file

As an operator,
I want the daemon to read its operational settings from one config file found by a single documented precedence,
So that a systemd unit and a Docker image behave identically without either inventing its own discovery logic.

**Acceptance Criteria:**

**Given** a valid System Config file supplying transport bind addresses, TLS certificate paths, log level, and default Session TTL
**When** the daemon starts
**Then** it loads those values and reports the resolved config file path in its startup log (FR-24)

**Given** a System Config path supplied by CLI flag, an environment variable, and a file at the fixed default OS path, all present at once
**When** the daemon starts
**Then** the CLI flag wins, then the environment variable, then the default path — and this precedence is identical regardless of packaging (AD-7)

**Given** a System Config that is missing entirely, or is present but malformed, or omits a required field
**When** the daemon starts
**Then** it exits non-zero with an error naming the file and the specific problem, and never falls back to an undocumented default for a required field (FR-24)

**Given** a running daemon
**When** a Backend changes its per-backend execution policy
**Then** no System Config file is read, written, or reloaded (AD-5)

### Story 2.2: Frame requests and responses on the wire

As a Backend developer,
I want one MessagePack envelope that correlates every request with its response,
So that my client can multiplex calls on a single connection without ambiguity about what answered what.

**Acceptance Criteria:**

**Given** the `hexput-port` envelope definition
**When** a request is encoded and decoded
**Then** it round-trips through MessagePack with its correlation id, message type, and payload intact (rmp-serde/serde)

**Given** two requests issued on one connection before either has answered
**When** their responses arrive in either order
**Then** each response is matched to its originating request by correlation id

**Given** a malformed, truncated, or unknown-type frame
**When** the Port decodes it
**Then** it produces a defined protocol error response rather than dropping the connection silently or panicking (NFR4)

**Given** the envelope, Client ID representation, and error shapes
**When** any transport adapter is added later
**Then** it reuses these definitions from `hexput-shared`/`hexput-port` rather than declaring its own (AD-1, Consistency Conventions)

### Story 2.3: Accept connections over a Unix Domain Socket

As a Backend running on the same machine as the daemon,
I want to connect over a Unix Domain Socket,
So that I get the lowest-latency path to the runtime without touching the network stack.

**Acceptance Criteria:**

**Given** a System Config specifying a UDS path
**When** the daemon starts
**Then** it creates and listens on that socket, and removes a stale socket file left by a previous unclean shutdown

**Given** a listening daemon
**When** a Backend connects over the socket and sends a framed message
**Then** the adapter hands it to the core through the `Port` interface, and the core performs no transport-specific branching to handle it (AD-1)

**Given** several Backends connected over the socket simultaneously
**When** one of them disconnects abruptly
**Then** the others are unaffected and the daemon keeps serving (NFR4)

### Story 2.4: Complete the init handshake with inline config and registrations

As a Backend,
I want to send my execution policy and the host functions I expose as part of connecting,
So that I never have to place a configuration file anywhere for the daemon to read.

**Acceptance Criteria:**

**Given** a fresh connection with no prior Client ID
**When** it sends an init message carrying its Config and function registrations
**Then** the daemon creates a Session keyed by a newly issued Client ID, stores the Config and registrations on that Session, and returns the Client ID (FR-1)

**Given** a fresh connection that has not completed init
**When** it sends an execution request
**Then** the daemon rejects it with a defined "init not completed" error and does not execute anything (FR-1)

**Given** an init message missing its Config or its registrations
**When** the daemon processes it
**Then** the init is rejected with an error naming what was missing, and no Session is created (FR-1)

**Given** a stored per-backend Config
**When** it is inspected at runtime
**Then** it lives only in `hexput-session` as a single live copy, is never written to disk, and no other crate holds a snapshot of it (AD-5)

### Story 2.5: Attach connections to a session that outlives them

As a Backend,
I want my Session to be a distinct thing my connection attaches to rather than the connection itself,
So that my registered state is structurally able to survive a dropped socket.

**Acceptance Criteria:**

**Given** a Session created by init
**When** its attached Connection drops
**Then** the Session and its stored Config and registrations continue to exist, and the Connection actor's own state is discarded with it (AD-2)

**Given** a Session with more than one Connection attached
**When** a request is answered
**Then** the response goes only to the Connection that issued it, with no broadcast to sibling Connections (AD-2)

**Given** the Session data model
**When** a reviewer inspects it
**Then** a Session holds zero or more attached Connections — the type system does not encode "exactly one" — and each Connection is attached to at most one Session (AD-2)

**Given** a Session whose last attached Connection drops, at this epic's stage where reconnect does not yet exist
**When** the detach is processed
**Then** the Session is torn down and its Config, registrations, and any cached state are released, so no Session can outlive every Connection indefinitely
**And** teardown runs through one explicit teardown path, never implied by `Drop`, so Story 5.8 can later delay that same path behind a TTL without relocating it (AD-4)

### Story 2.6: Run a one-shot script and get the result back

As a Backend,
I want to submit a script and receive its result without registering anything for reuse,
So that I can hand off user-authored logic the moment I have it.

**Acceptance Criteria:**

**Given** an initialized connection
**When** it submits a Direct Execution request carrying script source and starting variables
**Then** the daemon parses and evaluates the script through the single shared `Executor` entry point and returns the result on that connection (FR-4, AD-3)

**Given** a Direct Execution request
**When** it completes
**Then** no AST Cache entry has been created for that script (FR-4)

**Given** a script that fails to parse or fails at runtime
**When** it is submitted for Direct Execution
**Then** the daemon returns the structured error from Story 1.8 as a protocol error response, and the daemon and every other connection are unaffected (NFR4)

**Given** any execution path in the codebase
**When** a reviewer traces how it reaches script evaluation
**Then** it enters through the one `Executor` entry point, with no second path around it (AD-3)

### Story 2.7: Keep slow executions from blocking anything else

As a Backend,
I want a long-running execution to stay out of the way of everything else,
So that one expensive script can't stall my traffic or another tenant's.

**Acceptance Criteria:**

**Given** a slow execution in progress on a connection
**When** a fast execution is submitted on that same connection
**Then** the fast one returns its result without waiting for the slow one to finish (FR-16)

**Given** a slow execution in progress on one connection
**When** a fast execution is submitted on a different connection
**Then** the fast one returns without waiting, and message processing on both connections continues throughout (FR-16, NFR5)

**Given** several execution requests submitted concurrently on one connection
**When** they are dispatched
**Then** each runs as an independent async task on the shared runtime, with no per-connection serial queue anywhere in the path (AD-6)

**Given** any execution path
**When** a reviewer inspects its locking
**Then** no lock is held across an `.await` point (Consistency Conventions)

### Story 2.8: Trace every request back to its Client ID

As an operator,
I want every log line the daemon emits to be attributable to the Client ID that caused it,
So that Epic 7's audit requirements have a foundation rather than a retrofit.

**Acceptance Criteria:**

**Given** the daemon handling a request on an initialized connection
**When** it emits any log event during that request
**Then** the event carries the Client ID as a structured field via a `tracing` span, not as interpolated message text (FR-12 foundation, Consistency Conventions)

**Given** the configured log level from System Config
**When** the daemon starts
**Then** the `tracing` subscriber honors it, and structured JSON output is available as a configured option

**Given** a request handled before init completes, such as a rejected execution
**When** it is logged
**Then** the event is still emitted with the connection identified, marked as having no Client ID rather than omitting the field

---

## Epic 3: Expose the host safely

A Backend can register the host functions — and, on keyed objects, the methods — a script may call, grant them either blanket at registration (`context.allow()`) or per call (`return true`), hand values across with hidden Value Secrets through which it tracks and changes them, and bound every execution independently across CPU time, memory, allocations, RPC calls, output size, and side-effect count — tuning those limits and the language feature toggles per Config or per execution. This epic is the entire trust boundary of the system; there is no layer behind it.

### Story 3.1: Call a registered host function from a script

As a Backend,
I want a script to be able to call the host functions I registered and receive their return values,
So that user-authored logic can reach my data without reaching anything else.

**Acceptance Criteria:**

**Given** a Session whose init registered a host function by name
**When** a script calls that name with arguments
**Then** the daemon issues an outbound RPC to the Backend over the same connection, awaits the response, and resumes the script with the returned value (FR-6)

**Given** a script calling a registered function
**When** the Backend's response is an error
**Then** the script receives a defined runtime error attributable to that call, and the daemon does not treat it as its own failure (Brief: host-side errors are not Hexput's problem)

**Given** an outbound RPC in flight
**When** the script's execution is inspected
**Then** no lock is held across the await, and the execution occupies no thread while waiting (Consistency Conventions, AD-6)

**Given** a script calling a name that was never registered
**When** it executes
**Then** it fails with a capability-denied error rather than an unknown-identifier error, so unregistered and unauthorized are indistinguishable to the script

**Given** a host call
**When** the daemon sends it
**Then** it is one generic `Call` message `{name, arguments}` — the same message Registered Methods (Story 3.12) extend with `receiver` — and the Backend answers with `Result {value}` or `Error` under the call's daemon-issued id (FR-6, Spine 2026-09-24 amendment) **[Added 2026-09-24]**

**Given** a call the Backend answers with an error or a malformed reply, or one pending when the connection ends
**When** the script resumes
**Then** it fails with `host.function_failed` or `host.no_reply` respectively, spanned on the call — a `host` error, distinct from `capability` (LANGUAGE-REFERENCE §7, §8) **[Added 2026-09-24]**

**Given** a script whose own binding shares a registered name, or an argument that is a function, cyclic, or nested deeper than 12 levels
**When** it runs
**Then** the local binding is called and nothing is sent; an unsendable argument fails with a `type` error, and a too-deep one with `depth.argument_too_deep`, spanned on that argument, before anything is sent (LANGUAGE-REFERENCE §8; the limit becomes configurable in Story 3.7) **[Added 2026-09-24]**

### Story 3.2: Grant a function blanket access at registration

As a Backend,
I want to mark a registered function as callable by any script on my Session,
So that I don't pay a per-call round trip for functions that are safe by definition.

**Acceptance Criteria:**

**Given** a function registered with `context.allow()` at init
**When** any script on that Session calls it
**Then** the call proceeds with no per-call authorization round trip to the Backend (FR-6)

**Given** a blanket-allowed function
**When** the call is enforced
**Then** the grant is checked inside `hexput-enforce` via the shared `Executor`, not at the transport or registry layer (AD-3)

**Given** two Sessions where only one granted a given function
**When** a script on the other Session calls that name
**Then** it is capability-denied — grants never leak across Sessions

### Story 3.3: Decide per call whether a function may be used

As a Backend,
I want to evaluate each individual call against my own logic and allow or refuse it,
So that access can depend on who the script is acting for, not just which function it named.

**Acceptance Criteria:**

**Given** a function registered without a blanket allow
**When** a script calls it
**Then** the daemon asks the Backend's handler for that call, proceeds when the handler returns `true`, and returns a capability-denied error to the script otherwise (FR-6)

**Given** a per-call handler that returns a non-boolean, errors, or never answers
**When** the authorization is evaluated
**Then** the call is denied rather than allowed, and the denial is distinguishable in logs from an explicit refusal

**Given** a denied call
**When** the script continues
**Then** the denial is a catchable, defined script-level error naming the function — never a host-level exception surfacing into the script (FR-7)

### Story 3.4: Deny every path to the host that isn't a registered function

As a Backend operating in a multi-tenant daemon,
I want a script to have no way to touch the filesystem, network, or host memory except through what I registered,
So that the language itself is the sandbox, as designed.

**Acceptance Criteria:**

**Given** a script attempting to reference any filesystem, socket, process, environment, or host-memory capability
**When** it executes
**Then** it fails with a defined capability-denied error, because no such ambient binding exists in the language's global environment at all (FR-7)

**Given** the interpreter's global environment
**When** a reviewer enumerates every name reachable from a script
**Then** each is either a pure language builtin or a Registered Function for that Session — the enumeration is exhaustive and asserted by test (FR-7, NFR1)


### Story 3.5: Stop an execution that burns too much CPU or memory

As a Backend,
I want an execution killed when it exceeds the CPU time or memory I allotted it,
So that one pathological script cannot degrade the daemon for everyone else.

**Acceptance Criteria:**

**Given** an execution whose script loops or allocates without bound
**When** it exceeds its CPU time budget or its memory budget
**Then** it is terminated with an error naming which dimension was exceeded, and each dimension is reported distinctly (FR-8)

**Given** a terminated execution
**When** the daemon continues
**Then** other in-flight executions on the same and other connections complete normally, and the daemon does not panic or restart (NFR4)

**Given** budget enforcement
**When** a reviewer traces where it happens
**Then** it lives in `hexput-enforce`, reached only through the shared `Executor` — no other crate depends on `hexput-enforce`, so no second implementation can exist on any execution path (AD-3)

### Story 3.6: Bound allocations, RPC calls, output size, and side effects

As a Backend,
I want the remaining four budget dimensions enforced independently,
So that safety is a multi-dimensional budget rather than a single limit that misses the abuse I actually face.

**Acceptance Criteria:**

**Given** an execution exceeding its allocation count, RPC call count, output size, or side-effect count
**When** the limit is crossed
**Then** the execution terminates with an error naming that specific dimension (FR-8)

**Given** the four dimensions in this story
**When** their definitions are fixed
**Then** each counts exactly what this criterion states, because FR-8 names them without defining them: **allocation count** is every heap-backed value the execution creates — each string, array, and object construction, including one per resize of a growing collection, and excluding scalars and rebindings; **RPC call count** is every outbound call to a Registered Function, counted on dispatch whether or not it is later denied or fails; **output size** is the serialized byte length of the execution's returned value; **side-effect count** is every operation whose effect outlives the execution — every Registered Function dispatch plus every committed Global Variable write (AD-3), so a call counts against both its own dimension and this one

**Given** those definitions
**When** they are implemented
**Then** the counters are asserted by tests that pin exact counts for a known script, so a later refactor cannot silently change what a dimension means

**Given** all six dimensions
**When** any one is configured and the others are not
**Then** the configured one is enforced independently, with no dimension collapsing into or substituting for another (FR-8, SM-C2)

**Given** an execution that exceeds its RPC call budget mid-script
**When** it terminates
**Then** the RPC calls already made stand — no compensation or rollback is attempted (PRD §4.4 Out of Scope)

### Story 3.7: Tune budgets per backend and per execution

As a Backend,
I want to set default budget limits for my Session and override them for a single execution,
So that an expensive report and a cheap rule check can coexist under one connection.

**Acceptance Criteria:**

**Given** budget values supplied in a Session's Config
**When** an execution runs without overrides
**Then** those values are the enforced limits (FR-8)

**Given** an execution request carrying budget overrides
**When** it runs
**Then** the overrides apply to that execution only, and the Session's stored Config is unchanged afterward (FR-3)

**Given** an override requesting a limit outside the daemon's allowed range
**When** it is submitted
**Then** it is rejected with a defined error rather than silently clamped

**Given** the argument depth limit for values sent in a `Call` (default 12, Story 3.1)
**When** a Config or per-execution override sets it
**Then** arguments and receivers nested past the configured depth fail with `depth.argument_too_deep` before anything is sent, and an out-of-range value is rejected like any other override (FR-27) **[Added 2026-09-24]**

### Story 3.8: Change execution policy without reconnecting

As a Backend,
I want to update my stored Config on a live connection,
So that I can change policy without dropping and re-establishing my Session.

**Acceptance Criteria:**

**Given** an initialized connection
**When** it sends a runtime Config update message
**Then** subsequent executions on that connection use the new values, with no reconnect required (FR-3)

**Given** a Session with several Connections attached
**When** one of them updates the Config
**Then** every attached Connection's subsequent executions see the update, because `hexput-session` holds the single live copy (AD-2, AD-5)

**Given** the crates that consume Config
**When** a reviewer inspects `hexput-script`, `hexput-plugin`, and `hexput-exec`
**Then** each reads through the `hexput-session` copy on every dispatch and none caches or snapshots it at registration time (AD-5)

### Story 3.9: Switch off language constructs by policy

As a Backend,
I want to disable specific language constructs for my scripts,
So that I can narrow what user-authored logic is even able to express.

**Acceptance Criteria:**

**Given** a Config or per-execution override disabling one of `loops`, `conditionals`, `callbacks`, `object_literals`, `array_literals`, or `rpc_calls`
**When** a script uses that construct
**Then** the execution fails with a "construct disabled by policy" error naming the toggle — distinct in type and code from a budget violation and from a capability denial (FR-3)

**Given** every toggle
**When** none are set
**Then** all six default to enabled (OQ-3 resolution)

**Given** a request to disable variable declaration, scalar literals, operators, property or index access, `return`, or Global Variable access
**When** it is submitted
**Then** it is rejected as an unknown toggle — the togglable set is closed and these constructs are always on (OQ-3 resolution)

**Given** `rpc_calls` disabled
**When** a script calls a function that holds a valid blanket capability grant
**Then** the call still fails by policy, because the toggle is evaluated independently of capability grants

### Story 3.10: Turn the static check on or off

As a Backend,
I want to decide whether submitted scripts are statically checked, and how strictly,
So that I can reject broken user-authored logic at submission time in development and pay nothing for it on a hot path in production.

**Acceptance Criteria:**

**Given** a Config setting the check mode to `off`
**When** a script is submitted
**Then** no check pass runs and execution proceeds straight from parsing — this is the default when the Backend sets nothing (FR-26, FR-3)

**Given** a Config setting the mode to `error`
**When** a script with an error-severity finding is submitted
**Then** the execution is rejected before any statement runs and before any Registered Function is called, and the findings are returned to the Backend (FR-26)

**Given** a Config setting the mode to `warn`
**When** a script with findings is submitted
**Then** it executes normally and the findings are returned alongside the result rather than replacing it (FR-26)

**Given** any of the three modes in Config
**When** an execution request carries a check-mode override
**Then** the override applies to that execution only and the stored Config is unchanged afterward (FR-3)

**Given** the check running inside the daemon
**When** it resolves which names are callable
**Then** it uses that Session's Registered Functions, so a call to an unregistered host function is reported as a finding instead of surfacing later as a `capability` error (FR-6, FR-26)

**Given** the check running in `error` mode against a script that also violates a language feature toggle (Story 3.9)
**When** it is submitted
**Then** the disabled-construct usage is reported as a check finding at submission time, with the same `policy` category the runtime failure would have used (FR-3)

**Given** a Cached Execution (Epic 4 registers scripts once)
**When** the check mode is `error`
**Then** the check runs at registration rather than on every execution and never again on `CachedExecutionStart`, so the hot path pays for it once (AD-8)

**Given** the check's outcome
**When** a script passes it
**Then** nothing about that pass grants a capability or reduces a budget charge — enforcement remains `hexput-enforce`'s alone through the `Executor`, unreachable from `hexput-check` (AD-3, AD-8)

### Story 3.11: Carry hidden metadata on values the script cannot touch

*[Added 2026-09-24 by course correction — sprint-change-proposal-2026-09-24.md.]*

As a Backend,
I want every value I hand a script, and every value a script hands me, to carry a Value Secret only I can read and edit,
So that I can identify and annotate values across the boundary without a script ever seeing or forging that metadata.

**Acceptance Criteria:**

**Given** a starting variable or a `Call` result sent as a holder `{__secret: {ref, key?, ...}, value}`
**When** the script runs
**Then** the value behaves exactly as its plain `value`, and its Value Secret — `ref`, optional `key`, and every further field — is kept intact (FR-28)

**Given** any value in a script
**When** the script reads `__secret` by `.`, `[]` or `?.`, writes it by assignment or an object-literal key, or iterates the object with `for … in`
**Then** reading yields `null`, writing is silently ignored, and iteration never yields `__secret` — no error, and no way to observe or change the metadata (LANGUAGE-REFERENCE §3)

**Given** a value sent to the Backend in a `Call`
**When** it, or any value nested inside it, has no Value Secret yet
**Then** it travels as a holder with a Daemon-generated Reference ID, and the same value sent again in that execution carries the same Reference ID (FR-28)

**Given** a referenced value
**When** the script copies it by binding (`let m = n;`) or computes a new value from it
**Then** a referenced object or array stays the same collection wherever it is held, while a copied or computed scalar or string is a plain value with no Value Secret; `==`, truthiness and conversion never see the Value Secret (LANGUAGE-REFERENCE §3)

**Given** an execution's result containing values with Value Secrets
**When** it is returned to the Backend
**Then** each travels as a holder with its Value Secret unchanged, and values without one travel plain, so Story 2.6's results stay valid

### Story 3.12: Call a Backend method on a keyed object

*[Added 2026-09-24 by course correction.]*

As a Backend,
I want to register methods under an object key and have scripts call them on objects I marked with that key,
So that host objects feel like objects with methods, as in other languages.

**Acceptance Criteria:**

**Given** an init registration naming a function with an object key (`registerMethod(objKey, fn)` in an SDK)
**When** a script calls `value.name(args)` on a value whose Value Secret carries that key
**Then** the daemon sends one `Call {name, receiver, arguments}` with the receiver itself, Value Secret included, and resumes the script with the returned value (FR-27)

**Given** a keyed value that also has an own property of the method's name
**When** the script calls it as a method
**Then** the Registered Method is called, not the property (LANGUAGE-REFERENCE §8)

**Given** a keyed value
**When** the script writes a property named like a Registered Method under its key
**Then** it fails with `capability.method_override` on the assignment target and the object is unchanged; with the static check on and the target a starting variable the environment declares keyed, the same mistake is an error finding before execution (FR-26, FR-27)

**Given** a keyed value and a name registered for no method under its key and held by no own property
**When** the script calls it
**Then** it fails with the same `capability` error as an unregistered function (FR-7, FR-27)

**Given** a value without a key
**When** the script calls `value.name(args)`
**Then** it is an ordinary property call, and nothing is sent to the Backend

**Given** a Registered Method
**When** its call is checked and counted
**Then** it obeys the same Capability grants (Stories 3.2, 3.3) and counts against RPC calls and side effects exactly like a plain Registered Function call (Story 3.6), and `rpc_calls` disabled (Story 3.9) blocks it too

### Story 3.13: Keep referenced values in step between Backend and script

*[Added 2026-09-24 by course correction.]*

As a Backend,
I want to change the values a script handed me while I handle its call, and to learn which of my values the script changed,
So that my state and the script's stay in step without a second round trip.

**Acceptance Criteria:**

**Given** a `Call` reply `Result {value, modifications: [{ref, value}]}`
**When** the daemon receives it
**Then** it applies every modification before the script resumes with `value` (FR-28)

**Given** a modification naming a referenced object or array
**When** it is applied
**Then** the collection changes in place and keeps its identity, so every binding and container holding it observes the new content (LANGUAGE-REFERENCE §4.2, §8)

**Given** a modification naming a referenced string, number, bool or `null`
**When** it is applied
**Then** the value at the location its Reference ID names — the variable, property or element it arrived in or was passed from — is replaced, and copies and values previously computed from it are unchanged

**Given** a starting variable `n` supplied as `8` with Reference ID `r1`, and the Script `n = 9; return { ok: true };`
**When** the execution finishes
**Then** its result is `{ ok: true }` with `modifications: [{ ref: "r1", value: 9 }]` — every referenced location the script wrote (a reassigned variable or property, a store into a referenced collection) is listed once with its final value, and unwritten ones are not (FR-28)

**Given** a modification naming a Reference ID the execution does not hold, or a malformed `modifications` list
**When** the reply arrives
**Then** an unknown Reference ID is ignored, and a malformed list fails the call with `host.function_failed`

---

## Epic 4: Execute hot logic fast

A Backend can register a script once and re-execute it many times with fresh variables served from the AST Cache, with no state bleeding between runs — and can prove the hot path is fast via a Criterion harness measuring steady-state latency and throughput against Rhai.

### Story 4.1: Register a script once for repeated use

As a Backend,
I want to hand the daemon a script once and get back a handle for it,
So that the parse cost is paid a single time instead of on every evaluation.

**Acceptance Criteria:**

**Given** an initialized connection
**When** it sends a `CodeRegister` request carrying script source
**Then** the daemon parses it, stores the resulting AST in the moka-backed AST Cache, and returns a handle identifying it (FR-5)

**Given** a `CodeRegister` request whose source fails to parse
**When** it is processed
**Then** the structured parse error is returned and no cache entry is created

**Given** a registered script
**When** its cache entry is inspected
**Then** it is scoped to the registering Session, and an identical source registered by a different Session does not collide with or reuse it

### Story 4.2: Re-run a registered script with fresh variables

As a Backend,
I want to execute a registered script repeatedly with different inputs,
So that a rule evaluated on every request costs a cache lookup plus interpretation, not a parse.

**Acceptance Criteria:**

**Given** a registered script handle
**When** a `CachedExecutionStart` is submitted with a set of variables
**Then** the daemon executes the cached AST without re-parsing and returns the result (FR-5)

**Given** two `CachedExecutionStart` calls against the same handle with different variables
**When** both complete
**Then** their results are independent, with no variable, state, or mutation bleeding from one into the other (FR-5)

**Given** a Cached Execution
**When** it runs
**Then** it passes through the same `Executor`, the same capability checks, and the same Resource Budget accounting as a Direct Execution (AD-3)

**Given** a `CachedExecutionStart` naming an unknown or evicted handle
**When** it is processed
**Then** it returns a defined "unknown script handle" error telling the Backend to re-register, rather than silently re-parsing anything

### Story 4.3: Keep the cache bounded and clean it up with the session

As an operator,
I want the AST Cache to stay within bounds and release memory when a Session ends,
So that long-running daemons don't grow without limit.

**Acceptance Criteria:**

**Given** a configured cache capacity
**When** registrations exceed it
**Then** entries are evicted by the cache's policy and an eviction is observable in metrics, while a subsequent execution against an evicted handle returns the defined unknown-handle error from Story 4.2

**Given** an explicit unregister for a script handle
**When** it is processed
**Then** the entry is removed immediately and its memory released

**Given** a Session being torn down
**When** teardown completes
**Then** every AST Cache entry belonging to that Session is released, with no entries surviving their owning Session

### Story 4.4: Measure the cache win

As a Hexput contributor,
I want a benchmark that separates first-parse cost from cache-hit cost,
So that the central design claim of the project is measured rather than asserted.

**Acceptance Criteria:**

**Given** the Criterion harness
**When** it runs
**Then** it reports cold first-parse time and warm cache-hit execution time as separate figures for the same script (SM-1, Brief addendum)

**Given** the harness
**When** it measures steady-state Cached Execution
**Then** it reports p50 and p99 latency and throughput under concurrent load, on a host-callback-heavy workload representative of the pass/fail-rule anchor case

**Given** the harness
**When** it is run twice on the same machine
**Then** its numbers are reproducible within a stated variance, and the workload and machine conditions are recorded alongside them

### Story 4.5: Compare the hot path against Rhai

As a Hexput contributor,
I want the same workload run against Rhai through the harness,
So that the project's performance claim has a like-for-like comparator.

**Acceptance Criteria:**

**Given** the benchmark harness
**When** it runs the comparator suite over UDS
**Then** it produces Hexput and Rhai numbers for the same workload under the same conditions, reported side by side (SM-1, NFR3)

**Given** the comparison run
**When** results are reported
**Then** resource-budgeting overhead is reported as a separate line item rather than folded into the headline number (Brief addendum)

**Given** the harness exists and produces numbers
**When** launch readiness is assessed
**Then** SM-1 is satisfied by the harness itself — no specific latency figure is a gate, and no correctness of capability or budget enforcement is traded for a better number (SM-C1, NFR3)

---

## Epic 5: Connect from anywhere, survive disconnects

A Backend reaches the daemon over TCP+TLS, WebSocket, or Named Pipe with semantics identical to UDS, and reconnects on a previously issued Client ID — proving authorization, not merely knowing the ID — resuming its stored Config and registrations, with several connections live concurrently under one ID.

### Story 5.1: Connect remotely over TCP with mandatory TLS

As a Backend running on a different machine from the daemon,
I want to connect over TCP with TLS always on,
So that my scripts and my host data never cross the network in the clear.

**Acceptance Criteria:**

**Given** a System Config supplying a bind address and TLS certificate and key paths
**When** the daemon starts
**Then** it listens on that address and accepts only TLS connections via rustls (FR-9, FR-24)

**Given** a client attempting a plaintext TCP connection to that port
**When** the handshake is attempted
**Then** it is refused — TLS is not negotiable or optional for any non-local transport (FR-9)

**Given** missing, unreadable, or malformed certificate files
**When** the daemon starts
**Then** it fails to start with an error naming the file and problem, rather than silently listening without TLS (FR-24)

**Given** an established TLS connection
**When** it sends framed messages
**Then** the adapter passes them through the same `Port` interface, and the core does not branch on the fact that this is TCP (AD-1)

### Story 5.2: Connect over WebSocket

As a Backend constrained to HTTP-friendly infrastructure,
I want to reach the daemon over a WebSocket,
So that proxies and gateways between me and the daemon don't block my connection.

**Acceptance Criteria:**

**Given** a System Config enabling the WebSocket transport
**When** a client opens a WebSocket connection via tokio-tungstenite and sends a framed message
**Then** the daemon handles it through the same `Port` with the same MessagePack envelope as every other transport (FR-9, AD-1)

**Given** a remote WebSocket connection
**When** it is established
**Then** TLS is enforced exactly as for TCP, since it is a non-local transport (FR-9)

**Given** a WebSocket connection dropped by an intermediary
**When** the drop is detected
**Then** the Connection is detached from its Session and the Session survives, exactly as for any other transport (AD-2)

### Story 5.3: Connect locally on Windows over a Named Pipe

As a Backend running on Windows on the same machine as the daemon,
I want a native local IPC path,
So that I am not forced onto TCP just to talk to a daemon on my own machine.

**Acceptance Criteria:**

**Given** a Windows host and a System Config naming a pipe
**When** the daemon starts
**Then** it creates and listens on that Named Pipe (FR-9)

**Given** a Backend connecting over the Named Pipe
**When** it runs a script
**Then** the behavior is identical to the UDS path, and TLS is not required because this is a local transport (FR-9)

**Given** a non-Windows host
**When** the daemon starts
**Then** the Named Pipe adapter is cleanly absent rather than failing startup, and the UDS adapter serves the local role

### Story 5.4: Behave identically no matter how the backend connected

As a Backend developer,
I want the protocol to behave the same across all four transports,
So that my choice of transport is a latency and topology decision, never a semantic one.

**Acceptance Criteria:**

**Given** one shared conformance suite covering init, Direct Execution, Cached Execution, capability denial, budget violation, and error shapes
**When** it is run against UDS, Named Pipe, TCP+TLS, and WebSocket
**Then** every case produces identical results and identical error codes on every transport, modulo latency (FR-9)

**Given** the core crate graph
**When** a reviewer greps every crate but `hexput-transport` for transport names or transport-specific branching
**Then** there are no matches, and no crate but `hexput-daemon` even depends on `hexput-transport` (AD-1)

**Given** a new transport adapter added in future
**When** it implements the `Port` interface
**Then** the conformance suite runs against it unchanged

### Story 5.5: Issue a reconnect credential at first connect

As a Backend,
I want a secret issued alongside my Client ID,
So that knowing my ID is not by itself enough for anyone to take over my Session.

**Acceptance Criteria:**

**Given** a fresh connection completing init
**When** the daemon issues the Client ID
**Then** it also generates a reconnect secret of at least 256 bits from a CSPRNG and returns it in that response exactly once (FR-13, OQ-2 resolution)

**Given** an issued secret
**When** the Session stores it
**Then** only a salted hash is persisted — the secret itself is never stored, logged, or included in any later response (FR-13, NFR1)

**Given** the credential mechanism
**When** it is inspected across transports
**Then** it is defined once and applied uniformly to all four, with no transport-specific variant (FR-13, AD-1)

### Story 5.6: Reconnect without resending configuration

As a Backend recovering from a network blip,
I want to reattach to my existing Session by proving I own it,
So that my registrations and policy survive the interruption without being re-sent.

**Acceptance Criteria:**

**Given** a live Session and its Client ID and reconnect secret
**When** a new connection sends a reconnect message carrying both
**Then** the daemon attaches the Connection to that Session and the Backend resumes without sending Config or registrations again (FR-2)

**Given** a reconnect carrying a valid Client ID with a wrong secret, or an unknown Client ID
**When** it is processed
**Then** both are rejected with the same defined reconnect-denied error, indistinguishable from each other, so Client IDs cannot be enumerated (FR-13, OQ-2 resolution)

**Given** the reconnect credential
**When** it is validated
**Then** validation happens once in `hexput-session` using a constant-time comparison, and no transport adapter performs any validation of its own (AD-2)

**Given** a reconnect message that also carries Config
**When** it is processed
**Then** the Config is ignored or rejected per the protocol's defined rule, and the stored Session Config remains authoritative (FR-2)

### Story 5.7: Run several connections under one client ID

As a Backend running multiple processes or instances,
I want them to share one Session concurrently,
So that my deployment topology is not constrained to a single socket per identity.

**Acceptance Criteria:**

**Given** a live Session with one attached Connection
**When** a second connection reconnects with the same Client ID and secret
**Then** both Connections are attached and live simultaneously (FR-2, AD-2)

**Given** two Connections attached to one Session
**When** each submits an execution
**Then** each result returns only to the Connection that issued it, with no cross-Connection broadcast (AD-2)

**Given** two Connections attached to one Session
**When** one updates the Config or registers a function
**Then** the change is visible to both, because the state is Session-owned (AD-2, AD-5)

### Story 5.8: Expire a session only after the last connection is gone

As an operator,
I want a Session's TTL clock to start only when nothing is attached to it,
So that a busy Backend is never torn down underneath itself.

**Acceptance Criteria:**

**Given** a Session with at least one attached Connection
**When** time passes beyond the default Session TTL
**Then** no TTL countdown is running and the Session stays alive (AD-2, FR-21)

**Given** a Session whose last attached Connection drops
**When** the TTL from System Config elapses with no reconnect
**Then** the Session is torn down along with its Config, registrations, and AST Cache entries (FR-24, AD-2)
**And** this replaces Story 2.5's immediate teardown by delaying the same teardown path behind the TTL, rather than adding a second teardown path

**Given** a Session whose last Connection dropped
**When** a valid reconnect arrives before the TTL elapses
**Then** the countdown stops and the Session's state is intact and unchanged (FR-2)

**Given** a Session torn down by TTL expiry
**When** a reconnect arrives afterward with the same Client ID and secret
**Then** it is rejected exactly like an unknown Client ID, and the Backend must init fresh (FR-2)

### Story 5.9: Benchmark the remote path separately from the local one

As a Hexput contributor,
I want TCP/TLS benchmark numbers reported apart from UDS numbers,
So that the transport's latency contribution is never conflated with the runtime's.

**Acceptance Criteria:**

**Given** the Epic 4 benchmark harness
**When** it is extended with a TCP+TLS leg
**Then** it reports UDS and TCP/TLS results as separate figures, never averaged or merged (NFR3, Brief addendum)

**Given** both legs
**When** results are published
**Then** the transport and machine topology for each leg is recorded alongside its numbers

---

## Epic 6: Stateful Plugins driven by events

A Backend can register a Plugin — metadata block, top-level Global Variables, `@Event`-annotated handlers — declaring upfront every Event name it may fire and each Event's return shape, then fire those Events to drive handlers that share persistent state across invocations under the configured locking and lifetime strategies.

### Story 6.1: Parse plugin source with metadata, globals, and event handlers

As a script author writing a plugin,
I want the `plugin { }` block, top-level `let` globals, and `@Event(...)` annotations to be understood by the language,
So that a plugin is a first-class program shape rather than a convention.

**Acceptance Criteria:**

**Given** plugin source containing a `plugin { name = ... }` metadata block, top-level `let` declarations, and functions annotated `@Event(<name>)`
**When** the parser runs
**Then** it produces a Plugin AST recording the metadata fields, the Global Variable declarations, and each handler's Event name and annotation arguments (FR-17)

**Given** an `@Event` annotation carrying `priority` or `async` arguments
**When** it is parsed
**Then** both are captured as structured annotation values, with `async` defaulting to `false` when absent (FR-22, FR-23)

**Given** plugin source missing its `plugin { }` block, declaring a duplicate metadata key, or annotating a non-function
**When** it is parsed
**Then** a structured error naming the span is returned and no Plugin AST is produced

### Story 6.2: Register a plugin as its own kind of registration

As a Backend,
I want plugin registration to be unmistakably distinct from submitting a script,
So that stateless and stateful work can share one connection without ambiguity.

**Acceptance Criteria:**

**Given** an initialized connection
**When** it sends a Plugin registration message
**Then** the daemon can tell at the protocol level that this is a Plugin and not a Script, with no inference from payload shape (FR-17)

**Given** a connection with a registered Plugin
**When** it also submits Direct and Cached Executions
**Then** both modes work concurrently without interfering with each other (FR-17)

**Given** a Session that already holds a Plugin named `x`
**When** the same Session registers another Plugin named `x`
**Then** it is rejected as a duplicate name, while a different Session registering `x` succeeds — names are unique per Client ID, not daemon-wide (FR-17)

**Given** one Session
**When** it registers several differently-named Plugins
**Then** all of them are live simultaneously with independent state (FR-17)

### Story 6.3: Declare every event and its return shape upfront

As a Backend,
I want to state at registration exactly which events I may fire and what each returns,
So that nothing about a plugin's interface is discovered ad hoc at runtime.

**Acceptance Criteria:**

**Given** a Plugin registration
**When** it omits the Event name and return-shape declarations
**Then** it is rejected, or accepted with no Events available beyond `BackendRegisteredInit` — declaration is never optional for a custom Event (FR-19)

**Given** a registered Plugin
**When** the Backend fires an Event name that was not declared
**Then** the daemon rejects the fire with a defined error rather than accepting it silently (FR-19)

**Given** a declared Event
**When** its return shape is expressed
**Then** it reuses the language's existing object-shape representation rather than introducing a second schema language (PRD addendum)

**Given** a declared Event that requires a partition key for `keyed` Global Variables
**When** it is declared
**Then** the key requirement is recorded per-Event at registration time (FR-25)

### Story 6.4: Run initialization handlers before the plugin serves anything

As a Backend,
I want `BackendRegisteredInit` handlers to finish before any other event can reach my plugin,
So that my plugin's state is fully prepared the first time real work arrives.

**Acceptance Criteria:**

**Given** a Plugin with one or more `@Event(BackendRegisteredInit)` handlers
**When** it is registered
**Then** the daemon invokes each exactly once and only marks the Plugin ready after they complete (FR-18)

**Given** an init handler that fails or exceeds its budget
**When** it runs
**Then** the registration fails, the Plugin is not marked ready, and its state is torn down rather than left half-initialized (FR-18)

**Given** an Event fired against a Plugin whose init has not completed
**When** the fire arrives
**Then** it is not executed ahead of init completion (FR-18)

**Given** a Backend attempting to declare or fire `BackendRegisteredInit` itself
**When** the registration or fire is processed
**Then** it is rejected — the name is reserved (FR-18)

### Story 6.5: Fire an event and run its handlers

As a Backend,
I want firing a declared event to invoke that plugin's matching handlers,
So that my application's happenings drive user-authored logic.

**Acceptance Criteria:**

**Given** a ready Plugin with handlers bound to a declared Event
**When** the Backend fires that Event with parameters
**Then** every matching handler is invoked with those parameters (FR-19)

**Given** a handler invocation
**When** it runs
**Then** it passes through the same single handler-invocation function in `hexput-exec`, carrying the same Capability checks and Resource Budget accounting as Direct and Cached Execution (FR-19, AD-3)

**Given** a declared Event fired against a Plugin with no handler bound to it
**When** it is processed
**Then** it is a defined no-op — not an error, and never a teardown of the Plugin (FR-19)

**Given** a handler that fails or exceeds its budget
**When** it terminates
**Then** the failure is reported to the Backend and neither the Plugin, its sibling handlers, nor the daemon are taken down (NFR4)

### Story 6.6: Hold handler results to their declared shape

As a Backend,
I want a handler's return value checked against the shape I declared,
So that a plugin author's mistake surfaces as a clear error instead of corrupting my application's data.

**Acceptance Criteria:**

**Given** an Event whose return shape was declared at registration
**When** a handler returns a value conforming to it
**Then** the value is returned to the Backend unchanged (FR-19)

**Given** a handler returning a value that does not conform — a missing key, a wrong value type, or an extra key where the shape forbids it
**When** the result is validated
**Then** a defined shape-conformance error naming the offending path is returned, not an ambient type-mismatch failure (FR-19)

**Given** several handlers on one Event
**When** each returns
**Then** each result is validated independently, and one handler's non-conforming result does not discard the others'

### Story 6.7: Share plugin state across invocations without stalling siblings

As a plugin author,
I want my top-level variables to persist across event invocations with fine-grained locking,
So that concurrent handlers touching different keys don't wait on each other.

**Acceptance Criteria:**

**Given** a Plugin with top-level Global Variables
**When** several Event invocations read and write them over time
**Then** the values persist and are shared across every invocation of that Plugin (FR-20)

**Given** two concurrent invocations writing different top-level keys of the same Global Variable
**When** they run
**Then** neither blocks the other (FR-20, NFR6)

**Given** two concurrent invocations writing the same top-level key under the default strategy
**When** they run
**Then** the writes serialize and no update is lost (FR-20)

**Given** any Global Variable access
**When** a reviewer inspects the critical section
**Then** the lock is short, synchronous, and never held across an `.await`; a handler needing the value across a suspension re-acquires it (AD-4)

**Given** the Global Variable store
**When** a reviewer inspects its lifetime
**Then** it is a shared per-Plugin concurrent structure reachable without the Plugin actor being alive, not state owned by the actor's mailbox (AD-4)

**Given** a handler reading or writing a Global Variable
**When** capability rules are applied
**Then** the access is treated as intrinsic language state exempt from the FR-6/FR-7 capability-grant requirement, reached only via the `Executor`/store path and never through `hexput-rpc` — `hexput-globalvar` has no dependency on it (AD-3, Epic 3 Story 3.4)

### Story 6.8: Choose the locking strategy

As a Backend,
I want to trade locking safety for latency where I know it is safe,
So that hot plugin paths aren't paying for guarantees they don't need.

**Acceptance Criteria:**

**Given** a Plugin registration selecting the unsafe/lock-free Global Variable strategy
**When** concurrent handlers write the same key
**Then** writes are not serialized and lost updates and stale reads are possible by design, without any data corruption or panic (FR-20)

**Given** a Backend Config permitting code-level override
**When** plugin code selects a strategy for a specific Global Variable
**Then** that variable uses the overriding strategy and its siblings keep the Backend's default (FR-20)

**Given** a Backend Config not permitting code-level override
**When** plugin code attempts one
**Then** the attempt is rejected at registration with a defined error rather than silently honored (FR-20)

**Given** the lock-free strategy's implementation
**When** it is reviewed
**Then** it contains no `unsafe` Rust — it uses the store's finer-grained or optimistic access, and the word "unsafe" here names concurrency semantics, not memory safety (AD-4, NFR2)

### Story 6.9: Keep a global variable forever or let it expire

As a plugin author,
I want a global to either persist for the session or reset after a period of inactivity,
So that cached values don't go stale and grow unbounded.

**Acceptance Criteria:**

**Given** a Global Variable declared with no explicit behavior
**When** it is accessed across invocations
**Then** it behaves as `forever`, persisting for the Session's lifetime (FR-25)

**Given** a Global Variable declared with `ttl: <duration>`
**When** it is read after the duration has elapsed since its last write
**Then** it reads as its initial or reset value, never the stale prior value (FR-25)

**Given** a `ttl` variable written again before its window elapses
**When** it is read
**Then** the written value is returned and the window restarts from that write (FR-25)

### Story 6.10: Isolate state per trigger or per key

As a plugin author,
I want globals that are private to one invocation, or partitioned by a key my backend supplies,
So that per-tenant or per-request state doesn't have to be threaded through every handler by hand.

**Acceptance Criteria:**

**Given** a Global Variable declared `separate_each_trigger`
**When** two invocations of the same Event on the same Plugin mutate it
**Then** neither invocation observes the other's mutation (FR-25)

**Given** a Global Variable declared `keyed`
**When** the Backend fires the Event with different partition keys
**Then** each key sees an independent value, and concurrent access under different keys never contends for the same lock (FR-25, AD-4)

**Given** an Event declared as requiring a partition key
**When** it is fired without one
**Then** it fails with a defined error and never falls back silently to an unkeyed default (FR-25)

**Given** a `keyed` variable
**When** its key is resolved
**Then** it comes only from what the Backend supplied when firing, never inferred from handler params or from the Client ID (FR-25)

### Story 6.11: Order handlers on one event

As a plugin author,
I want to control which of my handlers on an event runs first,
So that a handler depending on another's effect can be sequenced deliberately.

**Acceptance Criteria:**

**Given** a Backend Config permitting Plugin-level ordering and handlers annotated with `priority`
**When** the Event fires
**Then** handlers run in ascending priority order — lower value first (FR-22, OQ-11 resolution)

**Given** handlers with no `priority` annotation
**When** the Event fires
**Then** they run in source-declaration order within the Plugin (OQ-11 resolution)

**Given** a Backend Config not permitting Plugin-level ordering
**When** a Plugin using `priority` is registered
**Then** the annotations are ignored and the default order applies, or registration is rejected — whichever the implementation chooses, applied consistently and documented (FR-22)

**Given** a priority chain running for one Event
**When** a different Event fires concurrently on the same Plugin
**Then** the second Event is not blocked behind the first Event's chain, because sequencing is a per-(Plugin, Event) task spawned outside the actor's mailbox (AD-4, AD-6)

### Story 6.12: Let a handler skip the queue

As a plugin author,
I want a handler that runs concurrently without waiting its turn,
So that slow independent work doesn't hold up the ordered chain.

**Acceptance Criteria:**

**Given** a handler annotated `async = true`
**When** its Event fires
**Then** it is dispatched concurrently alongside the other handlers rather than queued behind them (FR-23)

**Given** a handler annotated with both `async = true` and `priority`
**When** its Event fires
**Then** async wins: it dispatches concurrently and the `priority` value is ignored rather than rejected (FR-23, OQ-11 resolution)

**Given** an `async = true` handler mutating a Global Variable
**When** the Backend has not opted async handlers into the safe strategy
**Then** its mutations default to the unsafe/lock-free strategy regardless of the Plugin's general default; when the Backend has opted in, the safe strategy applies (FR-20, FR-23)

**Given** an `async = true` handler still running after its dispatching call returned
**When** it performs budget-relevant operations, including Global Variable writes counted as side effects
**Then** they charge against a still-live budget-accounting handle from the same `Executor`, and exceeding a dimension terminates the handler (AD-3)

**Given** both dispatch paths
**When** a reviewer compares them
**Then** the priority-sequencing task and directly-spawned async handlers call the same single handler-invocation function, differing only in when they are scheduled (AD-3)

### Story 6.13: Keep plugin state for the life of the session

As a Backend,
I want my plugin and its state to survive reconnects and to be cleaned up deterministically,
So that a network blip doesn't reset my users' plugin state and a dead session doesn't leak memory.

**Acceptance Criteria:**

**Given** a registered Plugin with mutated Global Variables
**When** the Backend disconnects and reconnects within the Session TTL using its Client ID and secret
**Then** the Plugin is still registered and its Global Variable values are exactly as they were before the disconnect (FR-21)

**Given** a Session torn down by TTL expiry
**When** teardown runs
**Then** `hexput-session` calls `hexput_globalvar::teardown(plugin_id)` synchronously before the Plugin actor is dropped, and teardown is never triggered by `Drop` (AD-4, FR-21)

**Given** an explicit unregister message for a Plugin
**When** it is processed
**Then** the Plugin and its state are removed immediately regardless of TTL, and a subsequent Event fired at that name is rejected as unknown (FR-21)

**Given** a Session that expired and a Backend that then connects again
**When** it initializes
**Then** it starts fresh with no prior Plugin or Global Variable state, exactly like a first-ever connection (FR-21)

### Story 6.14: Finish in-flight work when the backend vanishes

As a Backend,
I want an event already running when my connection drops to finish rather than be killed arbitrarily,
So that half-applied plugin state is the exception, not the norm.

**Acceptance Criteria:**

**Given** an Event invocation in flight
**When** its Backend's last Connection drops
**Then** the invocation is not immediately cancelled — the daemon runs it to completion locally (FR-21)

**Given** such an in-flight invocation
**When** it attempts an outbound RPC to the now-disconnected Backend and the disconnection is detected at that point
**Then** the invocation is cancelled there with a defined error, rather than hanging or failing silently (FR-21)

**Given** an invocation cancelled at its RPC boundary
**When** its effects are inspected
**Then** Global Variable mutations already made stand — no rollback is attempted (PRD §4.4, Brief Risks)

---

## Epic 7: Operate the daemon in production

An operator can confirm the daemon is alive and serving, scrape execution metrics with per-budget-dimension violation counters through the same RPC Port without completing an init handshake, and reconstruct what any given Client ID did from structured logs alone.

### Story 7.1: Check that the daemon is alive

As an operator,
I want to ask the daemon whether it is up and serving without registering anything,
So that my supervisor and my monitoring can probe it the way they probe any other service.

**Acceptance Criteria:**

**Given** a running daemon
**When** a client sends a `HealthCheck` message without having completed the init handshake
**Then** the daemon answers with its status, uptime, and whether it is accepting connections (FR-11, OQ-1 resolution)

**Given** the health surface
**When** a reviewer inspects how it is served
**Then** it rides the same `Port` as every other message, exempt from the init gate, with no separate listener or HTTP server anywhere in the daemon (AD-1, OQ-1 resolution)

**Given** a `HealthCheck` over each of the four transports
**When** each is answered
**Then** the response is identical in shape and content, modulo latency (AD-1, FR-9)

### Story 7.2: Scrape execution metrics

As an operator,
I want metrics in a format my existing tooling already understands,
So that Hexput fits into the monitoring I run for everything else.

**Acceptance Criteria:**

**Given** a running daemon that has served executions
**When** a client sends a `MetricsScrape` message pre-init-gate
**Then** the response body is a Prometheus text-exposition-format payload (FR-11, OQ-1 resolution)

**Given** the metrics payload
**When** it is parsed
**Then** it includes execution counts, execution latency, and active Session and Connection counts, each with stable metric names (FR-11)

**Given** repeated scrapes
**When** they occur under load
**Then** scraping does not block or measurably slow execution handling (NFR5)

### Story 7.3: Distinguish which budget dimension is failing

As an operator,
I want budget violations counted per dimension rather than lumped together,
So that I can tell a runaway loop from a memory hog from an RPC-call abuser.

**Acceptance Criteria:**

**Given** executions that violate different budget dimensions
**When** metrics are scraped
**Then** CPU time, memory, allocation count, RPC call count, output size, and side-effect count violations appear as six independently incremented series — never one aggregate counter (FR-11, FR-8, SM-C2)

**Given** capability denials
**When** metrics are scraped
**Then** they are counted separately from budget violations (FR-11)

**Given** the counters
**When** enforcement runs under sustained load
**Then** every violation the enforcement layer raises is reflected in the counters, so a silently unenforced dimension is detectable (SM-3)

### Story 7.4: Reconstruct a client's connection history from logs

As an operator investigating an incident,
I want every connection lifecycle event attributable to a Client ID,
So that I can reconstruct what a given Backend did without a separate audit subsystem.

**Acceptance Criteria:**

**Given** a Backend that connects, initializes, reconnects, and disconnects
**When** its activity is logged
**Then** each lifecycle event is a distinct structured log event tagged with the Client ID (FR-12)

**Given** a Session torn down by TTL expiry or explicit unregister
**When** it is logged
**Then** the event records which cause applied (FR-12, FR-21)

**Given** any log event carrying credentials or script payloads
**When** it is emitted
**Then** the reconnect secret is never present in any form, and payload logging is bounded rather than unbounded (NFR1, FR-13)

### Story 7.5: Identify denials and violations in the log stream

As an operator,
I want capability denials and budget violations to be independently identifiable log events,
So that a security question and a capacity question can be answered from the same stream without conflating them.

**Acceptance Criteria:**

**Given** a script whose call is capability-denied
**When** the denial is logged
**Then** it is a distinct event type naming the function and the Client ID, distinguishable from any budget event (FR-12, NFR1)

**Given** an execution terminated by a budget violation
**When** it is logged
**Then** the event names the specific dimension exceeded and the Client ID (FR-12, FR-8)

**Given** a construct-disabled-by-policy failure
**When** it is logged
**Then** it is distinguishable from both a capability denial and a budget violation (FR-3, FR-12)

**Given** a sustained load test exercising malformed input, budget violations, and capability denials
**When** the run completes
**Then** the daemon recorded zero crashes and zero unhandled panics attributable to script or handler execution (SM-2, NFR4)

### Story 7.6: Install and run the daemon as ordinary infrastructure

As an operator,
I want the daemon delivered as a systemd unit and a container image,
So that I can install and run Hexput the way I run Redis or Postgres, which is the entire deployment premise of the product.

**Acceptance Criteria:**

**Given** the MVP scope's "standalone daemon (systemd/Docker)" commitment, which the Architecture Spine deliberately left as packaging-time detail rather than structure
**When** this story is implemented
**Then** both packaging artifacts exist in the repository and are built by CI (PRD §6.1)

**Given** the systemd unit
**When** it is installed and started on a host with a System Config at the default path
**Then** the daemon starts, is supervised, restarts on failure, and reports readiness — and the unit points at the config via the environment variable or the default path, never by changing discovery logic (AD-7)

**Given** the container image
**When** it is run with a System Config bind-mounted at the default path
**Then** the daemon starts identically to the systemd deployment, resolving its config through the same precedence (AD-7)

**Given** either packaging artifact
**When** the daemon is stopped
**Then** shutdown is graceful: listeners stop accepting, in-flight executions are given a bounded window to finish, and Sessions are torn down through the normal teardown path (AD-4)

**Given** the deferred operational questions the Spine named — environment profiles, upgrade and rollback, TLS certificate reload
**When** this story is scoped
**Then** they remain out of scope and are not invented here

---

## Epic 8: Connect from JavaScript and Python

A backend engineer working in JavaScript or Python can install a thin client SDK and perform the full Phase 1 surface idiomatically — connect, send init Config and registrations, run Direct and Cached Executions, receive results, and reconnect via Client ID with its reconnect credential — without hand-writing MessagePack frames.

### Story 8.1: Hold every SDK to one protocol conformance suite

As a Hexput maintainer,
I want one language-agnostic conformance suite every SDK must pass,
So that clients never drift from the daemon's protocol or from each other.

**Acceptance Criteria:**

**Given** the conformance suite
**When** it is defined
**Then** it covers init with Config and registrations, Direct Execution, Cached Execution, inbound host-function calls, capability denial, budget violation, error shapes, and reconnect with credential (FR-10)

**Given** any SDK claiming Phase 1 completeness
**When** it runs the suite against a live daemon
**Then** every case passes, and a failure names the protocol behavior that diverged (FR-10, PRD addendum)

**Given** the suite
**When** a Phase 2 SDK is started later
**Then** it is reusable unchanged for that language

### Story 8.2: Connect and run a script from JavaScript

As a backend engineer working in JavaScript,
I want to connect, register my functions, and run a script in idiomatic JS,
So that I can adopt Hexput without learning its wire format.

**Acceptance Criteria:**

**Given** the JavaScript SDK
**When** I connect and supply Config and function registrations
**Then** it completes the init handshake and surfaces the issued Client ID and reconnect secret to me (FR-10, FR-1, FR-13)

**Given** a connected client
**When** I submit a Direct Execution with variables
**Then** I receive the result as a native JS value, and a script error arrives as a typed SDK error carrying the daemon's category, code, and source span (FR-4)

**Given** a connection that drops mid-request
**When** the SDK surfaces it
**Then** the pending call rejects with a distinguishable connection error rather than hanging indefinitely

### Story 8.3: Serve host-function calls from JavaScript

As a backend engineer working in JavaScript,
I want my registered functions to be called by running scripts and my per-call guards honored,
So that the capability model works from my side of the socket.

**Acceptance Criteria:**

**Given** a registered function with a JS implementation
**When** a script calls it
**Then** the SDK dispatches the inbound RPC to my implementation and returns its value to the daemon (FR-6)

**Given** a function registered with a per-call guard
**When** a script calls it
**Then** the SDK invokes my guard and reports its boolean decision, denying the call when it returns false (FR-6)

**Given** my implementation throwing or rejecting
**When** the SDK handles it
**Then** the failure is reported to the daemon as a host-side function error rather than crashing my process or the connection

**Given** a method registered with `registerMethod(objKey, fn)`
**When** a script calls it on a keyed object
**Then** the SDK dispatches the `Call` to my method with the receiver, exposes every value's Value Secret (`ref`, `key`, extra fields) to me, and reports the changes I make to referenced values as the reply's `modifications`, and hands me the `modifications` an execution's result reports (FR-27, FR-28) **[Added 2026-09-24]**

**Given** several inbound calls arriving concurrently
**When** they are dispatched
**Then** they are handled concurrently without serializing behind each other (NFR5)

### Story 8.4: Cache scripts and reconnect from JavaScript

As a backend engineer working in JavaScript,
I want cached execution and automatic reconnection,
So that my hot path is fast and a blip doesn't cost me my registrations.

**Acceptance Criteria:**

**Given** the JS SDK
**When** I register a script and execute it repeatedly with different variables
**Then** each run returns an independent result from the cached AST (FR-5)

**Given** a dropped connection and a stored Client ID and reconnect secret
**When** the SDK reconnects
**Then** it reattaches to the existing Session without re-sending Config, and my registered functions keep serving (FR-2, FR-13)

**Given** a reconnect rejected because the Session expired
**When** the SDK handles it
**Then** it surfaces a distinguishable session-expired error so I can choose to initialize fresh, rather than retrying the same rejected reconnect forever (FR-2)

### Story 8.5: Connect and run a script from Python

As a backend engineer working in Python,
I want to connect, register my functions, and run a script in idiomatic Python,
So that Hexput fits into my application the way any other client library would.

**Acceptance Criteria:**

**Given** the Python SDK
**When** I connect and supply Config and function registrations
**Then** it completes the init handshake and surfaces the issued Client ID and reconnect secret (FR-10, FR-1, FR-13)

**Given** a connected client
**When** I submit a Direct Execution with variables
**Then** I receive the result as native Python values, and a script error raises a typed SDK exception carrying the daemon's category, code, and source span (FR-4)

**Given** the SDK's public surface
**When** it is used from an asyncio application
**Then** it integrates with the running event loop rather than blocking it

### Story 8.6: Serve host-function calls from Python

As a backend engineer working in Python,
I want my registered functions called by running scripts with my guards honored,
So that capability decisions stay in my code.

**Acceptance Criteria:**

**Given** a registered function with a Python implementation
**When** a script calls it
**Then** the SDK dispatches the inbound RPC to my implementation and returns its value to the daemon (FR-6)

**Given** a function registered with a per-call guard
**When** a script calls it
**Then** the SDK invokes my guard and reports its boolean decision, denying the call when it returns false (FR-6)

**Given** my implementation raising
**When** the SDK handles it
**Then** the failure is reported as a host-side function error without tearing down my process or the connection

**Given** a method registered with `registerMethod(obj_key, fn)`
**When** a script calls it on a keyed object
**Then** the SDK dispatches the `Call` to my method with the receiver, exposes every value's Value Secret (`ref`, `key`, extra fields) to me, and reports the changes I make to referenced values as the reply's `modifications`, and hands me the `modifications` an execution's result reports (FR-27, FR-28) **[Added 2026-09-24]**

**Given** several inbound calls arriving concurrently
**When** they are dispatched
**Then** they are handled concurrently on the event loop (NFR5)

### Story 8.7: Cache scripts and reconnect from Python

As a backend engineer working in Python,
I want cached execution and reconnection,
So that my steady-state path is fast and resilient.

**Acceptance Criteria:**

**Given** the Python SDK
**When** I register a script and execute it repeatedly with different variables
**Then** each run returns an independent result from the cached AST (FR-5)

**Given** a dropped connection and stored credentials
**When** the SDK reconnects
**Then** it reattaches without re-sending Config and my registered functions keep serving (FR-2, FR-13)

**Given** a Session that has expired
**When** reconnect is attempted
**Then** a distinguishable session-expired error is raised rather than an indefinite retry (FR-2)

---

## Epic 9: Author Hexput scripts in an editor

A script author — often not the backend engineer running the daemon — gets syntax highlighting and folding from a tree-sitter grammar, and syntax-error diagnostics plus basic completion from a language server, in any LSP-compatible editor.

### Story 9.1: Parse Hexput with a tree-sitter grammar

As a script author,
I want a tree-sitter grammar that understands every Hexput construct,
So that the editors and tools built on tree-sitter can understand my code structurally.

**Acceptance Criteria:**

**Given** the tree-sitter grammar
**When** it parses the language's example scripts covering variables, callbacks, loops, conditionals, objects, and arrays
**Then** every file parses with no ERROR nodes (FR-14)

**Given** the grammar's corpus tests
**When** they run in CI
**Then** each construct has at least one test asserting its expected parse tree, and a grammar change that breaks one fails the build

**Given** a source file containing a syntax error
**When** the grammar parses it
**Then** it recovers and produces a usable partial tree rather than failing wholesale, as editors require

**Given** a plugin source file with a `plugin { }` block and `@Event` annotations
**When** it is parsed
**Then** those constructs are represented as named nodes, not as generic fallback text (FR-14, FR-17)

### Story 9.2: Highlight and fold Hexput in an editor

As a script author,
I want my Hexput file to be colored and foldable in my editor,
So that reading and navigating a rule is not a wall of plain text.

**Acceptance Criteria:**

**Given** the grammar's highlight queries
**When** a file is opened in a tree-sitter-backed editor
**Then** keywords, identifiers, literals, comments, operators, and `@Event` annotations are distinctly captured (FR-14)

**Given** the fold queries
**When** a file with nested blocks, functions, and object literals is opened
**Then** each of those regions is foldable at its correct boundaries (FR-14)

**Given** the grammar package
**When** an author follows its README
**Then** they can install and use it in at least one named editor without building it from source themselves

### Story 9.3: Connect an editor to a Hexput language server

As a script author,
I want my editor to talk to a Hexput language server,
So that authoring assistance arrives through the standard mechanism my editor already supports.

**Acceptance Criteria:**

**Given** the language server binary
**When** an LSP client initializes it
**Then** it responds to `initialize` advertising exactly the capabilities it implements — diagnostics and completion — and nothing it does not (FR-15)

**Given** an open document
**When** `textDocument/didOpen` and `didChange` are sent
**Then** the server maintains the document's current text and reparses incrementally without leaking state between documents

**Given** the server's parsing
**When** a reviewer inspects it
**Then** it reuses Epic 1's parser and its structured error spans rather than reimplementing the grammar (Epic 1 Story 1.8)

**Given** a malformed LSP request or a document that fails to parse
**When** the server handles it
**Then** it responds with an error or empty result and keeps serving, never exiting

### Story 9.4: See syntax errors where they are

As a script author,
I want my mistakes underlined at the exact place I made them,
So that I can fix a rule without deciphering a stack trace.

**Acceptance Criteria:**

**Given** a document containing a syntax error
**When** it is opened or edited
**Then** the server publishes a diagnostic via `textDocument/publishDiagnostics` whose range covers the offending span at the correct line and character (FR-15)

**Given** a document with several errors
**When** diagnostics are published
**Then** each is reported separately rather than only the first

**Given** an error the author then fixes
**When** the document is edited
**Then** the stale diagnostic is cleared in the next publish

**Given** a diagnostic
**When** it is rendered
**Then** its message and code match what the CLI (Story 1.9) prints for the same source, so the two never disagree

**Given** a document that parses cleanly but has static-check findings (Story 1.10)
**When** diagnostics are published
**Then** those findings appear as diagnostics at their own spans, with warning-severity findings distinguished from error-severity ones — this is what lets the language server report more than syntax errors (FR-15, FR-26)

**Given** a document that assigns a property named like a Registered Method on a starting variable its environment declares keyed
**When** diagnostics are published
**Then** the `capability.method_override` finding appears at the assignment target (FR-27; LANGUAGE-REFERENCE §8, §10) **[Added 2026-09-24]**

### Story 9.5: Get basic completion while writing

As a script author,
I want simple completion suggestions as I type,
So that I can recall the language's own constructs without leaving my editor.

**Acceptance Criteria:**

**Given** a document being edited
**When** completion is requested
**Then** the server returns the language's keywords and constructs, plus variables in scope at that position (FR-15)

**Given** a completion request inside a comment or a string literal
**When** it is handled
**Then** no misleading suggestions are returned

**Given** the completion surface
**When** its scope is reviewed
**Then** it deliberately excludes go-to-definition across Registered Functions and capability-aware autocomplete, which are out of scope for v2 (PRD §4.8 Out of Scope)
