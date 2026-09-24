---
title: 'Story 3.4: Deny every path to the host that isn''t a registered function'
type: 'feature'
created: '2026-09-24'
status: 'in-progress'
route: 'oneshot'
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** FR-7 says a Script reaches nothing outside itself but its Session's Registered Functions, and Story 3.4 wants that asserted exhaustively: every name reachable from a Script is either a pure language builtin or a Registered Function of that Session. Today that holds only by construction — the root scope is seeded from starting variables alone and there is no standard library — and nothing names or tests the builtin set, so a future builtin could widen the trust boundary silently.

**Approach:** Make the builtin set one named, documented constant of the interpreter — empty in v2 (LANGUAGE-REFERENCE §11: no standard library at all) — from which the root scope is seeded, and pin it with tests: the constant is empty; the root scope of a fresh execution binds exactly the starting variables; and a sweep of ambient-host names (filesystem, network, process, environment, memory, module and reflection spellings) shows each one is `reference.undeclared_identifier` when read, `capability.unknown_function` when called — through the Daemon, with nothing sent to the Backend — and a syntax error where it would need grammar the language lacks (`import`, `require … from`). Per LANGUAGE-REFERENCE §7/§8 (which wins over the story's wording), only a *call* is a capability denial; a bare read of an undeclared name stays a `reference` error.

</frozen-after-approval>

## Implementation Notes

- As built: `hexput_interpreter::BUILTINS` (empty) and `Execution::root_names()` (via a crate-private `Heap::names` / `Machine::scope_names`). The root scope is *not* seeded from `BUILTINS` — a builtin would need a value, and there is none to give; instead the enumeration test asserts root names = starting variables ∪ hoisted top-level functions ∪ `BUILTINS`, which fails the moment anything else is bound. Named functions need a trailing `;` before a following statement (§2), which the first test draft missed. Tests: `interpreter.rs` (no builtins, root scope enumerated with and without inputs, no module syntax) and `exec.rs` (38 ambient spellings: read → `reference`, call → `capability`, nothing sent; `process.exit(1)` → `reference`). LANGUAGE-REFERENCE §11 and AGENTS.md updated. 530 tests pass.
- Planning-time notes below are superseded where they differ.

- `crates/hexput-interpreter/src/lib.rs` + `machine.rs`: add `pub const BUILTINS: &[&str] = &[];` (doc: the complete set of names a Script can reach that it did not declare and was not given — empty in v2, LANGUAGE-REFERENCE §11; adding one widens FR-7's trust boundary and needs a spec). `Machine::with_variables` seeds the root scope from `BUILTINS` (a no-op today) before the starting variables, so the constant is the single source rather than a comment. Expose a narrow, test-facing way to list the root scope's names of a fresh `Execution` (e.g. `Execution::root_names() -> Vec<String>`, before `run`), documented as for enumeration/audit.
- `crates/hexput-tests/tests/interpreter.rs`: `BUILTINS` is empty; `root_names()` of a fresh execution equals exactly the starting-variable names (with and without variables; top-level `let`/`fn` of the Script are not bound before it runs except hoisted functions — assert against what `with_variables` actually binds before `run`, and document it).
- `crates/hexput-tests/tests/exec.rs` (or connection.rs): a sweep over names such as `fs`, `readFile`, `open`, `require`, `process`, `env`, `exec`, `spawn`, `system`, `socket`, `fetch`, `http`, `net`, `eval`, `globalThis`, `window`, `global`, `Deno`, `std`, `__proto__`, `constructor`, `memory`, `ptr`: read → `reference.undeclared_identifier`; call with a Session registering a different function → `capability.unknown_function`, nothing sent; `import fs;` / `require("fs")` already covered as syntax/capability respectively.
- `LANGUAGE-REFERENCE.md` §8 or §11: one sentence naming `hexput_interpreter::BUILTINS` as the enumerated (empty) builtin set, and that reads of undeclared names stay `reference`, calls `capability` (the AC's "capability-denied" for a mere reference is superseded by §7/§8).
- `AGENTS.md` Project Status: Story 3.4 line.
- Verification: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `python3 scripts/check-crate-graph.py`, `cargo test --workspace --locked`.
