---
title: 'Story 4.5: Compare the hot path against Rhai'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'a64a73a269371fb1ebf600d9b3396a0537793f8e'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-4-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-4-measure-the-cache-win.md'
warnings: ['oversized']
deferred:
  - summary: >-
      The Story 4.3 test `a_removed_handle_is_unknown_and_its_program_is_released_at_once` failed once in a full release run and passed on five reruns.
    evidence: |-
      Seen during this review's verification (`cargo test --workspace --locked --release`): tests/script.rs:2136 asserted that the program's weak reference was gone right after `CodeUnregister`. The test and `AstCache` are untouched by Story 4.5; the likely cause is moka's maintenance timing under load. A stress loop over the test, or an `AstCache` removal that runs `run_pending_tasks` before returning in every path, would settle it.
    location: >-
      crates/hexput-tests/tests/script.rs:2136
    severity: medium (unverified)
---

<intent-contract>

## Intent

**Problem:** The project's performance claim (SM-1, NFR3) needs a like-for-like comparator, and Rhai is the named one. Story 4.4's `cache` bench measures Hexput alone. Nothing runs the same workload on Rhai, and nothing reports what the Resource Budget costs as a separate line.

**Approach:** Extend the `cache` bench in `hexput-tests` with a comparator suite:
- **Rhai over UDS.** A small Rhai server inside the bench speaks the same wire as the Daemon (same framing, `CodeRegister`, `CachedExecutionStart`, `Call` round trips). The same Backend client, the same students and the same verification drive both servers, and both sets of figures are reported side by side.
- **Engine section.** An in-process section reports the budget-enforcement overhead as its own line item, next to Rhai's in-process engine.

## Boundaries & Constraints

**Always:**
- **Rhai version.** Pin `rhai = "1.26.1"` in `[workspace.dependencies]`, with its default features, and add it as a `hexput-tests` **dev-dependency only**. 1.27.0 is one day old, so 1.26.1 is the latest release that has been out for more than a month.
- **The Rhai rule** is the `RULE` workload in Rhai syntax, as constant `RHAI_RULE`. Every number crosses as `f64` (Rhai `FLOAT`), so the arithmetic is the same f64 operations in the same order as Hexput's:
  ```
  let total = 0.0; let weights = 0.0;
  for grade in grades { let w = weight(grade.course); total = total + grade.score * w; weights = weights + w; }
  let passed = total / weights >= passMark(student.program);
  record(student.id, passed);
  passed
  ```
- **Rhai server** (module `rhai`, in `benches/cache/rhai.rs`):
  - It runs on a `std::os::unix::net::UnixListener` in a fresh temp dir, with one thread per connection, one `rhai::Engine::new()` per connection, and default engine limits.
  - It is framed exactly as the bench's `wire::Client` frames (`hexput_port::{encode, encode_frame, decode}`, the length prefix capped at `MAX_FRAME_LEN`) and answers:
    - `Init` → `Result {client_id: <32 hex>}`;
    - `CodeRegister {source}` → compiles the source **once** into an `AST` kept per connection, then `Result {handle: <16 hex>}`;
    - `CachedExecutionStart {handle, variables}` → converts the variables (integer/float → `FLOAT`, string, bool, nil → `()`, array, map) into a fresh `Scope`, runs `eval_ast_with_scope`, and answers `Result {value}`;
    - `CodeUnregister {handle}` → `Result {}`;
    - anything else is a panic.
  - `weight`, `passMark` and `record` are registered Rhai functions. Each one writes a `Call {name, arguments, execution}` with its own id counter on the same stream, blocks for that id's `Result {value}`, and converts the value back.
  - `RhaiServer::{start, connect, stop}` mirror `daemon::Daemon`. `stop` ends the accept thread without hanging and removes the temp dir.
- **One client for both servers.**
  - `workload::register` gains a source parameter (or a sibling `register_source`): Hexput sends `RULE` and Rhai sends `RHAI_RULE`.
  - Everything else is reused unchanged on both sides: `init`, `cached_payload`, `run_cached` (verdict and exactly 10 `Call`s checked) and `unregister`.
  - `steady::measure` is generalized from `&Daemon` to a connect closure plus the source to register. Hexput's steady state stays byte-for-byte the same measurement.
- **Criterion group `rhai_compare`.** It uses `noise_threshold(report::VARIANCE.p50)`, one connection, one execution in flight, and the same student rotation. It has four figures: `hexput_cold_first_run`, `rhai_cold_first_run`, `hexput_warm_cache_hit` and `rhai_warm_cache_hit`. The cold figures are defined exactly as `cache_win/cold_first_run` (unregister untimed).
- **Rhai steady state.** It runs right after Hexput's, inside the same `steady_state` section and gated by the same filter rule, with the same `CONCURRENCY`/`ROUNDS`/`RUNS_PER_CONNECTION`/`WARM_UP_RUNS`. It prints its rounds, then a side-by-side table: rows p50, p99 and throughput; columns Hexput, Rhai and the Hexput/Rhai ratio.
- **Engine section** (`engine`, new, hand-timed):
  - Three in-process drivers run the same 16 students. The variables are converted once per student and cloned per run, and host calls are answered natively (no wire). Every run's verdict and its 10 host calls are verified.
    1. `hexput_unmetered`: `hexput_interpreter::Execution::with_variables(...)` with `Meter::UNMETERED`, resolving each `Outcome::HostCall` with `HostCall::resume`.
    2. `hexput_enforced`: the same driver with the Executor's synchronous enforcement:
       - `.metered(Meter { slice: Some(hexput_exec::SLICE), memory_ceiling: Some(budget.memory_ceiling()), allocation_ceiling: Some(budget.allocation_ceiling()) })` from `hexput_enforce::Budget::with_limits(Limits::default())`;
       - `charge_cpu` with an `Instant`-timed elapsed at every `Paused`, at every host call and at the finish;
       - `charge_rpc_call` at every host call;
       - `charge_output(hexput_exec::wire::result_payload_size(..))` at the finish;
       - an `Exceeded` is a panic.
    3. `rhai_engine`: one `Engine` with native `weight`/`passMark`/`record`, a precompiled `AST` and a fresh `Scope` per run.
  - Rounds and runs:
    - `ENGINE_ROUNDS = 5` rounds; each round runs `ENGINE_RUNS = 10_000` runs of each driver, interleaved per round.
    - Per round, each driver's mean ns/run. The summary is the median across rounds.
    - It prints the per-driver figures and the line **"budget enforcement overhead: +X ns/run (+Y%)"**, with X = enforced − unmetered and Y relative to unmetered.
    - It runs when there is no filter or the filter is a substring of `engine`, and is skipped under `--list` and `--profile-time`, like `steady_state`.
- **Report.**
  - `Conditions` workload gains `rhai_source`, `comparator` (`"rhai 1.26.1, Engine::new() defaults, thread-per-connection UDS server in the bench"`), `engine_rounds` and `engine_runs`.
  - The JSON gains `rhai_steady_state {rounds, summary}` and `engine {hexput_unmetered_ns, hexput_enforced_ns, rhai_engine_ns, budget_overhead_ns, budget_overhead_pct}`.
  - The previous-run comparison also compares the Rhai summary within the stated variance. As in 4.4, the report is read, compared and written only when the steady-state section runs (not quick); its `engine` field is `null` when the engine section was filtered out.
  - The output states that the UDS headline figures run with the full default budget, and names what the overhead line covers.
- **Quick mode** (`cargo test`):
  - every Criterion figure runs once;
  - each steady state runs 1 round of 20 runs per connection with no warm-up;
  - the engine section runs 1 round of 20 runs;
  - everything is verified, and no file is read or written.
  - `report::self_check` covers the new parts: the side-by-side ratio, the overhead arithmetic, the `engine` filter rule, the Rhai summary in the comparison, and the wire↔Dynamic conversion round trip.

**Never:**
- no production crate change, crate edge or wire change;
- no Rhai in any normal (non-dev) dependency;
- no budget limit raised, lowered or collapsed for the headline numbers (SM-C1/SM-C2);
- no figure as a gate;
- no TCP numbers (Story 5.9);
- no WASM/OPA comparators;
- no `unsafe`;
- no `plotters`/`rayon`/`html_reports`;
- no debug-profile cargo command.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|---|---|---|---|
| Full run | `cargo bench -p hexput-tests --bench cache --locked` | conditions → `cache_win/*` and `rhai_compare/*` figures → Hexput and Rhai steady rounds plus the side-by-side table → engine table with the overhead line; JSON written | none |
| Wrong Rhai verdict / call count | a broken conversion | panic before any number is published | the run fails |
| Second run, same machine and workload | previous JSON | Hexput and Rhai figures each "within/outside ±N%" | informational |
| Old 4.4 report | workload lacks the comparator fields | "no comparable previous run" | no panic |
| Filter `engine` | `cargo bench … -- engine` | only the engine section runs (Criterion matches nothing); no report is written, since `steady_state` was skipped | none |
| Quick mode | `cargo test -p hexput-tests --bench cache --release` | all sections shortened and verified; no file | a mismatch panics |

</intent-contract>

## Code Map

- `Cargo.toml` (`[workspace.dependencies]`, near `criterion`): add `rhai = "1.26.1"` with a comment (bench comparator, `hexput-tests` dev-only).
- `crates/hexput-tests/Cargo.toml`: add the `rhai` dev-dependency with a Story 4.5 comment. `hexput-exec`, `hexput-enforce` and `hexput-interpreter` are already dev-dependencies. `[[bench]] cache` is unchanged.
- `crates/hexput-tests/benches/cache.rs`, the Story 4.4 harness:
  - `main` (l.69–171) gains the `rhai_compare` group, the Rhai steady state and the engine section.
  - `workload` (l.351–612): `register` (l.483), `run`/`answer`/`unwrap_holder` (l.528–611). Rhai sends plain args, which `unwrap_holder` passes through.
  - `steady::measure` (l.626) is generalized.
  - `report` (l.715–1374): `runs_steady_state` (l.787), `Conditions` (l.820), `Summary`/`render_steady_state` (l.1034–1108), `compare` (l.1150), `to_json` (l.1181), `self_check` (l.1205).
  - Add `mod rhai;` and the `engine` module.
- `crates/hexput-tests/benches/cache/rhai.rs` (new): the Rhai server, the Rhai engine driver and the wire↔`Dynamic` conversion. A non-`mod.rs` bench file finds its submodules in `benches/cache/`, and Cargo does not auto-discover a dir without `main.rs` as a target.
- `crates/hexput-tests/tests/daemon.rs:1929`: the drift guard `include_str!("../benches/cache.rs")`. Keep `RULE`, `COURSES`, `STUDENTS` and `HOST_CALLS_PER_RUN` in `cache.rs`, so the guard still holds.
- Read-only APIs:
  - `hexput_interpreter::{Execution, Meter, Outcome, HostCall::resume, Value}` (`crates/hexput-interpreter/src/lib.rs:155–660`);
  - `hexput_exec::{SLICE, wire::{to_hexput, result_payload_size}}`;
  - `hexput_enforce::{Budget, Limits}` (`crates/hexput-enforce/src/lib.rs:135–500`).
  - The Executor's enforcement sequence to mirror is `hexput_exec::execute_held` (`crates/hexput-exec/src/lib.rs:379` on).
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md`: the Stack table gets a `rhai (bench comparator, hexput-tests dev-only) | 1.26.1` row and a dated `[Amended 2026-10-22, Epic 4 Story 4.5]` paragraph. Also add a frontmatter `amended` mention.
- `.memlog.md` (same folder): a dated decision line.
- `scripts/check-crate-graph.py`: unchanged, because dev-dependencies are exempt. Confirm it passes.

## Tasks & Acceptance

**Execution:**
- `Cargo.toml` and `crates/hexput-tests/Cargo.toml`: pin and add rhai. Update `Cargo.lock` once with `cargo build --release -p hexput-tests --benches` (without `--locked`), then use `--locked` everywhere.
- `crates/hexput-tests/benches/cache/rhai.rs`: the server, engine driver and conversion, per the Always rules.
- `crates/hexput-tests/benches/cache.rs`: the source-parameterized registration, the generalized steady state, the `rhai_compare` group, the Rhai steady state with the side-by-side table, the `engine` module with the overhead line, the report/JSON/comparison additions, the `self_check` additions and the module doc update.
- `AGENTS.md`: Project Status (Story 4.5 implemented; Epic 4 code-complete), the bench section (comparator suite, `-- engine` filter) and the test count.
- The Spine and `.memlog.md`: the Rhai pin and the decisions below.

**Acceptance Criteria:**
- Given the harness, when `cargo bench -p hexput-tests --bench cache --locked` runs over UDS, then it prints Hexput and Rhai figures for the same workload, the same client and the same concurrency side by side (`rhai_compare/*` and the steady-state table), and every run of both is verified.
- Given the comparison run, when results are reported, then the budget-enforcement overhead appears as its own line (`budget enforcement overhead: +X ns/run (+Y%)`) and is in the JSON. The UDS headline figures run with the full default budget and are not adjusted by it.
- Given `cargo test --workspace --locked --release`, when it runs, then the bench's quick mode runs every section once with every result verified, writes no report, and no figure fails a run.
- Given `python3 scripts/check-crate-graph.py`, when it runs, then it passes with rhai reachable from no production crate.

## Design Notes

**Decisions (agent, 2026-10-22, unattended run):**
1. **"Over UDS" for an embedded engine** means a bench-local Rhai server on the same transport, framing and Backend client. Each host call is the same round trip, so only the engine and the server around it differ. Rhai's thread-per-connection server is simpler than the Daemon's tokio runtime, and the report says so.
2. **The overhead line is measured in-process.** The Daemon has no off switch, and adding one is a production change that SM-C1/SM-C2 argue against. The line therefore compares the interpreter with and without the Executor's synchronous enforcement (metering, CPU/RPC/side-effect/output charges). It does not cover the async hops and capability lookups, which are not budgeting.
3. **One bench target.** Extending `cache` reuses its Daemon, client, workload, steady state and report without moving modules, and keeps the 4.4 drift guard valid.
4. **Rhai at its defaults** (`Engine::new()`, no operation limit). That is how a Rhai user would embed it. Rhai's own limits would not be the same six dimensions anyway.

**Decisions made during implementation (agent, 2026-10-22, unattended run):**
5. **The Rhai module is declared `#[path = "cache/rhai.rs"] mod rhai;`.** The Code Map assumed a non-`mod.rs` bench root finds its submodules in `benches/cache/`; it does not — a crate root's modules resolve beside it (`benches/`), and the plain `mod rhai;` failed with E0583. The file stays at `benches/cache/rhai.rs` (no bench target is discovered there, as it has no `main.rs`), and the drift guard's `mod rhai;` needle still matches.
6. **`rhai = "1.26.1"` is a caret requirement, so `Cargo.lock` is pinned with `cargo update -p rhai --precise 1.26.1`.** The first lock update resolved 1.27.0; the manifest line stays exactly as the spec and the pinning test require, and the lock holds 1.26.1. The lock gained only Rhai's own dependencies; no existing package moved.
7. **The overhead line's sign is explicit.** In quick mode (20 runs) or on a noisy machine the enforced median can come out below the unmetered one; the line then reads `budget enforcement overhead: -X ns/run (-Y%), within measurement noise` rather than `+-X`. The JSON keeps the signed difference.
8. **The in-process drivers check `record` without allocating.** Each run's `record` is checked against the student's id and verdict as it is made (Hexput: in the native answer; Rhai: the last `record` kept as an `ImmutableString`), and a run must make exactly one `record` and 10 host calls, so verification costs the same on all three drivers.
9. **Both steady-state summaries must parse for a comparison.** A previous report whose Rhai (or Hexput) summary is missing or garbled is "no comparable previous run", like a partial 4.4 report; a report whose `engine` is `null` still compares its steady states. `rhai_steady_state` also carries `spread`, like `steady_state`.
10. **One pre-written test was reshaped for clippy, not weakened.** `rhai_and_hexput_reach_the_same_verified_verdict_for_every_student_over_the_socket` kept a manual id counter that `clippy::explicit_counter_loop` rejects under `-D warnings`; it now iterates `(3..).zip(&students)` with the same ids and assertions.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: OK
- `cargo build --workspace --all-targets --locked --release` -- expected: OK
- `cargo test --workspace --locked --release` -- expected: all pass, including the bench's quick mode
- `cargo bench -p hexput-tests --bench cache --locked -- engine` -- expected: the engine table and the overhead line (no report written)

## Auto Run Result

Status: done.
Blocking condition: none.

**Implemented change:** the `cache` bench gained the Rhai comparator (SM-1, NFR3). A bench-local Rhai server speaks the Daemon's wire over a UDS and is driven by the same client, students and verification (`rhai_compare/*` Criterion figures, Rhai's steady state and a side-by-side table). An in-process `engine` section times `hexput_unmetered`, `hexput_enforced` and `rhai_engine` and prints the budget-enforcement overhead line. The report gained the comparator fields, `rhai_steady_state` and `engine`. `rhai` 1.26.1 is a `hexput-tests` dev-dependency only.

**Files changed:**
- `Cargo.toml`, `Cargo.lock`: the `rhai = "1.26.1"` workspace pin; the lock holds 1.26.1.
- `crates/hexput-tests/Cargo.toml`: the `rhai` dev-dependency.
- `crates/hexput-tests/benches/cache.rs`: the comparator group, the Rhai steady state with the side-by-side table, the `engine` module, the report and JSON additions, and the `self_check` additions.
- `crates/hexput-tests/benches/cache/rhai.rs` (new): the Rhai server, the in-process engine driver and the wire/`Dynamic` conversion.
- `crates/hexput-tests/tests/daemon.rs`: the `cache_win::rhai_comparator` tests.
- `ARCHITECTURE-SPINE.md`, `.memlog.md`: the Stack amendment and the decisions.
- `epic-4-context.md`: the Story 4.4 and 4.5 contracts.
- `sprint-status.yaml`: 4-5 is in `review`.
- `AGENTS.md`: Project Status.

**Review findings:** 33 findings: 7 medium, 20 low, 6 false, 0 maybe-false.
- 12 entries were patched: 3 medium and 9 low (see the triage log).
- Deferred: no review finding was deferred. One pre-existing flaky test that verification surfaced was deferred.
- Rejected, with reasons in the triage log:
  - the 6 `false` findings: the Rhai accept-thread hang, `-- engine` writing no report, "halt after planning", "same conditions", the in-process overhead reading, and the `+` needle;
  - 6 low findings that are unlikely to occur and whose fixes would add guards: workload-key coupling and the `-- rhai` filter, Rhai server failure cleanup, truncated-prefix EOF, the temp-socket collision, other arities, and the leak when `daemon.stop()` panics.

**Follow-up review recommended: true.** Three medium entries were patched on this first pass:
- the argument measure in the enforced driver;
- the `Cargo.lock` pin check;
- engine warm-up and rotation.

The unverified risk: the full `cargo bench` path (1 000 warm-up runs and 5 rotating rounds) only ran once here, on a noisy 4-thread machine. Nothing checks that the enforced driver keeps following `hexput_exec::execute_held`'s sequence beyond needles and the test copy.

**Verification:**
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings`: clean.
- `python3 scripts/check-crate-graph.py`: OK.
- `cargo build --workspace --all-targets --locked --release`: OK.
- `cargo test --workspace --locked --release`: 942 passing. The first run hit the deferred flaky Story 4.3 test once. It passed on five reruns, and a `--no-fail-fast` run passed 942/942.
- `cargo bench -p hexput-tests --bench cache --locked -- engine`: printed the engine table, the per-driver spread and the overhead line, and wrote no report.

**Residual risks:**
- The engine figures are noisy on a shared machine. The printed spread now shows how noisy, but there is no previous-run comparison for them.
- The Rhai server's failure paths panic rather than reply.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/daemon.rs` (module `cache_win::rhai_comparator`: `rhai_is_pinned_at_one_point_two_six_one_and_is_a_dev_dependency_only`, `the_crate_graph_check_passes_with_rhai_reachable_from_no_production_crate`, `the_rhai_rule_is_the_one_the_bench_measures`, `the_bench_declares_the_comparator_group_engine_section_and_report_fields`, `the_rhai_rule_gives_the_verified_verdict_and_ten_host_calls_for_every_student`, `rhai_and_hexput_reach_the_same_verified_verdict_for_every_student_over_the_socket`, `the_in_process_hexput_drivers_verify_every_student_unmetered_and_enforced`, `the_enforced_driver_charges_ten_calls_against_the_unchanged_default_budget`, `the_unmetered_driver_keeps_no_budget`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- The drift guard expects `RHAI_RULE` in `benches/cache/rhai.rs` as one Debug-escaped literal (`pub const RHAI_RULE: &str = "...\n...";`), the same form as `RULE` in `benches/cache.rs`, and `mod rhai;` in `benches/cache.rs`. The guard is the copy in `daemon.rs`, which must match the spec's rule text exactly.
- Not covered by those tests, by design: the Rhai server over UDS (its framing, `Call` round trips and `wire`-to-`Dynamic` conversion), the report and its JSON, the overhead arithmetic, the side-by-side table, the `engine` filter rule, quick-mode file behaviour, and the bench's own panics on a wrong verdict or call count. They live in `benches/cache.rs` and `benches/cache/rhai.rs`, which `tests/` cannot import; the implementer covers them with `report::self_check` and `#[cfg(test)]` unit tests inside the bench target, as Story 4.4 did.
- The test file was not compiled by the test agent (release build not run from this step); `rustfmt --edition 2024` was applied to it.

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 33 findings — high 0, medium 7, low 20, false 6, maybe-false 0
- findings:
  - `[low]` `[patch]` (blind) The tested drivers are copies of the bench's, they already differ, and the needle test's doc comment claims "the numbers the bench prints are the ones this module verified". — The claim was false. The bench's own drivers are verified run by run only by the quick mode. The doc comment now says the needles pin names only and that the copies check the same sequence. The test copy now uses `program.span` at the finish and does the same argument measure. `measure_held` and `argument_depth()` were added as needles.
  - `[medium]` `[patch]` (blind) The overhead line leaves out `execute_held`'s argument measure against the argument depth limit and the frame, and the CPU charge for that work. — Confirmed in `advance` (`crates/hexput-exec/src/lib.rs`). The enforced driver now measures each argument with `wire::measure_held` against `limits().argument_depth()` and `MAX_FRAME_LEN`, counts the call, and charges the measuring time as CPU. The `covers:` line and the doc comment now name it, and name the wire conversion, toggles, methods and capability decision as not mirrored.
  - `[low]` `[patch]` (blind) The Rhai UDS server has no test, and the "over the socket" test runs Rhai in-process. — The server is exercised. `cargo test --workspace` runs the bench's quick mode, which drives `RhaiServer` through every section with the verdict and call count checked. The test name did overclaim, so it was renamed to `hexput_over_the_socket_and_rhai_in_process_reach_the_same_verified_verdict`, and its doc comment says where the Rhai server is run.
  - `[low]` `[patch]` (blind) `the_crate_graph_check_passes_with_rhai_reachable_from_no_production_crate` tests nothing about rhai. — Confirmed: the script knows nothing about rhai. That rhai reaches no production crate is the manifest test, which shows only `hexput-tests` names it, as a dev-dependency. The test was renamed `the_crate_graph_check_still_passes_with_rhai_added`, and its doc comment says what each test covers.
  - `[medium]` `[patch]` (blind) `rhai = "1.26.1"` is a caret requirement, so a `cargo update` could move the lock to 1.27.x while the report says "rhai 1.26.1". — Confirmed: the pin test read only the manifest. It now also asserts that the `Cargo.lock` `rhai` package's version is exactly `"1.26.1"`. The manifest line stays as the intent's Always rule spells it.
  - `[medium]` `[patch]` (blind) The engine drivers always run in the same order with no warm-up, which biases the comparisons. — Confirmed for the engine section: the first driver of round 1 pays for cold caches. The steady states already have their own `WARM_UP_RUNS`, and their Hexput-then-Rhai order is set by the intent. Added `ENGINE_WARM_UP_RUNS` = 1 000 untimed runs per driver (0 in quick mode, recorded in the conditions and workload JSON), and each round's first driver now rotates.
  - `[low]` `[patch]` (blind) The engine figures have no spread or history, and "within measurement noise" checks no noise estimate. — Fixed: `EngineFigures::spread` gives each driver's largest relative deviation across rounds, printed before the overhead line, and `self_check` covers it. The previous-run comparison of the engine figures was left out. The intent limits the comparison to the steady states, and this low gap would need new comparison plumbing for little gain.
  - `[low]` `[patch]` (blind) The two servers do not send the same messages: the Daemon wraps `Call` arguments in Value Secret holders. — Confirmed. The module docs (`cache.rs` and `rhai.rs`) and the printed `comparator:` condition now state the difference. The `COMPARATOR` string is unchanged, because the intent fixes its text.
  - `[low]` `[patch]` (blind) The Hexput/Rhai ratio means opposite things on different rows. — The side-by-side table now ends with "Hexput/Rhai below 1x: a lower p50/p99 (better) but a lower throughput (worse)", and `self_check` asserts it.
  - `[low]` `[patch]` (blind) The Spine's Story 4.5 Stack amendment is out of date order. — It was moved after the 2026-09-23 Story 2.8 paragraph, the last one in that section.
  - `[low]` `[patch]` (blind) The spec's Auto Run Result is stale, and `epic-4-context.md` has no Story 4.5 bullet. — A Story 4.5 contract bullet was added to the epic context. The Auto Run Result was rewritten by this finalization.
  - `[low]` `[reject]` (blind) `same_workload` compares the whole workload, so a Rhai bump makes Hexput's history incomparable, and `-- rhai` skips Rhai's steady state. — The intent sets the steady-state filter rule (substring of `steady_state`), and `-- steady` runs both. A Rhai version bump is rare, and a per-section comparison key would add plumbing.
  - `[low]` `[reject]` (blind) A Rhai server failure panics, leaks the temp dir and skips `stop()`. — A failure fails the run loudly, and Rust prints the server thread's panic message to stderr. This only happens on a broken bench, and the Daemon helper from Story 4.4 has the same pattern. Drop guards would add complexity.
  - `[false]` `[reject]` (edge) `stop`'s wake-up connect could fail, or an open client could hang the joins. — Every client is dropped before `stop`: the Criterion clients are scoped in blocks, and `steady::measure` owns its clients. A connect to a live listening socket succeeds through its backlog.
  - `[low]` `[reject]` (edge) A partial length prefix followed by EOF is read as a clean hang-up. — The only peer is `wire::Client`, which writes whole frames with `write_all`. Unlikely to occur, and the fix adds branching.
  - `[low]` `[reject]` (edge) A stale `r.sock` in the temp dir makes `bind` panic. — It needs the same pid and the same subsecond nanos, and the Daemon helper uses the same scheme. Unlikely, and the fix adds a guard.
  - `[low]` `[reject]` (edge) A host function with an arity other than 1 or 2 is registered as two-argument. — `HOST_FUNCTIONS` is a private const with arities 1, 1 and 2, so this cannot happen. The fix adds a branch.
  - `[low]` `[reject]` (edge) If `daemon.stop()` panics, `rhai_server.stop()` never runs. — This is a failure path of a failing run. Same reasoning as the blind failure-cleanup finding.
  - `[medium]` `[patch]` (edge) The engine section has no warm-up and times `hexput_unmetered` first. — Grouped with the blind ordering finding: warm-up runs and a rotating first driver were added.
  - `[false]` `[reject]` (edge) With `-- engine`, the overhead figures never reach the JSON. — The intent's I/O matrix specifies this ("no report is written, since `steady_state` was skipped"). The figures are printed.
  - `[medium]` `[patch]` (edge) The enforced driver skips `execute_held`'s argument-depth measure. — Grouped with the blind overhead finding: the argument measure was added.
  - `[low]` `[patch]` (edge) Over UDS, `answer` accepts a `record` with any string id and any bool. — Confirmed. `workload::answer` now takes the student and accepts `record` only with that student's id and verdict, as the in-process drivers already did.
  - `[low]` `[patch]` (edge) The test claims Rhai reaches the verdict "over the socket". — Grouped with the blind Rhai-server finding: the test was renamed.
  - `[low]` `[patch]` (edge) The claim that "the drift guards pin the copies" is false. — Grouped with the blind copies finding: the doc comment was corrected and the copies aligned.
  - `[medium]` `[patch]` (verification-gap) The rhai pin is checked only in the manifest, never in `Cargo.lock`. — Grouped with the blind pin finding: the `Cargo.lock` assertion was added.
  - `[medium]` `[patch]` (verification-gap, other) The `hexput_enforced` driver omits the argument-depth measure and its CPU charge. — Grouped with the blind overhead finding: the measure was added.
  - `[low]` `[patch]` (verification-gap, other) The "over the socket" test sends only Hexput over the socket. — Grouped with the blind Rhai-server finding: the test was renamed.
  - `[false]` `[reject]` (intent) The diff implements the story although the invocation said "Halt after planning". — The planning stage did halt, as the earlier Auto Run Result recorded. The orchestrator then invoked the implementation and review stages separately, and they carry the run's authority over stage sequencing. The story's own intent (the `<intent-contract>`) is implemented, not exceeded.
  - `[low]` `[patch]` (intent) The spec is internally inconsistent (status `in-review`, Auto Run Result "ready-for-dev"). — Grouped with the blind stale-documents finding: finalization rewrote the Auto Run Result.
  - `[false]` `[reject]` (intent) "Same conditions" is not met because Rhai runs at its defaults while Hexput runs under the full budget. — The `<intent-contract>` itself chooses this reading (Always: `Engine::new()` defaults; never a budget limit collapsed for the headline numbers), and the report states it.
  - `[false]` `[reject]` (intent) The overhead line is measured in-process, not as a breakdown of the UDS headline. — The `<intent-contract>` Approach specifies "an in-process section reports the budget-enforcement overhead", and the output says the line covers the in-process synchronous enforcement.
  - `[false]` `[reject]` (intent) The needle `"budget enforcement overhead: +"` contradicts a line that can be negative. — The `+` format string exists in the code for the non-negative branch, and the negative branch is an addition. The needle pins the line's spelling and nothing is wrong.
  - `[low]` `[patch]` (intent) The test name is misleading (Rhai runs in-process). — Grouped with the blind Rhai-server finding: the test was renamed.

