---
title: 'Story 3.9: Switch off language constructs by policy'
type: 'feature'
created: '2026-09-27'
status: 'done'
baseline_commit: 'a1651f6daefc76f47f422c35a676fa83bb36b8fb'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** A Backend cannot narrow what its scripts can express. The six feature toggles that FR-3 and OQ-3 define exist only as `hexput-check`'s static `Policy`, and no execution enforces them.

**Approach:** Add a `features` map to the Config shape. It is decoded by the same decoder as the Story 3.7 settings, so `Init.config`, `ExecutionStart.overrides` and `ConfigUpdate` all carry it and overlay it the same way. The Executor hands the effective toggles to the interpreter. The interpreter refuses a disabled construct **when it evaluates it**, with a `policy.construct_disabled` error naming the toggle and spanned on the construct. That error is distinct from every `budget` and `capability` error.

## Boundaries & Constraints

**Always:**
- The toggle set is closed: `loops`, `conditionals`, `callbacks`, `object_literals`, `array_literals`, `rpc_calls`.
- Every toggle defaults to enabled.
- Any other name is refused as an unknown toggle; the always-on constructs are never togglable.
- A policy error ends the Script like every error (§7): uncatchable, nothing sent after it.
- `hexput eval` runs with everything enabled.
- The Config remains a single live copy (AD-5); an override changes nothing stored.

**Decisions (agent, 2026-09-27 — Erdem delegated: "kendin tahmin et", no approvals):**
1. **Wire shape.** `features: { loops, conditionals, callbacks, object_literals, array_literals, rpc_calls }`.
   - Every key is optional and every value is a MessagePack boolean.
   - A non-boolean is refused as `` `config.features.loops` must be a boolean; found … ``.
   - An unknown key is refused as `` `config.features.return` is not a known feature toggle (the toggles are loops, conditionals, callbacks, object_literals, array_literals, rpc_calls) ``.
   - Overlay works per toggle, like a limit. `ConfigUpdate`'s replace semantics apply: a toggle left out of an update is back to enabled.
2. **Runtime, not a pre-scan.** A construct is refused when evaluation reaches it; one on a path never taken does not fail the Script. This matches LANGUAGE-REFERENCE §7 ("parse or runtime"). It is also the only uniform choice, because telling a host call from a local call needs scope resolution. The static check (Story 3.10) is what reports a disabled construct anywhere in the source before anything runs.
3. **What each toggle refuses, and where it is spanned:**
   - `loops`: entering a `while` or `for … in` statement.
   - `conditionals`: an `if` statement (every `else if`/`else` is part of it).
   - `callbacks`: defining any function, named or anonymous. The Script's top-level named functions are hoisted before its first statement runs, so a Script declaring one fails before anything runs. With no function definable, nothing local can be invoked.
   - `object_literals` / `array_literals`: evaluating the literal.
   - `rpc_calls`: a host call, refused **before** its arguments are checked, before it is counted as an RPC call or side effect, and before any capability decision. A blanket-granted function is therefore refused too, and nothing is sent.
4. **Error.**
   - Code `policy.construct_disabled` (already in `Code::ALL`), category `policy`.
   - Message: `` `<construct>` is disabled by policy (`features.<toggle>`) ``, where the construct is `while`, `for … in`, `if`, `fn`, `{ … }`, `[ … ]`, or `a host call to \`name\``.

**Never:**
- no static pre-scan in the Executor;
- no change to `hexput-check`'s `Policy` (Story 3.10 maps it);
- no new toggle;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Each toggle | `features.<t> = false`; a Script using that construct | `policy.construct_disabled` naming `features.<t>`, spanned on the construct |
| Defaults | `config: {}` | every construct runs |
| Unknown toggle | `features.variables` / `return` / `operators` / `global_variables` / `property_access` = false | `protocol.invalid_payload` naming the path; no Session (Init) or nothing run (override) |
| Non-boolean | `features.loops = 0` / `"no"` / nil | `protocol.invalid_payload` naming the path |
| `rpc_calls` beats a grant | `rpc_calls = false`; blanket-granted `f`; `f(1)` | policy error; no `Call` sent; RPC-call and side-effect counts unchanged |
| `rpc_calls` beats argument checks | `rpc_calls = false`; `f(fn() {})` | policy error, not `type.function_argument` |
| Distinct | the same Script tripping a budget or a capability refusal with toggles on | a `budget`/`capability` code, never `policy` |
| Hoisted function | `callbacks = false`; `f(1); fn g() {};` with `f` blanket-granted | policy error spanned on `g`; nothing sent |
| Not reached | `loops = false`; `if (false) { while (true) {} } return 1;` | returns `1` |
| Override / update | an override disabling `loops` for one execution; a `ConfigUpdate` disabling it for later ones | only those executions refuse; the stored Config is unchanged by the override |

</frozen-after-approval>

## Code Map

- `crates/hexput-shared/src/policy.rs` (new; add it to `lib.rs` and the Spine's `hexput-shared` module list with a dated amendment):
  - `Feature` enum with the six toggles, `ALL`, `as_str()` (the wire and toggle name) and `Display`;
  - `Features`, a `Copy` set of enabled toggles: `Features::ALL_ENABLED`, `is_enabled(Feature)`, `with(Feature, bool)`.
- `crates/hexput-shared/src/budget.rs`:
  - `Settings` also holds `[Option<bool>; 6]` feature toggles: `set_feature`, `feature`, and `features() -> Features` (unset means enabled);
  - `overlay` merges toggles per toggle.
  - Keep the `u64` `Setting` table untouched.
- `crates/hexput-port/src/settings.rs`: decode `features` as a map of booleans under the same prefix, with the messages in decision 1. Keep the existing refusals for other keys.
- `crates/hexput-ast/src/lib.rs`: re-export `Feature` and `Features`, as it re-exports diagnostics, so the interpreter reaches them without a `hexput-shared` edge.
- `crates/hexput-interpreter`:
  - `Execution` gains a builder, `with_features(Features)`, with the default all enabled. `evaluate*` stay all enabled.
  - The machine refuses at the sites in decision 3. A named function is checked where top-level functions hoist and where a nested `fn` statement is evaluated; an anonymous function where its expression is evaluated; `rpc_calls` where a host call is recognised, before arguments are detached. Each refusal is the §7 error, built like the interpreter's other runtime diagnostics.
  - Reuse the existing span of each node, the keyword or literal.
- `crates/hexput-enforce/src/lib.rs`:
  - `Limits` gains `features: Features` (default all enabled), the builder `with_features`, and `features()`;
  - `from_settings` copies `settings.features()`.
- `crates/hexput-exec/src/lib.rs`: pass `limits.features()` to the `Execution`. No other change: the policy error arrives as an ordinary finished-with-error Diagnostic.
- Tests:
  - `tests/shared.rs`: `Feature` spellings;
  - `tests/port.rs`: decoding, overlay, and the refusals;
  - `tests/interpreter.rs`: each toggle, span and message, hoisting, not-reached, and `rpc_calls` before argument checks;
  - `tests/exec.rs`: `rpc_calls` with a blanket grant, nothing sent, not counted (cross the RPC limit afterwards to prove the count);
  - `tests/connection.rs`: Config, override and `ConfigUpdate` end to end; unknown toggle refused at `Init`.
- Docs:
  - LANGUAGE-REFERENCE §7 or §8: a dated decision with the toggle semantics and the runtime rule;
  - the Spine: a dated amendment for `policy.rs` and the `features` key;
  - AGENTS.md: the Epic 3 paragraph, test count, and "next step Story 3.10".

## Tasks & Acceptance

**Execution:**
- [x] `hexput-shared`, `hexput-port`: `Feature`/`Features`, the `Settings` toggles, and the decoder.
- [x] `hexput-ast`, `hexput-interpreter`: re-export; runtime refusals.
- [x] `hexput-enforce`, `hexput-exec`: carry toggles through `Limits`.
- [x] Tests for every matrix row.
- [x] Docs.

**Acceptance Criteria:**
- Given any single disabled toggle, when a Script evaluates that construct, then it fails with `policy.construct_disabled` naming `features.<toggle>`, and with no toggle set every construct runs.
- Given a name outside the closed set, when it appears under `features`, then the payload is refused and nothing runs or is created.

## Spec Change Log

- 2026-09-27 (implementation): a named function is refused where **every** block hoists — the top level's on the first run, a nested block's when it is entered — rather than where a nested `fn` statement is evaluated. Nested named functions are hoisted when their block opens (§6), so a check at the statement would let the function be called earlier in the block; checking at the hoist keeps "with no function definable, nothing local can be invoked" true. The error is spanned on `fn name`.

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | verification-gap | Callbacks refusal at a `for … in` body's scope opening (the one block reached through `open` directly) is untested | low | Pre-verified. **patch** (test) |
| 2 | edge-case + blind | `Execution::with_features` works after `HostCall::resume`/`Paused::resume`, loosening toggles mid-run and skipping the top-level hoist check | low | Real: `resume` returns an `Execution`; only `hexput-exec` calls it, before the first run. **patch**: ignored once started, `debug_assert!` |
| 3 | blind | A `ConfigUpdate` omitting `features` silently re-enables every toggle (Story 3.8 replace semantics) | medium | Real consequence of Story 3.8 decision 1, already deferred there. **patch** (documented in LANGUAGE-REFERENCE) + existing deferred entry stands |
| 4 | blind | A policy stop is never logged, unlike budget stops and capability refusals | low | Real; direct. **patch** (`debug` log) |
| 5 | blind | Runtime and `hexput-check` word and span `policy.construct_disabled` differently; the Spine says the types are shared by `hexput-check` | low | Real; the check keeps its own `Policy` by this spec. **patch** (Spine wording) + **defer** (align in Story 3.10) |
| 6 | blind | `rpc_calls` covers only bare-name host calls; §8 Registered Methods (Story 3.12) must check it too | low | Real future gap. **defer** to Story 3.12 |
| 7 | blind | Docs don't say toggles are syntactic (recursion still repeats, `&&`/`||`/`?.` still branch) | low | Real; direct. **patch** |
| 8 | blind | No test of an invalid `features` map in a `ConfigUpdate` | low | Real; cheap. **patch** (test) |
| 9 | blind | Thin coverage: `else if` chain, toggles after a resume | low | Real; cheap. **patch** (tests) |
| 10 | blind | `Feature::index()` duplicates `ALL`'s order with no assertion | low | Real; direct. **patch** (test) |
| 11 | blind | Spine `updated:` not bumped | low | Real; direct. **patch** |
| 12 | blind | Code Map still says nested `fn` is checked at statement time | — | Rejected: fix edits this build's spec; the Spec Change Log records the as-built rule |
| 13 | blind | Sprint status `in-progress` vs spec `in-review` | false | Step 5 syncs the sprint status |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
