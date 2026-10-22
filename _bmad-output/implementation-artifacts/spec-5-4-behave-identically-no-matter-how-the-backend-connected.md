---
title: 'Story 5.4: Behave identically no matter how the backend connected'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: 'c89d71ea42ca4a7ac4afd997e760b7463b5cf734'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred:
  - summary: >-
      The conformance suite does not yet cover several envelope-level behaviours every Port shares:
      a reply too large to frame, a Backend `Error` reply to a `Call`, an `Authorize` answered
      `true` followed by its `Call`, several executions in flight on one connection, a reply
      carrying Value Secret holders and `modifications`, and `findings` under a check mode.
    evidence: |-
      Review finding (blind-hunter). The story's intent contract fixes the case set ("The full set
      is the I/O matrix below"), so adding rows needs a new intent. The frame-size path is where the
      adapters differ internally (WebSocket rejects an oversize send with `InvalidInput`, the byte
      streams in the shared framing), and completion-order replies would need a deterministic,
      timing-free construction. Settle by extending the matrix in a later Epic 5 story (5.5-5.8 add
      cases already) or a dedicated follow-up.
    location: >-
      crates/hexput-tests/tests/daemon/conformance.rs (CASES)
    severity: medium
  - summary: >-
      The Named Pipe conformance run (`conformance::named_pipe` and the Windows transcript
      equality) has never executed; it is compile-checked only.
    evidence: |-
      Review finding (intent-alignment). This machine cannot run Windows code; the `windows` CI job
      runs the `conformance` filter. Settled by one green run of that job, which must show
      `conformance::named_pipe` and `every_transport_on_this_platform_gives_the_same_transcript`
      passing.
    location: >-
      .github/workflows/ci.yml (windows job)
    severity: medium (unverified)
---

<intent-contract>

## Intent

**Problem:** FR-9 promises that the transport is a latency and topology choice, never a semantic one. Today each adapter story checked only an `Init` + `return 1 + 2;` round trip against UDS. Nothing proves that Cached Execution, host calls, capability denial, budget stops and the error shapes behave the same over TCP+TLS, WebSocket and the Named Pipe. AD-1's "no crate but the wiring root names a transport" is only half guarded: `check-crate-graph.py` pins who *depends* on `hexput-transport`, but no check stops a core crate from naming or branching on a transport.

**Approach:**
- Add one shared **conformance suite**, end to end against the real Daemon (`run_with`). It is a list of cases written against a transport-neutral `Client` trait, and it runs unchanged over every transport the platform serves. Adding a transport means one `Transport` impl plus one test line; no case changes.
- Each case asserts its own expected replies. Each transport's run also yields a normalized **transcript**, and one test asserts every platform transport's transcript is byte-identical.
- Extend `scripts/check-crate-graph.py` with a **transport-name guard** over production crates (AD-1), and reword the one core doc comment it would flag.

## Boundaries & Constraints

**Always:**
- **Where the suite lives.** Put it in `crates/hexput-tests/tests/daemon/conformance.rs`, included from `tests/daemon.rs` as `#[path = "daemon/conformance.rs"] mod conformance;`. It stays in the `daemon` test target: one `tests/<crate>.rs` per crate under test, and Cargo does not auto-discover a file in a subdirectory as a target. The module doc states the contract: what a case is, how to add a transport, and the normalization rules.
- **Harness, no third copy.**
  - Move `fixture`, `Daemon` (`start`, `log_with`, `bound_address`, `stop`), `tls` and `Wss` out of `#[cfg(unix)] mod tls_serving` into an ungated `conformance::harness`, and have `tls_serving` import them.
  - `Daemon::uds`, `Wss::send_text`, `Wss::vanish` and anything else unused off Unix become `#[cfg(unix)]`, so `scripts/check-windows.sh --release` has no dead code.
  - The pipe client reuses `named_pipe_serving`'s `pipe_name`/`open_once`/`retryable`, made `pub(super)`.
- **Transports.** `trait Transport { fn name(&self) -> &'static str; fn section(&self, sandbox: &Sandbox) -> String; fn connect(&self, daemon: &Daemon) -> Box<dyn Client>; }`.
  - Implement it for:
    - UDS (`#[cfg(unix)]`, `sandbox.socket()`);
    - TCP+TLS (`bind = "127.0.0.1:0"`, address from the `listening on TCP with TLS` line, the fixture CA client);
    - WebSocket (same, `WebSocket`);
    - Named Pipe (`#[cfg(windows)]`, a fresh `pipe_name()`, waiting until the name is served, retrying 231/`NotFound` for up to 10 s).
  - Each run starts its own Daemon whose System Config holds that transport's section **alone**, and stops it at the end. Stop must exit `0`.
- **Client.** `trait Client { fn send(&mut self, envelope: &Envelope<Value>); fn send_body(&mut self, body: &[u8]); fn reply(&mut self) -> Option<Envelope<Value>>; }`.
  - `send_body` sends one message body in the transport's own framing: a length-prefixed frame on a byte stream, one binary message on WebSocket.
  - `reply` has a 10 s bound where the client type allows it. The blocking pipe `File` has no read timeout; record that as accepted.
- **Cases.** Each case is `fn(&mut dyn Client, &mut Transcript)`. A case opens no connection itself: the runner hands it a fresh connection, and every case except `uninitialized` begins with the suite's one `Init`.
  - The `Init` payload is `{config: {}, registrations: [{name: "double", blanket: true}, {name: "ask"}]}`.
  - Every envelope sent and received is recorded in the `Transcript`, in order.
  - Every case asserts `message_type`, the correlation id and the payload fields its row names. For an `Error` that is `code`, plus `span` where the row names one.
  - The full set is the I/O matrix below, run in matrix order.
- **Normalization (transcript only, after the case's own assertions).**
  - A `client_id` value is asserted to be 32 lowercase hex characters and then becomes `"<client-id>"`.
  - A `handle` value is asserted to be 16 lowercase hex characters and then becomes `"<handle>"`.
  - Any string matching `^hx:[0-9a-f]{16}:` (a generated Reference ID) has its 16-hex nonce replaced by `*`.
  - Nothing else is rewritten. The transcript stores each envelope as its `hexput_port::encode` bytes, after normalization.
- **Tests (in `conformance`).**
  - One `#[test]` per transport, named exactly `uds`, `tcp_tls`, `websocket`, `named_pipe`, cfg-gated as above. Each runs the whole suite and passes only if every case passes.
  - `every_transport_on_this_platform_gives_the_same_transcript` runs the suite on each transport the platform serves: UDS, TCP+TLS and WebSocket on Unix; Named Pipe, TCP+TLS and WebSocket on Windows. It asserts all transcripts are equal. On a mismatch, the panic names the two transports, the case, the index, and both envelopes decoded (`{:?}`).
  - A `macro_rules!` (or a `const TRANSPORTS` list) is the single place a transport is added.
- **AD-1 guard (`scripts/check-crate-graph.py`).**
  - Add `TRANSPORT_NAMES`: case-insensitive, matching the words `uds`, `tcp`, `tls`, and the spellings `websocket`, `named pipe`/`named_pipe`/`namedpipe`, `unix domain`, `unixstream`, `unixlistener`, `tcpstream`, `tcplistener`, `rustls`, `tungstenite`.
  - Scan every `*.rs` file (comments and docs included, like `RESTRICTED_NAMES`) and the `Cargo.toml` of every workspace crate except:
    - `hexput-transport` (the adapters);
    - `hexput-daemon` (the wiring root, its sole dependent);
    - `hexput-config` (System Config owns the `[transport.*]` sections, AD-7);
    - `TEST_CRATES`.
  - Each hit is a failure naming the crate, the file:line and the AD-1 reason. Count the guard in the "Crate graph OK" summary.
  - `hexput-transport`'s `SOLE_DEPENDENTS` entry already enforces "only `hexput-daemon` depends on it". Keep it unchanged.
  - Reword `crates/hexput-port/src/lib.rs`'s "A message-oriented transport (WebSocket, Epic 5)" so it names no transport. That is today's only hit.
- **CI.** The `windows` job's test step becomes `cargo test --locked -p hexput-tests --test transport --test daemon --test config -- named_pipe conformance`. libtest takes several filters, so the whole suite runs on Windows. Update its comment to match.
- **Disk:** every cargo command on this machine runs with `--release`.

**Never:**
- A change to any production crate's behaviour. This story adds tests and a guard. If a case shows a transport-dependent difference, that is a bug: fix it in the adapter, never by special-casing the suite, and log it in Implementation Notes.
- A case that names, branches on, or skips by transport. A per-transport expectation is not allowed either.
- Transport-specific protocol checks (plaintext probes, text messages, oversized prefix, socket mode). They stay in their transport's own tests. The suite covers only envelope-level behaviour every `Port` must share.
- Timing assertions, CPU-time stops, or anything whose outcome depends on load. Budget cases use the counted dimensions only.
- Reconnect, credentials, several Connections per Client ID, or TTL (Stories 5.5–5.8). The suite gains those cases when they land.
- A new dependency, a crate edge, or a new `[[test]]` target.

## I/O & Edge-Case Matrix

Each row is one case, and each runs on its own fresh connection. Ids are the client's. "Init" means the suite `Init` (id 1) was answered first with `Result {client_id}`.

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| `init` | the suite `Init` (id 1) | `Result` id 1, payload exactly `{client_id}` | — |
| `uninitialized` | `ExecutionStart` id 1 before any `Init` | `Error` id 1, `protocol.init_not_completed`; the connection keeps serving, and a following `Init` (id 2) succeeds | — |
| `init_twice` | Init, then the same `Init` with id 2 | `Error` id 2, `protocol.already_initialized` | — |
| `direct` | Init; `ExecutionStart` id 2 `{source: "return a + 2;", variables: {a: 1}}` | `Result` id 2, exactly `{value: 3}` | — |
| `script_error` | Init; id 2 `"return missing;"`; id 3 `"return (;"` | id 2: `Error` `reference.undeclared_identifier` with its span. id 3: the parser's `syntax.*` error with its span. Severity, category and message are carried in the transcript | — |
| `host_call` | Init; id 2 `"return double(21);"` | The Daemon sends `Call` (a Daemon id) with `name: "double"`, `execution: 2` and one argument. The client answers `Result {value: 42}` under the `Call`'s id. The reply is `Result` id 2 `{value: 42}` | — |
| `cached` | Init; `CodeRegister` id 2 `{source: "return n * 2;", variables: ["n"]}`; `CachedExecutionStart` id 3 `{handle, variables: {n: 21}}`; `CodeUnregister` id 4 `{handle}`; `CachedExecutionStart` id 5 again | id 2 `{handle}`; id 3 `{value: 42}`; id 4 `{}`; id 5 `Error` `protocol.unknown_script_handle` | — |
| `config_update` | Init; `ConfigUpdate` id 2 `{config: {budget: {rpc_calls: 0}}}`; id 3 `"return double(1);"` | id 2 `Result {}`; id 3 `Error` `budget.rpc_calls_exceeded`, and no `Call` is sent | — |
| `capability_unregistered` | Init; id 2 `"return nope(1);"` | `Error` id 2 `capability.unknown_function`, spanned on the call; no `Call` or `Authorize` is sent | — |
| `capability_denied` | Init; id 2 `"return ask(1);"` | The Daemon sends `Authorize` with `name: "ask"`. The client answers `Result {value: false}`, and then `Error` id 2 `capability.unknown_function`; no `Call` is sent | — |
| `budget` | Init; id 2 `"return double(1);"` with `overrides: {budget: {rpc_calls: 0}}`; id 3 `"return [1, 2];"` with `overrides: {budget: {allocations: 0}}` | id 2 `budget.rpc_calls_exceeded` (no `Call`); id 3 `budget.allocations_exceeded` | — |
| `invalid_payload` | Init; `ExecutionStart` id 2 `{source: "return 1;", variables: {}, overrides: {budget: {rpc_calls: 200000}}}`; id 3 `{source: 1}` | Both are `Error` `protocol.invalid_payload`, with the message naming the path | — |
| `malformed` | Init; `send_body(b"\xc1")`, which is never valid MessagePack; then `ExecutionStart` id 3 `"return 1;"` | `Error` with nil id, `protocol.malformed_frame`; then `Result` id 3 `{value: 1}`. The connection keeps serving | — |
| `unknown_type` | Init; a body encoding `{id: 4, type: "Bogus", payload: {}}` | `Error` id 4, `protocol.unknown_message_type` | — |
| `unexpected_reply` | Init; a Backend `Result` id 99 that no pending call names | `Error` id 99, `protocol.unexpected_message` | — |
| Transcript equality | the suite on every platform transport | the transcripts are byte-equal after normalization | mismatch → panic naming the transports, case and envelopes |
| AD-1 guard hit | a production crate other than transport/daemon/config contains `WebSocket` (or a `rustls` dependency) | `check-crate-graph.py` exits 1, naming the crate, the file:line and AD-1 | — |

</intent-contract>

## Code Map

- `crates/hexput-tests/tests/daemon.rs`:
  - `Sandbox`, `SharedLog`, `Run`, `nothing` (lines 19–130) are platform-independent and are reused as `super::…`.
  - `mod tls_serving` (~2616): `fixture`, `Daemon` (~2667), `tls` (~2777), `mod frames` (~2803, `send`/`reply` over any `Read + Write`) and `Wss` (~3146). Move these into `conformance::harness`, and have `tls_serving` import them back.
  - `named_pipe_serving` (~3678): `pipe_name`, `open_once`, `retryable` and `ERROR_PIPE_BUSY` for the pipe client.
  - The top-level `init_envelope`/`execution_envelope` (~3570) may be reused.
- `crates/hexput-tests/tests/daemon/conformance.rs` (new) -- the suite, the harness, the transports and the tests.
- `crates/hexput-shared/src/wire.rs` (~54) -- the payload contracts per `MessageType`: `CodeRegister` → `{handle}`, `CachedExecutionStart`, `CodeUnregister` → `{}`, `ConfigUpdate` → `{}`, `Call`/`Authorize` payloads. Read-only.
- `crates/hexput-port/src/error.rs` (`ErrorBody::to_value`, ~202) -- the error payload keys are `severity`, `category`, `code`, `message`, `span{offset,len,line,column}`, plus optional `findings`. Read-only.
- `crates/hexput-port/src/lib.rs:9` -- the only core text the new guard flags. Reword it.
- `crates/hexput-tests/tests/connection.rs` -- examples of `Call`/`Authorize` handling and holder-shaped arguments (`{__secret: {ref}, value}`, ~3449). A scalar argument may arrive as a holder with a generated `hx:` ref, which is why the nonce is normalized.
- `scripts/check-crate-graph.py` -- `RESTRICTED_NAMES`/`restricted_name_uses` (~140–200) is the model for the new scan. `SOLE_DEPENDENTS` already holds `hexput-transport → {hexput-daemon}`. A grep today finds transport names only in `hexput-transport`, `hexput-daemon`, `hexput-config`, `hexput-tests` and `hexput-port/src/lib.rs:9`. No other crate manifest mentions one.
- `.github/workflows/ci.yml` -- the `windows` job's "Named Pipe tests" step.
- `scripts/check-windows.sh` -- the cross-lint that must stay clean.
- `crates/hexput-tests/tests/daemon.rs` `the_crate_graph_check_still_passes_with_rhai_added` (~2153) -- it already runs the graph script from a test, so the new guard is exercised by `cargo test` too.

## Tasks & Acceptance

**Execution:**
- `crates/hexput-tests/tests/daemon/conformance.rs` -- harness (moved from `tls_serving`), `Client`/`Transport`, one transport impl each, every matrix case, normalization, the four per-transport tests and the transcript-equality test.
- `crates/hexput-tests/tests/daemon.rs` -- add the `#[path]` module, remove the moved items from `tls_serving` and import them, make the pipe helpers `pub(super)`, and gate unix-only helpers.
- `scripts/check-crate-graph.py` -- add `TRANSPORT_NAMES` and its scan over `*.rs` and `Cargo.toml`, with the exemptions and the summary count.
- `crates/hexput-port/src/lib.rs` -- reword the doc line so it names no transport.
- `.github/workflows/ci.yml` -- add the `conformance` filter to the windows test step, and update its comment.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- add a dated amendment `[Amended 2026-10-22, Epic 5 Story 5.4]` after Story 5.3's. It records:
  - the conformance suite as FR-9's proof and the extension rule (a new adapter adds one `Transport` impl);
  - the transport-name guard and its exemptions (transport, daemon, config);
  - that no crate edge changed.
- `AGENTS.md` -- Project Status: add Story 5.4 to the Epic 5 notes, and update "Next step" and the test count.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` -- `5-4-…` → `review` at finalization.

**Acceptance Criteria:**
- Given the Daemon serving one transport, when the conformance suite runs over it (UDS, TCP+TLS or WebSocket here; the Named Pipe in the `windows` CI job), then every case passes with the same results and error codes (FR-9).
- Given every transport this platform serves, when `every_transport_on_this_platform_gives_the_same_transcript` runs, then their normalized transcripts are byte-identical.
- Given the workspace, when `python3 scripts/check-crate-graph.py` runs, then it passes, and it fails naming the crate and line if any production crate but `hexput-transport`/`hexput-daemon`/`hexput-config` names a transport, or if any crate but `hexput-daemon` depends on `hexput-transport` (AD-1).
- Given a future adapter implementing `Port`, when it is wired into the Daemon, then running the suite over it needs one `Transport` impl and one test entry, and no case changes. The module doc states this, and no case mentions a transport.

## Design Notes

These decisions were taken unattended, as the most defensible readings:

1. **End to end, not at the `Port`.** The AC says "run against UDS, Named Pipe, TCP+TLS and WebSocket", which is the Backend's view. Only the Daemon path exercises TLS, the WebSocket upgrade, the framing and `serve` together. An in-memory `Port` already covers `serve` in `tests/connection.rs`. "Runs unchanged against a new adapter" is met by structure: cases see only `Client`.
2. **"No matches" excludes the composition root and System Config.** `hexput-daemon` must name the adapters it binds, and `hexput-config` must parse `[transport.*]`. Neither branches on a live connection's transport. The literal reading would forbid wiring and configuring transports at all. Every *core* crate (session, connection, script, exec, port, …) is held to zero matches, comments included, because a doc that names a transport is where a branch starts.
3. **Assertions per case plus transcript equality.** Per-case assertions make a failure readable. Transcript equality catches any difference the assertions did not name: an extra field, ordering, an id, a span. Normalization touches only values that are random or counter-issued by design.
4. **Budget cases use counted dimensions** (`rpc_calls`, `allocations`). These are deterministic. A CPU or memory stop would make the suite load-sensitive.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: OK, with the guard counted. Temporarily adding the word `WebSocket` to a comment in `crates/hexput-session/src/lib.rs` makes it exit 1 naming that file:line. Revert afterwards.
- `scripts/check-windows.sh --release` -- expected: clean
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass, including `conformance::{uds,tcp_tls,websocket,every_transport_on_this_platform_gives_the_same_transcript}`
- `git diff --stat Cargo.lock` -- expected: no change

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/daemon/conformance.rs` and `crates/hexput-tests/tests/daemon.rs` (`conformance::{uds, tcp_tls, websocket, named_pipe (Windows), every_transport_on_this_platform_gives_the_same_transcript, no_case_names_a_transport}`; the fifteen matrix rows run inside each transport test). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Test-agent decisions, to be re-checked by the implementer:
  - `unexpected_reply` asserts a **nil** id, not id 99 as the matrix row says. The Story 3.1 contract (`tests/connection.rs`, `a_stray_reply_after_init_is_an_unexpected_message`) refuses a stray reply with a nil id, and the row contradicts it. The code is what the row checks.
  - `invalid_payload`'s `{source: 1}` (no `variables`) accepts a message naming `source` or `variables`, since the decoder reports the missing `variables` first.
  - Moving the harness out of `tls_serving` also needed: `Daemon.socket` (unix only, for the UDS transport), `Wss::send_bytes` (a raw binary body), a 10 s bound on `Wss::reply`, and `frames::init`/`frames::request` gated to unix.
  - `no_case_names_a_transport` reads the case section of `conformance.rs` between its `BEGIN CASES`/`END CASES` markers. The AD-1 graph guard itself has no test here; its failing path is the manual check in Verification.
  - Windows is not verified here: `clang-cl` is missing, so `scripts/check-windows.sh` and the Windows target cannot build (`ring`/`alloca` build scripts). The `cfg(windows)` code was reviewed by reading only.
- Implementer decisions (Story 5.4, unattended):
  - The pre-written suite compiled and passed as delivered; no transport-dependent difference was found, so no adapter changed.
  - `unexpected_reply`: kept the nil-id expectation. The matrix row's "id 99" contradicts the pinned Story 3.1 contract (`hexput-connection`'s module doc and `tests/connection.rs`), and changing it would change production behaviour, which this story's Never list forbids. The row's intent (a stray reply is `protocol.unexpected_message` and the connection keeps serving) is what the case checks; the id discrepancy is recorded here for review rather than treated as a blocking ambiguity.
  - The transport-name regex is case-insensitive: `\b(uds|tcp|tls)\b`, `websocket`, `named[ _]?pipe`, `unix\s+domain`, `unixstream`, `unixlistener`, `tcpstream`, `tcplistener`, `rustls`, `tungstenite`. Today's only hit was `hexput-port/src/lib.rs:9`, reworded to "a message-oriented transport, whose own framing already delimits each message".
  - Beyond the manual check in Verification, `conformance::the_transport_name_guard_names_the_crate_and_line` (unix) runs the real script over a throwaway dependency-free workspace and pins the guard's two failure lines (a source comment and a manifest line naming the crate, the file:line and AD-1), and that exempt crates are spared, so the AD-1 matrix row has an automated test.
  - `scripts/check-windows.sh --release` does run on this machine now; it found an unused-import warning (`CorrelationId`/`MessageType` in `harness`, used only by the unix-gated `frames::init`/`request`), fixed by gating that import to unix. The Named Pipe transport is compile-checked only; it executes only in the `windows` CI job.
  - The transport list is the `served()` function per platform plus one `#[test]` per transport (the spec allows a `const` list in place of a macro).
  - `sprint-status.yaml` is set to `review` with the spec's `in-review`.

## Spec Change Log

No spec amendment in this review: no finding routed to `bad_spec`.

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 40 findings — high 3, medium 9, low 22, false 5, maybe-false 1
- findings:
  - `[high]` `[patch]` (group A: case-section scan) blind-hunter: `no_case_names_a_transport` scans only its own text, because `find` stops at the marker literals inside the test — confirmed. The markers are now built with `concat!("// BEGIN", " CASES")`, and the test asserts the scanned section holds `const cases`, `fn init(` and `fn unexpected_reply(`.
  - `[medium]` `[patch]` (group B: transport-name guard too narrow) blind-hunter: `\b(?:uds|tcp|tls)\b` misses snake_case and CamelCase identifiers (`tcp_tls`, `TcpTlsPort`, `UdsInbound`, `is_tls`) — confirmed with the regex. The guard now splits each line into identifier parts (`[A-Z]+(?![a-z])|[A-Z]?[a-z]+`) and matches `uds`/`tcp`/`tls`/`wss` per part, plus compounds with an optional space, `_` or `-` (`web-socket`, `Named Pipe`, `UnixSocket`, `UnixDatagram`) and `ws://`/`wss://`. The workspace still passes.
  - `[low]` `[patch]` blind-hunter: the guard's self-test tried only a comment and a manifest comment. The throwaway crate now has a real (path) `rustls = { package = … }` dependency line, `pub struct TcpTlsPort;` and `pub fn uds_path()`, and the test pins all five hits, each with its file:line.
  - `[medium]` `[patch]` (group C: nothing checked after a case's last reply) blind-hunter: an extra or repeated message after a case's last expected reply is never read, so neither the assertions nor transcript equality see it. `run` now calls `quiet` after every case: it sends a sentinel `ExecutionStart` (id 1000, `return 0;`) and asserts the very next message is its `Result {value: 0}`. The sentinel is recorded in the transcript.
  - `[medium]` `[patch]` blind-hunter: the suite and its docs claim every transport behaves identically, but Story 5.2 documents one difference: a WebSocket cannot send replies still owed after the peer's Close or a TLS `close_notify`. The module doc ("The one known difference"), the Spine amendment and the AGENTS.md status now state that exception and why no case half-closes with work in flight.
  - `[medium]` `[defer]` blind-hunter: envelope-level behaviours missing from the matrix (response too large, Backend `Error` reply, `Authorize` true, concurrent executions, holders and `modifications`, `findings`). The intent contract fixes the case set ("The full set is the I/O matrix below"), so this is new scope. Deferred, see frontmatter and deferred-work.md.
  - `[low]` `[patch]` blind-hunter: span assertions were weak (the syntax span was discarded; `starts_with("nope")`). `span_text` now also checks `line`/`column` against `offset`. `script_error` pins the parser's span to `;`, and `capability_unregistered` pins the exact text `nope(1)`.
  - `[low]` `[reject]` blind-hunter: the matrix row `unexpected_reply` still says id 99 while the case asserts nil. The fix edits this build's spec (the matrix is in the frozen intent contract). The Implementation Notes already record why the case follows the pinned Story 3.1 contract.
  - `[low]` `[patch]` blind-hunter: epic-5-context.md said the `windows` job's filter is "today `named_pipe`". It now names both filters, `named_pipe` and `conformance`.
  - `[low]` `[patch]` blind-hunter: a hang in the Windows pipe client has no limit short of GitHub's 6-hour default. The `windows` job now has `timeout-minutes: 45`, with a comment saying why.
  - `[low]` `[reject]` blind-hunter: `Daemon::uds(&self, socket)` takes a path although `Daemon.socket` exists, and `socket` is set on TCP- or WebSocket-only Daemons. Six `tls_serving` callers pass `sandbox.socket()`, which is the same path, so no caller misbehaves. Changing it to `Option` adds branches for no observed harm.
  - `[low]` `[reject]` blind-hunter: `Wss::reply` turns a stream error into `None`, so the panic reads "the Daemon closed the connection". The Daemon sends no Close frame of its own (Story 5.2), so a TCP end without a Close frame arrives as an error and must stay `None`. Telling the two apart adds branches, and only a failure message would improve.
  - `[low]` `[reject]` blind-hunter: the transcript-equality test reruns each transport's suite (six Daemon starts instead of three). The whole conformance target takes about 0.14 s in release, and sharing runs across tests would need global state.
  - `[high]` `[patch]` (group A) edge-case-hunter: `find` returns the marker literal inside the test, so about two lines are scanned. Fixed as in group A.
  - `[medium]` `[patch]` (group B) edge-case-hunter: snake_case and CamelCase identifiers (`tls_config`, `uds_path`, `TcpTls`, `is_tls`) escape the guard. Fixed as in group B.
  - `[medium]` `[patch]` (group B) edge-case-hunter: `web-socket`, `Web Socket`, `named-pipe`, `ws`/`wss` and a bare Unix socket escape the guard. Fixed as in group B. The URL forms `ws://`/`wss://`, the `wss` identifier part and `unix` + socket/stream/listener/datagram/domain are now matched; a bare `ws` word stays unmatched because it is too common to be a transport name.
  - `[low]` `[patch]` edge-case-hunter: only the first hit on a line was reported. `transport_names_in` now reports every hit in order, and counts a short name inside a longer match once.
  - `[false]` `[reject]` edge-case-hunter: a non-UTF-8 source file crashes the script. Every scanned file is a `*.rs` or `Cargo.toml` of a crate that builds, and rustc and Cargo both require UTF-8, so the crash cannot happen in a workspace that passes the build step.
  - `[low]` `[patch]` edge-case-hunter: a `client_id` that is not a string went unasserted, because `normalize` checks only strings. `init_result`, which every `Init` goes through, now asserts that `client_id` is a string of 32 lowercase hex characters.
  - `[low]` `[reject]` edge-case-hunter: the pipe connect loop does not stop early when the Daemon thread has exited. It is bounded at 10 s and panics naming the pipe and error. Startup failures here come only from the suite's own fixed config, and the fix needs a new accessor on `Daemon`.
  - `[low]` `[reject]` edge-case-hunter: a Daemon that fails before its listening line is reported as "never logged …". `log_with` already includes the whole log and stops early when the thread ends. stderr matters only for a usage error, which the fixed test config cannot produce.
  - `[low]` `[reject]` edge-case-hunter: when a case panics, the Daemon is dropped without `stop()`, so its log is lost from the message. Dropping the stop sender still shuts it down. Surfacing the log needs a new accessor, and the case's own assertion message already names the case and the transport.
  - `[low]` `[reject]` edge-case-hunter (claim): the matrix row says id 99 while the suite pins nil. Same as the blind-hunter row: the fix edits the spec's frozen matrix.
  - `[high]` `[patch]` (group A) edge-case-hunter (claim): the AC's "no case mentions a transport" was unenforced. Fixed as in group A.
  - `[medium]` `[patch]` (group B) edge-case-hunter (claim): the guard misses common identifier forms. Fixed as in group B.
  - `[low]` `[patch]` edge-case-hunter (claim): a new adapter also needed a `served()` line beside its `#[test]`, against the spec's "a `macro_rules!` (or a `const TRANSPORTS` list) is the single place". `transports!` now generates both the per-transport `#[test]` and `served()` from one `test => transport` line per transport, and the module doc and Spine amendment say so.
  - `[medium]` `[patch]` (group C) verification-gap: transcript equality never reads messages after a case's last expected reply. Fixed with the sentinel `quiet` check, as the layer proposed.
  - `[medium]` `[patch]` (group B) verification-gap: `TRANSPORT_NAMES` misses `conn.is_tls()`, `uds_path`, `on_tcp_connect`, `TcpTlsPort`. Fixed as in group B; the self-test now pins identifier spellings.
  - `[low]` `[reject]` verification-gap: the matrix row says id 99. Same as above: the fix edits the spec.
  - `[false]` `[reject]` intent-alignment: the diff goes beyond "Halt after planning". That directive bound the run's planning stage. This build's later stages (implement, review, finalize) were assigned explicitly by the orchestrating workflow.
  - `[false]` `[reject]` intent-alignment: UDS and the Named Pipe are never compared in one transcript-equality run. They are never served on the same platform (UDS is Unix-only, the pipe Windows-only, Story 5.3). Each platform compares its local transport against TCP+TLS and WebSocket, which are shared by both.
  - `[maybe-false]` `[defer]` intent-alignment: the Named Pipe run has never executed. If it fails, the severity would be medium. A green `windows` CI job settles it. Deferred, see frontmatter.
  - `[low]` `[reject]` intent-alignment: budget is covered only for the counted dimensions. The intent contract's Never list excludes CPU-time stops and load-dependent outcomes on purpose.
  - `[low]` `[reject]` intent-alignment: the `unexpected_reply` case departs from the matrix. Same as the id-99 rows.
  - `[low]` `[reject]` intent-alignment: the suite drives the Backend's client surface, not something generic over `Port`. The intent contract settles this in Design Note 1 (end to end, not at the `Port`); `tests/connection.rs` already covers `serve` with an in-memory `Port`.
  - `[low]` `[reject]` intent-alignment: the guard exempts the daemon and config crates as well as transport. The intent contract settles this in Design Note 2 and the Boundaries.
  - `[low]` `[reject]` intent-alignment: the guard detects names, not branching that names no transport. The intent contract defines the guard as a name scan. Branching on a peer kind without naming one would first need a type the core cannot reach, because `SOLE_DEPENDENTS` keeps `hexput-transport` out of every core crate.
  - `[false]` `[reject]` intent-alignment: the dependency clause holds only over normal dependencies. AGENTS.md makes the rules about normal dependencies on purpose ("Crates under test are dev-dependencies, never normal ones"), and no production crate depends on `hexput-transport` but `hexput-daemon`.
  - `[false]` `[reject]` intent-alignment: the guard's failure path is tested over a throwaway workspace rather than the real one. The real workspace is checked by the script itself in CI (its pass path), and the throwaway workspace exists only to exercise the failure path without editing production code. Both paths are covered.
  - `[low]` `[reject]` intent-alignment: the Implementation Notes contradict themselves about the guard test. The fix edits this build's spec. The later note (the test exists) is the accurate one.

## Auto Run Result

**Summary.** Story 5.4 adds the FR-9 conformance suite (`crates/hexput-tests/tests/daemon/conformance.rs`). It has fifteen matrix cases, end to end against `run_with`, written against a transport-neutral `Client`, run per transport (`uds`, `tcp_tls`, `websocket`; `named_pipe` on Windows) and compared by normalized transcript. It also adds the AD-1 transport-name guard to `scripts/check-crate-graph.py`. The review pass then made several repairs:
- `no_case_names_a_transport` now actually scans the cases.
- The guard now catches identifier and compound spellings and reports every hit.
- A sentinel after each case proves nothing extra arrived.
- Spans are pinned exactly, including line and column.
- The `client_id` type is asserted.
- `transports!` is the single place a transport is listed.
- The one known WebSocket difference is documented.
- The Windows CI job is time-bounded.

**Files changed.**
- `crates/hexput-tests/tests/daemon/conformance.rs` — the suite: harness, `Client`/`Transport`, transports, `transports!`, cases, normalization, the `quiet` sentinel, the guard self-test.
- `crates/hexput-tests/tests/daemon.rs` — includes the module; `tls_serving` imports the moved harness; the pipe helpers are `pub(super)`.
- `scripts/check-crate-graph.py` — the `TRANSPORT_NAMES`/`IDENTIFIER_PARTS` guard and `transport_names_in`.
- `crates/hexput-port/src/lib.rs` — one doc line reworded so it names no transport.
- `.github/workflows/ci.yml` — the `windows` job runs the `conformance` filter, with `timeout-minutes: 45`.
- `_bmad-output/planning-artifacts/architecture/.../ARCHITECTURE-SPINE.md` — the Story 5.4 amendment.
- `_bmad-output/implementation-artifacts/epic-5-context.md` — the Windows verification line.
- `_bmad-output/implementation-artifacts/deferred-work.md` — two deferrals.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` — 5-4 → `review`.
- `AGENTS.md` — Project Status.

**Review findings.** 40 findings.
- Patches: 18 rows, in 3 grouped entries plus 8 single findings.
  - group A (high, 3 rows), group B (medium, 6 rows), group C (medium, 2 rows);
  - singles: the known-difference doc (medium), and the guard self-test, span exactness, epic context line, CI timeout, every hit per line, `client_id` type and `transports!` (all low).
- Deferred: 2 (missing envelope-level cases, medium; the Named Pipe run, maybe-false, medium if true).
- Rejected: 20, each with its reason in the triage log above: four id-99 rows (the fix edits the frozen spec), four intent-contract decisions, one Implementation Notes wording (a spec edit), five `false`, and six harness niceties whose fixes add surface for no observed harm.

**Follow-up review recommended: true.** This first pass patched a `high` (group A) and four `medium`-level patch entries (groups B and C, the known-difference doc). Patched by verdict: high 1 entry (3 rows), medium 3 entries (9 rows), low 7. The unverified risk is the Windows half: the `transports!` macro's `cfg` expansion, the sentinel and the exact spans have run only on Linux, and `conformance::named_pipe` has never executed. A green `windows` CI job is the remaining check.

**Verification (all with `--release`, this machine).**
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings`: clean.
- `python3 scripts/check-crate-graph.py`: OK, with the guard counted. A temporary `// a WebSocket peer, see TcpTlsPort` line in `hexput-session/src/lib.rs` made it exit 1 naming `WebSocket`, `Tcp` and `Tls` at that file:line, and was reverted.
- `scripts/check-windows.sh --release`: clean.
- `cargo build --workspace --all-targets --locked --release`: success.
- `cargo test --workspace --locked --release`: 1008 passed, 0 failed.
- `git diff --stat Cargo.lock`: no change.

**Residual risks.**
- Windows execution is unverified (deferred).
- The name guard is lexical. Branching that names no transport is outside it, though `SOLE_DEPENDENTS` keeps every transport type out of the core crates.
- The suite does not cover a half-closed connection with work in flight, the one documented WebSocket difference.
