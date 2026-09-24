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
- Story 3.13: Apply the Backend's changes to referenced values

## Requirements & Constraints

- **Host calls.** A call is a host call when its callee is a bare name that no scope declares. A local binding of the same name (`let`, parameter, named `fn`, starting variable) shadows it, and then nothing is sent. A host function is not a value: naming it without calling it is `reference.undeclared_identifier`. The script suspends until the Backend replies, holding no thread and no lock while it waits.
- **Capability.** A function registered with `context.allow()` is callable without a per-call round trip. Otherwise the Backend's handler must return `true` for each call. A non-boolean answer, an error or no answer at all denies the call, and the logs must tell that denial apart from an explicit refusal. An unregistered name and a denied call raise the same `capability` error, so a script cannot tell them apart. Grants never leak across Sessions.
- **No ambient host access.** The language has no filesystem, network, process or environment facility, and no standard library, not even `len`. A test must enumerate every name a script can reach and show that each is either a pure language builtin or a Registered Function of that Session.
- **Host errors are not the Daemon's failure.** A Backend error or a malformed reply becomes `host.function_failed`. A connection that ends before the reply becomes `host.no_reply`. Both are spanned on the call, are distinct from `capability`, and cannot be caught (there is no `try`).
- **Arguments are data.** A function or cyclic argument is a `type` error. An argument or receiver nested deeper than the argument depth limit (default 12, set by Config or per execution) is `depth.argument_too_deep`. Either error is spanned on the offending value, and nothing is sent when any argument is refused.
- **Resource Budget.** There are six dimensions: CPU time, memory, allocation count, RPC call count, output size and side-effect count. Each is enforced independently, never folded into a single limit. Exceeding one terminates the execution with an error naming that dimension, and every other in-flight execution carries on unaffected. The four counted dimensions have fixed meanings, and tests pin exact counts for known scripts:
  - **Allocation count:** each string, array and object construction, plus each resize of a growing collection. Scalars and rebindings do not count.
  - **RPC call count:** each Registered Function or Method dispatch, counted whether the call is later denied or fails.
  - **Output size:** the serialized byte length of the result.
  - **Side-effect count:** each host dispatch plus each committed Global Variable write.
  - Whether a Value Secret counts as an allocation is left to Story 3.6.
  - RPC calls already made before a budget violation stand; nothing is rolled back.
- **Config and overrides.** Config holds the budgets, the feature toggles, the check mode and the argument depth. It lives only in the Session and never on disk. A runtime update from any attached Connection is seen by every Connection's later executions. A per-execution override applies to that execution only and leaves the stored Config unchanged. A value outside the allowed range is rejected, never clamped.
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
- **Out of scope:** Cached Execution itself (Epic 4), Plugins and Global Variable writes beyond counting them (Epic 6), SDK-side `registerMethod` and Value Secret handling (Epic 8), budget-violation metrics and logs (Epic 7), and rollback of host side effects.

## Technical Decisions

- **One Executor (AD-3).** Every capability check and budget charge happens in `hexput-enforce`. Only `hexput-exec` may depend on `hexput-enforce`, and `check-crate-graph.py` pins that, so no execution path can check or charge anything on its own. The Executor hands out a budget-accounting handle that stays live for the whole execution. The Budget dimensions are one enum in `hexput-shared` (`budget.rs`), shared by enforce, check and metrics.
- **`hexput-rpc` owns host-call correlation.** That means Daemon-issued call ids, the pending-call table, the `Call` payload and decoding of the reply. `hexput-connection` gains a `hexput-rpc` edge and holds one per connection. It writes the `Call`s as the connection's single writer and routes each Backend `Result`/`Error` that names a pending call back to it. `hexput-rpc` never reaches `hexput-enforce`. `check-crate-graph.py` pins the exact dependency sets of `hexput-rpc` and `hexput-enforce`, and adds `hexput-rpc` to `hexput-connection`'s set.
- **Fixed wire contract.**
  - Every host call is one generic `Call {name, arguments}`, plus `receiver` for a method.
  - The Backend replies with `Result {value, modifications?}` or `Error` under the call's id.
  - Ids are per direction: a Backend `Result`/`Error` always answers a Daemon `Call`, and a Daemon `Result`/`Error` always answers a Backend request. This changes Epic 2's rule, where a Backend `Result`/`Error` was always `protocol.unexpected_message`.
  - A value with a Value Secret travels as a holder map with exactly the keys `__secret` and `value` (`ValueHolder { key: Option<String>, ref: String, value, rest }`). A value without one travels plain.
  - The error shapes remain the single ones defined in `hexput-port`, and the new `host` category and `depth.argument_too_deep` code join them.
- **Placement.** The Value Secret is hidden per-value metadata in `hexput-interpreter`'s value model and adds no crate edge. This is the largest code change of the epic and touches every value path. Wire conversion and applying modifications belong to `hexput-exec`. The interpreter itself stays free of host reach: suspending at a host call fits its existing explicit continuation stack.
- **Config ownership (AD-5).** `hexput-session` holds the single live Config. `hexput-script` and `hexput-exec` read it on every dispatch and never snapshot it. `hexput-script` is the one place Direct Execution invokes the check, reading the mode from the live Config.
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
