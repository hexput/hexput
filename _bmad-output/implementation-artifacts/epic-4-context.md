# Epic 4 Context: Execute hot logic fast

<!-- Generated from planning artifacts. Regenerate with compile-epic-context if planning docs change. -->

## Goal

A Backend registers a Script once (`CodeRegister`) and re-executes it many times with fresh variables (`CachedExecutionStart`), served from an in-memory AST Cache, with no state bleeding between runs. This is the parse-once/execute-many path behind the anchor use case — a pass/fail rule evaluated on every grade change in milliseconds — and the project's central performance claim. The epic closes by proving that claim with a Criterion harness: cold-parse versus cache-hit cost, steady-state p50/p99 and throughput, and a like-for-like comparison against Rhai.

## Stories

- Story 4.1: Register a script once for repeated use
- Story 4.2: Re-run a registered script with fresh variables
- Story 4.3: Keep the cache bounded and clean it up with the Session
- Story 4.4: Measure the cache win
- Story 4.5: Compare the hot path against Rhai

## Requirements & Constraints

- After `CodeRegister`, a `CachedExecutionStart` for that Script skips re-parsing and reuses the cached AST; two Cached Executions of the same Script with different variables produce independent results with no variable, state or mutation bleeding between them.
- A Direct Execution never creates an AST Cache entry.
- A registration whose source fails to parse returns the structured parse error and creates no entry.
- Cache entries are scoped to the registering Session: identical source registered by another Session never collides with or reuses it.
- An unknown or evicted handle is a defined "unknown script handle" error telling the Backend to re-register — never a silent re-parse.
- The cache is bounded by a configured capacity with an eviction policy observable in metrics; an explicit unregister removes an entry at once; Session teardown releases every entry the Session owns.
- Cached Execution goes through the same Executor, capability checks and six-dimension Resource Budget as Direct Execution. Correctness of capability and budget enforcement is never traded for a better benchmark number, and the multi-dimensional budget is never collapsed into one limit to simplify measurement.
- Executions (Direct or Cached) are asynchronous: a slow one never blocks message processing or another request on any connection.
- Benchmarks: cold first-parse and warm cache-hit times reported separately for the same Script; steady-state p50/p99 latency and throughput under concurrent load on a host-callback-heavy workload representative of the pass/fail rule; reproducible within a stated variance with workload and machine conditions recorded; Rhai numbers side by side under the same conditions; budget-enforcement overhead as a separate line item; UDS and TCP/TLS numbers reported separately. The harness existing and producing numbers is the success criterion — no latency figure is a gate.

## Technical Decisions

- `hexput-script` owns the AST Cache (moka, pinned in the workspace) and the Direct/Cached entry points; every execution reaches the interpreter only through `hexput-exec`'s one Executor (AD-3). `hexput-enforce` stays reachable only through `hexput-exec`.
- The static check (FR-26, AD-8) runs once per submission path: on Direct Execution and on `CodeRegister` — never again on `CachedExecutionStart`. Its mode is read from the Session's live Config at submission (AD-5); it grants nothing and charges nothing.
- `hexput-session` holds the single live copy of per-backend Config; Cached Execution reads the Config afresh on every dispatch, never snapshotting it at registration (AD-5).
- Every execution dispatches as an independent async task; no lock is ever held across an `.await` (AD-6).
- The crate graph is fixed: `hexput-session` cannot reach `hexput-script`, so Session-scoped cache cleanup must be wired without that edge; any new edge needs a dated Spine amendment and an update to `scripts/check-crate-graph.py`.
- The `CodeRegister` wire and handle are fixed: payload `{source, variables?}` (`variables` an array of starting-variable *names* for the static check only — nothing runs at registration), answered `Result {handle, findings?}`. The handle is a `ScriptHandle(u64)` spelled as exactly 16 lowercase hex characters, issued from one counter per AST Cache starting at 1, never reused in a Daemon run, not a secret. A parse error or an `error`-mode rejection creates no entry; every registration makes a new entry (no deduplication by source).
- The AST Cache is `hexput-script`'s `AstCache` (moka `sync`, keyed by `(ClientId, ScriptHandle)` to `Arc<Program>`), one per Daemon run, created by `hexput-daemon` beside the Session registry and handed to each connection through `serve(port, sessions, cache)`; every lookup is scoped by the caller's Client ID. Keying by Client ID is what lets Session cleanup release entries without a `hexput-session → hexput-script` edge.
- The `CachedExecutionStart` wire is fixed: payload `{handle, variables, overrides?}` (`variables` and `overrides` decoded exactly as `ExecutionStart`'s; `source`, any other unknown key and `overrides.check` — whatever its value — refused as `protocol.invalid_payload` naming the key), answered exactly as a Direct Execution (`Result {value, modifications?}`, never `findings`, or the `Error`). A malformed `handle` (not 16 lowercase hex characters) is `protocol.invalid_payload`; a well-formed one with no entry under the caller's Client ID — never issued (`0000000000000000` included), another Session's, or evicted/unregistered from Story 4.3 on — is the wire-only `protocol.unknown_script_handle`, naming the handle and telling the Backend to register again with `CodeRegister`. The payload is decoded fully before the lookup, so `invalid_payload` wins. A found entry runs through `hexput-script`'s one private run-and-reply helper shared with Direct Execution; nothing is parsed or checked. Registered variable names are neither stored nor enforced. A failure is logged at `debug` ("a Cached Execution failed", `code`, `handle` when well-formed).
- `AstCache::get` hands out a cloned `Arc<Program>`, so Story 4.3 may remove an entry while a run that already fetched it finishes safely (a test for that is deferred to 4.3, which adds the removal API).
- A later `ConfigUpdate` of the check mode or toggles never re-checks a registered Script; budgets, toggles at runtime and capability grants are still read live per execution.
- Wire messages are PascalCase `MessageType`s in a MessagePack envelope `{id, type, payload}`; failures use the one `ErrorBody` shape; static check findings ride as `findings`.
- Benchmarks use `criterion` (pinned in the workspace).

## Cross-Story Dependencies

- 4.2 builds on 4.1's handle and cache; 4.3 bounds and cleans up 4.1's cache and reuses 4.2's unknown-handle error; 4.4 and 4.5 measure 4.1–4.2's path.
- Depends on Epics 2–3: the Port, Session registry, connection loop, Executor, capability grants, Resource Budget, Value Secrets and the check mode.
- Eviction metrics (4.3) feed Epic 7's metrics scrape; TCP/TLS benchmark numbers (4.5) need Epic 5's transport, measured separately in Story 5.9.
