---
title: 'Story 3.1: Call a registered host function from a script'
type: 'feature'
created: '2026-09-24'
status: 'done'
baseline_commit: '5779a10024530d838b449bb3bb5e3d82d433f4b1'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** A Script cannot reach the Backend at all: a call to a name the Backend registered at init fails as `reference.undeclared_identifier`, and the interpreter runs to completion on one blocking thread, so it could not wait for a Backend reply without pinning that thread.

**Approach:** The interpreter becomes resumable: a bare-name call whose name is not declared suspends the execution and hands the Executor the call (name, detached arguments, span). The Executor checks the name against the Session's registrations — unregistered is `capability`, never `reference` — and for a registered one sends an outbound RPC to the Backend over the same connection, awaits the correlated reply with no thread held, and resumes the Script with the returned value or ends it with an error attributed to the call's span.

## Boundaries & Constraints

**Always:** Every Script step runs on the blocking pool (`spawn_blocking`); the wait for a reply is a plain `.await` with no thread and no lock held. The connection loop stays the only writer of its `Outbound` half and routes each Backend `Result`/`Error` whose id names a pending Daemon call to that call. Daemon-issued call ids are a per-connection counter, independent of the Backend's own ids (a `Result`/`Error` from the Backend is always a reply to a Daemon call). A local binding shadows a registered name; only a bare identifier callee that resolves to nothing is a host call. Unregistered calls go through `hexput-enforce` via `hexput-exec` (AD-3). `hexput eval` (no host) reports any host call as `capability`. When the connection stops reading or is lost, every pending call fails at once, so draining executions cannot deadlock. A Backend error is logged at `debug`, never as a Daemon failure. Every new code joins `Code::ALL`.

**Decisions (2026-09-24, Erdem):**
1. *Wire shape* — one generic `MessageType::Call` (not function-specific), because Backend-registered *methods* bound to objects will ride the same message later. Payload `{name, arguments: [..]}`; a later method call adds a `receiver` key, so the shape grows additively. The Backend replies `Result {value}` (a map, so later keys such as reported modifications are additive) or `Error` (its `message`, when a string, is carried into the Script error).
2. *Host-call failure* — a new §7 category `host`: `host.function_failed` (an `Error` reply, or a reply that is not a valid `{value}`) and `host.no_reply` (the connection ended first), with a dated LANGUAGE-REFERENCE amendment. Distinct from `capability`.
3. *Crate edge* — `hexput-connection → hexput-rpc`, added to the Spine by dated amendment and pinned in `check-crate-graph.py`.
4. *Scope* — plain Registered Function calls only. Methods (`registerMethod(objKey, fn)`), the hidden `__secret` object metadata and per-entity reference ids are Stories 3.11–3.13 (FR-27/FR-28, added by course correction 2026-09-24).
5. *Argument depth* — a call argument nested deeper than 12 levels is refused before anything is sent (`depth.argument_too_deep`, spanned on that argument). 12 is the default of a runtime limit that later becomes configurable (Story 3.7); here it is a documented constant.

**Never:** No grants (`context.allow()`, per-call handlers — 3.2/3.3), no budgets or RPC-reply timeout (3.5/3.6), no Config keys, no `?.` optional call, no Script-side catch, no new Session state beyond reading registration names per execution, no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Round trip | `getOrder` registered; `return getOrder(7).total;` | Daemon sends one `Call {name:"getOrder", arguments:[7]}`; Backend `Result {value:{total:3}}` → Script result `3` | N/A |
| Backend error | Backend answers with `Error` | Script ends with `host.function_failed` spanned on the call; other executions unaffected | debug log only |
| Unregistered | `nope(1)` | `capability.*` error on the call span; nothing sent | N/A |
| Shadowed | `fn getOrder(x) { return x; } return getOrder(1);` | `1`, nothing sent | N/A |
| Unsendable argument | a function or cyclic value as argument | `type` error spanned on that argument; nothing sent | N/A |
| Deep argument | an argument nested 13 levels | `depth.argument_too_deep` on that argument; nothing sent | N/A |
| Concurrent | two executions each waiting on a call | replies routed by id in any order; each resumes with its own value | N/A |
| Peer closes mid-call | inbound EOF while a call is pending | the call fails with `host.no_reply`, the execution ends, its reply is attempted, connection detaches | N/A |
| Stray reply | Backend `Result` with an id no call is pending on | `protocol.unexpected_message`, as today | N/A |

</frozen-after-approval>

## Code Map

- `crates/hexput-interpreter/src/machine.rs` -- `Machine<'p>` (L163) borrows `&'p Program`; `Frame` variants hold `&'p` AST refs (44 uses). Make it own `Arc<Program>` with index-based frames so a suspended machine is `'static + Send` (keep the `machine_is_send` test, L1512). Split `run`/`execute` (L262/266) into a non-consuming step loop returning `Finished | HostCall`. Host-call hook: `expression()` Access arm (L859) — base is a bare `Identifier` whose lookup misses and first link is `Call` → evaluate args, detach them (`heap.detach`, heap.rs:270), suspend. Resume = `heap.attach` + push + `Link{index+1}`, the continuation `return_from_call` (L607-623) uses.
- `crates/hexput-interpreter/src/lib.rs` -- `evaluate*` (L88/L131) keep their signatures; a host call there is `capability`. New public resumable type (e.g. `Execution::{new, with_variables, run, resume}`).
- `crates/hexput-shared/src/diagnostics.rs` -- `Category::Capability` exists (L69); add the new codes to `Code::ALL` (L246-278). `capability.unknown_function` (L238) is the checker's; reuse it at runtime unless review finds a reason not to.
- `crates/hexput-shared/src/wire.rs` -- `MessageType` (L51/L64/L68) gains `Call`. `Category` gains `Host`.
- `crates/hexput-enforce/src/lib.rs` -- stub; add the registered-name capability check (enforce→shared only).
- `crates/hexput-rpc/src/lib.rs` -- stub; outbound call type: builds the call payload, a per-connection `Calls` (id counter + pending map of `oneshot`s), decodes the reply. Depends on `hexput-port` (+ `tokio` sync).
- `crates/hexput-exec/src/lib.rs` -- `execute` becomes `async`: drives the machine segment-by-segment through `spawn_blocking`, converts arguments/replies wire↔Value, awaits `hexput-rpc`. Move `to_hexput`/`to_wire`/`check_result` from `crates/hexput-script/src/wire.rs` here (script keeps using them via exec).
- `crates/hexput-script/src/lib.rs` -- `direct_execution` (L57) becomes `async`, takes the registration names and the call handle.
- `crates/hexput-connection/src/lib.rs` -- `exchange` (L167): third `select!` branch on the outbound-call `mpsc` receiver → `deliver`; `Received::Message` of `Result`/`Error` checks pending first (before `answer`, L319-325); `spawn_blocking` (L211) becomes `spawn(async ….instrument(span))`; when reading stops or the stream is lost, fail all pending calls. Update module docs.
- `scripts/check-crate-graph.py`, Spine -- pin `hexput-rpc`/`hexput-enforce` exact sets; add `hexput-connection → hexput-rpc`.
- `crates/hexput-tests/tests/{interpreter,connection,script,exec,shared}.rs` -- the `Scripted` Port (connection.rs L27) is fixed up front; add a reactive Port that answers outbound calls.

## Tasks & Acceptance

**Execution:**
- [x] `crates/hexput-interpreter/src/{machine.rs,lib.rs}` -- owned program, resumable step API, host-call suspension -- AC1/AC3.
- [x] `crates/hexput-shared/src/{diagnostics.rs,wire.rs}` -- codes, message type.
- [x] `crates/hexput-enforce`, `crates/hexput-rpc` -- capability check; call correlation.
- [x] `crates/hexput-exec`, `crates/hexput-script` -- async driver, moved conversions.
- [x] `crates/hexput-connection/src/lib.rs` -- routing, outbound branch, fail-pending on close.
- [x] `scripts/check-crate-graph.py`, Spine (conn→rpc amendment), LANGUAGE-REFERENCE (`host` category, host-call semantics in §7/§8), `CLAUDE.md` status.
- [x] `crates/hexput-tests/tests/*` -- every matrix row; `hexput eval` host call → `capability`.

**Acceptance Criteria:**
- Given an execution waiting on a reply, when the runtime is inspected, then no blocking-pool thread is held for it and a second execution on the same connection completes meanwhile.
- Given any Daemon execution path, when traced, then it reaches the interpreter only through `hexput_exec::execute`.

## Implementation Notes

- Planning-side amendments already landed in the 2026-09-24 course correction: the Spine's `conn --> rpc` edge and wire-contract amendments, LANGUAGE-REFERENCE §7 (`host`, `depth.argument_too_deep`) and §8, and AGENTS.md's summary. Code-side, `scripts/check-crate-graph.py` and AGENTS.md's Project Status still need this story's updates. LANGUAGE-REFERENCE §3/§8 also describe Value Secrets, Reference IDs, methods and modifications — those are Stories 3.11–3.13, out of scope here (decision 4).

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence / route |
|---|-------|---------|---------|------------------|
| 1 | verification-gap | Fatal-frame branch's `calls.close()` untested; removing it hangs `serve` with a call pending | medium | Pre-verified gap; no test sends `FrameTooLarge` with a call pending. **patch** (test) |
| 2 | verification-gap + blind | A host call made *after* reading stops is untested (`Calls::close` closing the queue) | medium | Pre-verified; every test writes its call before closing or drops `Calls`. **patch** (test) |
| 3 | verification-gap + blind | Shared argument frame budget (`TooLarge` → `host.function_failed`, nothing sent) untested | medium | Pre-verified; no test reaches `arguments()`'s `TooLarge` arm. **patch** (test) |
| 4 | verification-gap | Malformed-reply message's `Path::reply` root never asserted | low | Pre-verified; direct assertion. **patch** (test) |
| 5 | edge-case + blind + verification-gap | `Unsendable::Unrepresentable` reports "encode to more than the maximum frame" | low | Real (wrong text if reached; unreachable today since the interpreter refuses functions). Direct correction. **patch** |
| 6 | blind | A stray Backend `Result`/`Error` is refused with its id echoed, which the Backend reads in its own id space as a failure of its request with that id | medium | Real: `answer`'s `UnexpectedMessage` refusal echoes the id; the new per-direction id spaces make that id meaningless to the Backend. Codec allows nil id on `Error`. **patch** (refuse with nil id) |
| 7 | blind + edge-case | Argument checks (type/depth/size) run before the capability check; unregistered call with a deep argument reports `depth` | low | Real but unordered by the spec; `type.*` argument errors necessarily come first from the interpreter, so argument-first keeps them consistent. Missing piece is documentation. **patch** (LANGUAGE-REFERENCE §8 states the order) |
| 8 | blind | LANGUAGE-REFERENCE §7 `type`/`host` rows and §12 not updated for the new codes | low | Real: §7 omits `type.function_argument`/`type.cyclic_argument` and the could-not-be-sent case; §12 silent on host calls. **patch** (docs) |
| 9 | blind | `hexput-check`'s unknown-call finding spans the name; the runtime's `capability.unknown_function` spans the whole call | low | Real: `pass.rs` uses `name.span`, frozen matrix puts the runtime error on the call span; CLI/LSP underline differently. Direct correction. **patch** (check spans the call) |
| 10 | blind | `Caller::call` is public, so `hexput-connection`/`hexput-script` could send a `Call` without `Capabilities::check_call`; AD-3's reach to a Registered Function becomes convention, not graph-enforced | medium | Real. Consequence of approved decision 3 (conn→rpc edge); closing it needs a sealed-grant mechanism (new public surface). **defer**, flagged to Erdem |
| 11 | blind | Runtime decides host-vs-local dynamically (lookup at call time), the check lexically; `f()` before a later `let helper` can go to the host once, locally later | low | Real but only for a call preceding its name's `let` — the language already resolves every name dynamically (reads before `let` are `reference`); fix means lexical resolution. Rejected (low, rare, non-trivial fix); mentioned to Erdem |
| 12 | blind | Status spellings disagree (`in-review` vs sprint `review`) | false | The workflow's step-05 syncs `sprint-status.yaml`; `in-review` is the spec template's value |
| 13 | blind | Spec implementation note is stale | — | Rejected: fix edits this build's spec |
| 14 | blind | `blocking` helper duplicated in `hexput-exec` and `hexput-script` | low | Rejected: no named divergence; sharing it adds public surface |
| 15 | blind | `Machine::resume` without a pending call is a silent no-op | low | Rejected: unreachable through `HostCall::resume`; fix adds a guard |
| 16 | blind | Lost stream (`Closed(Some)`) with a call pending untested | low | Rejected: dropping `JoinSet` aborts the tasks, nothing can hang; adds only a test of existing Epic 2 behaviour |
| 17 | edge-case | `Frame::Declare` silently discards on a statement-kind mismatch | false | Unreachable: `Declare(at)` is pushed only for a `Let` statement of the same immutable `Arc<Program>` |
| 18 | edge-case | `Frame::Branch` falls through when the branch index is missing | false | Unreachable: `Branch{at,index}` is pushed only by `next_branch` after `branches.get(index)` matched |
| 19 | edge-case | Blocking segments lose the request span | false | Nothing in `hexput-exec`/`hexput-script`/the interpreter logs inside a blocking segment (no `tracing` use there) |
| 20 | edge-case | `missingFn(x1)` now reports undeclared `x1`, and eval reports `capability` for a typo'd callee | false | Specified: arguments are evaluated before the call is judged (as for any call) and `hexput eval` reports host calls as `capability` (frozen Always) |

## Design Notes

A suspended machine must move between `spawn_blocking` segments, so it cannot borrow the `Program`; `unsafe`/self-referential wrappers are forbidden in the interpreter, hence `Arc<Program>` + indices. Direction disambiguates ids: the Backend sends `Result`/`Error` only as replies to Daemon calls, and the Daemon only as replies to Backend requests, so the two id spaces never meet.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
