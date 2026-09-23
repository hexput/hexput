# Epic 1 Context: Hexput language core

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Deliver the Hexput language itself — a workspace scaffold, lexer, AST, parser, tree-walking interpreter, one precise diagnostic shape, a static check pass, and a local CLI that evaluates or checks a file — so a script author can write source and see it evaluated correctly with no daemon, socket, or backend involved. This makes the language independently testable and fuzzable ahead of any I/O, and hands the later epics (cached execution, plugins, tree-sitter grammar, language server) a parser, AST, and error shape to build on instead of inventing their own.

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

- **The language reference is normative.** `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` defines Hexput; where a story and it disagree, it wins. Its `[DECISION]` markers are approved provenance, not open questions. Do not invent semantics it does not state, and do not "improve" a documented rule. Where a summary or operator table appears to contradict the prose rules on truthiness, conversion, or absent data, the specific prose rule is normative.
- **Semantics the epic must honour exactly.** One IEEE-754 number type and six value types; ASCII-only identifiers with full Unicode inside strings; multi-line string literals (unterminated only at end of input); empty collections are falsy; `&&`/`||` return an operand rather than a `bool`; equality is narrow — every cross-type comparison other than number/string is `false`; `+` concatenates when either side is a string and otherwise adds; conversions a person could not predict (arithmetic on a non-numeric string, stringifying a collection) are `type` errors, never `NaN` or a placeholder; division by zero and any non-finite result raise. `?.` suppresses only `null`, short-circuits the remainder of the chain, and has no call form. Absent object keys and out-of-range reads yield `null`, while array writes outside `0..=len` are `reference` errors; property access on `null` and undeclared identifiers are `reference` errors. Scoping is lexical and block-level with a fresh scope per loop iteration; closures capture by reference; named functions hoist within their block; `let`, named functions, and parameters share one block namespace. Recursion is bounded by a documented interpreter constant (1024) and must never overflow the host stack. Iterating a non-collection is a `type` error, and storing into the collection being iterated is a `reference` error observed when the loop next advances. A Script result may not be, or contain, a function or a cycle. There is no `try`/`catch` and no standard library whatsoever.
- **Errors are a first-class product of this epic.** Every lexical, parse, and runtime failure carries a severity (`error`/`warning`), a machine-readable category, a stable code, a human message, and a source span with line and column — reachable as structured fields, not only formatted text, because the daemon response, the CLI, and the future language server all render the same shape. Terminal rendering is a compact three-line form (location line, the offending source line in full, a caret marker), never truncated or windowed; multi-line spans report both ends and lose no location. Rendering is plain text with no colour and no I/O — colour is the CLI's layer on top.
- **The static check pass** takes a parsed AST, a caller-supplied callable-name set, and the active policy, and returns findings: undeclared identifier reads and assignments, duplicate `let`, `break`/`continue` outside a loop, arity mismatches against locally declared functions, literal-operand type errors, unreachable code, disabled-construct usage, and calls to unknown names. That last finding is raised only when a callable-name list is supplied at all — **a *missing* list suppresses it entirely; an *empty* list does not**, since an empty list means "nothing but local functions is callable". How the CLI expresses "no list" versus "empty list" is a contract to settle before Story 1.10. Unused locals are warnings and can never reject a script; the result distinguishes clean from warnings-only. The check is never a type system: no inference across bindings, no judgement that depends on runtime values.
- **Robustness:** no input — malformed, deeply nested, or adversarial — may panic the language crates or overflow the host stack. Failures are always typed errors.
- **Out of scope here:** the check's `off`/`warn`/`error` mode selection and per-execution override, capability enforcement, and resource budgets all belong to later epics. This epic produces findings and errors, never authorization decisions. Plugin grammar (`plugin { }`, `@Global`, `@Event`) is a later epic's extension.

## Technical Decisions

- **Crate landing is fixed:** AST types in `hexput-ast`; tokenizer in `hexput-lexer`; parser in `hexput-parser`; evaluator in `hexput-interpreter`; the single diagnostic/finding shape and its rendering in `hexput-shared::diagnostics`; check pass in `hexput-check`; CLI logic in `hexput-cli-core`, with only a thin `main()` in `hexput-bin`.
- **Crate boundaries are compiler-enforced, not conventions.** `hexput-check` depends on `hexput-ast` only — never the interpreter, RPC, or enforcement crates — so it structurally cannot execute a script or reach the host, and its entry point is a single pure function holding no state between calls. Adding a forbidden edge must fail the build, not merely a review.
- **The language crates have no host reach at all.** There is no import, module, filesystem, network, or process facility anywhere in the grammar; the absence is structural, not a runtime guard. The interpreter depends on the AST only.
- **The toolchain is pinned:** Rust 1.98.1, edition 2024, workspace-wide; shared dependency versions pinned once at the workspace root and inherited, never re-pinned per crate. Every crate is a `lib`; exactly one crate produces binaries.
- **Memory safety is mechanically enforced** in the lexer, parser, interpreter, and executor: any `unsafe` without a reviewed `SAFETY:` justification fails CI, alongside clean `rustfmt` and warning-denied `clippy` across the workspace.
- **Tests are integration tests** living in the dedicated test crate, one file per crate under test, exercising only public APIs; anything needing a private item stays in-crate with a comment saying why.
- **Naming follows the glossary verbatim** (`Session`, `Plugin`, `Capability`, `Registered Function`, `Resource Budget`, `Client ID`) — no synonyms, even in this epic's internal types.
- **The CLI binds starting variables.** The eval command accepts input variables on the command line and binds them as the Script's starting variables before evaluation; it prints the result and exits zero, or prints the standard diagnostic rendering to stderr and exits non-zero. The check command shares that surface, executes nothing, and exits non-zero only for an error-severity finding.
- **The local-run contract belongs to the language, not to one command.** A starting variable's value is ordinary Hexput source evaluated as a single expression, seeing no other starting variables. A result prints in Hexput literal form — never truncated, no depth limit — so the printed result is valid source and exactly the grammar an input value accepts: one surface for reading a result and writing an input. Exit codes are `0` success, `2` usage (bad flag, missing operand, malformed or repeated input variable), `1` everything else (any rendered diagnostic, an unreadable file, non-UTF-8 bytes). Every failure goes to stderr with the script path as the origin label, leaving stdout for the result and for explicitly requested help or version text. Every command this epic ships obeys this contract, not just the first one to need it.

## Cross-Story Dependencies

- Strictly sequential in the obvious way: the scaffold precedes everything, the lexer feeds the parser (1.3–1.5 build on 1.2), the interpreter consumes what those produce (1.6–1.7), and the CLI (1.9–1.10) drives all of them.
- Story 1.8 defines the diagnostic shape that 1.2–1.7 must already produce and that 1.9, 1.10, and later epics consume — landing it after them means retrofitting the earlier errors onto it rather than keeping a second shape.
- Story 1.10's rendering, categories, severities, and spans must be identical to 1.8's, and its CLI surface reuses 1.9's.
- Downstream: cached execution and plugin registration both reuse this parser and invoke this check pass on each submission path; the tree-sitter grammar and language server describe and reuse this language and its diagnostics rather than reimplementing them. Do not build the plugin grammar here, but do not design the parser in a way that forecloses it.
