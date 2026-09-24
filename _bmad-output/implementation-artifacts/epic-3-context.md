# Epic 3 Context: Expose the host safely

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Let a Backend expose host functions to Scripts, and bound everything a Script can do. A Backend registers the functions a Script may call. It grants each one either blanket at registration (`context.allow()`) or per call, through a handler that must return `true`. Every execution is limited on six independent dimensions: CPU time, memory, allocation count, RPC call count, output size and side-effect count. The Backend tunes those limits, the closed set of language feature toggles, and the static-check mode in its Config, updates them on a live connection, and can override them for a single execution. This epic is the whole trust boundary. There is no OS sandbox and no second enforcement layer behind it, so an enforcement bypass is a failed project, not a trade-off.

## Stories

- Story 3.1: Call a registered host function from a script
- Story 3.2: Grant a function blanket access at registration
- Story 3.3: Decide per call whether a function may be used
- Story 3.4: Deny every path to the host that isn't a registered function
- Story 3.5: Stop an execution that burns too much CPU or memory
- Story 3.6: Bound allocations, RPC calls, output size, and side effects
- Story 3.7: Tune budgets per backend and per execution
- Story 3.8: Change execution policy without reconnecting
- Story 3.9: Switch off language constructs by policy
- Story 3.10: Turn the static check on or off

## Requirements & Constraints

- **Host calls:** a Script reaches the host only by calling a Registered Function by name, as an ordinary call expression. The daemon sends an outbound RPC to the Backend over the same connection, waits for the reply and resumes the Script with the returned value. If the Backend replies with an error, the Script gets a defined runtime error tied to that call; the daemon does not treat it as its own failure. The Script cannot catch errors (v2 has no `try`/`catch`). Story 3.3 calls a denial "catchable", but the language reference wins: a denial, like any error, ends the execution and is reported to the Backend.
- **Capability errors:** calling an unregistered name and calling a denied function produce the same `capability` error, never an unknown-identifier (`reference`) error. `?.` must not hide it; there is no optional-call form. Grants never leak across Sessions. A per-call handler that returns a non-boolean, fails, or never answers means the call is denied, and the logs must tell that apart from an explicit refusal.
- **No ambient host access:** the grammar has no import, filesystem, network, environment or process facility, and there is no standard library at all. A test enumerates every name a Script can reach and asserts that each one is a pure language builtin or a Registered Function of that Session.
- **Resource Budget:** the six dimensions are enforced independently. Exceeding any one ends the execution with a `budget` error that names that dimension. Setting one dimension never stands in for another. The model must not collapse into one limit, even to make benchmarks simpler. A terminated execution never panics or restarts the daemon, and never disturbs other in-flight executions. RPC calls already made before a violation stand: nothing is compensated or rolled back.
- **Fixed dimension definitions (tests pin exact counts for a known Script):**
  - **Allocation count:** every heap-backed value created. That is each string, array or object construction, plus one per resize of a growing collection. Scalars and rebindings do not count.
  - **RPC call count:** every dispatch to a Registered Function, counted whether it is later denied or fails.
  - **Output size:** the serialized byte length of the returned value.
  - **Side-effect count:** every Registered Function dispatch plus every committed Global Variable write. A host call therefore counts against both RPC calls and side effects.
- **Tuning:** budget values in Config are the limits by default. A per-execution override applies to that execution only and leaves stored Config unchanged. An override outside the daemon's allowed range is rejected with a defined error, never silently clamped. A runtime Config update is its own message type, separate from init. After an update, every Connection attached to the Session sees the new values on its next execution.
- **Feature toggles:** the set is closed: `loops`, `conditionals`, `callbacks`, `object_literals`, `array_literals`, `rpc_calls`. All six default to enabled. Any other toggle name is rejected as unknown; declarations, assignment, scalars, operators, property and index access, `return` and Global Variable access are always on. Using a disabled construct raises a `policy` error that names the toggle, with a type and code distinct from `budget` and `capability`. `rpc_calls` off blocks a host call even when that function holds a valid blanket grant.
- **Static-check mode:** `off` (the default), `warn` or `error`, set in Config and overridable per execution.
  - `off`: no check runs.
  - `error`: a Script with any error-severity finding is rejected before any statement runs or any host call happens, and the findings are returned.
  - `warn`: the Script runs normally and the findings come back alongside the result.
  - The check takes the Session's Registered Function names as its callable list, so a typo'd host call becomes a finding instead of a runtime `capability` error. Disabled constructs are reported under the same `policy` category the runtime uses.
  - Passing the check grants nothing.
- **Attribution:** every Registered Function call must be attributable to a Client ID. Capability denials and budget violations are logged as distinct, identifiable events (Epic 7 builds on them).

## Technical Decisions

- **One Executor (AD-3):** every execution path goes through the single entry point in `hexput-exec`. Capability checks, budget accounting and the Registered Function dispatch all happen behind it. `hexput-enforce` has exactly one dependent, `hexput-exec`. `hexput-script`, `hexput-rpc`, `hexput-plugin` and `hexput-connection` cannot reach it. Grants are checked in `hexput-enforce`, never at the transport or registry layer.
- **Crate roles:**
  - `hexput-rpc`: the host-function registry, capability grants, and outbound RPC to the Backend. It depends on `hexput-port`.
  - `hexput-exec`: depends on `hexput-enforce`, `hexput-interpreter`, `hexput-rpc` and `hexput-globalvar`.
  - `hexput-interpreter`: depends on `hexput-ast` only. It has no host reach, so host calls and budget charging must be supplied to it by the Executor.
  - `hexput-shared` `budget.rs`: holds the six dimensions as one enum, shared by `hexput-enforce`, `hexput-check` and metrics.
  - `hexput-check`: stays `hexput-ast`-only. It is invoked from `hexput-script` on Direct Execution (Epic 4 adds `CodeRegister`), never on `CachedExecutionStart`.
  - Any new edge follows the spine's crate graph, gets a dated amendment, and updates `scripts/check-crate-graph.py` in the same change.
- **Config ownership (AD-5):** `hexput-session` holds the single live copy of per-backend Config. `hexput-script`, `hexput-exec` and `hexput-plugin` read through it on every dispatch and never snapshot it. That rule is what makes runtime updates reach every Connection. Config is execution policy only; it never touches System Config or disk.
- **Non-blocking waits (AD-6, locking rule):** while an outbound RPC is in flight, no lock is held across the `.await`, and the execution occupies no thread. Every execution stays its own task; a Script waiting on the host must not block other executions or message processing.
- **Wire and errors:** the new messages (the outbound RPC call and its reply, the Config update, per-execution overrides, and findings returned with a result) reuse the one MessagePack envelope and the single error shape in `hexput-port`. Denials, violations and policy failures travel as the existing diagnostic shape: category, stable code, severity, message and span. Exact schemas are the implementation's call; each new code joins the `Code::ALL` enumeration.
- **Safety and naming:** `unsafe` stays forbidden in the lexer, parser, interpreter and executor crates. Use the glossary terms verbatim: `Registered Function`, `Capability`, `Resource Budget`, `Config`.

## Cross-Story Dependencies

- **Order:**
  - 3.1 (the outbound RPC round trip, plus `capability` for unregistered names) comes first.
  - 3.2 and 3.3 add the two grant mechanisms on top of it. 3.4 asserts that nothing else is reachable.
  - 3.5 sets up budget enforcement in `hexput-enforce` through the Executor. 3.6 adds the other four dimensions, and needs 3.1 for the RPC and side-effect counts.
  - 3.7 needs budget keys in Config (3.5/3.6) and per-execution overrides. 3.8 adds the Config update message.
  - 3.9 and 3.10 add toggles and check mode as more Config keys, using the same override path. 3.10 needs 3.9's `policy` category and the Registered Function names from 3.1.
- **From Epic 1:** the Epic 1 static check pass and its six-toggle `Policy` (all enabled by default), and the diagnostic shape with its categories.
- **From Epic 2:** Sessions with their registrations and empty Config (every `config` key is currently refused), the `ExecutionStart` path through `hexput_exec::execute`, per-execution tasks, and Client-ID-tagged tracing spans.
- **Later epics:**
  - Epic 4 runs the check once at `CodeRegister` and budgets every Cached Execution the same way.
  - Epic 6's Plugin handlers go through the same Executor, with a live budget handle for `async` handlers and Global Variable writes counted as side effects.
  - Epic 7 turns this epic's denial and violation events into per-dimension metrics and logs.
