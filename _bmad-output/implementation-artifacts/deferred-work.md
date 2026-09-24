# Deferred Work

Append-only. Each entry is work identified during a build but deliberately not done in it.

- source_spec: `spec-1-1-project-scaffold-and-pinned-toolchain.md`
  status: RESOLVED 2026-09-18 — the Spine's graph gained `hexput-session --> hexput-globalvar`, the edge was added to `crates/hexput-session/Cargo.toml`, and `scripts/check-crate-graph.py` now asserts it as a REQUIRED_EDGE so it cannot silently disappear again. Resolved in favour of AD-4's normative text (session is the sole caller) over the derived graph, on the reasoning that the graph was mechanically derived later during the crate split and simply dropped the edge. If the intended mechanism was instead a store handle passed down from `hexput-daemon`, this is the decision to revisit.
  summary: The Architecture Spine's crate graph omits `hexput-session -> hexput-globalvar`, contradicting AD-4's rule that `hexput-session` is the only caller of `hexput-globalvar::teardown(plugin_id)`.
  evidence: AD-4's text names session's TTL-expiry and explicit-unregister paths as teardown's only callers, invoked before the Plugin actor drops. But the Spine's graph gives `hexput-session` edges to `hexput-port` and `hexput-shared` only, so it cannot call into globalvar at all; the only crate that can is `hexput-plugin` — exactly the caller AD-4 forbids. Story 1.1 transcribed AD-4 faithfully into `hexput-globalvar`'s doc comment, so the contradiction is now stated in code. Resolve before Epic 7 (Global Variables) by either adding the `session -> globalvar` edge to the Spine, or amending AD-4 to describe the mechanism that actually reaches teardown (e.g. a store handle passed down from `hexput-daemon`).

- source_spec: `spec-1-1-project-scaffold-and-pinned-toolchain.md`
  summary: CI runs on `ubuntu-latest` only, so the Windows-only Named Pipe transport adapter will never be compiled.
  evidence: AD-1 requires a Named Pipe adapter alongside UDS/TCP+TLS/WebSocket, and AD-7 implies per-OS System Config default paths. Neither is exercised by a Linux-only job. `hexput-transport` is an empty stub today so there is nothing to cross-compile yet; add a `strategy.matrix.os` when Epic 2 lands the adapters.

- source_spec: `spec-1-1-project-scaffold-and-pinned-toolchain.md`
  status: PARTIALLY RESOLVED 2026-09-23 by `spec-2-1-start-the-daemon-from-a-system-config-file.md` — feature sets are now hoisted into `[workspace.dependencies]` alongside versions (`tokio` gains `rt-multi-thread` + `signal`, `serde` gains `derive`), `tracing-subscriber 0.3.23` (`fmt`, `std`) and `toml 1.1.6` are pinned there and in the Spine's Stack table, and members write `workspace = true` only. `tokio`'s `net`, `io-util`, `macros`, `time` and `sync` features joined the pin with `spec-2-3-accept-connections-over-a-unix-domain-socket.md`. Still open: `rustls`'s crypto provider — add it to the same pin when Epic 5's TCP+TLS first consumes it.
  summary: `[workspace.dependencies]` pins bare versions with no feature sets, and omits `tracing-subscriber` which FR-12's structured logging will need.
  evidence: `tokio = "1.53.1"` with default features has no `rt-multi-thread`/`net`/`sync`/`macros`/`time`; `serde` lacks `derive`; `rustls` lacks a crypto provider. Hoisting versions only pays off if features are hoisted too, otherwise members re-add them locally and diverge. Deliberately not decided in Story 1.1: no crate consumes any of the nine yet, so the correct feature set per crate is not yet knowable. Decide when the first consumer lands (Epic 2).

- source_spec: `spec-1-1-project-scaffold-and-pinned-toolchain.md`
  status: RESOLVED 2026-09-23 by `spec-2-1-start-the-daemon-from-a-system-config-file.md` — the stub is gone. The resolver is the free function `hexput_config::resolve(flag: Option<&Path>, env: Option<&OsStr>, default_path: &Path) -> Result<Loaded, ConfigError>`; `hexput-daemon` feeds it the `clap`-parsed `--config`, the process's `HEXPUT_CONFIG` and `hexput_config::default_path()`.
  summary: `SystemConfig::resolve()` takes no arguments, so AD-7's "CLI flag > env var > default path" precedence has no channel for the CLI flag.
  evidence: The stub exists only so the `hexput-daemon` binary's call chain type-checks. Implementing AD-7 requires `resolve()` to accept the parsed `--config` flag and to report parse failure, e.g. `resolve(cli_flag: Option<&Path>) -> Result<Self, ConfigError>`. Settle the signature in the story that implements System Config discovery.

- source_spec: `spec-1-1-project-scaffold-and-pinned-toolchain.md`
  summary: AGENTS.md / CLAUDE.md "Project Status" still says the repo is pre-implementation with no `Cargo.toml` and no build commands, which is false as of this commit.
  evidence: That section explicitly instructs: "Once code exists, replace this whole section with real status ... and add build/lint/test commands to a new section below." This story created `Cargo.toml`, 22 crates, a CI workflow, and a crate-graph guard. The now-real commands are `cargo build --workspace --locked`, `cargo test --workspace --locked`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `python3 scripts/check-crate-graph.py`. Deferred only because build routes fixes that edit agent-context files to deferred work; this should be the next thing done.

- source_spec: `spec-1-2-tokenize-hexput-source.md`
  summary: The lexer materializes the entire source as `Vec<(usize, char)>` — roughly 16 bytes per character, about 16x the source size for ASCII.
  evidence: Chosen so arbitrary lookahead is total, but actual lookahead is bounded at 3 (`peek_at(1 + sign_width)` in `lex_number`). A `Peekable<CharIndices>` with a small buffer, or byte indexing since every lookahead target is ASCII, gets the same result. Deferred because rewriting the cursor touches every scanner and the story was already patched substantially; revisit when Epic 3 lands Resource Budget enforcement, which is also what should bound submitted script size (AD-3 puts that in `hexput-enforce`, not here).

- source_spec: `spec-1-2-tokenize-hexput-source.md`
  status: RESOLVED 2026-09-23 by `spec-2-2-frame-requests-and-responses-on-the-wire.md` decision 4 — no serde was added to the diagnostics types. The wire error shape is `hexput_port::ErrorBody`, with owned strings for severity, category, code and message plus an optional span map, built from a `Diagnostic` via `From<&Diagnostic>` and deserializable, so a round-tripped or Backend-side code is a plain string. `Code` stays a `&'static str` newtype for the language side.
  summary: `Code` wraps `&'static str` and no diagnostics type derives serde, but the module declares these as `hexput-port`'s error-response types over MessagePack.
  evidence: A `&'static str` newtype has no inbound representation, so a round-tripped or Backend-supplied code cannot be deserialized, and `hexput-shared` has no serde dependency at all. Settling this before `hexput-port` exists (it is still a stub) avoids forking a parallel wire type there. Likely shape: `Cow<'static, str>` or an interned form, plus feature-gated `Serialize`/`Deserialize`.

- source_spec: `spec-1-2-tokenize-hexput-source.md`
  summary: A numeric literal that underflows, e.g. `1e-999`, silently becomes `0.0` while one that overflows is rejected.
  evidence: LANGUAGE-REFERENCE §3 rejects infinity ("a rules engine that returns NaN has failed, not computed") but says nothing about underflow, so the lexer rejects one end of the range and not the other. Every mainstream language underflows silently, which is why this was not changed unilaterally — it is a language decision for §3, not a lexer bug. Decide whether §3 should name underflow, then make the lexer match.

- source_spec: `spec-1-3-parse-expressions-declarations-and-member-access.md`
  summary: Clarify Story 1.10's empty-versus-omitted callable-name list contract and align the Epic 1 context before implementing the checker.
  evidence: Story 1.10 calls the CLI list empty but separately exempts a caller supplying no list. The refreshed context distinguishes empty and omitted without specifying the CLI representation; a medium-impact future checker divergence is unverified while that crate remains a stub.

- source_spec: `spec-1-6-evaluate-expressions-and-variable-scope.md`
  summary: Reference cycles between `Arc`-shared values and scopes are never freed, so a self-containing collection (`let a = []; a[0] = a;`) leaks for the life of the process, and Story 1.7 closures will make such cycles routine.
  evidence: `Value::Array`/`Value::Object` and `Scope` are `Arc`-linked with no cycle collection (crates/hexput-interpreter/src/value.rs, environment.rs). In 1.6 only an explicit self-reference triggers it, but in 1.7 every named function bound in the scope it captures forms a scope → function → scope cycle, so each Script execution in the long-running Daemon would leak its root scope. Decide the memory model (per-execution arena/heap owned by the machine, a cycle collector, or explicit teardown of the root scope at execution end) in Story 1.7, before closures land, and make Story 3.5's memory budget consistent with it.
  status: RESOLVED 2026-09-19 by `spec-interpreter-per-execution-arena.md` — every execution's collections and scopes now live in one heap owned by the `Machine`, runtime values are handles into it, and the whole heap (cycles included) is dropped when the execution ends by result or by error. The Script result is detached into owned values first; returning a cyclic value is `type.cyclic_result`. Block scopes are reclaimed on exit; Story 1.7 must mark a scope captured before a closure references it so the reclaim skips it. Story 3.5's memory budget should bound heap growth within one execution, since nothing is collected until it ends.

- source_spec: `spec-interpreter-per-execution-arena.md`
  summary: A Script result's logical size can be exponential in its heap size (`let a = []; a = [a, a];` repeated 40 times returns 41 collections but 2^40 logical nodes), so any consumer that walks or serializes it — MessagePack encoding, `to_vec`, `entries` — can be made to do ~10^12 work.
  evidence: Shared acyclic structure is legitimate language behavior (it existed identically under the pre-arena `Arc` model) and `detach` keeps it linear in memory, but walkers see the full tree. Bound it where output is produced: Story 3.6's output-size Resource Budget should count logical nodes during detach or serialization and fail with a `budget` error, rather than letting the Daemon serialize an exponential tree.

- source_spec: `spec-interpreter-per-execution-arena.md`
  summary: Nothing is garbage-collected within an execution, so a long loop that allocates a temporary collection every iteration grows the heap until the execution ends.
  evidence: By design of the per-execution arena (only block scopes are reclaimed eagerly). Harmless for short rules, but once Story 1.7 adds loops a `while` allocating `[]` per turn grows linearly with iterations. Story 3.5's memory budget must count heap slots (including garbage) so this terminates with a `budget` error; if real workloads hit it, add a mark-sweep pass over the heap from the value stack and live scopes.

- source_spec: `spec-1-7-execute-control-flow-functions-and-callbacks.md`
  summary: A Script cannot return a function; the intended replacement is a re-triggerable callable handle the Backend can invoke later over the socket.
  evidence: Story 1.7 decision 1 makes returning a function, or a value containing one, a `type` error (`type.function_result`) because a function closes over an execution-scoped environment and has no wire representation. The long-term direction recorded with that decision is a frozen closure the Backend receives as a handle and invokes later, rebound to its outer context and Global Variables — which needs a Session-scoped owner for the frozen environment, a wire identity, a lifetime/eviction rule, and an interaction with Global Variable Behavior (FR-25), none of which exist yet. Deliberately out of Epic 1. `Value` is `#[non_exhaustive]` and the error is a refusal rather than a representation, so widening it into a value later is not a breaking change. Revisit alongside Epic 4's Plugin work, where the Session-scoped lifetime it needs already exists.

- source_spec: `spec-1-7-execute-control-flow-functions-and-callbacks.md`
  summary: A loop body that creates any function value pins one scope (and one function slot) per iteration for the rest of the execution, even when the function is never referenced.
  evidence: `Machine::open` runs on every block entry, so a named `fn` inside a loop body is re-created each iteration, and `make_function` marks the defining scope and its ancestors captured, which makes `release_scope` skip them. Heap growth is therefore linear in iterations with no reclaim. The marking is deliberately conservative — it records that a function value was created, not that one escaped — because proving escape needs reachability analysis the interpreter does not do. Same class as the already-recorded "no garbage collection within an execution" entry: Story 3.5's memory budget must count these slots so such a loop terminates with a `budget` error. If real workloads hit it, the narrower fixes are escape analysis at function-value creation, or hoisting a loop-invariant function definition out of the iteration.

- source_spec: `spec-1-7-execute-control-flow-functions-and-callbacks.md`
  summary: Regenerating `epic-1-context.md` dropped four constraints it was the only carrier of, including the empty-versus-omitted callable-name-list distinction Story 1.10 depends on.
  evidence: This run recompiled the epic context because planning artifacts were newer than the cached file, and the new text replaced the Requirements, Technical Decisions, and Cross-Story sections wholesale. Lost: that a *missing* callable-name list suppresses the unknown-call finding while an *empty* one does not (already an open deferred-work entry from Story 1.3, and the exact contract Story 1.10 implements); the precedence rule that the reference's specific truthiness and absent-data rules beat its summary tables; the pinned toolchain (Rust 1.98.1, edition 2024); and that Story 1.9 binds CLI-supplied starting variables. The file is an agent-context file, so the fix is routed here rather than patched inside this story. Restore these before Story 1.9 or 1.10 plans against the context, or fold them into `compile-epic-context`'s source material so a future regeneration keeps them.

- source_spec: `spec-1-8-report-errors-with-precise-source-locations.md`
  summary: Regenerating `epic-1-context.md` again reversed one recorded contract and dropped three others.
  evidence: This run's step-1 recompiled the epic context because `LANGUAGE-REFERENCE.md` was newer than the cached file, and the four constraints the previous run lost (recorded in the Story 1.7 entry above) were restored by hand afterwards. The rewrite still changed the empty-versus-missing callable-name-list wording to the opposite of the old text — verified against `_bmad-output/planning-artifacts/epics.md`, where the *new* wording is the correct one, so that part is a correction and the Story 1.3 entry above can be closed against it. Three constraints the old file was the only carrier of are still gone: that a function may not be stored in a Global Variable, that the interpreter must grow no host-call path, and that the check pass has three modes (`off`/`warn`/`error`). The real defect is the regeneration itself: `compile-epic-context` rebuilds the file wholesale from planning artifacts that do not contain these, so every regeneration will drop them again. Fold them into the source material, or stop regenerating a file that has been hand-corrected twice.

- source_spec: `spec-1-8-report-errors-with-precise-source-locations.md`
  summary: Nothing mechanically ties the `Code` constants to the three producer sweeps, so a future code with a wrong span ships green.
  evidence: Story 1.8's sweeps in `crates/hexput-tests/tests/{lexer,parser,interpreter}.rs` hard-code arities of 7, 5 and 15, and `tests/shared.rs` is itself a hand-maintained enumeration; all 27 codes are covered today, but a 28th added later is covered by nothing and asserted by nothing. The fix is a `pub const ALL: &[Code]` in `hexput-shared` that `shared.rs` and the three sweeps assert coverage against. Deliberately not done inside Story 1.8, whose deliverable is the rendering; do it when the next code is added — Epic 2's `capability` and Epic 3's `budget` codes are the first candidates.

- source_spec: `spec-1-8-report-errors-with-precise-source-locations.md`
  summary: `Code::UNTERMINATED_STRING`'s doc comment says a raw newline ends a string literal, but the lexer legally allows a string to span lines.
  evidence: `let x = "a\nb";` parses clean, and Story 1.8's own multi-line rendering test depends on that — the epic context states that string literals legally span lines and that their spans must keep full location information. The doc comment in `crates/hexput-shared/src/diagnostics.rs` is stale from Story 1.2 and now contradicts the behaviour it describes. Not caused by Story 1.8; fix it alongside the next change to the lexer's string scanning, and check §3 of the reference says the same thing.

- source_spec: `spec-1-9-evaluate-a-script-from-the-command-line.md`
  summary: A Script result with shared subtrees now hangs the CLI: printing it is exponential in the result's heap size, and `Heap::attach` inverts the same shape on the way in.
  evidence: `let a = [1]; let i = 0; while (i < 30) { a = [a, a]; i = i + 1; }; return a;` runs to completion and then hangs `hexput eval` past a 10-second timeout with no output. The property belongs to the result, not the printer: `spec-interpreter-per-execution-arena.md` already recorded that a result's logical size can be exponential in its heap size and that any walker — `to_vec`, `entries`, MessagePack encoding — can be made to do ~10^12 work, routing the bound to Story 3.6's output-size Resource Budget. Story 1.9 adds two more walkers of that shape: `print::literal` on the way out (reachable from the CLI today, as above) and `Heap::attach` on the way in (not reachable from a single `--var` expression, which cannot build sharing, but open to any library caller of `evaluate_with_variables`). Fix them together with Story 3.6 by counting logical nodes and failing with a `budget` error; a memo table keyed on `Arc::as_ptr` would also collapse both walks to linear if the shared structure should print as a tree rather than fail.

- source_spec: `spec-1-9-evaluate-a-script-from-the-command-line.md`
  summary: Regenerating `epic-1-context.md` dropped the explicit repository path of the normative language reference.
  evidence: The Requirements bullet now says "the language definition under the planning artifacts' `language/` directory" where it previously named `_bmad-output/planning-artifacts/language/LANGUAGE-REFERENCE.md` outright, so an agent reading the context has to guess which file is normative. Caused by this run's step-1 regeneration, not by the implementation; the fix edits an agent-context file, and the underlying defect is the one already recorded twice above — `compile-epic-context` rebuilds the file wholesale and drops whatever the planning artifacts do not spell out.

- source_spec: `spec-1-10-check-a-script-without-running-it.md`
  status: RESOLVED 2026-09-22 — the empty-versus-omitted contract is now implemented and tested, not merely described. `hexput_check::Environment::with_callables` supplies a list (an empty iterator supplies an *empty* list); never calling it supplies none, which suppresses `capability.unknown_function` entirely. `hexput check` supplies no list unless `--callable` is given. This closes the Story 1.3 entry above.
  summary: Story 1.3's deferred entry asked that the empty-versus-omitted callable-name-list contract be settled before the checker was written.
  evidence: Settled as Story 1.10 decision 1 and asserted by `an_empty_list_is_not_the_same_as_no_list` in `crates/hexput-tests/tests/check.rs`.

- source_spec: `spec-1-10-check-a-script-without-running-it.md`
  status: PARTIALLY RESOLVED 2026-09-22 — `Code::ALL` now enumerates all 31 codes, and `tests/shared.rs`'s `every_code_is_enumerated_exactly_once` asserts it against the file's own hand-written list, rejects duplicates, and checks each code's `category.name` shape against the §7 category set. That catches *half*-updating — a code added to one list and not the other — but not forgetting entirely: both sides are still hand-maintained, so a `pub const` added to neither list is asserted by nothing. Closing it needs a macro that declares the constants and `ALL` together. Raised by the review of Story 1.10, which had overstated the guard.
  summary: Story 1.8's deferred entry routed `pub const ALL: &[Code]` to the next change that adds a code; this story added four.
  evidence: `crates/hexput-shared/src/diagnostics.rs` and `crates/hexput-tests/tests/shared.rs`.

- source_spec: `spec-1-10-check-a-script-without-running-it.md`
  summary: The check pass reports literal-operand type errors only when *every* operand is a literal, so `[1] * x` and `for (item in 5)` go unreported although both are decidable.
  evidence: Deliberate, for one reason: a finding must carry the runtime's message verbatim, and that message names *both* operand types (`cannot apply `*` to array and number: …`), which a half-known pair cannot produce. Several cases need only one side — an array or object operand of `-`/`*`/`/`/`%` fails whatever the other side holds, and `for … in` over a non-collection literal is `type.operand_mismatch` however the loop body reads. Widening this means either a second message wording for the half-known case (which breaks "a finding reads exactly like the error") or teaching the interpreter to phrase a one-sided mismatch. Decide which when the language server (Epic 9) makes the missing findings visible to an author as they type.

- source_spec: `spec-1-10-check-a-script-without-running-it.md`
  summary: The `hexput check` command has no way to exercise the FR-3 construct toggles, so the `policy.construct_disabled` finding is reachable only from a library caller.
  evidence: `Policy`'s six toggles are implemented and tested at the library level, but the CLI always passes `Policy::new()` — every construct enabled. Story 3.9 owns the toggles as a Backend-configured surface, and inventing a CLI spelling for them now would fix an interface that story has not designed yet. Revisit in Epic 3: if the toggles get a config-file form there, the check command should read the same form rather than growing six ad-hoc flags.

- source_spec: `spec-1-10-check-a-script-without-running-it.md`
  summary: Literal-operand failures other than a type error — `2 / 0`, `2 % 0`, `1e308 * 10` — are equally decidable from the AST and are not reported.
  evidence: LANGUAGE-REFERENCE §10 scopes the pass to "type errors between literal operands", so the silence is in scope rather than a bug, and the interpreter cross-check in `tests/check.rs` deliberately compares `type.operand_mismatch` alone. But an author writing `2 / 0` gets no warning from a pass that exists to catch exactly that class of mistake before execution. Reporting them means new codes in the `arithmetic` category from a static pass and a decision about whether constant folding — which §4 makes raise — is something the check may do at all; the Boundaries of this story forbade it. Decide alongside the language server (Epic 9), where the missing findings become visible to an author as they type.

- source_spec: `spec-1-10-check-a-script-without-running-it.md`
  summary: `hexput-check`'s `operands.rs` and `hexput-interpreter`'s `convert.rs` hold two byte-for-byte copies of the §4.3 string-to-number rule, and AD-8 forbids the edge that would let them share one.
  evidence: Deliberate and accepted for Epic 1: the cross-check sweep in `tests/check.rs` now compares all 2,700+ operator/operand combinations — including the strings that reach the trailing-garbage guard, and a function literal — against `hexput_interpreter::evaluate`, so a divergence fails a test rather than shipping. The structural fix, if the duplication grows past this one rule, is to move the pure conversion predicates into `hexput-ast` (which both crates already depend on) rather than to weaken AD-8. Revisit when Epic 6 extends the language, since every new conversion rule has to be written twice until then.

- source_spec: `spec-2-1-start-the-daemon-from-a-system-config-file.md`
  summary: System Config has no Named Pipe transport section, although AD-1 lists a Named Pipe adapter.
  evidence: Story 2.1's spec enumerates exactly three sections (`uds`, `tcp`, `websocket`), so `[transport.pipe]` is rejected today as an unknown key. When Epic 5 adds the Named Pipe adapter it needs a `[transport.named_pipe]` (or similar) section with its pipe name, and a decision on whether a Windows-only section is an error or ignored on Unix.

- source_spec: `spec-2-1-start-the-daemon-from-a-system-config-file.md`
  summary: A missing field inside a transport section is located at the section header, not at a line of its own, and the message names the field but not the section.
  evidence: The TOML deserializer reports `missing field `tls_cert`` with the span of the enclosing table, so the error reads `config.toml:4:1: missing field `tls_cert`` where line 4 is `[transport.tcp]`. The file, line and field are all named, which meets the story, but with two transports missing the same field the section is only implied by the line. If operators find this ambiguous, qualify the message with the dotted table path (the second validation layer in `crates/hexput-config/src/file.rs` already does this for invalid values).

- source_spec: `spec-2-1-start-the-daemon-from-a-system-config-file.md`
  status: MITIGATED 2026-09-23 by `spec-2-3-accept-connections-over-a-unix-domain-socket.md` — the socket such an early signal leaves behind is exactly the stale socket the UDS adapter now removes on the next start, so the only remaining effect is a non-zero exit status for a Daemon killed during its first milliseconds.
  summary: A signal arriving between process start and the first poll of the shutdown future meets the default disposition and kills the Daemon without a clean exit.
  evidence: `hexput-daemon` installs its SIGINT/SIGTERM handlers on the shutdown future's first poll, which happens before "waiting for a shutdown signal" is logged, so any signal sent after that line is caught. A signal sent in the few milliseconds before it still ends the process with the signal's status instead of `0`. Harmless while there is nothing to clean up; once Story 2.3 creates a socket file, an early signal leaves it behind — which the UDS adapter's stale-socket removal already has to handle.

- source_spec: `spec-2-1-start-the-daemon-from-a-system-config-file.md`
  summary: The Windows System Config default path (`%ProgramData%\hexput\config.toml`) and the Ctrl-C shutdown path are never compiled or run in CI, and Windows console close/logoff/shutdown events are not listened for.
  evidence: `default_path()`'s `#[cfg(windows)]` branch and `shutdown_signal`'s `#[cfg(not(unix))]` branch are unexercised by the Linux-only CI; only `tokio::signal::ctrl_c()` is awaited, so `ctrl_close`/`ctrl_logoff`/`ctrl_shutdown` end the process without the clean path. Add a `#[cfg(windows)]` default-path assertion and the extra handlers when the CI OS matrix lands with the Named Pipe adapter.

- source_spec: `spec-2-1-start-the-daemon-from-a-system-config-file.md`
  summary: `hexput-config`'s `locate` slices `&text[..offset]` on a toml error span offset, which would panic if a span ever landed inside a multi-byte character (unverified, would be medium).
  evidence: Six malformed non-ASCII inputs probed through the binary produced correct char-based locations and no panic, so spans appear char-aligned. Settle by confirming toml's error spans are always char boundaries, or harden with `str::floor_char_boundary`.

- source_spec: `spec-2-2-frame-requests-and-responses-on-the-wire.md`
  status: RESOLVED 2026-09-23 by `spec-2-6-run-a-one-shot-script-and-get-the-result-back.md` — a reply the adapter cannot frame (`InvalidInput` from `send`) is replaced by `protocol.response_too_large` carrying the request id, and a result certain to exceed the frame is refused as that code before it is converted.
  summary: `hexput_port::encode` has no size check, so a Script result whose envelope encodes past `MAX_FRAME_LEN` has no defined response — only `encode_frame` refuses it.
  evidence: No result producer exists until Story 2.6. That story must turn an `EncodeError::FrameTooLarge` on a result into an `Error` response carrying the request's id (a new non-fatal code), never a dropped response that leaves the Backend's request pending forever.

- source_spec: `spec-2-2-frame-requests-and-responses-on-the-wire.md`
  summary: A decoded frame's `rmpv::Value` tree amplifies memory roughly 30x — a 16 MiB frame of 1-byte values (nils, empty arrays) becomes about 0.5 GiB of `Value`s, per frame and per connection, reachable pre-init.
  evidence: Each `rmpv::Value` is about 32 bytes. The codec never allocates for a length a header merely claims, but the actual decoded size is unbounded. Fix candidates: a total decoded-element budget in phase 1, a lower `MAX_FRAME_LEN`, or a per-connection memory bound. Belongs with Resource Budget work (Epic 3) and must be settled before TCP+TLS (Epic 5) exposes the daemon beyond UDS permissions.

- source_spec: `spec-2-2-frame-requests-and-responses-on-the-wire.md`
  status: RESOLVED 2026-09-23 by `spec-2-6-run-a-one-shot-script-and-get-the-result-back.md` — `hexput-script` walks a result iteratively and refuses one nested past `MAX_RESULT_DEPTH` (`MAX_NESTING_DEPTH - 2`, for the envelope and `{value}` maps) as `protocol.result_too_deep` before any recursive conversion or encoding.
  summary: `hexput_port::encode` recurses once per nesting level with no depth bound, and the interpreter builds arbitrarily deep values without host recursion — a Script returning a deeply nested array can overflow the daemon's stack when its result is converted and serialized.
  evidence: e.g. repeatedly wrapping `a = [a]` in a loop then returning `a`. Story 2.6's value-to-`rmpv::Value` conversion and `encode` are the first recursive consumers of a Script result. 2.6 must reject a result nested deeper than `MAX_NESTING_DEPTH - 1` (the depth the daemon's own decoder accepts) with a defined error before converting it, using an iterative walk.

- source_spec: `spec-2-3-accept-connections-over-a-unix-domain-socket.md`
  status: RESOLVED 2026-09-23 by `spec-2-6-run-a-one-shot-script-and-get-the-result-back.md` — `ProtocolCode::NotImplemented` is gone from the enum, `ALL` and the stability test.
  summary: `protocol.not_implemented` is temporary. Story 2.4 serves `Init`, so its only remaining use is `ExecutionStart` on an initialized connection; Story 2.6 must remove the code from `ProtocolCode` (and `ALL`, and the stability test) when it serves execution.
  evidence: Decision 1 of Story 2.3 pulled the init gate forward before init existed; Decision 3 of Stories 2.4 + 2.5 (`spec-2-4-complete-the-init-handshake-with-inline-config-and-registrations.md`) moved the removal from 2.4 to 2.6.

- source_spec: `spec-2-3-accept-connections-over-a-unix-domain-socket.md`
  summary: The Daemon has no bound on concurrently open connections; each accepted connection is a task holding up to one frame (16 MiB) of buffered input.
  evidence: NFR4 is met (one connection cannot disturb another's correctness), but a local client opening thousands of connections grows memory without limit. UDS access is gated by the socket's mode, so this is an operator-trust issue today; it becomes real with Epic 5's network transports and belongs with Resource Budget work (Epic 3) or a System Config `max_connections`.

- source_spec: `_bmad-output/implementation-artifacts/spec-2-3-accept-connections-over-a-unix-domain-socket.md`
  summary: A peer that keeps its connection open but stops reading blocks that connection's `write_all` forever; nothing times the write out.
  evidence: `UdsOutbound::send` awaits `write_all` with no deadline, so the task and its buffers live until shutdown. Isolation holds (only that connection stalls), but together with the uncapped connection count it lets a local peer pin memory and file descriptors. Settle with a write timeout or per-connection budget alongside the connection cap (Epic 3 / before Epic 5's network transports).

- source_spec: `_bmad-output/implementation-artifacts/spec-2-3-accept-connections-over-a-unix-domain-socket.md`
  summary: The cleanup of the temporary socket and private directory when `chmod` or the link fails is never exercised by a test.
  evidence: Placement can only fail after `clear_stale` passed through a race or an OS fault; covering it needs a fault-injection seam in `hexput-transport::uds::bind_with_mode`.

- source_spec: `spec-2-4-complete-the-init-handshake-with-inline-config-and-registrations.md`
  summary: A panic inside `hexput_connection::serve` (or the CSPRNG `expect` in `Sessions::create`, after which nothing is attached) skips the explicit detach, so a panicking attached connection leaks its Session until the Daemon stops.
  evidence: Detach is an explicit call on `serve`'s one exit path rather than a `Drop` guard, by design (AD-4: teardown never implied by `Drop`). Nothing in `serve` is known to panic on peer input. Settle when Story 5.8's TTL lands: a Session with no attached Connection whose TTL has expired is torn down anyway, or a supervisor that detaches on a joined panic.

- source_spec: `spec-2-4-complete-the-init-handshake-with-inline-config-and-registrations.md`
  summary: The Session registry is unbounded — every accepted connection may create one Session holding its registrations (up to one 16 MiB frame's worth) for as long as it stays connected.
  evidence: Same exposure as the uncapped connection count noted for Story 2.3, now with per-Session state; settle together with that cap (Epic 3 / before Epic 5's network transports).

- source_spec: `spec-2-4-complete-the-init-handshake-with-inline-config-and-registrations.md`
  summary: `Sessions::create`'s Client ID collision retry (and its CSPRNG-failure panic) has no test, because the id source cannot be injected.
  evidence: Review finding (verification-gap layer): replacing the vacant-entry loop with an overwriting `insert` passes every test, since 128-bit random draws never collide in a test; closing it needs a test seam such as `create_with(init, id_source)`, best added with Epic 5's reconnect when Client IDs carry authority.

- source_spec: `spec-2-6-run-a-one-shot-script-and-get-the-result-back.md`
  summary: A Script that never ends (`while (true) {}`) pins a blocking-pool thread forever, keeps its connection and Session alive until shutdown, and hangs Daemon shutdown.
  evidence: Since Story 2.7 (`spec-2-7-keep-slow-executions-from-blocking-anything-else.md`) Direct Execution runs through `spawn_blocking`, so it no longer pins a runtime worker — other connections and this connection's reads are unaffected. But a blocking task cannot be aborted once started, and no Resource Budget exists yet: a clean close waits for it (Story 2.7 decision 2), and at shutdown Tokio's `Runtime` drop waits indefinitely for running blocking tasks, so the Daemon never exits. Settle with a step/time budget checked inside the evaluator loop (Epic 3, Story 3.5) so a runaway Script ends with a defined error; until then `Runtime::shutdown_timeout` in `hexput-daemon` would at least let the process exit.

- source_spec: `spec-2-6-run-a-one-shot-script-and-get-the-result-back.md`
  summary: A Script result is bounded in wire bytes, not memory — about 16M one-byte scalars pass `check_result` and become an `rmpv::Value` tree of 512 MiB or more before encoding.
  evidence: `check_result` counts one byte per scalar against `MAX_FRAME_LEN`; each `rmpv::Value` is about 32 bytes, so `to_wire` amplifies ~30x on the connection task. The outbound twin of the 2.2 decoded-size entry; belongs with Resource Budget work (Epic 3), e.g. an element-count bound in the same walk or a streaming encoder.

- source_spec: `spec-2-6-run-a-one-shot-script-and-get-the-result-back.md`
  summary: AD-3's "every Daemon evaluation goes through `hexput_exec::execute`" is a convention in `hexput-script`, not a graph rule — the Spine's `script --> interp` edge lets it call `evaluate*` directly.
  evidence: Nothing but review stops Epic 4's Cached Execution from calling `hexput_interpreter::evaluate_with_variables` and skipping Epic 3's enforcement. Closing it means `hexput-exec` re-exporting the value and diagnostic types `hexput-script` needs, dropping the `script --> interp` edge in a dated Spine amendment, and pinning it in `check-crate-graph.py` — best done when Epic 3 puts real enforcement behind the Executor.

- source_spec: `spec-2-7-keep-slow-executions-from-blocking-anything-else.md`
  summary: A connection may have any number of Direct Executions in flight; nothing caps them, per connection or Daemon-wide.
  evidence: By the spec's decision (no cap until Epic 3). Each in-flight execution holds its payload (up to one 16 MiB frame) and occupies one blocking-pool thread while it runs; Tokio's blocking pool defaults to 512 threads, beyond which executions queue behind each other Daemon-wide — the head-of-line blocking AD-6 forbids, reintroduced at scale. One local peer pipelining many slow Scripts can therefore exhaust the pool for every connection. Settle with a per-connection and/or per-Session in-flight cap refused with a defined error, alongside the connection cap and Resource Budgets (Epic 3), before Epic 5's network transports.

- source_spec: `spec-2-7-keep-slow-executions-from-blocking-anything-else.md`
  summary: Finished Direct Execution replies accumulate in a connection's `JoinSet` while its loop is blocked writing to a peer that reads slowly, each holding its whole result `Value`.
  evidence: Review finding (blind layer). Before Story 2.7 at most one result per connection existed at a time; now every in-flight execution can finish and park its reply, multiplying the 2.6 entry's ~512 MiB-per-result amplification by the uncapped in-flight count. Settle with the in-flight cap (same entry above) and the write timeout deferred from Story 2.3.

- source_spec: `spec-2-7-keep-slow-executions-from-blocking-anything-else.md`
  summary: A panicking Direct Execution task is logged and its request goes unanswered, and neither behaviour has a test.
  evidence: Review findings (blind, edge-case, verification-gap layers). The `JoinError` carries no correlation id, and no `protocol.*` code exists for an internal failure; changing `continue` to `return` in `Finished(Err)` passes every test because nothing can make `direct_execution` panic. Settle with a panic-injection seam (e.g. a `#[cfg(test)]` executor hook inside `hexput-connection`) and, if a Backend should be told, `join_next_with_id` or `catch_unwind` plus an internal-error code — best with Epic 3's enforcement, which adds the first code paths that could plausibly panic.

- source_spec: `spec-2-8-trace-every-request-back-to-its-client-id.md`
  summary: A panicked Direct Execution task is logged in its connection's span only, without the request `id` span field.
  evidence: Review finding (edge-case layer). The `JoinError` is observed in the loop after the task's request span is gone. Settle together with the 2.7 panic entry above, e.g. `JoinSet::join_next_with_id` mapping task ids to request spans, or `catch_unwind` inside the blocking closure so the panic is logged in the request span.

- source_spec: `spec-2-8-trace-every-request-back-to-its-client-id.md`
  summary: Debug-level Daemon tests asserting on log events may be sensitive to `tracing`'s process-wide callsite-interest cache (unverified, would be medium).
  evidence: Review finding (edge-case layer). `tests/connection.rs` showed ~30% flake before a discarding subscriber was installed; `tests/daemon.rs` passed 40 consecutive runs and every `serve` there runs under a Daemon dispatcher. Settle by running the daemon binary repeatedly under `--test-threads` stress in CI; if it flakes, apply the same discarding-dispatch approach or split the Story 2.8 tests into their own binary.

- source_spec: `spec-3-1-call-a-registered-host-function-from-a-script.md`
  status: promoted 2026-09-24 to FR-27/FR-28 and Stories 3.11–3.13 (sprint-change-proposal-2026-09-24.md); kept for provenance.
  summary: Backend-registered methods bound to objects — `registerMethod(objKey, fn)`, an object's hidden `__secret` (`__secret.key` selecting the method set, `__secret.ref` a Backend-supplied or auto-generated reference id), and modifications to referenced objects/strings reported back when a `Call` finishes.
  evidence: Split from Story 3.1 by Erdem (2026-09-24, decision 4); a new language feature absent from the PRD, epics and LANGUAGE-REFERENCE, so it needs `bmad-correct-course`. Decided semantics to carry over: a method `Call` sends the receiver object itself (adds a `receiver` key to the `Call` payload); argument/receiver nesting is capped by a runtime limit, default 12; `__secret` always travels to the Backend and may be edited there, but is untouchable inside Hexput — reading `o.__secret` silently yields `null`, a Script-written `__secret` key is silently ignored, and `for … in` skips it; strings likely become a custom `HexputString` so they can carry a reference id.

- source_spec: `spec-3-1-call-a-registered-host-function-from-a-script.md`
  status: PARTIALLY RESOLVED 2026-09-24 by `spec-3-2-grant-a-function-blanket-access-at-registration.md` decision 3 — `Caller::call` is now `Caller::dispatch_authorized`, called only by `hexput-exec` after `hexput_enforce::Capabilities::check_call` allowed the call, and `scripts/check-crate-graph.py` (`RESTRICTED_NAMES`) fails CI when `dispatch_authorized` appears in any production crate but `hexput-exec` and `hexput-rpc`, or `MessageType::Call` in any but `hexput-rpc`, `hexput-shared` and `hexput-port`. That is a source-text guard over those two names, not a compiler-enforced one: `hexput-connection` and `hexput-script` still hold a callable `Caller` (and the connection the `Outbound` half), and a renamed re-export or macro would evade the scan. Still open: a sealed token that only `hexput-exec` can construct after `check_call`, so the compiler enforces the rule.
  summary: `hexput_rpc::Caller::call` is public, so `hexput-connection` (which creates the `Caller`) and `hexput-script` (which receives it through `hexput-exec`'s re-export) could send a `Call` to the Backend without `hexput-enforce`'s capability check — AD-3's "no path reaches a Registered Function outside the Executor" is now a convention, not a graph rule.
  evidence: Review finding (blind layer, medium). Before Story 3.1 only `hexput-exec` depended on `hexput-rpc`; the approved `hexput-connection → hexput-rpc` edge (decision 3) and the `Caller` re-export put a callable handle in two crates that must not call it. Closing it needs a sealed grant — e.g. `Caller::call` taking a token only `hexput-exec` can construct after `check_call`, or `hexput-exec` owning the `Caller` wrapper and exposing only an opaque handle — best done with Story 3.2/3.3, when grants make the check richer.

- source_spec: `spec-3-2-grant-a-function-blanket-access-at-registration.md`
  summary: `scripts/check-crate-graph.py` has no tests: its `RESTRICTED_NAMES` guard (and every older rule) could be silenced by a typo, a widened allow-set or a dropped `failures.extend(...)` and CI would still print "OK".
  evidence: Review finding (verification-gap layer, medium). Verified by hand only (planting `dispatch_authorized` in `hexput-script` fails the script). Settle with a small harness that calls `restricted_name_uses`/the edge checks on synthetic metadata pointing at a temporary crate tree and asserts a failure comes back.

- source_spec: `spec-3-2-grant-a-function-blanket-access-at-registration.md`
  summary: AGENTS.md's Epic 3 status sentence says "Stories 3.1 and 3.2 are implemented (host calls; it supersedes …)" — "it" and "host calls" no longer fit two stories, and the paragraph carries rename history ("renamed from `call`") instead of only current names.
  evidence: Review finding (blind layer, low); routed to defer because the fix edits an agent-context file. Tidy at the next AGENTS.md status update.
