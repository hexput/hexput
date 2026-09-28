---
title: 'Story 3.12: Call a Backend method on a keyed object'
type: 'feature'
created: '2026-09-28'
status: 'in-progress'
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
- [ ] Registration `key` and capability by `(key, name)`.
- [ ] Interpreter dispatch, receiver, override, toggle.
- [ ] Exec and RPC: receiver on the wire, counting, deciding.
- [ ] Check: keyed environment and the override finding.
- [ ] Tests for every matrix row.
- [ ] Docs.

**Acceptance Criteria:**
- Given a keyed receiver and a registered method, when it is called, then exactly one `Call` carrying `receiver` is sent, with the receiver's secret byte-identical, and every capability, budget and toggle rule of a function call applies.
- Given any value without a key, when `value.name(args)` runs, then behavior and wire output are identical to before this story.

## Spec Change Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
