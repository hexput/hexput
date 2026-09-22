# Epic 1 Context: Hexput language core

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Deliver the Hexput language itself — lexer, AST, parser, tree-walking interpreter, a precise diagnostics shape, a static check pass, and a local CLI that evaluates or checks a file — so a script author can write source and see it evaluated correctly with no daemon, socket, or backend involved. This makes the language independently testable and fuzzable ahead of any I/O, and gives the later epics (cached execution, plugins, tree-sitter grammar, language server) a parser, AST, and error shape to build on instead of inventing their own.

## Stories

- Story 1.1: Project scaffold and pinned toolchain
- Story 1.2: Tokenize Hexput source
- Story 1.3: Parse expressions, declarations, and member access
- Story 1.4: Parse conditionals and loops
- Story 1.5: Parse functions, callbacks, objects, and arrays
- Story 1.6: Evaluate expressions and variable scope
- Story 1.7: Execute control flow, functions, and callbacks
- Story 1.8: Report errors with precise source locations
- Story 1.9: Evaluate a script from the command line
- Story 1.10: Check a script without running it

## Requirements & Constraints

- **The language reference is normative.** `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` defines Hexput; where a story and it disagree, it wins. Its `[DECISION]` markers are approved provenance, not open questions. Do not invent semantics not written there, and do not "improve" a documented rule.
- **Semantics the epic must honour exactly** (all from the reference): one IEEE-754 number type; six value types; empty collections are falsy; `&&`/`||` return an operand, not a `bool`; equality is narrow — every cross-type comparison other than number/string is `false`; `+` concatenates when either side is a string, otherwise adds; conversions a person could not predict (`"abc" * 2`, stringifying a collection) are `type` errors, never `NaN` or a placeholder; division by zero and any non-finite result raise; `?.` suppresses only `null`, short-circuits the rest of the chain, and has no call form; absent object keys and out-of-range reads yield `null`, while array writes outside `0..=len` are `reference` errors; property access on `null` and undeclared identifiers are `reference` errors; block-level lexical scoping with per-iteration loop scopes; closures capture by reference; named functions hoist within their block; recursion is bounded by a documented call-depth limit and never a host stack overflow; a Script result may not be, or contain, a function or a cycle; there is no `try`/`catch` and no standard library whatsoever.
- **Errors are a first-class product of this epic.** Every lexical, parse, and runtime failure carries a machine-readable category, a stable code, a human message, and a source span with line and column — accessible as structured fields, not only formatted text, because the daemon response, the CLI, and the future language server all render the same shape. Terminal rendering must show the offending line with the span marked, and multi-line spans (string literals legally span lines) must keep full location information.
- **The static check pass (FR-26)** takes a parsed AST, a caller-supplied callable-name set, and the active policy, and returns findings. It reports undeclared identifiers, duplicate `let`, `break`/`continue` outside a loop, arity mismatches against locally declared functions, literal-operand type errors, unreachable code, disabled-construct usage, and calls to unknown names — the last only when a callable-name list is supplied at all. **A *missing* list suppresses the unknown-call finding entirely; an *empty* list does not** — an empty list means "nothing is callable but local functions", so every host call is flagged. How the CLI represents "no list" versus "empty list" is an open contract to settle before Story 1.10. Unused locals are warnings and can never reject a script; the result distinguishes clean from warnings-only.
- **Within the language reference, specific rules beat summary tables.** Where a summary or operator table appears to contradict the prose rules on truthiness, conversions, or absent data, the specific prose rule is normative.
- **Robustness:** no input — malformed, deeply nested, or adversarial — may panic the language crates or overflow the host stack. Failures are always typed errors.
- **Out of scope here:** the check's Config mode and per-execution override, capability enforcement, and resource budgets all belong to later epics. This epic produces findings and errors, never authorization decisions.

## Technical Decisions

- **Crate landing is fixed:** AST types in `hexput-ast`; tokenizer in `hexput-lexer`; parser in `hexput-parser`; evaluator in `hexput-interpreter`; the single error/finding shape in `hexput-shared::diagnostics`; check pass in `hexput-check`; CLI logic in `hexput-cli-core`, with only a thin `main()` in `hexput-bin`.
- **Crate boundaries are compiler-enforced, not conventions.** `hexput-check` depends on `hexput-ast` only — never the interpreter, RPC, or enforcement crates — so it structurally cannot execute a script or reach the host. The check's entry point is a single pure function holding no state between calls.
- **The language crates have no host reach at all.** There is no import, module, filesystem, network, or process facility anywhere in the grammar; the absence is structural rather than a runtime guard. The interpreter depends on the AST only.
- **The toolchain is pinned:** Rust 1.98.1, edition 2024, workspace-wide.
- **Every crate is a `lib`; exactly one crate produces binaries.** Shared dependency versions are pinned once at the workspace root and inherited, never re-pinned per crate.
- **Memory safety is mechanically enforced** in the lexer, parser, interpreter, and executor: any `unsafe` without a reviewed `SAFETY:` justification fails CI, alongside clean `rustfmt` and warning-denied `clippy` across the workspace.
- **Tests are integration tests** living in the dedicated test crate, one file per crate under test, exercising only public APIs; anything needing a private item stays in-crate with a comment saying why.
- **Naming follows the glossary verbatim** (`Session`, `Plugin`, `Capability`, `Registered Function`, `Resource Budget`, `Client ID`) — no synonyms, even in this epic's internal types.

- **The CLI binds starting variables.** Story 1.9's `eval` accepts input variables on the command line and binds them as the Script's starting variables before evaluation.

## Cross-Story Dependencies

- Strictly sequential in the obvious way: the lexer feeds the parser (1.3–1.5 build on 1.2), the interpreter consumes what those produce (1.6–1.7), and the CLI (1.9–1.10) drives all of them.
- Story 1.8 defines the diagnostic shape that 1.2–1.7 must already produce and that 1.9, 1.10, and later epics consume — if it lands after them, expect to retrofit the earlier errors onto it rather than keeping a second shape.
- Story 1.10's rendering, categories, and spans must be identical to 1.8's; the CLI check command reuses 1.9's surface.
- Downstream: cached execution and plugin registration both reuse this parser and invoke this check pass; the tree-sitter grammar and language server describe and reuse this language and its diagnostics rather than reimplementing them. Plugin-specific grammar (`plugin { }`, `@Global`, `@Event`) is a later epic's extension of this grammar — do not build it here, but do not design the parser in a way that forecloses it.
