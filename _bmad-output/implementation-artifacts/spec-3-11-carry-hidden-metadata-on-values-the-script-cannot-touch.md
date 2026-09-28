---
title: 'Story 3.11: Carry hidden metadata on values the script cannot touch'
type: 'feature'
created: '2026-09-28'
status: 'done'
baseline_commit: 'bb507c7e4df71e5ed29dcf1b0dd9e92a4eda2b22'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** A Backend cannot identify or annotate a value it hands to a Script, or one a Script hands back. There is no Value Secret (FR-28, LANGUAGE-REFERENCE §3/§8). Every value crosses the wire plain, so a Backend cannot tell which of its objects a `Call` argument is. Stories 3.12 (methods by key) and 3.13 (modifications by Reference ID) both need that identity.

**Approach:** Give the interpreter's value model a hidden, opaque **Value Secret**: a Reference ID `ref`, an optional `key`, and every further Backend field.

- **Where it is carried.** Per LANGUAGE-REFERENCE §3, a Reference ID names a *location*:
  - a collection carries its own secret, shared by identity;
  - a string, number, bool or `null` carries it on the variable, property or element that holds it.
- **Script invisibility.** Scripts can never observe or change a secret:
  - reading `__secret` gives `null`;
  - writing it is ignored;
  - `for … in` never yields it;
  - `==`, truthiness and conversion ignore it.
- **The boundary:**
  - Starting variables, `Call` results and the Script's result travel in the holder shape `{__secret: {ref, key?, …}, value}`.
  - Every value sent in a `Call` goes as a holder. One without a secret is given a Daemon-generated Reference ID, which stays stable for the rest of the execution.

This is the trust boundary (Epic 3): any path that lets a Script see, forge or strip a secret is a security defect.

## Boundaries & Constraints

**Always:**
- LANGUAGE-REFERENCE §3/§8 and the Spine's FR-27/FR-28 wire-contract amendment (`ValueHolder { key, ref, value, rest }`) are normative.
- The interpreter still depends on `hexput-ast` alone. It holds the secret's further fields as an opaque blob: `hexput-exec` encodes and decodes them as MessagePack bytes, and the interpreter never reads them.
- Values without a secret travel exactly as before, so every Story 2.6 wire value and result stays valid.
- `hexput eval` / `evaluate*` have no holders and behave as before.

**Decisions (agent, 2026-09-28 — Erdem delegated: "kendin tahmin et", no approvals):**
1. **Holder recognition (inbound).** A MessagePack map with exactly the two keys `__secret` and `value` is a holder.
   - `__secret` must be a map with a string `ref` (non-empty, at most 256 bytes) and an optional string `key`. Any further keys are kept verbatim, in order.
   - `value` may be any value, but not itself a holder. Holders may nest *inside* a collection's elements or properties.
   - Everything else is refused, as `protocol.invalid_payload` for starting variables and as `host.function_failed` (a malformed reply) for a `Call` result. This covers:
     - a malformed `__secret`;
     - a holder directly inside a holder;
     - any other map that has a `__secret` key.
   - So the key `__secret` never enters a Script's view.
2. **Where an inbound secret lands.**
   - A holder around an array or object sets that collection's secret.
   - A holder around a scalar or string sets the secret of the location it arrives in: the starting variable, the property or element it sits in, or, for a `Call` result, the result value's destination. For a `Call` result the secret is kept only if the call's value is stored directly into a variable or property by the statement making the call (`let x = f();`, `o.p = f();`). Otherwise the scalar is a computed value and plain.
3. **Copies are plain.** Binding a scalar location to another (`let m = n;`), placing it in a new literal, or computing from it gives a plain value. Writing a new value into a referenced location keeps the location's secret; that keeps Story 3.13's modifications keyed to it.
4. **Outbound in a `Call`.** Every argument and every nested element or property travels as a holder:
   - An argument written directly as a location (`f(n)`, `f(o.p)`, `f(a[0])`) carries that location's secret.
   - A collection carries its own secret.
   - A value with no secret is given a fresh Daemon Reference ID, stored back on its collection or location (when it has one), so the same collection or location sent again in this execution carries the same ID. A computed argument (`f(1 + 2)`) gets a fresh ID each time.
5. **Reference ID format.** `hx:<16 lowercase hex nonce>:<counter>`.
   - The nonce is drawn once per execution from `std`'s `RandomState` hasher, which needs no new dependency.
   - The counter starts at 1.
   - Backend-supplied IDs are used as given and never renamed.
6. **Outbound in the result.**
   - A collection with a secret, and a scalar location inside a collection that has one, travel as holders with the secret unchanged.
   - A top-level returned value carries a secret only when the return expression is directly a referenced location, or a collection that has one.
   - Nothing is generated for the result; values without a secret travel plain.
7. **Budgets and limits.**
   - A secret is not an allocation. Its bytes are charged to the memory budget.
   - The output-size budget measures the exact `{value}` payload *including* holders (`payload_size` is extended and still pinned against `rmp_serde`).
   - The argument depth limit counts the Script's nesting, not holder wrappers. A `Call` whose holder-wrapped encoding would pass the frame nesting limit is unsendable (`host.function_failed`), like any over-frame argument.
8. **Invisibility rules.**
   - Reading `__secret` by `.`, `[]` or `?.` yields `null` on *every* value, never a type error.
   - Assigning it by `.` or `[]` is a silent no-op.
   - An object-literal `__secret` key is dropped.
   - `for … in` can never see it, because it is never stored as a key.

**Never:**
- no modifications in either direction (Story 3.13);
- no Registered Methods (Story 3.12);
- no secret exposed to `hexput-check` or the CLI;
- no new crate edge;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Holder in | starting variable `u = {__secret:{ref:"r1",key:"User",tier:3}, value:{name:"a"}}`; `return u.name;` | `"a"` |
| Round trip | same `u`; `return u;` | result is a holder with `ref` "r1", `key` "User", `tier` 3 and the same value |
| Invisible | `return [u.__secret, u["__secret"], u?.__secret, 5 .__secret];` | `[null, null, null, null]` |
| Writes ignored | `u.__secret = 1; u["__secret"] = 2; let o = {__secret: 3, a: 1}; for (k in o) {…}` | no error; `o` has only `a`; the result shows `u`'s secret unchanged |
| Equality | `u == u`, a holder string `s` with `s == "x"`, truthiness | as for the plain values |
| Call gives refs | `f(o)` where `o = {a: 1}` has no secret, called twice | both `Call`s carry `o` with the same `hx:` ref; `o.a` carries its own stable ref |
| Call keeps refs | `f(u)` | `u` is sent with `r1`, `key` and `tier` intact |
| Scalar location | starting `n` held with ref `r2`; `f(n); let m = n; f(m);` | the first argument carries `r2`; the second a fresh `hx:` ref |
| Result from call | `let x = f();` where the reply is a holder scalar with `r3`; `g(x)` | `g` receives `r3` |
| Bad holders | `{__secret: 1, value: 2}`; `{__secret:{}, value:1}`; a holder inside a holder; `{__secret:{ref:"r"}, value:1, x:2}` | starting variable: `protocol.invalid_payload` naming the path; `Call` reply: `host.function_failed` |
| Plain unchanged | no holders anywhere | wire output byte-identical to before |

</frozen-after-approval>

## Code Map

- `crates/hexput-interpreter/src/value.rs`, `heap.rs`, `machine.rs`, `lib.rs`:
  - add a public `Secret { reference: Arc<str>, key: Option<Arc<str>>, extra: Arc<[u8]> }`, where `extra` is opaque encoded MessagePack of the further fields;
  - attach it to heap collections and to storage locations (variable slots, object properties, array elements);
  - extend the detached `Value` exchange (`Heap::attach`, argument detaching, the result, and `HostCall::resume`) so secrets cross in and out. For example, `Array`/`Object` gain an optional secret, and entries/elements and top-level values carry an optional location secret. Choose the smallest shape that keeps `Value`'s existing variants and `evaluate*` untouched;
  - generate Reference IDs from a prefix set by `Execution::with_reference_prefix` (default `"hx:0000000000000000"` for tests);
  - enforce the invisibility rules at member read and write, index read and write, optional access, object literals and `for … in`.
- `crates/hexput-exec/src/wire.rs`:
  - `to_hexput` recognises holders (decision 1), with paths in refusals;
  - `to_wire` emits holders;
  - `measure`, `check_result` and `payload_size` account for holders (decision 7);
  - encode and decode `extra` with `rmp_serde`/`rmpv` (already dependencies of this crate or of `hexput-port`: reuse what exists).
- `crates/hexput-exec/src/lib.rs`: draw the nonce per execution (decision 5), pass it to the `Execution`, and treat a malformed holder in a `Call` reply as `host.function_failed`.
- `crates/hexput-script`: starting-variable holders via `wire::to_hexput`; the result via `to_wire`, with the output-size budget and frame checks unchanged in role.
- Tests:
  - `tests/interpreter.rs`: invisibility, identity, copy semantics, ref stability;
  - `tests/exec.rs` and `tests/connection.rs`: every matrix row over the wire;
  - `tests/exec.rs`: `payload_size` pinned with holders.
- Docs:
  - LANGUAGE-REFERENCE §3: a dated note recording decisions 2, 4, 5, 6 and 8 where §3 is silent;
  - the Spine: the FR-28 amendment gains a dated line with the holder-recognition rules and the ref format;
  - AGENTS.md: the Epic 3 paragraph, test count, next step Story 3.12;
  - `deferred-work.md`: resolve "whether a Value Secret counts as an allocation".

## Tasks & Acceptance

**Execution:**
- [x] `hexput-interpreter`: `Secret`, its carriers, invisibility, ref generation.
- [x] `hexput-exec`: holder decode and encode, size and depth accounting, nonce.
- [x] `hexput-script`: starting variables and result.
- [x] Tests for every matrix row.
- [x] Docs.

**Acceptance Criteria:**
- Given any Script and any holder input, when it runs, then no expression evaluates to anything containing a secret's `ref`, `key` or extra fields, and the Script's results equal those of the same Script over the plain values.
- Given a value with a secret, when it leaves in a `Call` or the result, then its secret is byte-identical to the one it arrived with.
- Given no holders anywhere, when a Script runs, then its wire output is byte-identical to the output before this story.

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | blind + edge-case + verification-gap | A place/collection given a generated `hx:` ref by a `Call` returns as a holder, so a secret-unaware Backend's result changes (AC 3), and gains output size and wire depth | medium | Real: decisions 4 and 6 combined contradict AC 3. Resolved under Erdem's delegation (2026-09-28): generated secrets are never emitted in the result; they still travel in later `Call`s. **patch**; the four `unheld` result adaptations reverted |
| 2 | blind + edge-case | `__secret` through a computed key on `null` errors on read, silently succeeds on write, unlike the literal key | medium | Real (machine.rs ~1631, assign_member). **patch**: every read yields `null`, every write a no-op, on every value, key evaluated first |
| 3 | blind + edge-case | AC 2 "byte-identical secret" not met: `ref`/`key` reordered, further fields re-encoded canonically; the test normalizes before comparing | low | Real; semantic identity is what a Backend needs. **patch** (LANGUAGE-REFERENCE states canonical form; test renamed) |
| 4 | verification-gap | Holder-induced `result_too_deep` never exercised | low | Pre-verified. **patch** (test at and past the limit) |
| 5 | verification-gap | Output-size charge on a top-level held scalar unpinned | low | Pre-verified. **patch** (test) |
| 6 | verification-gap | Memory charge for generated/cleared secrets untested | low | Pre-verified; approximate by design. **defer** |
| 7 | blind + edge-case | `wire::check_result` no longer matches production; tests assert through it | low | Real. **patch**: delegate to `result_to_wire`; tests use the production path |
| 8 | blind | `Authorize` carrying holders untested | low | Real; cheap. **patch** (test) |
| 9 | blind | Nonce doc claims no collisions | low | Real; 64-bit `RandomState`. **patch** (doc) |
| 10 | blind | `return (n);` plain vs `return n;` held undocumented | low | Real. **patch** (doc: parentheses make a computed value) |
| 11 | blind | A reply's secret is dropped when the place already has a ref | low | Real; keep-the-place's-ID is the rule Story 3.13 routes by. **patch** (doc) + **defer** (revisit key loss with Story 3.12/3.13) |
| 12 | blind | Backend refs may collide with `hx:` or be reused across places | medium | Real, matters once Story 3.13 routes modifications by ref. **defer** to 3.13 |
| 13 | blind | Holders amplify `Call` argument size, memory and wire depth (~62 effective nesting) undocumented | low | Real, by design of FR-28. **defer** (document with 3.13's cost notes) |
| 14 | blind | `Secret::new` accepts malformed `extra` and loses it silently; hand-parsed map headers | low | Rejected: unreachable from the wire; a validating constructor adds surface for a test-only path |
| 15 | blind | Spec deviations (`x = f();`, `a[i] = f();` also keep reply secrets; null/`__secret` rules) not logged | low | Real; accepted as the consistent reading of decision 2 — recorded here |
| 16 | blind | Spine `updated:` not bumped | low | Real; direct. **patch** |
| 17 | blind | Status disagreement across files | false | Step 5 syncs the sprint status |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
