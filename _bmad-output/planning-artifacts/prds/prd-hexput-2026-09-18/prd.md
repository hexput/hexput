---
title: Hexput v2 PRD
created: 2026-09-18
updated: 2026-09-24
status: final
amended: '2026-09-18 — FR-26 (optional static check) added during sprint planning, after this document was first marked final. 2026-09-24 — FR-27 (Registered Methods on keyed objects) and FR-28 (Value Secret, Reference IDs, host-side modifications) added by course correction during Story 3.1 planning. See .memlog.md.'
---

# PRD: Hexput v2

## 0. Document purpose

This PRD is for the people who build Hexput v2 — contributors, PR reviewers, and AI coding agents — and builds directly on the [product brief](../../briefs/brief-hexput-2026-09-18/brief.md) and its [addendum](../../briefs/brief-hexput-2026-09-18/addendum.md), which already settled the problem, architecture, deployment model, and known risks. This document does not re-derive those; it turns them into grouped features with globally-numbered functional requirements (FR-1…FR-N). Implementation-level tech choices (exact crates, wire-format details, benchmark tooling) live in [addendum.md](./addendum.md), not here.

## 1. Vision

Hexput v2 is a standalone scripting runtime service — installed and run the way Redis or Postgres is, not embedded like a library — that lets any backend application, in any language, hand off short, frequently-invoked, user-authored logic to a fast, capability-safe execution engine reached over a socket.

A backend registers the host-side functions it's willing to expose, sends (or updates) its config at connection time, and then either submits stateless scripts (one-shot direct execution, or parse-once/execute-many cached execution) or registers a stateful Plugin — persistent global state driven by backend-fired events — for the longer-lived "give my users a plugin ecosystem" case. Either way, the script or plugin can only do what its host explicitly allowed — there is no ambient access to the filesystem, network, or host memory, and no OS-level sandbox standing behind it: the language itself is built so unauthorized access is structurally impossible, not merely policed at runtime.

v2 exists because v1 never had real users and was written before the author had the experience to make it production-quality. This is a from-scratch rewrite, judged by whether it stays stable and fast as concurrent load and script complexity grow — substantiated by a published benchmark against Rhai, the closest comparable project.

## 2. Target user

### 2.1 Jobs to be done

- As a backend engineer building a plugin ecosystem, I need to let *my users* — not just people with server access — write logic that runs safely inside my system, without embedding and hardening a scripting language myself.
- As a backend engineer encoding a complex, frequently-changing business rule (e.g. a university's course pass/fail logic), I need a place to put that rule where a non-engineer domain expert could plausibly author or review it, separate from my application code.
- As a maintainer running Hexput in production, I need the daemon to be operable — connectable, configurable, observable, and reconnect-safe — without a bespoke config file per deployment.

### 2.2 Non-users (v1)

- Teams needing multi-tenant isolation *guarantees* backed by OS-level sandboxing (seccomp/cgroups/namespaces) — Hexput's isolation is a language-design property, not a kernel boundary, and this is a permanent design stance, not a gap to be filled later.
- Teams needing transactional/rollback semantics on host side effects — a script's RPC calls are not undoable in v2.
- Teams needing horizontal scaling out of the box — master-slave distribution is future work, not v2.

### 2.3 Key user journeys

*Lighter scope dial — Hexput is a developer/infra product; each JTBD above is realized by one concrete scenario, stated in one line rather than a full narrative:*

- **UJ-1.** A backend engineer building a plugin marketplace registers a handful of host functions (`getOrder`, `applyDiscount`), a user submits a Plugin through the product's UI, the engineer's backend registers it with Hexput, and from then on the Plugin's `@Event`-bound handlers run whenever the backend fires the matching event (e.g. `OrderPlaced`) — able to touch only the functions that were registered, nothing else.
- **UJ-2.** A university's systems team encodes "how a student passes or fails a course" as a Hexput script authored by an academic affairs staffer, registers the grade-lookup and enrollment functions it's allowed to call, and the enrollment system executes it on every grade change with fresh variables, in milliseconds, from the AST cache.
- **UJ-3.** An operator running the Hexput daemon in production reconnects a backend after a network blip using its existing client ID, sees the daemon pick back up without re-sending config, and checks daemon health/metrics without ever having written a config file.

## 3. Glossary

- **Daemon** — The standalone Hexput runtime process (systemd service or Docker container). One daemon may serve multiple independent backends concurrently.
- **Backend** — A host application that connects to the daemon, registers functions, sends config, and submits scripts for execution.
- **Client ID** — An identifier issued to a backend on first connection, used to reconnect and resume state after a dropped connection. It is not constrained to one concurrent connection.
- **Config** — Per-connection settings a backend sends at registration/init time; editable at runtime and overridable per execution. Primarily execution-policy defaults: resource budget values (§4.4), language-feature toggles (e.g. disabling loop constructs or if/else branching for a given execution), the Static Check mode (FR-26), and the nesting-depth limit for values sent in a `Call` (FR-27). Not a static file.
- **Registered Function** — A host-side function a backend has explicitly made callable from scripts. Registration and call-handling are separate steps.
- **Registered Method** — A Registered Function bound to an object key: registered with `registerMethod(objKey, fn)` and callable from a script only as a method on an object whose Value Secret carries that key (FR-27). Same Capability and Resource Budget rules as any Registered Function.
- **Value Secret** — Hidden metadata a value carries across the Daemon/Backend boundary: a Reference ID (`ref`), an optional object key (`key`, selecting Registered Methods), and any further Backend-defined fields. Written `__secret` on the wire; the Backend can read and edit it, a script can never observe or change it (FR-28).
- **Reference ID** — The `ref` in a Value Secret: an identifier for one value within one execution, supplied by the Backend or generated by the Daemon, through which the Backend reports modifications it made to that value during a call (FR-28).
- **Capability** — The explicit grant that allows a script to call a specific Registered Function, expressed via `context.allow()` at registration time or a per-call `return true` guard.
- **Script** — User-authored Hexput code submitted for execution.
- **Direct Execution** — One-shot parse-and-run of a script (`ExecutionStart`).
- **Cached Execution** — Parse/compile once (`CodeRegister`), then execute many times with fresh variables (`CachedExecutionStart`), served from the AST Cache.
- **AST Cache** — The in-memory store of precompiled script ASTs keyed for Cached Execution reuse.
- **Resource Budget** — The multi-dimensional execution limit (CPU time, memory, allocation count, RPC call count, output size, side-effect count) enforced per execution by the language runtime itself.
- **Transport** — The connection medium: Unix Domain Socket or Named Pipe (local, platform-dependent), TCP+TLS or WebSocket (remote).
- **Plugin** — A registered, stateful unit of Hexput code — a `plugin { name = ..., ... }` metadata block, top-level Global Variables, and one or more `@Event(<name>)`-annotated handler functions — that persists for the life of the Backend's Client ID, as opposed to a Script, which is stateless per invocation.
- **Plugin Registration** — The registration mode (alongside Direct/Cached Execution) in which a Backend registers a Plugin once; the daemon then invokes its handlers whenever a matching Event fires, sharing the Plugin's Global Variable state across every invocation.
- **Event** — A named trigger that invokes every handler function annotated `@Event(<name>)` for that name in a given Plugin. `BackendRegisteredInit` is reserved and fired once by the daemon right after registration; every other Event name is Backend-defined, the same way Registered Functions are.
- **Global Variable** — A top-level `let`-declared variable in a Plugin, shared and mutable across all of that Plugin's Event invocations (see Global Variable Locking). Private to the Plugin that declares it — not shared across Plugins.
- **Global Variable Locking** — The concurrency strategy for a Plugin's Global Variables: per-top-level-key locking by default (mutating one key doesn't block access to sibling keys), configurable by the Backend at registration time to an unsafe/lock-free mode, and further overridable from Plugin code itself where the Backend's config permits.
- **Global Variable Behavior** — The lifetime/scoping strategy for a Global Variable, distinct from its locking strategy: `forever` (default — persists for the Session's lifetime), `ttl: <duration>` (value expires/resets after the duration since last write), `separate_each_trigger` (each Event invocation gets an independent, non-shared value despite being declared global), or `keyed` (independent values partitioned by a key the Backend supplies when firing the Event).
- **Session** — The state tied to a Client ID (Config, Registered Functions, Plugins and their Global Variables) that outlives any single connection. A Session has a TTL (set in System Config); it is torn down if no reconnect happens before the TTL expires.
- **Static Check** — An optional analysis pass run over a parsed Script or Plugin *before* execution, reporting mistakes decidable without running the code (undeclared identifiers, arity mismatches, calls to names that are neither local functions nor Registered Functions, literal-operand type errors, unreachable code, disabled-construct usage). Its mode — `off`, `warn`, `error` — is part of the per-backend Config, never mandatory.
- **System Config** — The daemon's own file-based operational configuration (for example, `config.toml` in a system config directory) — transport bind addresses/ports, TLS certificate paths, log level, and the default Session TTL. Distinct from the per-backend Config (§Glossary), which is never file-based.

## 4. Features

*FR IDs are assigned in the order each requirement was finalized, not document position — FR-13 (§4.1) was added after FR-1…FR-12 already existed elsewhere, so it appears out of numeric sequence. IDs are stable regardless of where a requirement sits.*

### 4.1 Connection & registration lifecycle

**Description:** A backend establishes a connection to the daemon over one of the supported Transports, sends its config and function registrations as part of that connection's init handshake, and receives a Client ID it can use to reconnect after a drop. That state (Config, registrations, and any Plugins — §4.9) lives in a Session outliving the connection itself, bounded by a TTL from System Config (FR-24); reconnection does not resend config — the daemon retains it, keyed by Client ID, for as long as the Session lives. Config can be edited at runtime and overridden on a per-execution basis. Realizes UJ-3.

**Functional Requirements:**

#### FR-1: Connection init with inline config
A Backend can establish a new connection and supply its config and function registrations as part of the same init handshake, without a static config file. Realizes UJ-1, UJ-3.

**Consequences (testable):**
- A fresh connection with no prior Client ID always requires config + registrations in its init message; the daemon rejects execution requests on a connection that hasn't completed init.
- No file-based configuration path exists for the daemon to read on startup for per-backend settings.

#### FR-2: Reconnect via Client ID
A Backend can reconnect using a previously issued Client ID and resume without resending config. Realizes UJ-3.

**Consequences (testable):**
- A reconnect message carrying a valid Client ID skips the config/registration step and reuses the previously stored state for that ID.
- A Client ID is not limited to a single concurrent connection — multiple Backend processes may reconnect under the same ID concurrently.

**Out of Scope:**
- Master-slave routing of reconnected work across multiple daemons (future work).

#### FR-3: Runtime config edit and per-execution override
An already-connected Backend can update its stored config at runtime, and can override specific config values for a single execution without changing the stored config. Config is primarily execution-policy defaults: resource budgets and language-feature toggles (e.g. disable loops, disable if/else) — not general-purpose settings.

**Consequences (testable):**
- A runtime config update on connection A is visible to subsequent executions on connection A and does not require a reconnect.
- A per-execution override affects only that execution's run and leaves the connection's stored config unchanged afterward.
- A feature-toggle override (e.g. loops disabled) causes a script using that construct to fail with a defined "construct disabled by policy" error, distinct from a Resource Budget violation (FR-8) or a capability-denied error (FR-6, FR-7).

#### FR-13: Reconnect protection
Reconnecting via a Client ID (FR-2) requires proof of authorization, not just knowledge of the ID — protecting against a leaked or guessed Client ID being used to hijack another Backend's connection state.

**Consequences (testable):**
- A reconnect attempt with a valid-looking but unauthorized Client ID is rejected rather than silently granted access to that ID's stored config/state.
- The mechanism (e.g. a secret issued alongside the Client ID at first connect) is defined once and applies uniformly across all four Transports (§4.5).

#### FR-24: System Config
The daemon reads a file-based System Config at startup (operational settings: transport bind addresses/ports, TLS certificate paths, log level, default Session TTL) — distinct from, and not a workaround for, the per-backend Config's no-static-file rule (FR-1).

**Consequences (testable):**
- The daemon fails to start with a clear error if the System Config is missing or malformed, rather than silently falling back to undocumented defaults for required fields (bind addresses, TLS paths).
- Changing a per-backend execution policy (Config, §Glossary) never requires editing or reloading the System Config file — the two are independent surfaces.

### 4.2 Script execution model

**Description:** Scripts run either as a one-shot Direct Execution or as a Cached Execution registered once and re-run many times with fresh variables from the AST Cache, processed asynchronously so concurrent requests don't block each other. A Backend may optionally have scripts statically checked before they run. Realizes UJ-1, UJ-2.

**Functional Requirements:**

#### FR-4: Direct execution
A Backend can submit a script for one-shot parse-and-run without registering it for reuse.

**Consequences (testable):**
- A Direct Execution request returns a result (or a Resource Budget violation) without creating an AST Cache entry.

#### FR-5: Cached execution
A Backend can register a script once and trigger repeated executions of it with different variables, served from the AST Cache. Realizes UJ-2.

**Consequences (testable):**
- After `CodeRegister`, a `CachedExecutionStart` for the same script skips re-parsing and reuses the cached AST.
- Two `CachedExecutionStart` calls against the same registered script with different variables produce independent results with no state bleed between them.

#### FR-16: Non-blocking concurrent execution
Execution requests (Direct or Cached) are processed asynchronously; a slow or long-running execution does not block message processing or other execution requests, whether they arrive on the same connection or a different one.

**Consequences (testable):**
- Submitting a fast execution request while a slow one is still running (same connection or a different one) returns the fast result without waiting for the slow one to finish.
- A single Backend connection can have multiple execution requests in flight concurrently; none of them serialize behind another by default.

#### FR-26: Optional static check before execution
A Backend can have a submitted Script or Plugin analyzed before it executes, in one of three modes set in its Config (§4.1) and overridable per execution: `off` (default), `warn` (findings returned alongside a normal execution), or `error` (findings reject the submission before any statement runs). The check reports only mistakes decidable without running the code. Realizes UJ-2 — the non-engineer script author is the person a pre-execution error message helps most.

**Consequences (testable):**
- With mode `off`, no check runs and a script that would produce findings still executes — the check is never mandatory and costs nothing when unused.
- With mode `error`, a script with an error-severity finding is rejected before any statement executes and before any Registered Function is called.
- The check resolves callable names against that Session's Registered Functions (FR-6), so a call to an unregistered host function is reported as a finding at submission time rather than surfacing as a capability-denied error mid-execution.
- Findings carry the same category/code/message/source-span shape as any other error, so the daemon's response, the CLI, and the LSP (FR-15) render them identically.
- For a Cached Execution (FR-5), the check runs at registration rather than on every execution.
- Warning-severity findings (for example, an unused local variable) never reject a script, in any mode.

**Out of Scope:**
- Type inference across bindings, or any check whose outcome depends on runtime values — this is not a type system.
- Validating a Plugin handler's returned value against its declared shape, which stays the runtime check in FR-19.

### 4.3 Capability-based RPC

**Description:** Scripts reach the host exclusively through Registered Functions. Registration and call-handling are separate steps, and a Backend can grant access either at registration time (`context.allow()`) or per call (a handler returning `true`). This *is* the sandbox — there is no additional OS-level isolation layer, by design. Realizes UJ-1, UJ-2.

**Functional Requirements:**

#### FR-6: Function registration separate from call handling
A Backend can register a function as callable independently of implementing the logic that decides whether a specific call is allowed.

**Consequences (testable):**
- A function registered with `context.allow()` at registration time is callable by any script on that connection without a per-call check.
- A function registered without a blanket allow requires its handler to return `true` per call, and returns a capability-denied error otherwise.
- Every call to a Registered Function (or Registered Method, FR-27) reaches the Backend as one generic `Call` message over the connection that submitted the execution; the script waits for the reply without the execution holding a thread. A Backend-side failure reaches the script as a defined `host` error attributed to that call — distinct from a capability denial — and is never treated as the Daemon's own failure. **[Added 2026-09-24.]**

#### FR-7: No ambient host access
A script cannot access the filesystem, network, or host process memory except through a Registered Function explicitly granted to it.

**Consequences (testable):**
- A script referencing any capability (file, socket, process) not backed by a Registered Function fails at execution with a defined capability-denied error, not a host-level exception.

#### FR-27: Registered Methods on keyed objects
A Backend can register a function as a method bound to an object key (`registerMethod(objKey, fn)`), and a script can call it as `value.name(args)` on any value whose Value Secret carries that key. **[Added 2026-09-24 by course correction.]**

**Consequences (testable):**
- A method call reaches the Backend as the same generic `Call` message a plain Registered Function call uses, carrying the receiver value itself (with its Value Secret) alongside the arguments.
- A registered method takes precedence over an own property of the same name; a method name not registered for the receiver's key fails with the same capability-denied error as an unregistered function.
- Registered Methods obey the same Capability grants (FR-6) and Resource Budget dimensions (FR-8) as plain Registered Functions.
- The nesting depth of arguments and receiver sent in a `Call` is bounded by a Config limit (default 12, overridable per execution per FR-3); a deeper value fails before anything is sent.

#### FR-28: Value Secret, Reference IDs, and host-side modifications
Every value that crosses to the Backend carries a Value Secret with a Reference ID, which the Backend can read and edit but a script can never observe or change; modifications the Backend makes to referenced values during a call are reported in that call's reply and applied inside the execution. **[Added 2026-09-24 by course correction.]**

**Consequences (testable):**
- Reading `__secret` in a script yields `null`, writing it is silently ignored, and iterating an object never yields it — the Value Secret is invisible, not an error.
- A value the Backend supplies may carry its own Reference ID; any value sent to the Backend without one is given a Daemon-generated one, stable for the rest of that execution.
- A `Call` reply may list modifications by Reference ID; the Daemon applies them before the script resumes, and every binding holding that referenced value observes the change.
- Value Secrets always travel back to the Backend unchanged by the script, including in an execution's result.

**Feature-specific NFRs:**
- Security: this feature is the entire trust boundary of the system — see §8 Security & capability model.

### 4.4 Resource budgeting

**Description:** Every execution is bounded across multiple dimensions simultaneously, enforced by the runtime itself rather than an external sandbox. Realizes UJ-1, UJ-2.

**Functional Requirements:**

#### FR-8: Multi-dimensional budget enforcement
An execution (Direct or Cached) is bounded by CPU time, memory, allocation count, RPC call count, output size, and side-effect count, each independently enforceable.

**Consequences (testable):**
- Exceeding any one budget dimension terminates the execution with an error identifying which dimension was exceeded.
- Budget limits are configurable per Backend config (§4.1) and overridable per execution.

**Out of Scope:**
- Compensating/rollback of RPC side effects already performed before a budget violation (brief: no rollback in v2).

### 4.5 Transport layer

**Description:** The daemon accepts connections over Unix Domain Socket (Linux/macOS) or Named Pipe (Windows) for same-machine Backends, and TCP+TLS or WebSocket for remote Backends. Realizes UJ-1, UJ-2, UJ-3.

**Functional Requirements:**

#### FR-9: Multi-transport connectivity
A Backend can connect via Unix Domain Socket, Named Pipe, TCP+TLS, or WebSocket, with the RPC protocol behaving identically regardless of transport.

**Consequences (testable):**
- The same script/RPC request produces the same result whether submitted over UDS/Named Pipe or over TCP+TLS/WebSocket, modulo network latency.
- TLS is enforced (not optional) for any remote connection — that is, anything other than UDS or Named Pipe.
- Named Pipe is the Windows-native equivalent of UDS for local, same-machine connections — a Backend on Windows is not required to use TCP for local IPC.

### 4.6 Client SDKs

**Description:** Thin client libraries for connecting to the daemon, speaking the wire protocol, and exposing registration/execution calls idiomatically per language. Not the runtime itself. Realizes UJ-1, UJ-2.

**Functional Requirements:**

#### FR-10: SDK coverage, phased
Client SDKs ship in two phases. Phase 1 (v2 launch): JavaScript and Python. Phase 2 (post-launch): Node.js, Rust, and Go.

**Consequences (testable):**
- Each Phase 1 SDK can perform the full FR-1…FR-5, FR-13 surface: connect, send init config + registrations, register a Direct or Cached execution, receive results, and reconnect via Client ID with reconnect-protection credentials.
- v2 is considered launched once Phase 1 SDKs meet that bar; Phase 2 SDKs are not a launch blocker.

**Out of Scope:**
- Node.js, Rust, and Go SDKs at initial launch — Phase 2, tracked separately.

**Notes:**
- [NOTE FOR PM] "JavaScript" and "Node.js" are tracked as two distinct SDKs per the original scope conversation, but what differentiates them isn't yet defined (e.g. a browser/generic-JS SDK limited to WebSocket vs. a server-side Node.js SDK that can also use UDS/TCP directly) — see OQ-5.

### 4.7 Observability & operations

**Description:** [NOTE FOR PM] Authored by PM per explicit delegation ("geri kalanına sen karar verebilirsin" — Turkish for "you can decide the rest") — operational surface needed to run the daemon without a config file, since there's no static config to inspect. Not user-confirmed line-by-line; open for revision (OQ-1). Realizes UJ-3.

**Functional Requirements:**

#### FR-11: Daemon health and metrics surface
The daemon exposes a way to check it is alive and serving connections, and exposes execution metrics (counts, latency, budget-violation rate) suitable for scraping by standard tooling.

**Consequences (testable):**
- A health check (RPC message or exposed endpoint) returns daemon status without requiring an authenticated Backend connection.
- Metrics distinguish per-dimension budget violations (CPU vs memory vs RPC-count, etc.) rather than a single aggregate failure count.

#### FR-12: Structured logging
The daemon emits structured logs for connection lifecycle events, capability denials, and budget violations, sufficient to reconstruct what a given Client ID did without a separate audit subsystem.

**Consequences (testable):**
- A capability-denied event and a budget-violation event are each independently identifiable in the log stream, tagged with the originating Client ID.

### 4.8 Developer tooling

**Description:** Editor-facing tooling for people authoring Hexput scripts — a tree-sitter grammar for syntax highlighting/structural editing, and a simple LSP (language server) for basic authoring assistance. This is a v2 product deliverable, not just internal dogfooding, since script authors (§2, UJ-2's academic-affairs staffer) are often not the backend engineers running the daemon.

**Functional Requirements:**

#### FR-14: Tree-sitter grammar
A tree-sitter grammar for the Hexput language exists and produces a correct parse tree for the language's constructs (variables, callbacks, loops, conditionals, objects/arrays).

**Consequences (testable):**
- The grammar parses the language's example scripts without error and produces a tree an editor can use for syntax highlighting and folding.

#### FR-15: Basic LSP
A simple language server provides at minimum syntax-error diagnostics and basic completion for a Hexput script, usable from any LSP-compatible editor.

**Consequences (testable):**
- Opening a script with a syntax error surfaces a diagnostic at the correct location via the LSP `textDocument/publishDiagnostics` (or equivalent) mechanism.

**Out of Scope:**
- Deep semantic features (go-to-definition across registered host functions, capability-aware autocomplete) — v2 targets basic authoring assistance, not a full IDE experience.

### 4.9 Plugin registration & events

**Description:** A second registration mode alongside stateless Direct/Cached Execution (§4.2): a Backend registers a **Plugin** — source with a `plugin { name = ..., ... }` metadata block, top-level **Global Variables**, and one or more handler functions annotated `@Event(<name>)`. Event names and each Event's return shape are declared by the Backend up front, at registration time (FR-19) — not discovered ad hoc. The daemon invokes matching handlers when a declared Event fires, and the Plugin's Global Variable state persists and is shared across every invocation for the life of the Backend's Client ID. This is what makes "give your users a plugin ecosystem" (Vision) concrete: a long-lived, stateful unit rather than a one-shot script run. Example, including optional handler ordering (`priority`, gated by Backend config) and non-blocking handlers (`async`, FR-22/FR-23):

```
plugin {
  name = "plugin_name",
  // backend defined plugin details here
}

let a = 1;
let b = { some_prop_obj: { some_prop: "1" } }

@Event(BackendRegisteredInit)
fn init_function_here(params) { a = 2; }

@Event(BackendRegisteredEventName, priority = 1)
fn example_event_function(params) {
  // logic goes here — runs before other BackendRegisteredEventName
  // handlers with a higher priority value, if the Backend allows ordering
}

@Event(BackendRegisteredEventName, async = true)
fn example_async_handler(params) {
  // runs concurrently, doesn't wait its turn; Global Variable writes
  // here default to unsafe/lock-free unless the Backend says otherwise
}
```

Realizes UJ-1.

**Functional Requirements:**

#### FR-17: Plugin registration as a distinct mode
A Backend can register a Plugin as a mode distinct from Direct/Cached Execution (§4.2); a connection may use both modes concurrently for different purposes.

**Consequences (testable):**
- The daemon can tell, unambiguously at the protocol level, whether a given registration is a Script (§4.2) or a Plugin.
- Registering a Plugin does not consume or interfere with the Direct/Cached Execution path on the same connection, and vice versa.
- [NOTE FOR PM] A single Backend connection can register multiple distinct Plugins concurrently. A Plugin's `name` is unique per Backend Client ID, not daemon-wide — consistent with different Backends being independent and potentially mutually untrusted (§Deployment model, brief). This is a PM decision per explicit delegation, not separately user-confirmed.

#### FR-18: Reserved lifecycle event — BackendRegisteredInit
When a Plugin is registered, the daemon invokes every handler annotated `@Event(BackendRegisteredInit)` exactly once, before the Plugin accepts any other Event.

**Consequences (testable):**
- Init handlers run to completion (or fail the registration) before the daemon marks the Plugin ready to receive other Events.
- `BackendRegisteredInit` is reserved — a Backend cannot repurpose or redefine what fires it.

#### FR-19: Backend-defined events, declared upfront
A Backend declares, at Plugin registration time, the full set of Event names it may fire against that Plugin and the object shape each Event returns. Once registered, the Backend can fire any declared Event (any name other than the reserved `BackendRegisteredInit`), invoking every handler function in that Plugin annotated `@Event(<that name>)`.

**Consequences (testable):**
- A Plugin registration that omits the Event-name/return-shape declarations is rejected, or accepted with no Events beyond `BackendRegisteredInit` — declaration is not optional for any custom Event.
- Firing an Event name that wasn't declared at registration is rejected, not silently accepted.
- A handler's result for a given Event must conform to that Event's declared return shape; a non-conforming result is a defined error, not an ambient type-mismatch failure.
- Firing a declared Event with no matching `@Event` handler in the Plugin is a defined no-op, not an error that tears down the Plugin.
- Each Event handler invocation is bounded by the same Resource Budget (§4.4) and can only reach the host through the same Capability-based RPC (§4.3) as Direct/Cached Execution — Plugin mode changes state lifetime and invocation trigger, not the trust model.

#### FR-20: Global Variable state and locking
A Plugin's Global Variables persist and are shared across all Event invocations for that Plugin's lifetime (FR-21), with per-top-level-key locking by default so mutating one key does not block concurrent access to sibling keys.

**Consequences (testable):**
- Two concurrent Event invocations reading/writing different top-level keys of the same Global Variable proceed without blocking each other.
- Two concurrent Event invocations writing the same top-level key serialize under the default (safe) strategy.
- A Backend can configure a Plugin's Global Variable strategy to unsafe/lock-free at registration time; under that mode, concurrent same-key writes are not serialized and racy reads are possible by design.
- Where the Backend's config allows it, Plugin code can override the locking strategy for a specific Global Variable, taking precedence over the Backend's default for that variable only.
- An `async = true` handler (FR-23) mutating a Global Variable defaults to the unsafe/lock-free strategy regardless of the Plugin's general default, unless the Backend has explicitly opted async handlers into the safe/mutex strategy.

**Out of Scope:**
- Cross-Plugin shared state — Global Variables are private to the Plugin that declares them in v2.

#### FR-25: Global Variable behavior strategy
A Global Variable's lifetime/scoping is configurable per-variable, independent of its locking strategy (FR-20): `forever` (default), `ttl: <duration>`, `separate_each_trigger`, or `keyed`.

**Consequences (testable):**
- A Global Variable declared with no explicit behavior defaults to `forever` (persists for the Session's lifetime, §Glossary).
- A `ttl`-behavior variable not written within its TTL window reads as its initial/reset value on next access, not the stale prior value.
- A `separate_each_trigger` variable's mutations in one Event invocation are never visible to a different invocation, even for the same Event on the same Plugin.
- A `keyed` variable's value is partitioned by a key the Backend supplies when firing the Event (not derived from params implicitly, not from Client ID); whether a given Event requires a key is declared per-Event at registration (FR-19); firing a key-required Event without one is a defined error, not a silent fallback to an unkeyed default.

#### FR-21: Plugin lifecycle tied to Session
A Plugin's registration and Global Variable state persist for the lifetime of its Backend's Session (§Glossary) — surviving a reconnect (FR-2) — and are torn down when the Session's TTL (set in System Config, FR-24) expires without a reconnect, or on explicit unregister.

**Consequences (testable):**
- With multiple concurrent connections under one Client ID (FR-2), the Session's TTL clock only starts once the last attached connection drops — while at least one connection is attached, the Session is live with no TTL countdown running.
- After a reconnect using the same Client ID, within the Session's TTL, a previously registered Plugin's Global Variable values are unchanged from before the disconnect.
- A disconnect that reconnects after the Session's TTL has expired starts fresh — no prior Plugin or Global Variable state is available, the same as a first-ever connection.
- An explicit unregister message removes the Plugin and its state immediately, independent of TTL; a subsequent Event fired against that name is rejected as unknown.
- On Backend disconnect, an in-flight Event invocation is not immediately cancelled — the daemon attempts to run it to completion locally.
- If that in-flight invocation triggers a Remote Function call (an RPC back to the now-disconnected Backend) and the disconnection is detected at that point, the invocation is cancelled there rather than hanging or silently failing.

#### FR-22: Handler ordering via priority
Plugin code can bind a handler to an Event with an explicit order, for example `@Event(<name>, priority = 1)`, controlling execution order relative to other handlers bound to the same Event — permitted only when the Backend's config allows Plugin-level ordering control.

**Consequences (testable):**
- When priority is used and the Backend permits it, handlers for the same Event on the same Plugin run in ascending priority order (lower value first). [ASSUMPTION: "lower runs first" is the assumed convention — not separately confirmed.]
- When the Backend does not permit Plugin-level ordering control, `priority` annotations are ignored (or rejected at registration — implementation choice) and handlers run in a defined default order. [ASSUMPTION: default order is registration order — not separately confirmed.]

#### FR-23: Async handlers bypass ordering
A handler annotated `@Event(<name>, async = true)` runs concurrently, without waiting for its turn in the Event's ordering (FR-22) — it starts alongside other handlers for that Event rather than queued behind them. `async` defaults to `false`.

**Consequences (testable):**
- With `async = true`, the handler's default Global Variable mutation strategy is unsafe/lock-free (FR-20), independent of the Backend's otherwise-configured default — unless the Backend explicitly configures async handlers to use the safe/mutex strategy too.
- `priority` and `async = true` on the same handler binding is a defined combination, not undefined behavior: async takes precedence, since a handler that doesn't wait its turn has no ordering to apply priority to. [ASSUMPTION: precedence rule, not separately confirmed.]

## 5. Non-goals (explicit)

The three non-user boundaries in §2.2 (no OS-level sandboxing, no transactional RPC effects, no built-in horizontal scaling) apply here too and aren't repeated. Additionally:

- Hexput will not ship or require a static configuration file for per-backend/business-logic Config (§Glossary) — that travels inline per FR-1. This does not extend to the System Config (§Glossary) covering the daemon's own operational settings, which is file-based by design (§4.1).
- Hexput will not attempt to be a general-purpose embeddable language replacement for Rhai/Lua — it targets short, frequently-invoked, host-callback-heavy scripts specifically.

## 6. MVP scope

### 6.1 In scope
- Standalone daemon (systemd/Docker), all Transports (UDS, Named Pipe, TCP+TLS, WebSocket)
- Connection init with inline config + registration (FR-1), reconnect via Client ID with reconnect protection (FR-2, FR-13), runtime/per-execution config override incl. feature toggles (FR-3), file-based System Config for daemon operational settings (FR-24)
- Direct Execution (FR-4) and Cached Execution with AST Cache (FR-5), processed asynchronously and non-blocking (FR-16), with an optional Backend-configured static check before execution (FR-26)
- Capability-based RPC: register/handling separation, `context.allow()` and per-call `return true` (FR-6, FR-7), Registered Methods on keyed objects (FR-27), and Value Secrets with Reference IDs and host-side modifications (FR-28)
- Multi-dimensional Resource Budgeting (FR-8)
- Client SDKs, Phase 1: JavaScript and Python (FR-10)
- Health/metrics surface and structured logging (FR-11, FR-12)
- Tree-sitter grammar and basic LSP (FR-14, FR-15)
- Plugin Registration & Events: registration as a distinct mode, reserved `BackendRegisteredInit`, upfront-declared Backend-defined events, priority/async handler ordering, Global Variable state/locking/behavior, Session-tied lifecycle with disconnect handling (FR-17…FR-25)
- MessagePack wire protocol

### 6.2 Out of scope for MVP
- Client SDKs, Phase 2: Node.js, Rust, Go — deferred, tracked separately, not a launch blocker
- Master-slave horizontal scaling — deferred, future work
- Per-function rollback registration / transactional effects — deferred, future work
- OS-level sandboxing — permanently out, not a future-work item
- Published benchmark suite results — the harness is in scope to build (§7 SM-1); the comparative numbers themselves (vs. Rhai, the primary comparator — see the brief's addendum for the full plan) are a thesis deliverable tracked separately, not a shipping blocker
- LSP semantic features (go-to-definition, capability-aware autocomplete) — basic diagnostics/completion only in v2

## 7. Success metrics

*The Vision's benchmark-substantiated success claim needs to be reconciled with §6.2, which defers the comparative benchmark *result* past the MVP shipping bar. Resolution: the harness is the MVP-scoped metric; the comparison numbers themselves are a tracked thesis deliverable, not a v2 launch gate.*

**Primary**
- **SM-1**: A working benchmark harness ships that measures steady-state Cached Execution p50/p99 latency and throughput on a like-for-like workload against Rhai. Target: harness exists and produces numbers, not a specific latency figure (no target is fixed pre-benchmark — see §8 Performance NFR). Validates FR-5.

**Secondary**
- **SM-2**: Zero daemon crashes/unhandled panics attributable to script or Plugin Event handler execution (malformed input, budget violations, capability denials) over a sustained load-test run. Validates FR-8, FR-19, the Reliability NFR.
- **SM-3**: The daemon sustains its target concurrent Cached Execution throughput without any Resource Budget enforcement failure (a budget dimension silently not enforced under load). Validates FR-8.

**Counter-metrics (do not optimize)**
- **SM-C1**: Resource-budget/capability enforcement bypass rate must stay exactly zero. Do not trade correctness of FR-6…FR-8 for a better SM-1 latency number — a fast but leaky sandbox is a failed project, not a fast one. Counterbalances SM-1.
- **SM-C2**: Do not collapse the multi-dimensional Resource Budget (§4.4) into a single limit to make benchmarking simpler — the multi-dimensional model is the differentiator (§Why not existing solutions, brief), not overhead to shed for a cleaner number. Counterbalances SM-1.

## 8. Cross-cutting NFRs

### Security & capability model
- The capability model (§4.3) is the entire security boundary; there is no secondary enforcement layer. Every Registered Function call must be attributable to a Client ID (feeds FR-12 logging).
- No `unsafe` Rust in the parser/VM execution path should ship without explicit review — memory safety here is a security property, not just code quality (see addendum for the full hardening list).

### Performance
- Steady-state Cached Execution latency and throughput must be benchmarked against Rhai as the primary comparator; the full methodology lives in the [product brief's addendum](../../briefs/brief-hexput-2026-09-18/addendum.md#benchmark-plan), not this PRD's addendum.md. This PRD does not fix a numeric target pre-benchmark — the benchmark harness is FR-scope, the target is not.

### Reliability
- A single script's failure (panic, budget violation, malformed input) must not take down the daemon or affect other connections' in-flight executions.

### Concurrency
- Message handling and execution requests are fully asynchronous end to end (feeds FR-16): no execution request may block message processing or another execution request, whether on the same Backend connection or a different one. A slow Cached Execution and a fast Direct Execution submitted concurrently must not serialize behind each other.
- Plugin Global Variable access (FR-20) follows the same non-blocking principle at a finer grain: locking is per-top-level-key, not per-Plugin, so one Event handler mutating one key must not stall a concurrent handler working on a different key of the same Global Variable.

## 9. Open questions

- **OQ-1**: What exact shape does the health-check surface take — an RPC message type, or a separate lightweight endpoint (e.g. HTTP) alongside the RPC transports? (FR-11)
- **OQ-2**: What's the concrete reconnect-protection mechanism — a secret issued alongside the Client ID, a signed token, something else? (FR-13)
- **OQ-3**: Full enumeration of execution-policy feature toggles beyond loops/if-else (§Glossary, FR-3) — which language constructs need an on/off switch, and which are always-on?
- ~~**OQ-4**: Does config ever need to cover anything beyond execution policy...~~ **RESOLVED** — no, per-backend Config stays execution-policy-only; TLS/bind/log-level settings live in the separate, file-based System Config instead (FR-24).
- **OQ-5**: What differentiates the JavaScript SDK from the Node.js SDK (§4.6, FR-10) — transport support, API shape, or something else? Both are Phase 1, so this needs an answer before Phase 1 work starts, not after.
- **OQ-6**: No committed timeline exists for v2 (per brief). Without one, scope needs active, ongoing pruning to stay shippable — who makes that call, and on what cadence, given this is a solo-maintained, thesis-driven project? Not a blocker for this PRD, but worth an explicit answer before MVP Scope (§6) starts slipping.
- ~~**OQ-7**: Must a Backend declare, upfront, which Event names it may fire against a Plugin...~~ **RESOLVED** — yes, names and return shapes are declared upfront at registration (FR-19).
- ~~**OQ-8**: When multiple handler functions bind to the same Event name, is invocation order defined...~~ **RESOLVED** — `priority` (Backend-gated) and `async` control ordering; see FR-22, FR-23.
- ~~**OQ-9**: Can one Backend connection register multiple distinct Plugins concurrently...~~ **RESOLVED** (PM decision) — yes, `name` unique per Client ID; see FR-17.
- ~~**OQ-10**: What happens to an in-flight Event invocation when its Backend disconnects mid-execution...~~ **RESOLVED** — runs to completion locally, cancelled only when it next tries an RPC back to the disconnected Backend; see FR-21.
- **OQ-11**: Priority tie-breaking convention (lower-runs-first), the default handler order absent `priority`, and `async`+`priority` precedence are all currently [ASSUMPTION]s (FR-22, FR-23), not user-confirmed — low risk, but worth a quick confirm before implementation locks them in.

## 10. Assumptions index

- §4.7 (Observability & Operations): health/metrics/logging shape is PM-authored under explicit delegation, not user-confirmed line-by-line — flagged via OQ-1 above.
- §4.6 (Client SDKs), FR-10 note: JavaScript vs. Node.js SDK differentiation is undefined — flagged via OQ-5 above.
- §4.9 FR-17: multi-Plugin-per-connection and per-Client-ID name uniqueness is a PM decision, not separately user-confirmed.
- §4.9 FR-22/FR-23: priority tie-breaking ("lower runs first"), the default handler order absent `priority`, and `async`+`priority` precedence are all [ASSUMPTION]s — flagged via OQ-11 above.
