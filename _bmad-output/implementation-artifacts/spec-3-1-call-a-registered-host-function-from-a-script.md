---
title: 'Story 3.1: Call a registered host function from a script'
type: 'feature'
created: '2026-09-24'
status: 'draft'
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

**Never:** No grants (`context.allow()`, per-call handlers — 3.2/3.3), no budgets or RPC-reply timeout (3.5/3.6), no Config keys, no `?.` optional call, no Script-side catch, no new Session state beyond reading registration names per execution, no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Round trip | `getOrder` registered; `return getOrder(7).total;` | Daemon sends one outbound call `{name:"getOrder", arguments:[7]}`; Backend `Result {value:{total:3}}` → Script result `3` | N/A |
| Backend error | Backend answers with `Error` | Script ends with the host-call error spanned on the call; other executions unaffected | debug log only |
| Unregistered | `nope(1)` | `capability.*` error on the call span; nothing sent | N/A |
| Shadowed | `fn getOrder(x) { return x; } return getOrder(1);` | `1`, nothing sent | N/A |
| Unsendable argument | a function or cyclic value as argument | `type` error spanned on that argument; nothing sent | N/A |
| Concurrent | two executions each waiting on a call | replies routed by id in any order; each resumes with its own value | N/A |
| Peer closes mid-call | inbound EOF while a call is pending | the call fails, the execution ends, its reply is attempted, connection detaches | N/A |
| Stray reply | Backend `Result` with an id no call is pending on | `protocol.unexpected_message`, as today | N/A |

</frozen-after-approval>

## Open Questions

1. **Wire shape of the outbound call** — options: (A, recommended) new `MessageType::FunctionCall` with payload `{name, arguments: [..]}`; Backend replies `Result {value}` (mirrors `ExecutionStart`'s reply) or `Error` with an `ErrorBody` (its `message` is surfaced to the Script error) / (B) a generic `Call` name reserved for later Plugin/Event use too. Backends will code against whichever is chosen.
2. **Error for a failed host call** — options: (A, recommended) a new §7 category `host` (`host.function_failed` for an `Error` reply or malformed reply, `host.no_reply` when the connection ends first), with a dated LANGUAGE-REFERENCE amendment / (B) no new category: reuse `capability` (blurs "denied" with "host failed", which Story 3.3's logs must tell apart) / (C) pass the Backend's own category/code through verbatim (untrusted text becomes a Hexput code).
3. **Crate edge for the connection to hand calls to `hexput-rpc`** — options: (A, recommended) add `hexput-connection → hexput-rpc` to the Spine with a dated amendment and pin it in `check-crate-graph.py`; the connection owns the channel/pending map `hexput-rpc` defines / (B) no new edge: `hexput-exec` and `hexput-script` re-export `hexput-rpc`'s types so the connection names them through `hexput-script` (graph unchanged, but a type leaks through two crates that don't own it).

## Code Map

- `crates/hexput-interpreter/src/machine.rs` -- `Machine<'p>` (L163) borrows `&'p Program`; `Frame` variants hold `&'p` AST refs (44 uses). Make it own `Arc<Program>` with index-based frames so a suspended machine is `'static + Send` (keep the `machine_is_send` test, L1512). Split `run`/`execute` (L262/266) into a non-consuming step loop returning `Finished | HostCall`. Host-call hook: `expression()` Access arm (L859) — base is a bare `Identifier` whose lookup misses and first link is `Call` → evaluate args, detach them (`heap.detach`, heap.rs:270), suspend. Resume = `heap.attach` + push + `Link{index+1}`, the continuation `return_from_call` (L607-623) uses.
- `crates/hexput-interpreter/src/lib.rs` -- `evaluate*` (L88/L131) keep their signatures; a host call there is `capability`. New public resumable type (e.g. `Execution::{new, with_variables, run, resume}`).
- `crates/hexput-shared/src/diagnostics.rs` -- `Category::Capability` exists (L69); add the new codes to `Code::ALL` (L246-278). `capability.unknown_function` (L238) is the checker's; reuse it at runtime unless review finds a reason not to.
- `crates/hexput-shared/src/wire.rs` -- `MessageType` (L51/L64/L68) gains the call variant.
- `crates/hexput-enforce/src/lib.rs` -- stub; add the registered-name capability check (enforce→shared only).
- `crates/hexput-rpc/src/lib.rs` -- stub; outbound call type: builds the call payload, a per-connection `Calls` (id counter + pending map of `oneshot`s), decodes the reply. Depends on `hexput-port` (+ `tokio` sync).
- `crates/hexput-exec/src/lib.rs` -- `execute` becomes `async`: drives the machine segment-by-segment through `spawn_blocking`, converts arguments/replies wire↔Value, awaits `hexput-rpc`. Move `to_hexput`/`to_wire`/`check_result` from `crates/hexput-script/src/wire.rs` here (script keeps using them via exec).
- `crates/hexput-script/src/lib.rs` -- `direct_execution` (L57) becomes `async`, takes the registration names and the call handle.
- `crates/hexput-connection/src/lib.rs` -- `exchange` (L167): third `select!` branch on the outbound-call `mpsc` receiver → `deliver`; `Received::Message` of `Result`/`Error` checks pending first (before `answer`, L319-325); `spawn_blocking` (L211) becomes `spawn(async ….instrument(span))`; when reading stops or the stream is lost, fail all pending calls. Update module docs.
- `scripts/check-crate-graph.py`, Spine -- pin `hexput-rpc`/`hexput-enforce` exact sets; edge per Q3.
- `crates/hexput-tests/tests/{interpreter,connection,script,exec,shared}.rs` -- the `Scripted` Port (connection.rs L27) is fixed up front; add a reactive Port that answers outbound calls.

## Tasks & Acceptance

**Execution:**
- [ ] `crates/hexput-interpreter/src/{machine.rs,lib.rs}` -- owned program, resumable step API, host-call suspension -- AC1/AC3.
- [ ] `crates/hexput-shared/src/{diagnostics.rs,wire.rs}` -- codes, message type.
- [ ] `crates/hexput-enforce`, `crates/hexput-rpc` -- capability check; call correlation.
- [ ] `crates/hexput-exec`, `crates/hexput-script` -- async driver, moved conversions.
- [ ] `crates/hexput-connection/src/lib.rs` -- routing, outbound branch, fail-pending on close.
- [ ] `scripts/check-crate-graph.py`, Spine, LANGUAGE-REFERENCE (per Q2/Q3), `CLAUDE.md` status.
- [ ] `crates/hexput-tests/tests/*` -- every matrix row; `hexput eval` host call → `capability`.

**Acceptance Criteria:**
- Given an execution waiting on a reply, when the runtime is inspected, then no blocking-pool thread is held for it and a second execution on the same connection completes meanwhile.
- Given any Daemon execution path, when traced, then it reaches the interpreter only through `hexput_exec::execute`.

## Implementation Notes

## Spec Change Log

## Review Triage Log

## Design Notes

A suspended machine must move between `spawn_blocking` segments, so it cannot borrow the `Program`; `unsafe`/self-referential wrappers are forbidden in the interpreter, hence `Arc<Program>` + indices. Direction disambiguates ids: the Backend sends `Result`/`Error` only as replies to Daemon calls, and the Daemon only as replies to Backend requests, so the two id spaces never meet.

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings` -- clean
- `python3 scripts/check-crate-graph.py` -- passes
- `cargo test --workspace --locked` -- all pass
