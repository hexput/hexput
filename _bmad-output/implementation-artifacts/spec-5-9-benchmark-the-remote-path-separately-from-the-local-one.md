---
title: 'Story 5.9: Benchmark the remote path separately from the local one'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '6e87a59e9eb13e89565d091d03d6be65a002f087'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-4-measure-the-cache-win.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-4-5-compare-the-hot-path-against-rhai.md'
warnings: ['oversized']
deferred:
  - summary: >-
      AGENTS.md's Epic 5 heading names Stories 5.1-5.9 but its parenthetical summary listed only what 5.1-5.8 deliver.
    evidence: |-
      Blind-hunter review finding; routed to defer because its fix edits an agent-context file. The finalization's
      required Project Status update for this story added "the remote path benchmarked separately from the local one"
      to that parenthetical, so this item only records the routing; nothing further is outstanding.
    location: >-
      AGENTS.md (Project Status, Epic 5 paragraph)
    severity: low
---

<intent-contract>

## Intent

**Problem:** The `cache` bench (Stories 4.4/4.5) measures Hexput only over a Unix Domain Socket. The TCP+TLS path (Story 5.1) has no figure. The brief requires UDS and TCP/TLS numbers to be reported separately (NFR3), so that the transport's latency is never conflated with the runtime's.

**Approach:** Extend the same bench with a TCP+TLS leg against the same in-process Daemon:
- **Criterion.** A new group, `remote_path`, holds the TCP+TLS cold and warm figures.
- **Steady state.** A third closed-loop steady state runs over TCP+TLS.
- **Report.** The conditions record each leg's transport and machine topology, and the JSON gets a separate `tcp_tls_steady_state` section. The UDS and TCP/TLS numbers are never averaged or merged.

## Boundaries & Constraints

**Always:**
- **The Daemon serves both transports.** The bench's `daemon::Daemon::start` writes a System Config with `[transport.uds]` (as today) and a `[transport.tcp]` section:
  - `bind = "127.0.0.1:<port>"`, where `<port>` is a free ephemeral port picked just before start: bind a `std::net::TcpListener` to `127.0.0.1:0`, read the port, then drop the listener.
  - `tls_cert`/`tls_key` are the checked-in fixtures `crates/hexput-tests/fixtures/tls/server.pem` and `server.key`, located through `env!("CARGO_MANIFEST_DIR")`.
  - `log_level` stays `"error"`.
- **TLS client.** `Daemon::connect_tls()` returns the same `wire::Client`. It retries the connect like `connect()` until the Daemon listens. The client:
  - is a blocking `rustls::StreamOwned<ClientConnection, TcpStream>`;
  - uses `rustls::ClientConfig::builder_with_provider(ring::default_provider())`, `.with_safe_default_protocol_versions()` and roots from the fixture `ca.pem` only, with no client auth;
  - uses server name `localhost` and one `Arc<ClientConfig>` built at `start`;
  - sets `set_nodelay(true)` and a 60 s read timeout on the TCP stream;
  - completes the handshake explicitly (`complete_io` while `is_handshaking()`) before the client is returned.
- **`wire::Client` becomes transport-neutral.**
  - It holds `BufReader<Box<dyn Duplex>>`, where `Duplex: Read + Write + Send` is a private trait blanket-implemented for both `UnixStream` and the TLS stream. Writes go through `get_mut()`.
  - `send` calls `flush()` after `write_all`, so a TLS record is never held back.
  - `Client::new` takes any such stream, so the UDS connect, the Rhai server's `connect` (`benches/cache/rhai.rs:86-87`) and the TLS connect all build the same client.
  - The framing, the `MAX_FRAME_LEN` assert and `request` stay unchanged. The UDS leg's measured path differs only by the boxed stream.
- **Negotiated TLS parameters.** `Daemon::tls_parameters()` opens one TLS connection, completes the handshake and returns its `protocol_version()` and `negotiated_cipher_suite()` as text (e.g. `TLSv1_3, TLS13_AES_256_GCM_SHA384`). `main` starts the Daemon and the Rhai server first, then gathers and prints the conditions with these parameters, still before any figure.
- **Criterion group `remote_path`.** It uses `noise_threshold(report::VARIANCE.p50)`, one TLS connection with one execution in flight, and the same student rotation and closures `cold`/`warm`. Its two figures are:
  - `tcp_tls_cold_first_run`, defined exactly as `cache_win/cold_first_run` (the unregister is untimed);
  - `tcp_tls_warm_cache_hit`, defined exactly as `cache_win/warm_cache_hit`.

  The UDS counterparts stay `cache_win/cold_first_run` and `cache_win/warm_cache_hit` and are not duplicated.
- **TCP+TLS steady state.**
  - It is measured inside the `steady_state` section, with the same filter rule, in the order: Hexput over UDS, then Hexput over TCP+TLS, then Rhai over UDS.
  - It is `steady::measure(|| daemon.connect_tls(), workload::RULE, …)` with the same `CONCURRENCY`/`ROUNDS`/`RUNS_PER_CONNECTION`/`WARM_UP_RUNS`, so every result and its 10 `Call`s are verified.
  - It prints its own rounds under the label `report::TCP_TLS_SIDE` = `"Hexput Daemon, Cached Execution over TCP+TLS (loopback)"`.
  - It then prints a transport table: rows p50, p99 and throughput; columns `UDS`, `TCP+TLS` and the `TCP+TLS/UDS` ratio. Both raw figures are always shown, and no figure combines the two legs. `render_side_by_side` is generalized to take its column labels, and the Hexput/Rhai table is unchanged.
- **The topology of every leg is recorded.** `Conditions::workload()` replaces its single `transport` string with `legs`, and `render()` prints one line per leg:
  - `uds`: `{transport: "UDS", topology: "same machine: Daemon in-process on its own runtime, clients on threads of the bench process"}`;
  - `tcp_tls`: `{transport: "TCP+TLS", address: "127.0.0.1 (loopback)", tls: "<negotiated parameters>, ring provider, server authentication only", topology: "same machine over the loopback interface: no network hop, so the figure is the TCP and TLS stack cost, not network latency; Daemon in-process, clients on threads of the bench process"}`;
  - `rhai`: `{transport: "UDS", topology: <the uds topology>}`.
- **Report JSON.**
  - The JSON gains `tcp_tls_steady_state {rounds, summary, spread}`, beside the unchanged `steady_state` (UDS) and `rhai_steady_state`.
  - `report::Steady` gains `tcp_tls` and `tcp_tls_summary`.
  - `compare` also compares the TCP+TLS summary. `Comparison::Previous` gains a `tcp_tls: Delta`, and a previous report without `tcp_tls_steady_state` (a Story 4.5 report, whose workload also differs) is `NoPrevious`.
- **Quick mode** (`cargo test`):
  - the `remote_path` figures run once, and the TCP+TLS steady state runs 1 round of 20 runs per connection, with every result verified and no file read or written;
  - `report::self_check` covers the new parts: the transport table's labels and ratio, `legs` in the workload, the separate `tcp_tls_steady_state` section in `to_json`, a `Previous` comparison that carries the TCP+TLS delta, and `NoPrevious` for a report lacking it.
- **Module doc.** `benches/cache.rs`'s module doc gains the Story 5.9 figures, what they mean, and the loopback topology.

**Never:**
- **No change to the production side:** no production crate change, crate edge, dependency, wire message or System Config field (`rustls` is already a `hexput-tests` dev-dependency).
- **No figure that combines legs:** none averages or merges UDS and TCP+TLS figures, and no TCP leg goes into `cache_win` or `steady_state`'s UDS summary.
- **No new comparator legs:** no remote-host mode, no WebSocket or Named Pipe leg, and no Rhai over TCP.
- **No gates:** no latency threshold that fails a run.
- **Nothing else forbidden by 4.4/4.5:** no `plotters`/`rayon`/`html_reports`, no `unsafe`, and no debug-profile cargo command.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|---|---|---|---|
| Full run | `cargo bench -p hexput-tests --bench cache --locked` | conditions (with each leg's transport, topology and TLS parameters) → `cache_win/*`, `rhai_compare/*`, `remote_path/tcp_tls_*` → UDS, TCP+TLS and Rhai steady rounds, the transport table and the Hexput/Rhai table → engine; the JSON has three separate steady sections | none |
| Old 4.5 report | no `tcp_tls_steady_state`, workload without `legs` | "no comparable previous run" | no panic |
| Second run, same machine and workload | previous JSON | UDS, TCP+TLS and Rhai figures each "within/outside ±N%" | informational |
| Filter `remote_path` | `cargo bench … -- remote_path` | only the two `remote_path` figures; steady state and engine skipped; no report | none |
| Port taken between pick and bind | another process grabs the port | the Daemon exits with failure; `connect_tls` panics with "the Daemon never listened" | run fails, no number published |
| Quick mode | `cargo test -p hexput-tests --bench cache --release` | every section shortened, the TLS leg included, everything verified, no file | a mismatch panics |

</intent-contract>

## Code Map

- `crates/hexput-tests/benches/cache.rs` (the only source file that changes):
  - **Module doc**, l.1-73: add the Story 5.9 figures.
  - **`main`**, l.113-305:
    - move `Conditions::gather` after `Daemon::start` and pass it the TLS parameters;
    - add the `remote_path` group after `rhai_compare`, l.211-237;
    - add the TCP+TLS steady state between Hexput's and Rhai's, l.240-276;
    - pass the extended `Steady` to `compare`/`to_json`, l.296-306.
  - **`daemon` module**, l.309-397:
    - `start` builds the System Config and picks the port;
    - add `connect_tls` and `tls_parameters`;
    - `stop` is unchanged.
  - **`wire::Client`**, l.400-470: make the client transport-neutral and make `send` flush.
  - **`steady::measure`**, l.774: already takes a connect closure, so it needs no change.
  - **`report` module**:
    - `HEXPUT_SIDE`/`RHAI_SIDE`, l.1149: add `TCP_TLS_SIDE`;
    - `Conditions`, l.1257-1395: add the TLS parameters field; `workload()` gets `legs`; `render()` prints per-leg lines;
    - `render_side_by_side`, l.1581: take the column labels;
    - `Steady`, l.1603: add the TCP+TLS fields;
    - `Delta`/`Comparison`/`compare`, l.1731-1822;
    - `to_json`, l.1838;
    - `self_check`, l.1860.
- `crates/hexput-tests/benches/cache/rhai.rs:86-87`: `connect` calls `Client::new(UnixStream…)`. It must still compile against the generalized `Client::new`, and nothing else changes.
- **Read-only references:**
  - `crates/hexput-tests/tests/daemon/conformance.rs:93-110` (`fixture`, `tcp_section_with`) and `:264-287` (the blocking rustls client) are the patterns to copy, because a bench cannot import test modules.
  - `crates/hexput-tests/fixtures/tls/` holds the CA and the `localhost`/`127.0.0.1` leaf, valid until 2126.
  - `crates/hexput-transport/src/tcp_tls.rs:240`: the server already sets `TCP_NODELAY`.
- `crates/hexput-tests/Cargo.toml`: unchanged. `rustls` is already a dev-dependency, and `[[bench]] cache` already has `test = true`.
- `crates/hexput-tests/tests/daemon.rs`, module `cache_win` (drift guard `this_workload_is_the_one_the_cache_bench_measures`): `RULE`, the weight table, `STUDENTS`, `HOST_CALLS_PER_RUN` and the score formula must stay byte-identical.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-tests/benches/cache.rs`: implement every Always rule above: the Daemon's TCP+TLS section, `connect_tls`, the TLS parameters, the neutral `Client`, the `remote_path` group, the TCP+TLS steady state and transport table, the per-leg `legs`, the JSON section, the comparison, the `self_check` additions and the module doc. This adds the remote leg to the one harness with every result verified.
- `crates/hexput-tests/benches/cache/rhai.rs`: adjust `connect` only if the generalized `Client::new` needs it. It is the one client constructor.
- `AGENTS.md`:
  - Project Status: add Story 5.9 to the Epic 5 paragraph and mark Epic 5 code-complete. Update "Next step" and the test count (if it changed).
  - Build/Test: in the bench paragraph, name the `remote_path/*` figures, the TCP+TLS steady state, `tcp_tls_steady_state` in the JSON, and the loopback topology.

  This keeps the orientation file honest.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/.memlog.md`: add a dated line recording the decisions in the Design Notes. That is the decision log.
- `_bmad-output/implementation-artifacts/sprint-status.yaml`: set Story 5.9's status as the workflow advances.

**Acceptance Criteria:**
- **Separate figures.** Given the Epic 4 harness extended with the TCP+TLS leg, when `cargo bench -p hexput-tests --bench cache --locked` runs, then:
  - it prints `remote_path/tcp_tls_cold_first_run` and `remote_path/tcp_tls_warm_cache_hit` as figures separate from `cache_win/*`;
  - it prints a TCP+TLS steady state with its own rounds and median summary, separate from the UDS one, plus a table showing both legs' raw p50/p99/throughput side by side;
  - the JSON holds `steady_state` and `tcp_tls_steady_state` as separate sections, with no figure computed from both.
- **Topology recorded.** Given both legs, when the results are printed and written, then the conditions block and the JSON's `workload.legs` record each leg's transport and machine topology: UDS in-process on the same machine; TCP+TLS over loopback `127.0.0.1` on the same machine with its negotiated TLS version and cipher suite.
- **CI.** Given CI's `cargo test --workspace`, when the bench runs in quick mode, then the TCP+TLS leg runs over a real TLS connection with every verdict and `Call` count checked, and the self-check passes.

## Design Notes

Decisions (agent, 2026-10-22, unattended run):

1. **Loopback only, recorded as such.** The story's acceptance criteria require separate figures and a recorded topology, not a cross-machine run. A remote-host mode would need a Daemon the bench does not start and certificates it does not own. The TCP+TLS leg therefore measures the TCP stack plus TLS on one machine, and the report says plainly that this is not network latency. A cross-machine figure is left to whoever publishes one, and the topology field is where they would record it.
2. **The free port is pre-picked instead of read from the log.** Reading the bound address needs the `info` "listening on TCP with TLS" line, but the bench logs at `error` to keep logging off the measured path. The pick-then-bind race only fails loudly; it never yields a wrong number.
3. **One boxed client for every leg.** It keeps one verified `run` path for UDS, TCP+TLS and Rhai. The extra virtual call per read/write is the same on every leg.
4. **The UDS counterparts are not duplicated** inside `remote_path`, which keeps the bench time down. The Criterion figures are read by name (`cache_win/warm_cache_hit` vs `remote_path/tcp_tls_warm_cache_hit`), and the steady-state transport table puts them side by side.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes (no production change)
- `cargo test --workspace --locked --release` -- expected: all pass, including the bench's quick mode with the TCP+TLS leg
- `cargo bench -p hexput-tests --bench cache --locked -- remote_path` -- expected: the two `remote_path` figures and no report written

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/daemon.rs` (module `cache_win::remote_path`: `the_rule_over_tls_is_verified_for_every_student_with_ten_host_calls`, `cold_and_warm_runs_over_tls_agree_and_code_unregister_frees_the_handle`, `eight_concurrent_tls_connections_each_verify_every_run_of_the_rule`, `the_uds_and_tls_legs_answer_the_same_verified_verdict_for_every_student`, `the_daemon_listens_on_the_port_picked_just_before_start`, `a_port_taken_between_pick_and_bind_fails_the_daemon_with_exit_one`, `a_completed_handshake_names_its_version_and_suite_in_the_bench_format`, `a_client_that_trusts_no_root_is_refused_by_the_handshake`, `a_name_other_than_localhost_is_refused_by_the_handshake`, `the_bench_declares_the_remote_path_legs_steady_state_report_and_topology`, `the_bench_module_doc_names_the_remote_figures_and_the_loopback_topology`, `the_tcp_tls_steady_state_is_ordered_between_the_uds_and_rhai_legs_and_filtered_with_them`, `the_remote_figures_are_a_group_of_their_own_beside_the_uds_ones`, `the_rhai_server_still_builds_its_client_through_client_new`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 30 findings — high 0, medium 6, low 18, false 6, maybe-false 0
- findings:
  - `[low]` `[patch]` Leftover UDS-leg Sessions (300 s TTL) stay alive during the TCP+TLS leg — real but negligible on latency (a few extra DashMap entries); took the reviewer's second remedy and recorded the asymmetry in the module doc's new run-order sentence
  - `[low]` `[patch]` Run order (UDS, then TCP+TLS, then Rhai) confounds the TCP+TLS/UDS ratio — real; the module doc now says the legs run in sequence and the ratio carries drift between the runs as well as transport cost
  - `[medium]` `[patch]` The TLS handshake and connection setup are excluded from every TCP+TLS figure but the docs never say so — verified: `steady::measure` connects before the barrier and `remote_path` reuses one connection; the module doc and the transport table's reading now state that no figure includes the TCP connect or the TLS handshake
  - `[low]` `[patch]` `TRANSPORT_TABLE` reading says "above 1x … higher throughput (better)", wrong for the throughput row — verified (self-check sample 0.80x); reading rewritten per direction: above 1x on p50/p99 or below 1x on throughput is the stacks' cost; self-check assertion updated
  - `[low]` `[reject]` The negotiated TLS suite inside `workload()` drops the UDS and Rhai comparisons when the suite changes — real only after a rustls/provider bump, which happens rarely and also changes the binary measured; per-leg comparability would add branches to `compare`
  - `[low]` `[reject]` The steady-state local `warm` shadows the `warm` closure — real but harmless: a later call there fails to compile at once; renaming it (tried) makes rustfmt wrap the TCP+TLS `steady::measure` line and breaks pinned source-text tests, so more than a direct correction
  - `[medium]` `[patch]` The daemon.rs handshake test formats `{suite:?}` while the bench formats `suite.suite()`, so the bench's own string is unchecked — grouped with the verification-gap TLS-parameters finding; the test now formats `suite.suite()` and the bench's quick mode asserts its own `tls_parameters()` starts with `TLSv1_3, TLS13_`
  - `[low]` `[reject]` Many remote_path tests grep the bench source text instead of checking behaviour — real (they did break on a reformat during this pass), but a broken needle fails loudly and is cheap to fix; replacing them would weaken pre-written tests, and behaviour is now also checked by the quick-mode round trip
  - `[low]` `[reject]` `init_on`/`register_on`/`serve_run` duplicate existing test helpers — test-only drift risk; making the originals generic is a refactor, not a direct correction
  - `[low]` `[reject]` The port-race test stops at the Daemon's exit; a foreign TCP listener on the stolen port gives a different panic — the race needs another process to grab an ephemeral port within milliseconds; either way the run fails loudly and publishes no number, and guarding it needs a listen signal the bench does not have at `error` log level
  - `[false]` `[reject]` Spec says `Duplex` is a private trait but the code declares `pub trait Duplex` — the trait lives in the private module `wire` of a bench binary, so it is unreachable outside the bench; `pub` is needed because `Client::new` takes `impl Duplex` (else the private-bounds lint fails clippy)
  - `[low]` `[reject]` Read timeouts moved out of `Client::new` so a future constructor could forget one — every connect site sets 60 s today; a generic `Duplex` cannot set a timeout, so enforcing it needs new surface for a hypothetical caller
  - `[low]` `[defer]` AGENTS.md's Epic 5 summary skips Story 5.9 — fix edits an agent-context file, so deferred per rule (deferred item added); the finalization's required Project Status update added the 5.9 mention to that parenthetical
  - `[low]` `[reject]` The spec's Code Map line numbers are stale after the change — the fix is to edit this build's spec
  - `[low]` `[reject]` A foreign listener on the picked port makes the TLS connect succeed and hang up to 60 s — same race as the port-race row above: rare, loud, no wrong number; the guard needs a Daemon listen signal
  - `[low]` `[reject]` A loopback self-connect while nothing listens yet could look like a successful connect — needs the kernel to pick the target port as source during the few ms before the Daemon listens (~1 in 28k per attempt); the handshake then fails loudly; the guard adds a branch for a case not shown to occur
  - `[low]` `[reject]` `until_listening`'s assert drops the Daemon's stderr when the Daemon exits at startup — real but only on a startup failure; the thread is borrowed (`&self`), so surfacing stderr needs restructuring; `stop` already reports it
  - `[low]` `[patch]` The two handshake-refusal tests set no read timeout before `complete_io` — real hang risk if the server never answers; both now set a 10 s read timeout
  - `[medium]` `[patch]` The handshake-format parity between test and bench is coincidental — grouped with the TLS-parameters entry; fixed by the quick-mode assertion on the bench's own string and the test formatting `suite.suite()`
  - `[low]` `[reject]` Matrix claim: a stolen port makes `connect_tls` panic "the Daemon never listened", but a foreign listener gives a handshake panic — claim mismatch only in the frozen spec text; fixing it means editing this build's spec, and the behaviour (loud failure, no number) matches the matrix's error column
  - `[false]` `[reject]` `pub trait Duplex` contradicts the spec's "private trait" (edge-case claim) — same refutation as the blind-hunter row: unreachable outside the bench binary
  - `[medium]` `[patch]` `main`'s wiring of `compare` and the transport table never runs under `cargo test`; swapped `&Summary` arguments would go unnoticed — pre-verified gap; `main` now wires the legs only through `Steady::compare` and `report::render_transport` (both also used by `self_check`), and quick mode round-trips the run's own `to_json` through `Steady::compare`, asserting each leg's previous figures are its own median
  - `[medium]` `[patch]` The test for the TLS-parameters format checks its own copy, not the bench's `tls_parameters` — pre-verified gap; quick mode now asserts the bench's own `tls_parameters()` string starts with `TLSv1_3, TLS13_`
  - `[low]` `[reject]` (verification-gap other) Six remote_path tests only grep source text — same as the blind-hunter source-text row
  - `[low]` `[patch]` (verification-gap other) The transport table's reading is backwards for the throughput row — same group as the blind-hunter reading row; reading rewritten
  - `[false]` `[reject]` The diff goes past planning ("Halt after planning") into implementation — this workflow run assigned implementation and review stages; no relayed user instruction asks to halt after planning, and the status advance is the workflow's own
  - `[false]` `[reject]` "Remote path" may mean a cross-machine leg, but the diff measures loopback only — the story's acceptance criteria require separate figures and a recorded topology, not a cross-machine run; the diff records the loopback topology in conditions and `workload.legs`
  - `[false]` `[reject]` "When results are published" may expect a published results document — the acceptance criterion attaches topology to results when they are published; the bench's conditions block and JSON carry it, and no AC asks the story to publish figures
  - `[false]` `[reject]` The `TCP+TLS/UDS` ratio is a figure computed from both legs — the ratio is only a printed column beside both raw figures; `to_json` holds no ratio, and nothing averages or merges the legs, which is what the story forbids
  - `[medium]` `[patch]` Behavioural coverage of the reporting surface rests on the self-check, not on the bench's own wiring — grouped with the `main` wiring gap; the quick-mode round trip and the TLS-parameters assertion now exercise `main`'s own calls

## Auto Run Result

**Summary.** The `cache` bench gains a TCP+TLS leg against the same in-process Daemon: the Criterion group `remote_path` (`tcp_tls_cold_first_run`, `tcp_tls_warm_cache_hit`), a TCP+TLS steady state between the UDS and Rhai ones, a transport table showing both legs' raw p50/p99/throughput with a `TCP+TLS/UDS` ratio, per-leg transport and topology (`workload.legs`) with the negotiated TLS parameters, and a separate `tcp_tls_steady_state` JSON section compared on its own. No production crate changed.

**Files changed.**
- `crates/hexput-tests/benches/cache.rs` — TCP+TLS Daemon config, `connect_tls`/`tls_parameters`, transport-neutral `wire::Client`, `remote_path` group, TCP+TLS steady state, transport table, `legs`, JSON section, comparison, self-check, module doc; review: `Steady::compare`, `Steady::check_round_trip`, `report::render_transport`, quick-mode TLS-parameters assertion, corrected reading and docs.
- `crates/hexput-tests/benches/cache/rhai.rs` — builds its client through the generalized `Client::new`, setting its own read timeout.
- `crates/hexput-tests/tests/daemon.rs` — `cache_win::remote_path` tests; review: parity format `suite.suite()`, read timeouts on the refusal tests.
- `AGENTS.md` — Project Status (Epic 5 code-complete, Story 5.9, test count 1096) and the bench paragraph.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/.memlog.md` — the Design Notes decisions.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` — Story 5.9 `review`.

**Review findings.** 30 findings (medium 6, low 18, false 6). Patched: 7 entries — `main`'s leg wiring and behavioural coverage (medium), the bench's own TLS-parameters string (medium), the undocumented handshake exclusion (medium), the transport table's reading (low), the run-order documentation (low), the leftover-Session documentation (low), the refusal tests' read timeouts (low). Deferred: 1 (AGENTS.md Epic 5 parenthetical, addressed by the finalization update). Rejected: the per-leg comparability of the TLS suite, the `warm` shadowing, the source-text tests, the duplicated test helpers, the port-race/foreign-listener/self-connect/stderr edge cases, the `pub Duplex` claim, the read-timeout placement, the stale Code Map lines, and the four intent-alignment readings — each with its reason in the triage log.

**Follow-up review recommended: true.** Patched by verdict: medium 3, low 4. Named unverified risk: the full (non-quick) `cargo bench` path — `Steady::compare` against a real previous `cache-win.json` and the written report — was not run in this pass; only quick mode exercises the new wiring.

**Verification.** `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release` — 1096 passed, 0 failed, including the bench's quick mode (TCP+TLS leg over real TLS, round trip and TLS-parameters assertions).

**Residual risks.** Loopback figures only (no network hop); the source-text tests in `cache_win::remote_path` break on harmless reformatting; the pick-then-bind port race fails loudly but with a handshake message when a foreign listener takes the port.
