---
title: 'Story 4.4: Measure the cache win'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '244015c11b69efde4dadc90810f5e707556715da'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-4-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-3-keep-the-cache-bounded-and-clean-it-up-with-the-session.md'
warnings: ['oversized']
deferred:
  - summary: >-
      Nothing checks that the cache bench's quick mode neither reads nor writes `cache-win.json`.
    evidence: |-
      Verification-gap review finding. `main` guards the read and `report::write` with `quick`, but no check observes it: dropping the `if !quick` guard keeps `cargo test --workspace` green while each test run would write a 20-run, no-warm-up report that the next `cargo bench` compares against (the workload comparison added in review now refuses it, since its rounds and runs differ, which narrows the harm). Covering it means pulling the run decisions out of `main` into a function `report::self_check` can call.
    location: >-
      crates/hexput-tests/benches/cache.rs (main)
    severity: low
---

<intent-contract>

## Intent

**Problem:** The project's central claim is that you parse a Script once and execute it many times (FR-5, SM-1). Nothing measures that claim yet. `criterion` is pinned in the workspace, but no crate uses it and there is no benchmark target.

**Approach:**
- Add one Criterion benchmark target, `cache`, to `hexput-tests`. It starts a real Daemon in-process over a real Unix Domain Socket and drives it as a Backend would.
- It reports three things:
  1. **Cold vs warm.** Criterion figures for the first run of a Script (cold, including the parse) and for a cache hit (warm), plus a Direct Execution reference line.
  2. **Steady state.** Hand-measured p50/p99 latency and throughput of Cached Execution, with several concurrent connections, on a host-callback-heavy pass/fail-rule workload.
  3. **Conditions.** A report of the machine and workload conditions, plus a stated variance that a second run on the same machine is compared against.

## Boundaries & Constraints

**Always:**
- **Measure the real path end to end.** The bench uses `hexput_daemon::run_with` with a temp System Config (`[transport.uds] path` inside a fresh temp dir, `log_level = "error"`), on its own thread, stopped through a oneshot future. That is the `Serving` pattern in `tests/daemon.rs`, copied because a bench cannot import test modules.
- **Each client is a blocking `std::os::unix::net::UnixStream` on its own thread**, framed with `hexput_port::{encode, encode_frame, decode}`. It connects, then sends `Init {config: {}, registrations: [{name, blanket: true}…]}`.
- **Every execution runs under the default Resource Budget and capability checks** (SM-C1/SM-C2). There are no overrides, and no limit is raised or collapsed.
- **The fixed workload** (constant `RULE`, the anchor case):
  ```
  let total = 0; let weights = 0;
  for (grade in grades) { let w = weight(grade.course); total = total + grade.score * w; weights = weights + w; }
  let passed = total / weights >= passMark(student.program);
  record(student.id, passed);
  return passed;
  ```
  Each run makes 8 grades × `weight` + `passMark` + `record` = **10 host calls**. The Backend answers every `Call` with `Result {value}`:
  - `weight` comes from a fixed table of 8 courses;
  - `passMark` returns `50` for `"science"` and `60` for `"engineering"`;
  - `record` returns `null`.
- **Variables are fresh on every run.** Runs rotate through 16 deterministic students. Score `j` of student `i` is `(i*37 + j*11) % 101`, and programs alternate.
- **Every result is checked.** Each reply must equal the pass/fail value Rust computes with the same f64 operations in the same order, and must have taken exactly 10 `Call`s. A mismatch panics, so a broken run never publishes a number.
- **Criterion setup.** One `cache_win` group with `noise_threshold(0.10)`, so Criterion's own comparison with the previous run uses the stated variance. All three figures use one connection with one execution in flight:
  - `cold_first_run`: `CodeRegister` plus the first `CachedExecutionStart` on the new handle are timed together. The `CodeUnregister` is untimed, which keeps the cache bounded.
  - `warm_cache_hit`: `CachedExecutionStart` on one registered handle.
  - `direct_execution`: `ExecutionStart` with the source, parsed on every run.
- **Steady-state section** (`steady_state`, measured by hand because Criterion has no p99):
  - `CONCURRENCY = 8` connections, each running a closed loop. Each connection registers the Script and warms up with 100 runs.
  - `ROUNDS = 5` rounds of `RUNS_PER_CONNECTION = 1000` timed runs each. A run's latency is the time from sending `CachedExecutionStart` to its final `Result`.
  - Each round reports p50, p99 (nearest-rank on the sorted latencies) and throughput (runs ÷ the round's wall time).
  - The summary is the median of the rounds. It also reports the spread: each figure's largest relative deviation from that median.
- **Stated variance.** p50 and throughput within ±10%, p99 within ±20%. The bench prints whether this run's spread across rounds is within it. If a previous report exists with the same machine fingerprint, it also prints whether each figure is within it of the previous run's. The result is **informational, never a failure**: no figure is a gate (SM-1).
- **The conditions report.** It is printed before any figure and written as JSON to `<target>/hexput-bench/cache-win.json`, where `<target>` is `CARGO_TARGET_DIR` or else `<CARGO_MANIFEST_DIR>/../../target`. The previous file is read first, for the comparison.
  - **Machine:** CPU model (from the `/proc/cpuinfo` `model name`, else `"unknown"`), `available_parallelism`, `std::env::consts::{OS, ARCH}`, `rustc --version` and `git rev-parse HEAD` (each `"unknown"` on failure), and the Unix timestamp.
  - **Workload:** the Script source, host calls per run, students, concurrency, rounds, runs and warm-ups, "default Resource Budget", "UDS, Daemon in-process on its own runtime", and the build profile (`bench`).
  - **Figures:** the steady-state rounds and summary. The machine fingerprint is the CPU model, parallelism, OS and arch.
- **Quick mode.** Cargo passes `--bench` to a bench run. Without it (`cargo test`), Criterion runs each figure once, and `steady_state` runs 1 round of 20 runs per connection with no warm-up. It still checks every result, and it neither writes nor compares a report.
- **Filter.** A positional filter argument is passed to Criterion. `steady_state` runs only when there is no filter or the filter is a substring of `steady_state`.
- **Unix only.** On other platforms `main` prints that the bench needs a Unix Domain Socket and returns.
- **Shutdown.** The Daemon is stopped at the end. Its exit must be success, or the bench panics with its stderr.

**Never:**
- no Rhai comparison, budget-overhead line or TCP numbers (Stories 4.5 and 5.9);
- no change to any production crate, crate edge or wire message;
- no latency threshold that fails a run;
- no `plotters`, `rayon` or `html_reports` features (disk is scarce);
- no `unsafe`;
- no test helpers moved into production crates.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|---|---|---|---|
| Full run | `cargo bench -p hexput-tests --bench cache` | the conditions block, then the three `cache_win/*` Criterion figures, then the 5 steady-state rounds and a summary with p50/p99/throughput and spread; the JSON is written | none |
| Second run, same machine | the JSON exists with an equal fingerprint | each figure is printed with its relative change and "within ±N%" or "outside ±N%"; Criterion prints its change against the previous run | outside the variance is reported, never a failure |
| Different machine / no previous file / unreadable JSON | fingerprint differs, or the file is missing or corrupt | "no comparable previous run"; the file is overwritten | no panic |
| Quick mode | `cargo test -p hexput-tests --bench cache` (no `--bench`) | every figure runs once and the shortened steady state runs; all results are verified; no file is written | a wrong result or Call count panics |
| Filter | `cargo bench … -- warm` | only `cache_win/warm_cache_hit` runs; `steady_state` is skipped | none |

</intent-contract>

## Code Map

- `Cargo.toml:60`: `criterion = "0.8.2"`. Change it to `{ version = "0.8.2", default-features = false, features = ["cargo_bench_support"] }`, with a comment in the style of the moka line. criterion 0.8.2 needs Rust 1.86, which is fine.
- `crates/hexput-tests/Cargo.toml`:
  - Add the dev-dependencies `criterion = { workspace = true }` and `hexput-daemon` (already present).
  - Add `[[bench]] name = "cache"` with `harness = false` and `test = true`, so CI's `cargo test --workspace` runs it in quick mode.
- `crates/hexput-tests/benches/cache.rs` (new): the whole harness, in private modules (`daemon` for start/stop, `wire` for the client, `workload`, `report`).
- Read-only references:
  - `crates/hexput-tests/tests/daemon.rs:478-592`: `Serving` and the `wire` helpers to copy.
  - `crates/hexput-tests/tests/daemon.rs:622-660`: the `Init` payload with `blanket`.
  - `crates/hexput-tests/tests/connection.rs:4092-4125`: the `CodeRegister`/`handle` shapes.
  - `crates/hexput-shared/src/wire.rs:174`: `Envelope { id, message_type, payload }`, and `MessageType::{Init, ExecutionStart, CodeRegister, CachedExecutionStart, CodeUnregister, Call, Result}`.
- Wire shapes in use:
  - `CodeRegister {source}` → `Result {handle}`.
  - `CachedExecutionStart {handle, variables}` and `ExecutionStart {source, variables}` → `Result {value}`.
  - A `Call {name, arguments, execution}` from the Daemon is answered with a `Result {value}` that carries **the Call's own id**.
  - `CodeUnregister {handle}` → `Result {}`.
- `scripts/check-crate-graph.py`: unchanged, because dev-dependencies are exempt.
- `.github/workflows/ci.yml`: unchanged, because `test = true` already brings the bench into `cargo test --workspace`.

## Tasks & Acceptance

**Execution:**
- `Cargo.toml`: trim criterion's features. This keeps a lean, pinned feature set.
- `crates/hexput-tests/Cargo.toml`: add the criterion dev-dependency and the `[[bench]]` target. Update `Cargo.lock` once with a release build without `--locked`, then commit it.
- `crates/hexput-tests/benches/cache.rs`: implement the Daemon harness, the Backend client, the workload with result verification, the Criterion group, the steady-state measurement, and the report and comparison, all per the Always rules. The module doc states what each figure means and the stated variance.
- `AGENTS.md`:
  - In Build/Test, add how to run the bench (`cargo bench -p hexput-tests --bench cache`; quick check `cargo test -p hexput-tests --bench cache --release`) and where the report lands.
  - Update the Project Status Epic 4 text (Story 4.4 implemented) and the test count.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/.memlog.md`: add a dated line recording the decisions: the bench location (`hexput-tests`, no new crate), criterion's feature set, the stated variance, and the in-process UDS Daemon.

**Acceptance Criteria:**
- Given the Criterion harness, when `cargo bench -p hexput-tests --bench cache --locked` runs, then it prints `cache_win/cold_first_run` and `cache_win/warm_cache_hit` as separate figures for the same Script, plus `cache_win/direct_execution`.
- Given the harness, when its steady-state section runs, then it prints p50, p99 and throughput for each of 5 rounds and a median summary, measured with 8 concurrent connections each running the 10-host-call rule, with every result verified.
- Given the harness is run twice on one machine, when the second run finishes, then each steady-state figure is shown against the first run with "within" or "outside" the stated variance (±10% p50/throughput, ±20% p99), and Criterion reports its change against its 10% noise threshold. The conditions are both printed and in `target/hexput-bench/cache-win.json`.
- Given CI's `cargo test --workspace --locked`, when it runs, then the bench runs once in quick mode and passes, writing no report.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/daemon.rs` (module `cache_win`: `the_rule_over_the_socket_is_verified_for_every_student_with_ten_host_calls`, `cold_warm_and_direct_runs_agree_and_code_unregister_frees_the_handle`, `eight_concurrent_connections_each_verify_every_run_of_the_rule`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Not covered by those tests, by design: the bench's report, JSON file, machine fingerprint, nearest-rank percentiles, spread, variance comparison, quick-mode file behaviour and filter rows of the I/O matrix. They live in `benches/cache.rs`'s private modules, which `tests/` cannot import, and the bench file does not exist yet. The implementer should cover them with `#[cfg(test)]` unit tests inside `benches/cache.rs` (the bench target runs them under `cargo test` because of `test = true`), and must not move them into a production crate.
- The test file was not compiled by the test agent (release build avoided for disk reasons); the workload's expected verdicts were checked with an independent Python computation (6 of 16 students pass).

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 33 findings — high 0, medium 2, low 28, false 3, maybe-false 0
- findings:
  - `[medium]` `[patch]` blind: a panicking steady-state connection never reaches its next `barrier.wait()`, so the run hangs instead of failing — each connection's body now runs under `catch_unwind`, counts its waits, meets the barrier for every wait it has left and then resumes the panic; confirmed by injecting a panic in one connection (the quick run exits 101 at once).
  - `[low]` `[reject]` blind: the bench never states the cold/warm ratio and the JSON omits the Criterion figures — Criterion prints the three figures side by side in one group and keeps their history in `target/criterion/`; a ratio or a copy in the JSON means parsing Criterion's internal estimate files, more than a direct correction for a figure the reader already sees.
  - `[low]` `[patch]` blind: the comparison fingerprint leaves out the workload and the build — `compare` now also requires the previous report's `workload` (script, concurrency, rounds, runs, warm-ups, profile) to equal this run's; toolchain and revision stay in the printed conditions and the file.
  - `[low]` `[reject]` blind: each run silently becomes the next run's reference — this is the spec's "compared against the previous run" (Criterion's own default works the same way); a pinned baseline is a new feature. The limitation is now stated in AGENTS.md.
  - `[low]` `[patch]` blind: `profile: "bench"` is hard-coded — `Conditions` now records the profile actually measured: `test (quick mode)`, `bench`, or `unoptimised (debug assertions on)`, printed and written; an unoptimised run is never comparable with a bench one.
  - `[low]` `[reject]` blind: the Backend accepts any `record` id/verdict and never checks `weight` order or the `execution` field — the bench verifies every result and its exact `Call` count; argument routing and the `execution` id are the subject of `tests/connection.rs`, not of a benchmark, and the extra checks add Backend state for a defect nobody meets measuring.
  - `[low]` `[patch]` blind: the status files contradict each other — `sprint-status.yaml` now has `4-4-measure-the-cache-win: review`, like 4.1–4.3.
  - `[low]` `[patch]` blind: the workload is duplicated in `tests/daemon.rs` and can drift — new test `this_workload_is_the_one_the_cache_bench_measures` `include_str!`s `benches/cache.rs` and asserts its `RULE`, `weight` table, `STUDENTS`, `HOST_CALLS_PER_RUN` and score formula equal the copy.
  - `[low]` `[reject]` blind: the steady-state filter is a substring test, not Criterion's regex — the spec's Always rule defines the substring rule, everyday filters are plain words where both agree, and regex matching needs a new dependency.
  - `[low]` `[reject]` blind: nothing cleans up the temp dir after a panic — the leftover is one small dir with a socket and a TOML file under `$TMPDIR` after a run that already failed; a drop guard is more than a direct correction.
  - `[low]` `[patch]` blind: the client allocates whatever a frame prefix claims — `wire::Client::receive` now asserts the length is at most `hexput_port::MAX_FRAME_LEN` before allocating.
  - `[false]` `[reject]` blind: quick mode gathers `rustc`/`git` conditions it throws away — quick mode prints the conditions block (the spec prints it before any figure in every mode), so the work is shown, not discarded.
  - `[low]` `[reject]` blind: `report_path` ignores `CARGO_BUILD_TARGET_DIR` and `build.target-dir` — the spec defines the path as `CARGO_TARGET_DIR` else the workspace `target`, which is what this repository uses; other setups are rare and need extra resolution logic.
  - `[low]` `[patch]` blind: the documented commands are inconsistent about `--release` — AGENTS.md now gives the local `cargo test --workspace --locked --release` form next to CI's quick-mode run; every command this run executed used `--release` (CI's own command is CI's, on a runner without the disk limit; the frozen I/O matrix is not edited).
  - `[medium]` `[patch]` edge: a worker panic leaves the main thread waiting on the barrier forever — same fix as the blind barrier finding.
  - `[low]` `[reject]` edge: regex or `--exact` filters decide differently from Criterion — same as the blind filter finding.
  - `[low]` `[patch]` edge: `--profile-time` runs a full steady state and overwrites the report — `runs_steady_state` now skips under `--profile-time` (either spelling), like `--list`, with self-check asserts; `--ignored` is a libtest flag Criterion accepts and ignores, so it is left as a normal run.
  - `[low]` `[reject]` edge: a relative `CARGO_TARGET_DIR` puts the report under `crates/hexput-tests` — rare, and resolving it adds a branch; the file is still written and read consistently from the same place.
  - `[low]` `[reject]` edge: CPU `"unknown"` lets different machines share a fingerprint — the report lives in the local target dir, so another machine's file only appears with a copied target dir, and parallelism/OS/arch still have to match.
  - `[low]` `[patch]` edge: a previous report of another workload is compared — same fix as the blind fingerprint finding.
  - `[low]` `[patch]` edge: quick mode names the `bench` profile — same fix as the blind profile finding.
  - `[low]` `[reject]` edge: a temp path with control characters breaks the TOML System Config — `$TMPDIR` paths with control characters are not met in practice, and the Daemon's failure is still reported.
  - `[low]` `[patch]` edge: an oversized frame prefix allocates up to 4 GiB — same fix as the blind frame-length finding.
  - `[low]` `[patch]` edge: the `cache_win` tests claim to verify the bench's `RULE` but hold a copy — same fix as the blind duplication finding.
  - `[low]` `[patch]` verification-gap: the corrupt-report checks pass only because the fingerprint differs — `self_check` now passes this machine's conditions for `{"machine": 3}`, a report without `steady_state.summary` and one with a non-numeric figure, each expecting `NoPrevious`, plus another-workload and unoptimised-profile cases.
  - `[low]` `[defer]` verification-gap: nothing checks that quick mode skips reading and writing the report — kept the filed disposition: covering it means moving the run decisions out of `main`; recorded in `deferred` and `deferred-work.md` (the workload comparison now refuses a quick-mode report anyway).
  - `[low]` `[patch]` verification-gap (other): `sprint-status.yaml` is stale at `ready-for-dev` — same fix as the blind status finding.
  - `[low]` `[patch]` verification-gap (other): AGENTS.md lacks the quick check and the test count — the quick check command was already there; the test count is now updated (933).
  - `[false]` `[reject]` intent-alignment: scope "halt after planning" vs a full implementation — this run's assignment is the bmad-build-auto loop (plan, implement, review), and the spec's lifecycle reached `in-review` through it; the status mismatch itself is the patched sprint-status finding.
  - `[low]` `[reject]` intent-alignment: no standalone parse-only figure for AC1 — Design Note 2 records the reading (cold = parse + first run, set beside warm, so the parse cost is the difference); both are separate figures for the same Script, as the AC asks.
  - `[false]` `[reject]` intent-alignment: the anchor case is not traced and concurrency is connection-level only — the brief's addendum (line 75) names "the university pass/fail-rule example" as the anchor, "small, frequently evaluated, host-callback-heavy", which `RULE` is; 8 concurrent connections are concurrent load, which the AC does not further define.
  - `[low]` `[reject]` intent-alignment: the Criterion figures are not written next to the recorded conditions — same as the blind ratio finding; the conditions are printed before the figures in the same run.
  - `[low]` `[patch]` intent-alignment: the CI path is not a release build and the profile label is wrong — the label is fixed (blind profile finding) and the local release command documented (blind `--release` finding).

## Design Notes

**Decisions (agent, 2026-10-22, unattended run):**
1. **Location.** The bench lives in `hexput-tests`, where every crate is already a dev-dependency and the graph check does not apply. A new `hexput-bench` crate would need a Spine amendment, and the Spine defers harness structure.
2. **"Cold first parse"** means the time to the first result of a Script never seen before: `CodeRegister` (parse) plus its first run. Comparing against `warm_cache_hit` isolates the cache's win. `direct_execution` shows what every run would cost without the cache.
3. **Percentiles are measured by hand.** Criterion reports means, medians and confidence intervals, not p99, so `steady_state` measures outside Criterion. Its figures, reproducibility check and conditions go into the JSON report.
4. **The Daemon runs in-process and the clients block on threads.** That gives the same transport and Daemon code path as the shipped binary without spawning a process. Client threads share the machine with the Daemon, and the report records this.

**Decisions made during implementation (agent, 2026-10-22, unattended run):**
5. **`RULE` gains the `;` after its `for` block.** LANGUAGE-REFERENCE §5 makes a `for … in` statement end with `;` like every statement, and the source as first written failed `CodeRegister` with `syntax.expected_syntax`. The language reference wins over a story, so the bench and the pre-written tests in `tests/daemon.rs` both use `for (grade in grades) { … };`; the workload's meaning and its 10 host calls are unchanged.
6. **The Backend reads each `Call` argument inside its Value Secret holder.** Every argument the rule passes is read from a place (`grade.course`, `student.program`, `student.id`, `passed`), so Story 3.11 sends it as `{__secret, value}`. The bench's and the pre-written tests' `answer` unwrap a two-key map carrying `__secret` before matching; the tests' expectations are not weakened.
7. **The report's own checks run in quick mode, not under `#[cfg(test)]`.** A `harness = false` target is compiled without `--test`, so `cfg(test)` code in `benches/cache.rs` would never run. `report::self_check()` runs the same assertions (quick-mode detection, filter parsing, the steady-state filter rule, nearest-rank percentiles, median and spread, the comparison for the same machine / another machine / no file / corrupt JSON, the CPU-model parse) at the start of every quick-mode run, i.e. under `cargo test --workspace`.
8. **`--bench --test` counts as quick mode**, matching Criterion's own test mode, and `--list` skips the steady state. A failure to write the JSON report is printed to stderr, never a panic: the figures were already printed.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes
- `cargo test --workspace --locked --release` -- expected: all pass, including the bench's quick mode
- `cargo bench -p hexput-tests --bench cache --locked` run twice -- expected: the figures and the comparison as in the matrix; `target/hexput-bench/cache-win.json` exists

## Auto Run Result

**Summary.** Story 4.4 adds the workspace's first benchmark, the Criterion target `cache` in `hexput-tests`: a real in-process Daemon over a UDS, driven by blocking Backend clients on the anchor pass/fail rule (10 host calls per run, every result verified). It reports `cache_win/cold_first_run`, `warm_cache_hit` and `direct_execution`, a hand-timed 8-connection steady state (p50/p99/throughput per round, median and spread), and a conditions report compared against the previous run on the same machine and workload within the stated variance (±10% p50/throughput, ±20% p99; informational). Quick mode runs under `cargo test`.

**Files changed.**
- `Cargo.toml` — criterion trimmed to `cargo_bench_support`.
- `Cargo.lock` — criterion's dependencies.
- `crates/hexput-tests/Cargo.toml` — criterion dev-dependency, `[[bench]] cache` with `test = true`.
- `crates/hexput-tests/benches/cache.rs` — the harness: Daemon, wire client, workload, steady state, report and its self-check.
- `crates/hexput-tests/tests/daemon.rs` — `cache_win` module: the workload over the socket, cold/warm/direct agreement, 8 concurrent connections, and the drift guard against the bench.
- `AGENTS.md` — bench commands and Story 4.4 status, test count 933.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` — 4.4 at `review`.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/.memlog.md` — the bench decisions.

**Review findings.** 33 findings (high 0, medium 2, low 28, false 3). Patched: 1 medium entry (the steady-state barrier hang) and 9 low entries (workload/profile-aware comparison, actual profile recorded, frame-length cap, `--profile-time` skip, same-machine corrupt/partial report checks, drift guard test, sprint status, AGENTS `--release` command and test count). Deferred: 1 (quick mode's file behaviour is unchecked). Rejected, with reasons in the triage log: cold/warm ratio and Criterion figures not in the JSON (Criterion persists and shows them), each run as the next reference (the spec's design, now documented), weaker Backend argument checks (covered by connection tests), substring vs regex filter (spec rule), no temp-dir cleanup on panic, `CARGO_BUILD_TARGET_DIR`/relative target dir, unknown-CPU fingerprint, TOML path escaping, no parse-only figure (Design Note 2); false: quick-mode conditions "thrown away", "halt after planning" scope, untraced anchor case.

**Follow-up review recommendation:** `false` — first pass; patched entries at entry verdict: high 0, medium 1, low 9. The one medium fix was confirmed by an injected panic.

**Verification.** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 933 passing, 0 failing, plus the bench's quick mode (self-check, three figures, one steady-state round, all verified). An injected panic in one steady-state connection made the quick run fail at once (exit 101) instead of hanging. The full `cargo bench` was not re-run after review (disk and time); the patches touch only the report comparison, labels and failure paths.

**Residual risks.** Figures are informational and machine-dependent; the steady state shares the machine with the in-process Daemon; each full run replaces the reference, so slow drift within ±10% per run is not caught; quick mode's no-file behaviour is unchecked (deferred).
