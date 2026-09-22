# Epic 1 Context: Hexput language core

<!-- Compiled from planning artifacts. Edit freely. Regenerate with compile-epic-context if planning docs change. -->

## Goal

Make the Hexput language real and testable on its own, before any daemon, socket, or backend exists: source text goes in, a token stream and AST come out, an interpreter evaluates it, an optional static check inspects it without running it, and a CLI runs the whole thing against a local file. This epic gives the transport, execution, plugin, and tooling epics a working lexer, AST, parser, evaluator, and diagnostic shape to build on instead of inventing their own, and it establishes the crate graph in which every later story lands.

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

- **The language reference is normative.** LANGUAGE-REFERENCE.md defines the language; where a story, a spec, or this file disagrees with it, the reference wins. Its `[DECISION]` markers are approved provenance, not open questions. Every semantic choice below is a summary of it, not a substitute for reading it.
- **The language core performs no I/O and reaches no host.** There is no import, module, filesystem, network, environment, or process facility in the grammar at all — the absence is structural, not a runtime check. A script reaches outside itself only by calling a host-registered name as an ordinary call expression, which at this stage simply raises a capability error.
- **No standard library** in v2 — not even `len()` or `push()`. Everything beyond operators comes from host registrations.
- **Every error is structured.** Lexical, parse, and runtime failures all carry a machine-readable category, a stable code, a human message, and a source span with line and column, accessible as fields rather than only as formatted text, because the CLI, the daemon's error responses, and a future language server must render them identically. Terminal rendering shows the offending line with the span marked and must not truncate location info for multi-line spans (string literals legitimately span lines).
- **Never panic on bad input.** Malformed source, undeclared names, wrong types, and unbounded recursion all produce defined errors; a recursion limit means a script can never overflow the host stack. This is the epic-level expression of the reliability requirement that one script's failure must not take the daemon down.
- **No `unsafe` without review.** Memory safety is treated as a security property in the lexer, parser, and interpreter; any `unsafe` block requires a reviewed safety justification, enforced mechanically in CI.
- **The static check is optional and pure.** It runs in one of three modes — off (default), warn, error — is configured by the caller, and never executes the script. Its findings use the same category/code/message/span shape as runtime errors. Warning-severity findings (e.g. unused local) can never reject a script.

## Technical Decisions

- **Crate landing is fixed:** AST types in `hexput-ast` (data only), tokenizer in `hexput-lexer`, parser in `hexput-parser` (lexer + ast), evaluator in `hexput-interpreter` (ast only, no host reach), the shared diagnostic shape in `hexput-shared::diagnostics`, the check pass in `hexput-check`, and CLI logic in `hexput-cli-core` behind the one binary crate. A crate boundary is a compiler-enforced dependency edge: `hexput-check` must not depend on the interpreter, the RPC crate, or the enforcement crate, so it structurally cannot execute or reach the host. Every crate is a lib; only the binary crate produces binaries.
- **The check pass entry point is a single pure function:** parsed AST + callable-name set + policy in, findings out. It holds no state between calls. An empty callable-name set means unknown-call findings are simply not raised, rather than every host call being flagged.
- **Value model:** six dynamic types — null, bool, number (one IEEE-754 double, no integer type), string, array, object (string keys, insertion-ordered). Functions are first-class values but are deliberately not storable in Global Variables.
- **Truthiness is Python-flavoured, not JavaScript's:** falsy is exactly null, false, 0, "", empty array, empty object. `&&`/`||` return an operand, not a bool, and short-circuit.
- **Conversions that a person would predict are automatic; ones they would have to look up are errors.** `+` concatenates if either side is a string, otherwise converts both to number. Other arithmetic converts to number and raises a type error on a non-numeric string or any collection. Stringifying a collection is a type error. Equality is narrow: cross-type comparisons are false (`0 == false` is false), number/string compares by parsed number, collections compare by identity. Division by zero and any non-finite result raise rather than producing NaN or infinity.
- **Absent data is null; a wrong script is an error.** Reading a missing object key or an out-of-range index yields null; writing a missing key creates it, and an array write is only valid in range or exactly at the length (append). Undeclared identifiers and property access on null without `?.` are reference errors. `?.` short-circuits the rest of the chain and suppresses only null — never a type error; there is no optional call form.
- **Scoping is lexical and block-level.** `let`, named functions, and parameters share one block namespace; redeclaration in the same block and duplicate parameters are compile-time errors, inner blocks may shadow. Assignment to an undeclared name is a runtime error — no implicit globals. Closures capture by reference. `break`/`continue` are innermost-loop only and a function body starts a fresh loop context. Top-level `return` is the script result, and the result must be an acyclic tree of values.
- **No error handling in the language:** no try/catch/throw. Any error terminates the execution and is reported upward. Also deliberately absent: modules, classes, `this`, string interpolation, regex, bitwise operators, ternary, switch, in-script async.
- **Tests live in the dedicated test crate**, one file per crate under test, consuming crates as dev-dependencies so the graph rules over production dependencies stay intact.

## Cross-Story Dependencies

- Stories run in order: 1.1 creates every crate skeleton; 1.2 feeds 1.3–1.5; the parser stories feed 1.6–1.7; 1.8's diagnostic shape is used retroactively by everything from 1.2 onward, so design the error type early rather than bolting it on; 1.9 depends on 1.7 and 1.8; 1.10 depends on the AST from 1.3–1.5 and on 1.8's finding shape, and its CLI command extends 1.9's.
- Downstream: the socket-execution and cached-execution epics call this parser and interpreter through the shared executor; the plugin epic extends this grammar with the plugin block and annotations; the tooling epic's grammar and language server describe this same language and reuse these spans and categories. Anything those epics need must be exposed as a public API of the language crates, not kept private.
- The interpreter must not grow a host-call path of its own — host reach is granted later, from outside, through the single executor entry point.
