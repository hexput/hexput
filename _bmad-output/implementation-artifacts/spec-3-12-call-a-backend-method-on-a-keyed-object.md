---
title: 'Story 3.12: Call a Backend method on a keyed object'
type: 'feature'
created: '2026-09-28'
status: 'done'
baseline_commit: '67c6182607dd98e9ecc5d242fc03686ccffbd081'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-3-11-carry-hidden-metadata-on-values-the-script-cannot-touch.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** A Backend can expose only free functions. Its objects cannot have methods, even though Story 3.11 lets a value carry a Value Secret with a `key` (FR-27, LANGUAGE-REFERENCE §8).

**Approach:** An `Init` registration may name an object key, which makes it a **Registered Method** under that key.
- `value.name(args)` on a value whose Value Secret carries that key calls the method. The call is one generic `Call` carrying the receiver (a holder, with its secret) beside the arguments.
- It is decided, counted and toggled exactly like a Registered Function call: the same `hexput-enforce` decision, the same budgets and the same `rpc_calls` toggle.
- A Script can never override a method. Writing a property of that name on a keyed value is `capability.method_override`. The static check reports it too, when a starting variable is provably keyed.

## Boundaries & Constraints

**Always:**
- The capability decision stays in `hexput-enforce` and is driven only by `hexput-exec` (AD-3). The `Call`/`Authorize` envelopes stay built only in `hexput-rpc` (`RESTRICTED_NAMES`).
- The receiver travels exactly as Story 3.11 sends any value: a holder with its secret unchanged.
- Every denial reason is the same `capability.unknown_function` a function call gets, logged with its reason.

**Decisions (agent, 2026-09-28 — Erdem delegated: "kendin tahmin et", no approvals):**
1. **Registration wire.** An `Init` registration becomes `{name, blanket?, key?}`, where `key` is an optional non-empty string.
   - A registration with a `key` is a method under that key; one without is a function, as before.
   - Uniqueness is per `(key, name)`, so a function and a method, or methods under two keys, may share a name.
   - A non-string or empty `key` is refused as `protocol.invalid_payload` naming `registrations[i].key`.
   - `RegisteredFunction` gains `key() -> Option<&str>`.
2. **Dispatch rule, at `value.name(args)`** (dot or `?.` member call; `value["name"](args)` too):
   - (a) The receiver's key is the key in the Value Secret of the collection, or of the location the receiver expression reads.
   - (b) If `(key, name)` is registered, it is a method call, even when the value has an own property `name`.
   - (c) Otherwise, if the keyed value has an own property `name`, it is an ordinary property call.
   - (d) Otherwise, the call fails with `capability.unknown_function` spanned on the call, with nothing sent (Story 3.4's "reason = unregistered").
   - (e) A value with no key keeps today's behavior exactly, and nothing is sent.

   This is decided in the interpreter, which has no view of the registrations. It stops with a `HostCall` that carries `receiver: Some(…)` and the key whenever the receiver is keyed and the own-property rule (c) does not apply. `hexput-enforce`'s `check_call(key, name)` then decides allowed, ask-handler, or refused-as-unregistered.
3. **Wire.**
   - The method's `Call` payload is `{name, arguments, execution, receiver}`, and its `Authorize` question is `{name, arguments, execution, receiver}`.
   - `receiver` is present only for a method, so a function `Call` is unchanged.
   - The Backend learns the key from the receiver's secret.
4. **Order and counting.** A method call is refused by a disabled `rpc_calls` toggle (Story 3.9) before its receiver or arguments are checked. It is then checked for argument and receiver depth and sendability, counted as one RPC call and one side effect, and decided and dispatched like a function call. The receiver counts toward the argument depth limit.
5. **Override.** An assignment `v.name = x` or `v["name"] = x`, where `v` is keyed with key `K` and `(K, name)` is a registered method, fails with the new code `capability.method_override` (added to `Code::ALL`), spanned on the assignment target. The object is unchanged, and so is the location.
   - The interpreter does not know the registrations. `hexput-exec` hands the Execution the set of method names per key before the first run, through a builder such as `Execution::with_methods`. This is data only, like the toggles; it grants nothing.
6. **Static check.**
   - `hexput-check`'s `Environment` gains `with_keyed(variable, methods)`. `hexput-script` fills it from the starting variables whose holder carries a key, together with the Session's method names under that key.
   - An assignment to `v.m` or `v["m"]` (string literal), where `v` is such a starting variable that is never reassigned or shadowed anywhere in the Script and `m` is one of its methods, is an error finding `capability.method_override`, spanned like the runtime error.
   - No other method finding is added. `o.m()` never triggers the unknown-call rule.

**Never:**
- no modifications (Story 3.13);
- no method dispatch on a value without a key;
- no inheritance or fallback between keys;
- no new crate edge;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Method call | registration `{name:"save", key:"User", blanket:true}`; starting `u` holder with `key:"User"`; `return u.save(1);` | one `Call {name:"save", arguments:[1], receiver:<u holder>, execution}`; the reply value returned |
| Method beats property | `u`'s value has its own `save: 5` | still the method `Call` |
| Unknown on keyed | `u.nope()`, with no method and no own property | `capability.unknown_function`; nothing sent |
| Own property on keyed | `u.greet = fn() { return 2; }; return u.greet();` (`greet` is not a method under `User`) | `2`; nothing sent |
| No key | a plain object `o = {save: fn(){ return 1; }}`; `o.save()` | returns `1`; nothing sent |
| Per-call method | method without `blanket` | `Authorize` carrying `receiver`, then `Call` only on `true` |
| Unregistered key | `u` keyed `"Order"`, with methods only under `"User"` | `capability.unknown_function` |
| Toggle | `rpc_calls = false`; `u.save()` | `policy.construct_disabled`; nothing sent |
| Budget | `rpc_calls` limit 1; `u.save(); u.save();` | the second call is `budget.rpc_calls_exceeded` |
| Override | `u.save = 1;` / `u["save"] = 1;` | `capability.method_override` on the target; `u` unchanged (visible by passing `u` to another host call) |
| Override check | `check = "error"`; `u.save = 1;`, with `u` a keyed starting variable | rejected at submission with a `capability.method_override` finding |
| Registration refusal | `key: ""` / `key: 1` / duplicate `(key, name)` | `protocol.invalid_payload`, naming the path; no Session |

</frozen-after-approval>

## Code Map

- `crates/hexput-session/src/init.rs`: registration `key`; uniqueness on `(key, name)`; `RegisteredFunction::key()`.
- `crates/hexput-connection` and `crates/hexput-script`: pass `(name, key, blanket)` triples through to `hexput-exec`'s `Host`. Change the `(String, bool)` tuple to a small struct if clearer. `hexput-script` also fills the check `Environment` (decision 6).
- `crates/hexput-enforce/src/lib.rs`: `Capabilities` keyed by `(Option<key>, name)`; `check_call(key, name, span)`.
- `crates/hexput-interpreter`:
  - the member-call dispatch (decision 2);
  - `HostCall` gains an optional receiver (detached with its secret, by the Story 3.11 path) and key;
  - `Execution::with_methods`;
  - the override refusal on member and index assignment (decision 5);
  - the `rpc_calls` refusal on the method path (this resolves Story 3.9's deferred entry).
- `crates/hexput-exec`: measure the receiver with the arguments; count; decide with the key; pass the receiver to `hexput-rpc`.
- `crates/hexput-rpc`: `Call`/`Authorize` payloads gain `receiver` when present.
- `crates/hexput-shared/src/diagnostics.rs`: add `capability.method_override` to `Code::ALL`.
- `crates/hexput-check`: `Environment::with_keyed`, and the override finding.
- Tests: every matrix row, in `tests/{session,enforce,interpreter,exec,rpc,connection,check,shared}.rs` as fits.
- Docs:
  - LANGUAGE-REFERENCE §8: a dated note recording decisions 2, 4 and 5;
  - the Spine: a dated line on the FR-27 wire (`key` in the registration, `receiver` in `Call`/`Authorize`);
  - AGENTS.md: the Epic 3 paragraph, test count, next step Story 3.13;
  - `deferred-work.md`: resolve the Story 3.9 `rpc_calls`-on-methods entry.

## Tasks & Acceptance

**Execution:**
- [x] Registration `key` and capability by `(key, name)`.
- [x] Interpreter dispatch, receiver, override, toggle.
- [x] Exec and RPC: receiver on the wire, counting, deciding.
- [x] Check: keyed environment and the override finding.
- [x] Tests for every matrix row.
- [x] Docs.

**Acceptance Criteria:**
- Given a keyed receiver and a registered method, when it is called, then exactly one `Call` carrying `receiver` is sent, with the receiver's secret byte-identical, and every capability, budget and toggle rule of a function call applies.
- Given any value without a key, when `value.name(args)` runs, then behavior and wire output are identical to before this story.

## Spec Change Log

Implementation readings (agent, 2026-09-28), where the frozen intent left a choice open:

- **Whose key.** Decision 2 (a): an array or object is keyed by its *own* Value Secret only, and a string, number, bool or `null` by the secret of the place it is read from (the chain's base variable, or the property or element the previous link read). A place's key is never used for a collection, because the receiver then travels with the collection's secret and the Backend could not learn the key from it (decision 3). A function is never keyed.
- **Override receivers.** Decision 5 refuses a write only on an array or object whose own secret carries the key; a keyed scalar has no properties, so writing one stays the `type` error it was. For the same reason `hexput-script` declares keyed (decision 6) only a starting variable holding a keyed array or object — declaring a scalar would make the check's finding name a different code than the runtime's.
- **Unobservable "unchanged".** The override error ends the Script (errors are uncatchable), so "`u` unchanged" cannot be observed by a later host call; the tests pin that the refusal precedes the write and that nothing is sent, and that a non-method write (`u.other = 1; f(u)`) is what reaches the Backend.
- **Edges.** `?.` on `null` short-circuits before dispatch (§4.4), keyed or not. `__secret` is never a method name, neither at runtime nor in the check. The check's callable list (Story 3.10) now holds the Registered Functions' names only: a bare `save()` can never reach a method.
- **Refusal message.** A refused method call carries `` `name` is not a method this Script may call on this value `` — the same for every reason, as a function's refusals share theirs; the log line adds `key` beside `function` and `reason`.

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | blind + edge-case | Docs say a disabled `rpc_calls` refuses before the receiver is evaluated; the receiver (and an index key) must be evaluated to find the key | low | Real wording defect. **patch** (LANGUAGE-REFERENCE, AGENTS.md, deferred-work) |
| 2 | blind | Whether a typo'd method on a keyed value is counted, and which error wins under `rpc_calls` off, is undocumented and untested | low | Real. **patch** (doc + count test) |
| 3 | blind | `chain_place` is never cleared; correctness rests on an unstated ordering rule | low | Verification layer's probe showed no current bug. **patch** (take-on-read + stated rule) |
| 4 | blind | `check_call` allocates two `String`s per call; unused `Question::key()`; duplicated `debug!` | low | Real; direct. **patch** |
| 5 | blind + edge-case | The static check stays silent on `u.save()` under `rpc_calls` off, though it reports `f()` | low | Real gap in toggle parity. **patch** (finding pinned against the runtime) |
| 6 | verification-gap + blind | Untested: `?.` on a keyed `null`, a keyed array receiver/override, `u["__secret"]()`, a method call inside an assignment target, repeated dispatch in loops/recursion, a keyed scalar under the check, `fn u` shadowing in the check | low | Pre-verified. **patch** (tests) |
| 7 | edge-case | A method registered as `__secret` is accepted but undispatchable | low | Real. **patch** (refused at `Init`) |
| 8 | edge-case | A bad receiver's error blames "an argument" | low | Real; direct. **patch** |
| 9 | blind | `with_methods` also registers functions; `RegisteredFunction` now models methods too | low | Rejected: naming only; renaming public types across crates is more than a direct correction |
| 10 | blind | `arguments()` prepends the receiver and removes it with `remove(0)` | low | Rejected: correctness unaffected; restructuring is not a direct correction |
| 11 | blind | Per-execution method maps built even with the check off | low | Rejected: registrations are few; perf only |
| 12 | blind | Status disagreement across files | false | Step 5 syncs the sprint status |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
