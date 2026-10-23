# Epic 6 Context: Stateful Plugins driven by events

<!-- Generated from planning artifacts. Regenerate with compile-epic-context if planning docs change. -->

## Goal

A Backend registers a Plugin — Hexput source with a `plugin { }` metadata block, top-level Global Variables and `@Event`-annotated handler functions — declaring upfront every Event name it may fire and each Event's return shape, then fires those Events to drive handlers that share persistent state across invocations under configurable locking and lifetime strategies, ordered by `priority` or bypassing ordering with `async`. This turns Hexput from a one-shot script runner into a host for long-lived, stateful units of user-authored logic (the "give your users a plugin ecosystem" vision), without changing the trust model: handlers obey the same Capability and Resource Budget rules as Scripts.

## Stories

- Story 6.1: Parse plugin source with metadata, globals, and event handlers
- Story 6.2: Register a plugin as its own kind of registration
- Story 6.3: Declare every event and its return shape upfront
- Story 6.4: Run initialization handlers before the plugin serves anything
- Story 6.5: Fire an event and run its handlers
- Story 6.6: Hold handler results to their declared shape
- Story 6.7: Share plugin state across invocations without stalling siblings
- Story 6.8: Choose the locking strategy
- Story 6.9: Keep a global variable forever or let it expire
- Story 6.10: Isolate state per trigger or per key
- Story 6.11: Order handlers on one event
- Story 6.12: Let a handler skip the queue
- Story 6.13: Keep plugin state for the life of the session
- Story 6.14: Finish in-flight work when the backend vanishes

## Requirements & Constraints

- **Plugin source grammar (normative, language reference "Plugin source"):** exactly one `plugin { }` block, required, the first non-comment construct; keys are bare identifiers assigned scalar literals (string, number, `true`/`false`/`null`) with `=`, comma-separated, trailing comma allowed, each key once; `name` is a required non-empty string, every other key is Backend-defined and opaque. After the block the top level holds only `let`s (Global Variables) and `fn`s (an `@Event`-annotated `fn` is a handler, an unannotated one a helper); anything else is a `syntax` error, and `let`/`fn` share one namespace. A Global Variable's initializer is an expression over literals (array/object literals, operators, member access) — never an identifier, call or function. `@Global(behavior = "forever"|"ttl"|"separate_each_trigger"|"keyed", ttl = "<n>ms|s|m|h|d", locking = "safe"|"unsafe")` optionally annotates a top-level `let` (`ttl` with, and only with, `"ttl"`). `@Event(<Name>, priority = <integer>, async = <bool>)` binds a top-level `fn` as a handler; `async` defaults to `false`. One annotation per declaration; `@Event`/`@Global` anywhere else is a `syntax` error. A handler takes exactly one parameter (`params` by convention). The parser only records annotations; whether the Backend permits them and what `priority`/`async` do are decided at registration and dispatch. `hexput_parser::parse_plugin` produces the `PluginProgram` (an ordinary `Program` plus metadata, Global Variable and handler side tables).
- Plugin registration is a protocol-distinct mode from Direct/Cached Execution — never inferred from payload shape; one connection may use both concurrently. Plugin `name` is unique per Client ID, not daemon-wide; one Session may hold several Plugins with independent state.
- The Backend declares the full set of firable Event names and each Event's return shape at registration; an undeclared Event fire is rejected with a defined error; a declared Event with no handler is a defined no-op. Return shapes reuse the language's own object-shape representation (no second schema language). Whether an Event requires a partition key (for `keyed` Globals) is declared per Event.
- `BackendRegisteredInit` is reserved: every handler bound to it runs exactly once, to completion, before the Plugin is ready; a failing init fails the registration and tears down its state; a Backend can neither declare nor fire it.
- Handler results are validated against the declared shape; a non-conforming result is a defined shape error naming the offending path, validated per handler independently.
- Global Variables persist across all invocations of the Plugin; locking is per top-level key by default (safe), with a Backend-selectable unsafe/lock-free strategy (lost updates possible, never corruption or panic, no `unsafe` Rust), per-variable code-level override only where the Backend's Config permits (otherwise rejected at registration). `async = true` handlers default to unsafe unless the Backend opts them into safe.
- Lifetime per variable: `forever` (default), `ttl` (reads the initial value after the window since the last write elapses), `separate_each_trigger` (invocation-private), `keyed` (partitioned by a Backend-supplied key only — never params or Client ID; a key-required Event fired without one is a defined error).
- Ordering: `priority` ascending, lower first, only when the Backend's Config permits; absent `priority`, source-declaration order; `async` + `priority` → async wins and `priority` is ignored, not rejected.
- Plugin and Global Variable state live for the Session: survive reconnect, torn down on TTL expiry or explicit unregister (after which a fire at that name is rejected as unknown). An in-flight invocation on disconnect runs to completion and is cancelled only when it next attempts an RPC to the gone Backend; mutations already made stand (no rollback).
- Handler failures (error, budget) are reported to the Backend and never take down the Plugin, siblings or the Daemon. The static check (FR-26) also runs at Plugin registration, adding Plugin-only findings (an `@Event` naming an undeclared Event; a `@Global` strategy the Backend forbids).

## Technical Decisions

- Crates: `hexput-plugin` (Plugin actor: registration, event routing/ordering metadata) depends on `hexput-parser`, `hexput-globalvar`, `hexput-exec`, `hexput-check`; `hexput-globalvar` (per-Plugin concurrent store, `dashmap`) depends on `hexput-shared` only and never on `hexput-plugin`, `hexput-rpc` or `hexput-enforce`; `hexput-session → hexput-globalvar` exists so the Session alone calls `teardown(plugin_id)`; `hexput-exec → hexput-globalvar`; `hexput-daemon → hexput-plugin`. `scripts/check-crate-graph.py` pins edges — update it with any new one. Language crates (`hexput-ast`/`lexer`/`parser`/`interpreter`) stay free of execution/host reach; `hexput-check` depends on `hexput-ast` alone.
- Plugin registration wire (fixed for every Backend and SDK): `PluginRegister` (behind the init gate; the message type alone marks it) with payload `{source, events}` (Story 6.3: `events` maps each Event name to `{returns, keyed?}`, a return shape written as a Hexput literal template), answered `Result {name}` (with `findings` when the static check found anything) or `Error` (`protocol.invalid_payload` for a malformed payload or `events` declaration — a bad return shape included, naming `events.<name>.returns` —, the parser diagnostic, the first error finding with every finding under `findings` when the static check runs under `error`, `protocol.duplicate_plugin_name`, `protocol.init_not_completed` if the Session ended mid-registration); nothing is registered on failure. `PluginId` (never on the wire, one counter per run) keys a Plugin inside the Daemon; the Session owns its `name → PluginId` set, while the parsed Plugin lives in `hexput-plugin`'s per-run `Plugins` registry, opened at `Init` and closed by the Session teardown hook after the AST Cache. Edges `hexput-connection → hexput-plugin` and `hexput-plugin → hexput-port` exist. The static check at Plugin registration runs from Story 6.3, under the Session's live check mode; firing an undeclared Event is `protocol.undeclared_event`.
- **AD-3:** Direct, Cached and Plugin handler execution all go through the one `hexput-exec` Executor; both Plugin dispatch paths (priority sequencing task and directly spawned `async` handlers) call the same single handler-invocation function. An `async` handler gets a still-live budget handle; Global Variable writes count as side effects. Global Variable access is intrinsic language state — no capability grant, never via `hexput-rpc`.
- **AD-4:** the store is independent of the Plugin actor's mailbox; locking per `(top-level-key, partition-key)`; no lock held across `.await`; `hexput_globalvar::teardown(plugin_id)` is the sole lifecycle entry, called synchronously by `hexput-session` before the actor drops, never via `Drop`. Priority chains run in a per-(Plugin, Event) sequencing task, so one Event's chain never blocks another Event.
- **AD-5:** Plugin dispatch reads the Session's live Config on every dispatch; never snapshots it at registration.
- **AD-6:** every handler invocation is an independent async task; the only serialization allowed is a Plugin's opted-in `priority` order among its own handlers for one Event.
- **AD-8:** `hexput-plugin` invokes `hexput-check` once at Plugin registration; passing the check grants nothing.
- Errors keep the §7 category/code/span shape and stable codes in `Code::ALL`; wire messages are MessagePack envelopes defined in `hexput-shared::wire`/`hexput-port`. Identifiers use Glossary terms verbatim (`Plugin`, `Event`, `GlobalVariable`, `Session`).

## Cross-Story Dependencies

- 6.1 (parser/AST) underlies everything; 6.2 adds the wire mode and per-Session Plugin registry; 6.3 the Event declarations that 6.4–6.6, 6.10 and 6.11 consume; 6.5 the handler-invocation function in `hexput-exec` reused by 6.11/6.12; 6.7 the store that 6.8–6.10 extend; 6.13/6.14 build on Epic 5's Session TTL and reconnect and on Epic 3's host-call path (`host.no_reply`).
- Epic 9's tree-sitter grammar and language server must describe the plugin syntax this epic defines.
