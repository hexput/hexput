# Epic 3 Context: Expose the host safely

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Let a Backend expose its host to scripts on its own terms, and bound what any one script may cost. A script reaches the host only by calling the functions a Backend registered, or the methods it bound to keyed objects. Each is granted either blanket at registration or per call by the Backend's own handler. Values cross the boundary with hidden Value Secrets that the Backend can read, edit and use to report changes, and a script can never observe or forge them. Every execution is bounded independently across six Resource Budget dimensions. Budgets, language feature toggles, the static-check mode and the argument depth limit are set in the Session's live Config, can be updated at runtime, and can be overridden per execution. This epic is the entire trust boundary of the system: there is no OS sandbox and no second layer behind it, so any bypass or leak of `__secret` is a security defect.

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
- Story 3.11: Carry hidden metadata on values the script cannot touch
- Story 3.12: Call a Backend method on a keyed object
- Story 3.13: Keep referenced values in step between Backend and script

## Requirements & Constraints

- **Host calls.** A call is a host call when its callee is a bare name that no scope declares. A local binding of the same name (`let`, parameter, named `fn`, starting variable) shadows it, and then nothing is sent. A host function is not a value: naming it without calling it is `reference.undeclared_identifier`. The script suspends until the Backend replies, holding no thread and no lock while it waits. Where there is no host at all (the local `hexput eval`), every host call is `capability.unknown_function`, spanned on the call.
- **Capability.** A function registered with `context.allow()` is callable without a per-call round trip. Otherwise the Daemon sends the Backend an `Authorize` question before each call and proceeds only on `true`, never caching the answer. A non-boolean answer, an error, a timeout (the authorization timeout) or a lost connection denies the call, and the logs must tell that denial apart from an explicit refusal. An unregistered name is `capability.unknown_function`, spanned on the call, and a denied call raises the same `capability` error, so a script cannot tell them apart. Grants never leak across Sessions.
- **No ambient host access.** The language has no filesystem, network, process or environment facility, and no standard library, not even `len`; the builtin list is empty. A fresh execution binds only its starting variables and top-level functions. Reading an undeclared name is `reference.undeclared_identifier`; only calling one is `capability.unknown_function`. A test enumerates every reachable name.
- **Host errors are not the Daemon's failure.** A Backend error, a malformed reply, or a call that could not be sent at all becomes `host.function_failed`. A connection that ends before the reply becomes `host.no_reply`. Both are spanned on the call, are distinct from `capability`, and cannot be caught (there is no `try`).
- **Arguments are data.** A function argument is `type.function_argument` and a cyclic one `type.cyclic_argument`. An argument or receiver nested deeper than the argument depth limit (default 12, set by Config or per execution) is `depth.argument_too_deep`. Each error is spanned on the offending value, and nothing is sent when any argument is refused. The argument checks (type, depth, frame size) run before the capability check, so an argument error is reported whether or not the name is registered.
- **Resource Budget.** There are six dimensions: CPU time, memory, allocation count, RPC call count, output size and side-effect count. Each is its own limit and error code (`budget.cpu_time_exceeded`, `memory_exceeded`, `allocations_exceeded`, `rpc_calls_exceeded`, `output_size_exceeded`, `side_effects_exceeded`), never folded into another. Exceeding one terminates that execution only; every other in-flight execution carries on and the Daemon never panics. Tests pin exact counts for known scripts:
  - **CPU time:** time spent running Script code, measured on its thread; waits on the Backend never count. Spanned on the construct running when crossed.
  - **Memory:** approximate live bytes held by the Script's values.
  - **Allocations:** defined by the language, not by storage policy — each string literal evaluated, concatenation, to-string conversion of a concatenation's non-string operand (`"a" + 1` is three), `for … in` key, and array/object literal, plus each append taking a collection's length past a power of two (1→2, 2→3, 4→5, 8→9 …). Scalars, rebindings, copies, starting variables and host-call return values do not count.
  - **RPC calls:** each host call (function or method) once its arguments pass the checks and before any capability decision, so a refused, denied or failed call counts; one with an unsendable argument does not. An `Authorize` question is part of its call, not a second count. The call that would cross the limit is never sent.
  - **Side effects:** each host call (counted exactly as RPC calls) plus each committed Global Variable write.
  - **Output size:** exact MessagePack byte length of the result's `{value}` payload, spanned on the whole Script.
  - Calls made before a limit is crossed stand; nothing is rolled back.
- **Limits and ranges.** The limit in force is the Daemon default, overlaid by the Session's Config, overlaid by the execution's override. Each is an integer in an inclusive range, refused (never clamped) outside it, and an error naming a limit names the one in force:

  | Setting | Range | Default |
  | --- | --- | --- |
  | `budget.cpu_time_ms` | 1 – 60 000 | 1 000 |
  | `budget.memory_bytes` | 1 KiB – 1 GiB | 64 MiB |
  | `budget.allocations` | 0 – 100 000 000 | 1 000 000 |
  | `budget.rpc_calls` | 0 – 100 000 | 100 |
  | `budget.output_size_bytes` | 1 – 16 MiB (max frame) | 1 MiB |
  | `budget.side_effects` | 0 – 100 000 | 100 |
  | `argument_depth` | 1 – 64 | 12 |
  | `authorization_timeout_ms` | 1 – 60 000 | 5 000 |

  Zero is meaningful only for counted dimensions (`rpc_calls = 0` forbids every host call).
- **Config and overrides.** Config holds the budgets, argument depth and authorization timeout, and gains `features` (toggles) and `check` (mode) beside them. It lives only in the Session and never on disk. `Init.config` and `ExecutionStart.overrides` share one shape and one decoder: nested maps of MessagePack integers, every key optional; a float, wrong type, unknown/repeated/non-string key or out-of-range value is `protocol.invalid_payload` naming the full path and range, a refused `Init` creates no Session and a refused `ExecutionStart` runs nothing. An override applies to that execution only and leaves the stored Config unchanged. A runtime update (Story 3.8) from any attached Connection is seen by every Connection's later executions with no reconnect: `ConfigUpdate {config}`, a complete Config in `Init.config`'s shape that replaces the stored one (a setting it leaves out is back to its default; never a patch), answered `Result {}`.
- **Feature toggles.** The set is closed: `loops`, `conditionals`, `callbacks`, `object_literals`, `array_literals` and `rpc_calls`. All default to enabled. Any other name, including declarations, scalar literals, operators, property and index access, `return` and Global Variables, is rejected as an unknown toggle. Using a disabled construct raises a `policy` error naming the toggle, distinct from `budget` and `capability`. `rpc_calls` blocks every host call, including one that holds a blanket grant.
- **Static check mode.** The modes are `off` (the default, with no pass at all), `warn` (findings are returned alongside the result) and `error` (the script is rejected before any statement runs or any host call is made). The mode comes from Config and can be overridden per execution. The callable-name list is the Session's registrations, and a disabled construct is reported under the same `policy` category. For Cached Execution the check runs once at registration and never on each run. Passing the check grants nothing.
- **Value Secret.** Any value, even `null`, may carry a Value Secret: `ref`, an optional `key` and further Backend fields, all preserved intact. Inside a script it is invisible:
  - Reading `__secret` by `.`, `[]` or `?.` yields `null`.
  - Writing it, including as an object-literal key, is silently ignored.
  - `for … in` never yields it.
  - `==`, truthiness, conversion and printing never see it.
  - A copy made by binding or passing a value shares its Value Secret, while a value computed from it has none.
- **Reference IDs.** Every value sent in a `Call`, including each nested value, travels with a Value Secret. If it has none, the Daemon gives it a generated Reference ID, and the same value keeps that ID for the rest of the execution. Results send a holder only for values that already carry a Value Secret, so Epic 2's plain results stay valid.
- **Registered Methods.** When a value's Value Secret carries a key, `value.name(args)` calls the method registered under that key, even if the value has an own property of the same name. A script can never override one: writing a property of that name on a keyed value is `capability.method_override` (the object is unchanged), and the static check reports it when the environment declares a never-reassigned starting variable keyed, so the language server shows it too. A name that is neither a method under that key nor an own property is `capability`. A value without a key has no methods. Methods follow the same grants, budgets and `rpc_calls` toggle as plain Registered Functions.
- **Modifications.** A reply may carry `modifications: [{ref, value}]`, each a whole-value replacement. All of them are applied before the script resumes.
  - An object or array changes in place and keeps its identity.
  - A Reference ID names a location: an object or array itself (shared by identity), or the variable, property or element a string, number, bool or `null` arrived in or was passed from. A modified scalar or string is replaced at that location only; copies (`let m = n;`) and computed values carry no Value Secret and are unchanged.
  - The reverse direction too: every referenced location the script writes is reported once, with its final value, in the execution's `Result` under `modifications` (a Backend-supplied `n = 8`, ref `r1`, then `n = 9; return { ok: true };` → `modifications: [{ ref: "r1", value: 9 }]`).
  - An unknown ref is ignored, and a malformed list is `host.function_failed`.
- **Out of scope:** Cached Execution itself (Epic 4), Plugins and Global Variable writes beyond counting them (Epic 6), SDK-side `registerMethod` and Value Secret handling (Epic 8), budget-violation metrics (Epic 7), the playground's demo functions (Epic 10), and rollback of host side effects.

## Technical Decisions

- **One Executor (AD-3).** Every capability check and budget charge happens in `hexput-enforce`. Only `hexput-exec` may depend on `hexput-enforce`, and `check-crate-graph.py` pins that, so no execution path can check or charge anything on its own. The Executor creates one budget per execution, live for its whole duration; the interpreter only meters and never decides. The dimensions, the setting table (paths, ranges, defaults) and the validating settings type live in `hexput-shared::budget`, shared by enforce, check and metrics; the one Config/override decoder lives in `hexput-port`.
- **`hexput-rpc` owns host-call correlation.** That means Daemon-issued call ids, the pending-call table, the `Call` payload and decoding of the reply. `hexput-connection` gains a `hexput-rpc` edge and holds one per connection. It writes the `Call`s as the connection's single writer and routes each Backend `Result`/`Error` that names a pending call back to it. `hexput-rpc` never reaches `hexput-enforce`. `check-crate-graph.py` pins the exact dependency sets of `hexput-rpc` and `hexput-enforce`, and adds `hexput-rpc` to `hexput-connection`'s set.
- **Fixed wire contract.**
  - An `Init` registration is `{name, blanket}`, `blanket` an optional boolean: absent means no blanket grant, anything but a boolean is refused (Story 3.2). Without a blanket grant a call is decided per call (Story 3.3).
  - Every host call is one generic `Call {name, arguments, execution}` (`execution`: the id of the `ExecutionStart` that started the execution asking), plus `receiver` for a method; a per-call question is `Authorize` with the same payload.
  - The Backend replies with `Result {value, modifications?}` or `Error` under the call's id.
  - Ids are per direction: a Backend `Result`/`Error` always answers a Daemon `Call`, and a Daemon `Result`/`Error` always answers a Backend request. This changes Epic 2's rule, where a Backend `Result`/`Error` was always `protocol.unexpected_message`.
  - A value with a Value Secret travels as a holder map with exactly the keys `__secret` and `value` (`ValueHolder { key: Option<String>, ref: String, value, rest }`). A value without one travels plain.
  - The error shapes remain the single ones defined in `hexput-port`, and the new `host` category and `depth.argument_too_deep` code join them.
- **Placement.** The Value Secret is hidden per-value metadata in `hexput-interpreter`'s value model and adds no crate edge. This is the largest code change of the epic and touches every value path. Wire conversion and applying modifications belong to `hexput-exec`. The interpreter itself stays free of host reach: suspending at a host call fits its existing explicit continuation stack.
- **Config ownership (AD-5).** `hexput-session` holds the single live Config, read afresh together with the registrations for each execution; `hexput-script`, `hexput-exec` (and later `hexput-plugin`) never cache or snapshot it. `hexput-script` overlays the overrides and hands effective limits to `hexput-exec`. `hexput-script` is the one place Direct Execution invokes the check, reading the mode from the live Config.
- **Concurrency (AD-6).** Executions stay independent tasks. No lock of any kind is held across an `.await`, including while a call is pending.
- **Naming.** Glossary terms are used verbatim in code: `Registered Function`, `Registered Method`, `Capability`, `Resource Budget`, `Value Secret`, `Reference ID`, `Config`.

## Cross-Story Dependencies

- **Order:** 3.1 (generic `Call`) comes first, then 3.11 (Value Secret), then 3.12 (methods, which need `key`), then 3.13 (modifications, which need `ref`).
- **Enforcement:** 3.2, 3.3 and 3.4 build capability enforcement on 3.1's call path. 3.5 and 3.6 introduce `hexput-enforce` budgets. 3.7 makes budgets and the argument depth configurable, and 3.8 makes Config updatable. 3.9's toggles and 3.10's check mode read that same live Config and override path.
- **3.10** depends on 3.9, because disabled constructs become findings, and on the Session's registrations from 3.1.
- **Builds on earlier epics:**
  - Epic 1: the interpreter's continuation stack, the `hexput-check` pass with its `Policy` toggles and callable list, and the shared `Diagnostic`.
  - Epic 2: `hexput_exec::execute`, `hexput_script::direct_execution`, the connection loop as the single writer, Session registrations and Config, and the value conversion with its depth and size limits.
- **Later epics:**
  - Epic 4: runs the check at `CodeRegister`.
  - Epic 6: runs Plugin handlers through the same Executor and budget handle.
  - Epic 7: exposes per-dimension violation counters and denial logs.
  - Epic 8: implements `registerMethod`, holders and modifications in the SDKs.
