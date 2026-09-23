---
title: Hexput Language Reference (v2)
status: final
created: 2026-09-18
author: drafted by PM role during the sprint-planning readiness gate; approved by Erdem 2026-09-18 after four rounds of revision (@Global syntax, truthiness + implicit conversion, absent-key null, optional chaining + static check)
note: '[DECISION] markers are retained as provenance — they mark choices made here rather than inherited from the PRD, brief, or spine. They are approved, not open.'
purpose: Close readiness-gate finding A — Epic 1 (lexer/parser/interpreter), Epic 6 (plugin syntax) and Epic 9 (tree-sitter grammar + LSP) all depend on a language definition that no planning artifact recorded.
---

# Hexput Language Reference — v2

This is the normative definition of the Hexput language. Epic 1 implements it, Epic 9's grammar and language server describe it, and Epic 6's plugin parsing extends it. Where this document and a story disagree, this document wins and the story is corrected.

Every item marked **[DECISION]** is a choice made here for the first time, not inherited from the PRD, brief, or architecture spine. Each is defensible but reversible — they are exactly what the readiness gate flagged as "decisions nothing records".

## 1. Design stance

Hexput serves two audiences at once and the language leans toward the second: the backend engineer running the daemon, and the **non-engineer author writing the actual rule** (PRD §2, UJ-2's academic-affairs staffer). The language is therefore forgiving where forgiveness is guessable — conditions accept any value, `+` builds strings out of whatever you give it — and strict only where guessing would produce a confidently wrong answer.

The line between the two: **conversions that a person would predict are automatic; conversions that a person would have to look up are errors.** `"Total: " + 5` is obvious, so it works. `"abc" * 2` is not, so it fails. `0.1 + 0.2` is honest arithmetic. Dividing by zero is not a number, so it raises rather than yielding infinity.

The language is small on purpose — it targets short, frequently-invoked rules, not general programming.

## 2. Lexical structure

- **Encoding:** UTF-8 source. Identifiers are ASCII letters, digits, and `_`, not starting with a digit. **[DECISION]** Non-ASCII identifiers are rejected; non-ASCII content in string literals is fully supported.
- **Comments:** `//` to end of line, and `/* ... */` which does not nest. **[DECISION]**
- **Whitespace** is insignificant except as a token separator.
- **Statement termination:** statements are terminated by `;`. **[DECISION]** The terminator may be omitted for the last statement in a block or file — this accommodates the PRD §4.9 example, which omits it after the `let b = {...}` declaration.
- **Reserved words:** `let`, `fn`, `if`, `else`, `while`, `for`, `in`, `return`, `break`, `continue`, `true`, `false`, `null`, `plugin`. **[DECISION]** Reserved words may not be used as identifiers, object keys written bare, or parameter names.

## 3. Values and types

Hexput is dynamically typed with six value types:

| Type | Literal form | Notes |
| --- | --- | --- |
| `null` | `null` | The only value of its type; not a default for anything |
| `bool` | `true`, `false` | What `!`, comparisons, and equality produce. Conditions accept any value (§4.1) |
| `number` | `1`, `-3`, `2.5`, `1e3` | **[DECISION]** One numeric type, IEEE-754 double. No separate integer type |
| `string` | `"text"`, `'text'` | **[DECISION]** Escapes `\n \t \r \\ \" \' \u{...}`. No interpolation in v2. **[DECISION]** String literals may span lines: a raw newline inside one is ordinary content, not an error |
| `array` | `[1, 2, 3]` | Ordered, heterogeneous, zero-indexed; trailing comma allowed |
| `object` | `{ key: "value" }` | String keys, insertion-ordered; bare or quoted keys; trailing comma allowed. Reading an absent key yields `null` (§7) |

**[DECISION] String literals are multi-line.** A raw newline between the quotes is kept verbatim in the value, so

```
let a = "merhaba
sosis
ben";
```

is one string containing two newlines. Consequently a string is unterminated only at end of input, never at end of line, and a string's source span may cover several lines (Story 1.8 renders multi-line spans without truncating the location). There is no line-continuation escape: a `\` immediately before a newline is an invalid escape, not a join.

**[DECISION, 2026-09-19]** Object literals reject repeated decoded keys, including collisions between bare, quoted, and escaped spellings (for example, `a`, `"a"`, and `"\u{61}"`).

Functions are values (§6) but are not storable in a Global Variable. **[DECISION]** — a function closes over an environment, and persisting one across Event invocations would make Global Variable lifetime semantics (FR-25) undefinable.

**[DECISION] Number edge cases:** division by zero, and any operation producing `NaN` or infinity, raise a runtime error rather than yielding a non-finite value. A rules engine that returns `NaN` has failed, not computed.

## 4. Operators

Precedence, tightest first. All binary operators are left-associative; unary operators bind tighter than any binary operator.

| Level | Operators | Meaning |
| --- | --- | --- |
| 1 | `a.b` `a[b]` `a?.b` `a?.[b]` `f(...)` | Member access, index, optional access, call |
| 2 | `-a` `!a` | Numeric negation, boolean negation |
| 3 | `*` `/` `%` | Multiplication, division, remainder |
| 4 | `+` `-` | Addition/concatenation, subtraction |
| 5 | `<` `<=` `>` `>=` | Ordering comparison |
| 6 | `==` `!=` | Equality |
| 7 | `&&` | Logical and, short-circuiting |
| 8 | `\|\|` | Logical or, short-circuiting |

### 4.1 Truthiness

**[DECISION]** `if`, `while`, `&&`, `||`, and `!` accept **any** value. A value is **falsy** when it is one of exactly these five, and truthy otherwise:

`null` · `false` · `0` · `""` (empty string) · an empty array or empty object

**[DECISION]** Empty collections are falsy, following Python rather than JavaScript — `if (items)` reading as "if there are any items" is what a rule author expects, and JavaScript's truthy `[]` is a common source of silently wrong conditions.

`&&` and `||` **return one of their operands**, not a `bool`, so the useful idioms work: `let name = input.name || "unknown";` yields `"unknown"` only when `input.name` is falsy. `||` returns the left operand when it is truthy, otherwise the right; `&&` returns the left when it is falsy, otherwise the right. Short-circuiting applies — the right operand is not evaluated when the left decides the result. `!` accepts any value and always returns `bool`.

### 4.2 Implicit conversion

Conversions happen automatically in the cases below. **[DECISION]** Every other type mismatch is a `type` error naming both operand types.

**`+` — addition or concatenation.** If **either** operand is a `string`, the result is string concatenation and the other operand is converted with the to-string rules (§4.3). Otherwise both operands are converted with the to-number rules and added.

```
"Total: " + 5      // "Total: 5"
"Order " + null    // "Order null"
"10" + 5           // "105"      — string wins
true + 1           // 2          — no string involved
```

**`-` `*` `/` `%` — arithmetic.** Both operands are converted with the to-number rules. A string that does not look like a number is a `type` error, not a silent `NaN`.

```
"10" - 1     // 9
true * 3     // 3
"abc" * 2    // type error
```

**`<` `<=` `>` `>=` — ordering.** If **both** operands are `string`, they compare lexicographically by Unicode code point. Otherwise both are converted to number.

**`==` and `!=` — equality.** **[DECISION]** Deliberately narrower than JavaScript's, because JavaScript's is the part everyone gets wrong:

- Same type → compared directly. Arrays and objects compare **by identity**, never structurally.
- `number` vs `string` → the string is converted to number; if it does not look like a number, the result is `false` rather than an error.
- Every other cross-type comparison → `false`. So `0 == false` is `false`, `"" == false` is `false`, and `[] == false` is `false` — none of JavaScript's famous surprises apply.
- `null == null` is `true`; `null` equals nothing else.

Truthiness (§4.1) and equality are deliberately different questions: `0` is falsy but `0 == false` is `false`.

### 4.3 Conversion rules

| To string | Result |
| --- | --- |
| `null` | `"null"` |
| `bool` | `"true"` / `"false"` |
| `number` | Shortest representation that round-trips; whole values print without a decimal point (`5`, not `5.0`) |
| `array` / `object` | **[DECISION]** `type` error — no `"[object Object]"`. Formatting a collection is the host's job via a Registered Function |

| To number | Result |
| --- | --- |
| `null` | `0` |
| `bool` | `1` / `0` |
| `string` | Parsed if it is a valid number literal with optional surrounding whitespace; otherwise a `type` error |
| `array` / `object` | `type` error |

**[DECISION, 2026-09-19] Signed numeric strings.** To-number accepts one optional leading `+` or `-` directly before the number literal, inside the optional surrounding whitespace: `"-3" - 1` is `-4`. Anything else that is not a §3 number literal — including `""`, `"NaN"`, `"Infinity"`, `"0x10"`, `".5"`, and `"- 3"` — is a `type` error.

**[DECISION, 2026-09-19] Number to string is JavaScript-style.** Shortest round-tripping digits; plain notation for magnitudes in `[1e-6, 1e21)`, exponent notation outside it (`1e21` → `"1e+21"`, `1e-7` → `"1e-7"`); whole values without a decimal point; `-0` → `"0"`.

### 4.4 Optional access

**[DECISION]** `?.` reads a property or index without raising when the left side is `null`:

```
order.customer?.name        // null when customer is null, instead of a reference error
order?.items?.[0]           // null when order or items is null
```

- `a?.b` and `a?.[b]` evaluate to `null` when `a` is `null`, and otherwise behave exactly like `a.b` and `a[b]`.
- **[DECISION] Short-circuiting covers the rest of the chain**, as in JavaScript: in `a?.b.c.d`, if `a` is `null` the whole expression is `null` and `b.c.d` is never evaluated — so one `?.` at the uncertain link is enough, rather than one at every link.
- `?.` is only about `null`. It does not suppress `type` errors, and it is not a general error-swallowing operator.
- **[DECISION]** There is no optional call form (`a?.()`); a Registered Function either exists or the call is a `capability` error, which `?.` must not hide.

## 5. Statements

```
let x = <expr>;              // declaration; initializer required [DECISION]
x = <expr>;                  // assignment to a declared binding
obj.key = <expr>;            // member assignment
arr[0] = <expr>;             // index assignment

if (<expr>) { ... } else if (<expr>) { ... } else { ... }   // any value; §4.1 truthiness

while (<expr>) { ... }                                       // any value; §4.1 truthiness

for (item in <array or object>) { ... }

return <expr>;               // expression optional; bare `return` yields null
break;                       // innermost loop only
continue;                    // innermost loop only
```

- **Scoping** is lexical and block-level. A `let` binds in its enclosing block; an inner block may shadow an outer binding, and the outer binding is intact after the block ends.
- **[DECISION, 2026-09-19]** `let`, named functions, and parameters share one block namespace. Duplicate parameters and redeclarations in the function's own body are compile-time errors; nested blocks may shadow them.
- **[DECISION]** Re-declaring the same name in the same block is a compile-time error. Assignment to an undeclared name is a runtime error — there is no implicit global creation.
- **[DECISION]** `for (item in array)` binds each element; `for (key in object)` binds each key as a `string`, in insertion order. Mutating the collection being iterated is a runtime error rather than undefined behavior.
- **[DECISION, 2026-09-22] Each loop iteration gets a fresh scope**, shared by the `for` binding and the body, so a closure created in iteration *i* captures that iteration's value rather than the loop's final one. A `let` in the body therefore never collides with itself across turns.
- **[DECISION, 2026-09-22] Iterating a non-collection is a `type` error** (`type.operand_mismatch`) spanned on the iterable expression, and mutation of the iterated collection is `reference.collection_mutated` spanned on the `for` keyword. **Any store into the iterated collection counts** — appending, adding a key, and replacing an existing element or value alike — but mutating a collection *nested* inside it is not a mutation of it and is allowed. **The check fires when the loop advances**, immediately before it takes the next element (the turn that discovers the collection is exhausted included), so mutating and then leaving the loop in the same iteration — via `break`, or a `return` out of the enclosing function or Script — is not an error: no advance follows it to observe the change.
- **[DECISION]** `break` and `continue` outside a loop are compile-time errors. A function body starts its own loop context; loops surrounding its declaration do not authorize loop control inside it.
- **[DECISION, 2026-09-19]** Top-level `return` is valid, including inside top-level control flow, and represents the Script result. A bare return has no expression only before `;`, `}`, or end of input; whitespace never terminates it.

## 6. Functions and callbacks

```
fn name(a, b) { return a + b; }        // named declaration, statement position
let f = fn(a) { return a * 2; };       // anonymous function, expression position
items.each(fn(item) { ... });          // callback as an argument
```

- **[DECISION, 2026-09-19]** Calls and parameter lists allow a trailing comma, as array and object literals do; holes and missing list entries are invalid.
- Named function declarations follow the same semicolon rule as other statements (§2). A leading statement-position brace is a block; object expressions belong in value positions or may be grouped.
- Parameters are positional. **[DECISION]** Calling with the wrong argument count is a runtime error — no implicit `null` padding and no variadic collection.
- A function body that reaches its end without `return` yields `null`.
- **[DECISION]** Closures capture their defining scope **by reference**, so a callback sees later mutations of a captured binding.
- **[DECISION]** Recursion is permitted and bounded by a call-depth limit; exceeding it is a runtime error (never a host stack overflow — Story 1.7).
- **[DECISION, 2026-09-22] The call-depth limit is 1024**, a documented constant of the interpreter rather than a parameter. Exceeding it is `depth.call_depth_exceeded`, spanned on the call's argument list. A Backend-configurable limit belongs to the Resource Budget (FR-8), not to the language.
- **[DECISION, 2026-09-22] Named functions hoist within their block.** Every `fn name` declared in a block is bound before any of that block's statements run, so mutual recursion works in any declaration order. A declaration's captured scope is that same block scope.
- **[DECISION, 2026-09-22]** A call whose callee is not a function is `type.not_callable`, and a wrong argument count is `arity.argument_count`; both are spanned on the call's argument list.
- Functions are first-class values: passable, returnable, storable in local bindings, arrays, and objects — but not in Global Variables (§3).

## 7. Errors

Every failure carries a category, a stable code, a message, and a source span (Epic 1 Story 1.8). Categories:

| Category | Raised when | Detected |
| --- | --- | --- |
| `lexical` | Unterminated string, unknown character | Lex time |
| `syntax` | Malformed construct, `break` outside a loop, duplicate `let` | Parse time |
| `type` | A conversion §4.2 does not perform — arithmetic on a non-numeric string, stringifying a collection, a collection in a numeric operand; returning a value that is, or contains, a value referring back to itself | Runtime |
| `reference` | Undeclared identifier, property access on `null`, an array write outside the appendable range | Runtime |
| `arity` | Wrong argument count | Runtime |
| `arithmetic` | Division by zero, non-finite result | Runtime |
| `depth` | Call-depth limit exceeded | Runtime |
| `capability` | Call to an unregistered or denied Registered Function (FR-6, FR-7) | Runtime |
| `budget` | A Resource Budget dimension exceeded (FR-8) | Runtime |
| `policy` | A disabled language construct was used (FR-3) | Parse or runtime |

**[DECISION] Reading a missing object key yields `null`, not an error** — optional fields are the common case for a rule author, and `if (input.discount)` should read as "if a discount was supplied" rather than blowing up. Writing to a missing key creates it.

**[DECISION] Reading an index outside an array's range yields `null`** as well, for the same reason. A non-number index on an array, or a number index on a non-collection, is still a `type` error.

**[DECISION, 2026-09-19] Array writes.** `a[i] = v` replaces an existing element, and `a[len] = v` (exactly the length) appends. Any other out-of-range, negative, or fractional index write is a `reference` error (`reference.index_out_of_range`) — a write cannot be "absent data". Reads outside the range, including negative or fractional indices, still yield `null`.

**[DECISION, 2026-09-19] Object indices are strings.** A number (or any non-string) index on an object is a `type` error, as is any index on a `string`, `number`, or `bool`, and any `.property` on a value that is not an object.

**[DECISION, 2026-09-19] A Script result cannot contain a cycle.** The Script result leaves the execution as a plain tree of values, so returning a value whose reachable graph contains a cycle (`let a = []; a[0] = a; return a;`) is a `type` error (`type.cyclic_result`) pointing at the returned expression. Cycles a Script builds but does not return are fine. A value reached twice without a cycle (`let x = [1]; return [x, x];`) returns as two equal copies — identity (§4.2) exists only inside the execution.

**[DECISION] Two cases stay `reference` errors**, because each means the script is wrong rather than the data being absent:

- **Property access on `null` without `?.`** — `order.customer.name` where `customer` is `null` raises, naming `customer`. Use `order.customer?.name` to opt into `null` instead (§4.4). Without this default, one absent field would silently produce `null` three levels later and the rule would compute a confidently wrong answer while looking like it worked.
- **Undeclared identifier** — a typo'd variable name is never data. The static check (§11) turns this into a pre-execution error when it is enabled.

**[DECISION, 2026-09-22] A Script result cannot be, or contain, a function.** A Script result leaves the execution as data the Backend can receive, and a function — which closes over an environment that dies with the execution — has no wire representation. Returning one is a `type` error (`type.function_result`) spanned on the returned expression, exactly like `type.cyclic_result`. Functions the Script builds, passes and calls without returning are unaffected. Returning a *re-triggerable callable handle* — a frozen closure the Backend can invoke later over the socket, rebound to its outer context and Global Variables — is the intended long-term direction; widening this error into a value later is not a breaking change.

**[DECISION]** Scripts cannot catch errors in v2 — there is no `try`/`catch`. Any error terminates the execution and is reported to the Backend. Host-side errors from a Registered Function (§8) reach the script the same way and are equally uncatchable.

**[DECISION, 2026-09-22] Severity is part of the shape.** A diagnostic carries a severity — `error` or `warning` — alongside its category, code, message and span. Every lexical, syntax and runtime failure is an `error`; the static check's unused-local findings (§10) are `warning`s, which can never reject a script. One shape carries both, so the check and the language server reuse the diagnostic rather than wrapping it in a second, almost-identical type. The severity word appears in every rendering.

**[DECISION, 2026-09-22] The rendered form is compact, not a rustc-style block.** A diagnostic renders for a terminal as three lines — a location line `origin:line:col: severity category[code]: message` (the `origin:` prefix only when the caller supplies a file or Script name), the offending source line, then a marker line of carets beneath the span:

```text
rules.hxp:3:9: error type[type.operand_mismatch]: cannot multiply a string by a number
    let y = "abc" * 2;
            ^^^^^
```

A span crossing lines reports both ends in the location line (`line:col-line:col`) and its marker runs to the end of the line it opens on, so no location is lost. The end of that range is **one past the span's last character**, the same exclusive convention the span's own byte range uses, so a consumer highlighting it is not off by one. Three lines per diagnostic keeps a check pass emitting many findings at once scannable. The rendering is plain text: colour belongs to the CLI, layered over it. The marker aligns exactly for tabs and a leading BOM, but **not** for double-width or combining characters — a column is one Unicode scalar throughout, so an emoji or a CJK scalar shifts the marker one cell where the terminal draws two; measuring display width would need a Unicode width table, and the crate that renders is dependency-free by design.

**[DECISION, 2026-09-22] Long lines are never truncated or windowed.** The offending source line is always printed in full and the terminal may wrap it; the marker keeps its true column. A windowing rule would make the printed line no longer the literal source, and correctness of the text beats fitting the viewport.

## 8. Host interaction

A script reaches the host only by calling a Registered Function by its registered name, as an ordinary call expression (FR-6, FR-7):

```
let order = getOrder(orderId);
applyDiscount(order.id, 10);
```

There is no import, require, module, filesystem, network, environment, or process facility in the grammar at all — the absence is structural, not a runtime check (Epic 3 Story 3.4). A call to an unregistered name raises `capability`, indistinguishable from a denied call.

## 9. Plugin source

A Plugin (FR-17) is Hexput source with three additions to the grammar above.

```
plugin {
  name = "loyalty_rules",
  version = "1.0.0",
}

let counter = 0;

@Global(behavior = "ttl", ttl = "5m", locking = "safe")
let cache = {};

@Event(BackendRegisteredInit)
fn setup(params) { counter = 0; }

@Event(OrderPlaced, priority = 1)
fn on_order(params) { counter = counter + 1; return { ok: true }; }

@Event(OrderPlaced, async = true)
fn audit(params) { logOrder(params.id); return { ok: true }; }
```

- **`plugin { }` block** — required, exactly one, first non-comment construct in the file. Keys are bare identifiers assigned literal values with `=`, comma-separated, trailing comma allowed. `name` is required; all other keys are Backend-defined and opaque to the daemon (FR-17).
- **Top-level `let`** declarations are Global Variables (FR-20). **[DECISION]** Their initializer must be a literal or an expression over literals — it may not call a function, because init order across Global Variables would otherwise be observable.
- **`@Global(...)`** — **[DECISION] new in this document; FR-20 and FR-25 require per-variable locking and behavior overrides from plugin code but no syntax was ever specified.** Optional annotation on a top-level `let`, accepting `behavior` (`"forever"` default, `"ttl"`, `"separate_each_trigger"`, `"keyed"`), `ttl` (duration string, required when behavior is `"ttl"`), and `locking` (`"safe"` default, `"unsafe"`). An annotation the Backend's Config does not permit is rejected at registration (FR-20).
- **`@Event(<Name>)`** — binds a function as a handler. Optional `priority = <integer>` (ascending, lower first) and `async = <bool>` (default `false`). `async` wins when both are present and `priority` is then ignored (FR-22, FR-23, OQ-11).
- **[DECISION]** `@Event` and `@Global` may only annotate a top-level `fn` and a top-level `let` respectively; anywhere else is a `syntax` error. A handler takes exactly one parameter, `params`.

## 10. Static check (optional, Backend-configured)

**[DECISION] New capability, recorded here first.** Hexput can run a static check pass over a parsed AST **before** executing it, catching whole classes of mistake at submission time instead of mid-rule. The check is **never mandatory**: the Backend decides per Session whether it runs, and can override that per execution, exactly like any other execution-policy setting (FR-3).

**Modes.** `off` (default — parse straight to execution), `warn` (findings reported alongside a normal execution), `error` (any finding rejects the script before a single statement runs).

**What it checks.** Everything below is decidable without running the script:

| Finding | Why it is checkable |
| --- | --- |
| Undeclared identifier read, or assignment to one | Lexical scoping is fully known from the AST |
| Duplicate `let` in one block; `break`/`continue` outside a loop | Already `syntax` errors — the check reports them with the others |
| Wrong argument count calling a function declared in the same script | Arity is known |
| Call to a name that is neither a local function nor a Registered Function on this Session | The daemon knows the Session's registrations (FR-6) — this catches a typo'd host call before it becomes a `capability` error at runtime |
| Type errors between literal operands — `"abc" * 2`, an array in a numeric operand | No runtime information needed |
| Unreachable code after `return`, `break`, or `continue` | Control flow is structural |
| A disabled language construct (FR-3 toggles) appearing anywhere in the script | The toggle set is known before execution |
| Unused local variable | Reported as a warning only, never an error |
| Plugin-only: `@Event` naming an Event the Backend never declared; `@Global` using a strategy the Backend forbids | Declarations arrive with the registration (FR-19, FR-20) |

**What it deliberately does not do.** It is not a type system: it never infers types across bindings, never checks a handler's return value against its declared shape (that stays the runtime check in FR-19), and never rejects a script for a mistake that depends on runtime values.

**Where the findings go.** Each finding carries the same category, code, message, and source span as any other error (§7), so the CLI, the Backend's error response, and the language server (FR-15) all render them identically — the check is the main reason the language server can offer more than syntax diagnostics.

**[DECISION, 2026-09-22] Two of the findings above belong to the parser, and stay there.** Duplicate `let` in one block and `break`/`continue` outside a loop are rejected while a Script is *parsed*, so a parsed AST — which is all the check pass ever sees — can never carry them. They are listed above because a caller is shown them with the same category, stable code, message and span either way, not because the pass reproduces them. The pass documents the boundary rather than carrying code that can never run.

**[DECISION, 2026-09-22] A finding is never a false positive.** Everything the pass reports would really fail if that code were reached. Where a rule would need to know a value, the pass stays silent instead of guessing: a type error is reported only when *every* operand involved is a literal (`"abc" * 2` yes, `let s = "abc"; s * 2` no), an argument count only against a name that certainly still holds the function it was declared with (any assignment to that name anywhere ends that certainty), and unreachable code only after a `return`, `break` or `continue` in the very same block — never after an `if` whose branches all return, which needs control-flow analysis to prove. The converse does not hold and is not meant to: silence means "nothing decidable was wrong", never "this Script is correct".

**[DECISION, 2026-09-22] A finding is an error when the code would certainly fail, and a warning when it cannot.** Errors: an undeclared read or assignment, a wrong argument count, a literal-operand type error, a disabled construct, a call to a name the supplied list does not contain. Warnings: an unused local, and code unreachable after `return`/`break`/`continue`. This is what makes "a warning-only finding can never reject a script" true by construction rather than by a maintained list.

**[DECISION, 2026-09-22] The pass also takes the Script's starting-variable names.** A Script written for a Backend reads inputs its own source never declares (§12), so without them every such read is an undeclared-identifier finding and the check is useless on exactly the Scripts it is for. Only the names are taken — a static check needs to know a name exists, never what it holds — and they are declared in the root scope before the top-level named functions hoist, exactly where the interpreter binds them, so a name the Script's own top level also declares is `syntax.duplicate_declaration` rather than a silent double binding.

**[DECISION, 2026-09-22] A missing callable-name list is not an empty one.** Supplying *no* list suppresses the unknown-call finding entirely, because a caller that never said what is callable cannot have every host call held against it. Supplying an *empty* list does not: it means "nothing but this Script's own functions is callable". A call whose callee is a bare name that no scope declares is therefore reported only when a list was supplied and does not contain it — and is never reported as an undeclared identifier, since without a list the pass cannot tell a Registered Function from a typo.

## 11. Deliberately absent

Not in v2, and not an oversight: `try`/`catch`, `throw`, modules and imports, classes and inheritance, `this`, string interpolation, regular expressions, integer/float distinction, bitwise operators, ternary `?:`, switch, labeled break, generators, `async`/`await` inside scripts (concurrency is the daemon's concern, not the script's), and any standard library beyond what the host registers.

**[DECISION]** There is no built-in standard library at all in v2 — not even `len()` or `push()`. Everything a script can do beyond the operators above comes from Registered Functions. This keeps the trust boundary exactly at the capability edge (NFR1) and is the single most likely item to need revisiting once real scripts are written.

## 12. Running a Script locally

**[DECISION, 2026-09-22] New in this document.** Epic 1 Story 1.9 gives the language a local command — `hexput eval <script>` — that runs a Script with no daemon, socket, or Backend. It is the first surface that has to name a Script's *inputs* and its *result* outside the runtime, so the three choices below are part of the language's definition rather than one tool's convention: Story 1.10's check command, the language server, and any later Backend-side reader reuse them.

**[DECISION, 2026-09-22] A starting variable's value is a Hexput expression.** A Script's inputs are supplied as `--var name=<expression>`, where `name` is a §2 identifier (reserved words excluded) and the value is ordinary Hexput source, evaluated as `return <expression>;` before the Script is read. All six §3 types are therefore reachable — `--var n=5`, `--var s='"hi"'`, `--var xs='[1, 2]'`, `--var o='{ a: 1 }'` — and the quoting and literal rules are the language's own, so nothing here can drift from §3. An input expression sees no starting variables of its own: an input is a value, not a computation over the other inputs. Starting variables are bound into the Script's root scope before any top-level statement runs and before named functions hoist, so a starting variable behaves exactly like a top-level `let` — assignable, shadowable by an inner block, captured by reference by a closure (§5, §6) — and can never shadow a top-level `fn` of the same name.

**[DECISION, 2026-09-22] A Script result prints in Hexput literal form.** Strings print with their quotes and §3 escapes, numbers through §4.3's number-to-string so a number has exactly one spelling, `null`/`true`/`false` bare, arrays as `[1, 2]`, objects as `{ a: 1 }` — keys bare where §2 allows and quoted otherwise, in insertion order — and empty collections as `[]` and `{}`. Printed output is never truncated and has no depth limit. What is printed is valid Hexput source that can be pasted back into a script, and is exactly the grammar a starting variable's value accepts: one surface for reading a result and writing an input, not two.

**[DECISION, 2026-09-22] Exit codes: `0` success, `2` usage, `1` everything else.** A rendered diagnostic — lexical, syntax or runtime — an unreadable file, and bytes that are not UTF-8 all exit `1`; a bad flag, a missing operand, and a malformed, non-evaluating or repeated `--var` exit `2`, so a calling script can tell "I invoked it wrong" from "the Hexput was wrong". A `--var` name supplied twice is a usage error, never last-wins: silently dropping one of two supplied values is the kind of thing a person debugs for an hour. Every failure is reported on stderr, using §7's rendering unchanged with the script path as the origin label, so the printed result can be piped; stdout carries a successful evaluation's result, and the help or version text when it is explicitly asked for (exit `0` — asking is not a failure, and the answer is what the reader wanted). The rendering carries no colour (Story 1.9 decision 4).

**[DECISION, 2026-09-22] `hexput check` shares eval's surface, and declares names rather than binding values.** `hexput check <script>` reports a Script's mistakes without running any of it, over the same file handling, diagnostic rendering and exit contract as `hexput eval`. Two repeatable flags describe the environment the Script expects: `--var <name>` declares a starting variable **by name alone** — no `=<expression>`, because a static check needs the name and never the value, and evaluating one would be execution on a path whose whole promise is that nothing runs — and `--callable <name>` declares a name the host makes callable. Supplying `--callable` at all is what turns on reporting calls to unknown names; without it no call is reported. Each value must be a §2 identifier and may be given once, a repeat being a usage error for the same reason a repeated `--var` is under eval.

**[DECISION, 2026-09-22] The check command writes one summary line to stdout and its findings to stderr.** `rules.hxp: no findings` when clean, `rules.hxp: 3 findings, 1 error` otherwise, so a person reading it gets an answer either way and clean is visibly not the same as warnings-only. The findings themselves are diagnostics and go to stderr through §7's rendering with the script path as the origin label, exactly as under eval, which keeps the summary pipeable and the error channel where diagnostics belong. The exit codes are unchanged: `0` when no finding is an error — warnings included, since a warning can never reject a Script — `1` for an error-severity finding or a source that cannot be parsed or read, `2` for a usage error. Findings are reported in source order.
