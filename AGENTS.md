# AGENTS.md

This file provides guidance to AI coding agents (Claude Code and others) working with code in this repository. `CLAUDE.md` in this same directory is a symlink to this file — edit this file, not that one.

**Keep this file current.** As the project moves past planning and into implementation, update the Project Status section (and anything else below that's gone stale) to reflect what's actually true — don't let this drift into a description of a project that no longer exists.

## Language

The maintainer (Erdem) is Turkish. **Talk to him in Turkish; write everything else in English.**

- **Conversation** — Turkish. Chat replies, questions, explanations, status updates.
- **Everything written down** — English. Code, comments, doc comments, commit messages, PR descriptions, specs, planning artifacts, CI config, and this file.

The split is deliberate: the repository stays readable to any contributor or agent regardless of language, while day-to-day discussion happens in the maintainer's own. Do not "helpfully" switch a document to Turkish, and do not answer in English because the surrounding files are English.

BMAD mirrors this — `_bmad/custom/config.user.toml` pins `communication_language = "Turkish"` while `document_output_language` stays `English`. Note that `_bmad/config.toml`, `_bmad/config.user.toml` and `_bmad/*/config.yaml` are installer-generated and overwritten on every install; `_bmad/custom/` is the only durable place to change this.

## Project Status

_Last updated: 2026-09-22._

**Epic 1 Stories 1.1–1.7 are done; Stories 1.8–1.10 are `backlog`.** The Cargo workspace has 23 crates under `crates/`. `hexput-lexer` tokenizes source; `hexput-shared::diagnostics` defines shared errors and stable codes; `hexput-ast` holds owned, spanned syntax in flat expression and block arenas; `hexput-parser::parse` parses the whole Epic 1 grammar (expressions, declarations, assignments, member access, blocks, conditionals, loops, functions, calls, objects, arrays). `hexput-interpreter::evaluate` now runs the **whole language** on an explicit continuation stack — no host recursion from input nesting, iteration count, or call depth: values, §4 operators and conversions, optional access, §7 absent-data/reference rules, block scoping, `let`, assignment, `if`/`else`, `while`, `for … in`, `break`/`continue`, named and anonymous functions, closures (captured by reference), calls, and `return`. Call depth is bounded by the documented `hexput_interpreter::CALL_DEPTH_LIMIT` (1024). A function cannot be the Script result (`type.function_result`), as a function has no wire representation. Tests live in `hexput-tests`. The check pass and daemon crates remain stubs. This is a from-scratch v2 rewrite; there is no v1 code in this repository.

See [Build, Lint, Test](#build-lint-test) below for the commands that actually work today.

Planning is complete and final:
- [Product brief](_bmad-output/planning-artifacts/briefs/brief-hexput-2026-09-18/brief.md) (+ [addendum](_bmad-output/planning-artifacts/briefs/brief-hexput-2026-09-18/addendum.md)) — problem, solution, differentiation, deployment model, risks.
- [PRD](_bmad-output/planning-artifacts/prds/prd-hexput-2026-09-18/prd.md) (+ [addendum](_bmad-output/planning-artifacts/prds/prd-hexput-2026-09-18/addendum.md)) — FR-1 through FR-25, MVP scope, success metrics. FR-N ids are stable references; the PRD's `.memlog.md` in the same folder is the audit trail of every decision behind them.
- [Architecture spine](_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md) — the paradigm, crate boundaries, and AD-1 through AD-8 invariants below are distilled from it. Read the full spine before implementing anything it governs; this file only orients.

- [Epic breakdown](_bmad-output/planning-artifacts/epics.md) — 9 epics, 74 stories, every FR-1…FR-26 covered by Given/When/Then acceptance criteria. Also records the resolutions for PRD open questions OQ-1 (health/metrics are RPC messages, no HTTP listener), OQ-2 (reconnect secret mechanism), OQ-3 (the closed set of language feature toggles), and OQ-11 (handler ordering conventions) — treat those as settled, not open.
- [Language reference](_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md) — **normative definition of the Hexput language**, written during sprint planning because no earlier artifact specified it. Epic 1 implements it, Epic 9's grammar and LSP describe it, Epic 6 extends it. Where a story and this document disagree, the document wins. Its `[DECISION]` markers are approved provenance, not open questions.
- [Sprint status](_bmad-output/implementation-artifacts/sprint-status.yaml) — the tracking file; regenerate with `bmad-sprint-planning` whenever the epics change.

Three amendments landed after the PRD and spine were first marked final, all recorded in their `.memlog.md` files: **FR-26** (optional, Backend-configured static check before execution), **AD-8** with the `hexput-check` crate, and — largest of the three — the **crate split**: the Structural Seed is no longer one crate with an internal module tree but a Cargo workspace (22 crates at the time, plus `hexput-tests` since), one crate per module plus language (`hexput-ast`/`hexput-lexer`/`hexput-parser`/`hexput-interpreter`) and tooling crates, every crate a `lib`, with exactly one binary-producing crate (`hexput-bin`). Several Architecture Decisions (AD-1, AD-3, AD-4, AD-5, AD-8) are now compiler-enforced by which crate depends on which — see the spine's "Crate dependency graph" subsection for the exact mapping before writing any crate's `Cargo.toml`.

**Next step:** implement Story 1.8 (report errors with precise source locations) via `bmad-build`. Keep this section honest as stories land.

## Build, Lint, Test

Run from the repository root. These are exactly the CI steps, in order — CI adds `--locked` to catch `Cargo.lock` drift, so use it locally too.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/check-crate-graph.py
cargo build --workspace --all-targets --locked
cargo test --workspace --locked
```

`scripts/check-crate-graph.py` is the one non-obvious step. **A forbidden dependency edge does not fail `cargo build` while the crates are stubs** — nothing imports it yet, so the manifest edge resolves fine and clippy stays silent. The script asserts the AD-enforcing edges against the resolved graph from `cargo metadata`, which is what actually keeps AD-1/AD-3/AD-4/AD-5/AD-8 enforced rather than merely documented. If you add a crate or an edge, update it in the same change.

NFR2's memory-safety rule is a workspace lint (`undocumented_unsafe_blocks = "deny"` in `[workspace.lints.clippy]`, inherited by every crate via `[lints] workspace = true`). The four crates NFR2 names — `hexput-lexer`, `hexput-parser`, `hexput-interpreter`, `hexput-exec` — escalate it to `#![forbid(...)]`, which an inner `#[allow]` cannot override. Any `unsafe` block needs a `SAFETY:` comment.

## Where tests go

Tests live in **`crates/hexput-tests`**, one `tests/<crate>.rs` per crate under test. Add a crate to that manifest's `[dev-dependencies]` as it gains code worth testing.

Two things about this are load-bearing:

- **Crates under test are dev-dependencies, never normal ones.** `check-crate-graph.py` enforces the Spine's rules over normal dependencies only, because those rules are about what *production* code can reach. That is what lets one test crate reach `hexput-enforce` (AD-3) or `hexput-transport` (AD-1) without weakening them — a normal dependency there fails the graph check, as it should.
- **These are integration tests.** A separate crate sees only the public API. A test that needs a private item has to stay in a `#[cfg(test)] mod tests` inside its own crate — that is the exception, and worth a comment saying why.

## What Hexput Is

A standalone scripting runtime daemon (Rust) — installed and run like Redis, not embedded like a library. Backend applications connect over a socket (Unix Domain Socket, Named Pipe, TCP+TLS, or WebSocket) and either run stateless scripts (one-shot or cached execution) or register stateful **Plugins** (persistent, event-driven, capability-sandboxed code units). No OS-level sandboxing — the language itself is the trust boundary, permanently, by design (see the brief's Risks section for why).

## Architecture (from the spine — treat the spine as authoritative, this is a summary)

**Paradigm:** Hexagonal (Ports & Adapters) at the transport boundary, wrapping an Actor-model runtime core.

**One crate per boundary, all under `crates/`.** These are Cargo dependency edges, not module conventions — a forbidden reach is a manifest the graph check rejects, not a review comment.

```text
# cross-cutting
hexput-shared        # diagnostics.rs, wire.rs, ids.rs, budget.rs — never a grab-bag

# language (no execution, no I/O, no host access)
hexput-ast           # AST node types + Span — data only
hexput-lexer         # tokenizer
hexput-parser        # parser — depends on lexer + ast
hexput-interpreter   # tree-walking evaluator — depends on ast only, no host reach
hexput-check         # static check (FR-26) — ast + shared ONLY, cannot execute (AD-8)
hexput-cli-core      # eval + check command logic

# daemon
hexput-transport     # UDS, Named Pipe, TCP+TLS, WebSocket adapters — one Port impl each
hexput-port          # wire envelope (MessagePack), request/response + event correlation
hexput-session       # Client ID -> Session, TTL, reconnect + credential validation
hexput-connection    # transient per-connection actor, attached to a Session (0..N per Session)
hexput-script        # Direct/Cached Execution: AST cache, parse/interpret
hexput-plugin        # Plugin actor: registration, event routing/ordering metadata
hexput-globalvar     # Global Variable store: concurrent per-Plugin map, independent of the actor's mailbox
hexput-exec          # the one shared Executor every execution mode funnels through
hexput-enforce       # Capability + Resource Budget enforcement — reachable ONLY via hexput-exec (AD-3)
hexput-rpc           # host-function registry, capability grants, outbound RPC to Backend
hexput-config        # System Config (file) parsing — separate crate from per-backend Config (AD-5)
hexput-daemon        # wiring root: composes the above into run(SystemConfig)

# developer tooling
hexput-grammar       # tree-sitter grammar
hexput-lsp-core      # language server logic — lexer/parser/check only, never interpreter
hexput-tests         # every crate's tests, one tests/<crate>.rs per crate under test

hexput-bin           # the ONLY crate producing binaries: src/bin/{hexput-daemon,hexput,hexput-lsp}.rs
```

Read the spine's "Crate dependency graph" subsection before adding any edge. Nothing not listed there is permitted.

**Invariants that bind implementation** (full rationale in the spine; these are the ones a change is most likely to violate):

- Every Transport is an adapter into one `Port`; Core never branches on transport type (AD-1). Health/metrics ride the same `Port`, pre-init-gate.
- A Session may have zero or more concurrently attached Connections — never exactly one; no cross-Connection broadcast, each request/response stays with the Connection that issued it (AD-2).
- Direct Execution, Cached Execution, and Plugin Event handlers all call one `Executor` entry point — no path checks capability or consumes budget independently, including for the full duration of an `async = true` handler (AD-3).
- A Plugin's Global Variable store lives independently of its actor's mailbox; `hexput-session` is the only caller of its `teardown()`, invoked before the actor drops, never implied by `Drop` (AD-4).
- `hexput-session` holds the single live copy of per-backend Config; nothing else snapshots it (AD-5).
- Every execution dispatches as an independent async task; the only permitted serialization is a Plugin's own opted-in `priority` ordering among its own handlers for one Event (AD-6).
- System Config discovery (CLI flag > env var > default path) is identical regardless of packaging (AD-7).
- No lock is ever held across an `.await` point.

**Domain vocabulary** (use these terms verbatim in code — see PRD §3 Glossary for full definitions): `Daemon`, `Backend`, `Session`, `Connection`, `Client ID`, `Config` (per-backend, never a file) vs. `System Config` (file-based, daemon-operational), `Registered Function`, `Capability`, `Script`, `Direct Execution`, `Cached Execution`, `AST Cache`, `Resource Budget`, `Plugin`, `Event`, `Global Variable`, `Global Variable Locking` (concurrency strategy) vs. `Global Variable Behavior` (lifetime strategy: `forever`/`ttl`/`separate_each_trigger`/`keyed`).

**Stack** (pinned versions in the spine's Stack table — verify current before bumping): Rust 1.98.1 (2024 edition), Tokio, `tokio-tungstenite`, `rustls`, `serde`/`rmp-serde` (MessagePack wire format), `dashmap`, `moka` (AST cache), `criterion` (benchmarking), `tracing`.

**Deliberately out of scope** — don't reintroduce without checking the brief/PRD first: OS-level sandboxing (permanent, not deferred), transactional/rollback RPC effects, master-slave horizontal scaling (future work, not precluded by the design).

## Repo Layout Notes

- `.claude/skills/` and `.agents/skills/` — every entry under `.claude/skills/` is a symlink into `.agents/skills/`; that's the real location. Don't duplicate a skill in both places.
- `_bmad/` — BMAD methodology tooling (scripts, skill customization). `_bmad-output/planning-artifacts/` — where the brief, PRD, and architecture spine above live, each with its own `.memlog.md` decision log.
