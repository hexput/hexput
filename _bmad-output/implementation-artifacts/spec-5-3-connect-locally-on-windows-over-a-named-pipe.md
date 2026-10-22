---
title: 'Story 5.3: Connect locally on Windows over a Named Pipe'
type: 'feature'
created: '2026-10-22'
status: 'done'
baseline_revision: '2c632d3a0867d36eaddee8cc628e98768b5cf727'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-5-context.md'
warnings: ['oversized']
deferred:
  - summary: >-
      The Named Pipe has no way to let another account (a Backend's service account) open it read/write, which the Windows service install of Story 7.6 will need.
    evidence: |-
      The default security descriptor gives read/write only to LocalSystem, Administrators and the creator owner. Once Story 7.6 installs the Daemon as a Windows service (LocalSystem or a dedicated account), an ordinary Backend service account cannot use the Windows-native local transport. UDS has `mode` for this case. This story's intent excludes a DACL option, so it is deferred and tied to Story 7.6: add a `[transport.named_pipe]` option naming the accounts or an SDDL string, applied through `ServerOptions::create_with_security_attributes_raw`.
    location: >-
      crates/hexput-transport/src/named_pipe.rs (options)
    severity: medium
  - summary: >-
      epic-5-context.md does not record Story 5.3's decisions (platform rule, pipe policy, the windows CI job) for Stories 5.4-5.8.
    evidence: |-
      The context file gained Story 5.1/5.2 facts only. Stories 5.4 (conformance) and 5.5-5.8 (reconnect, caps) build on the platform rule (UDS Unix-only, the pipe Windows-only, an unservable section skipped with a warning, nothing servable fails startup), the pipe policy (byte mode with the shared framing, first instance only, local only, default descriptor, ERROR_PIPE_BUSY retry duty) and the new windows CI job. The Spine amendment and AGENTS.md carry them meanwhile; refresh the context file when Story 5.4 is planned.
    location: >-
      _bmad-output/implementation-artifacts/epic-5-context.md
    severity: low
---

<intent-contract>

## Intent

**Problem:** On Windows the Daemon refuses to start at all (`cfg(not(unix))` `serve` returns "no transport is served on this platform yet"). A Backend on the same Windows machine has no local IPC path, and could not even use TCP or WebSocket, although both adapters are platform-independent. FR-9 makes the Named Pipe the Windows-native equivalent of UDS. The Story's AC also says a non-Windows host must start cleanly without the Named Pipe adapter.

**Approach:**
- Add a Windows-only adapter, `hexput_transport::named_pipe`, implementing the existing `Port` over tokio's `NamedPipeServer`. It reuses the shared length-prefixed framing (`framed.rs`), exactly as UDS and TCP+TLS do.
- Add `[transport.named_pipe] path = '\\.\pipe\<name>'` to System Config. It is parsed and validated identically on every platform.
- Make the Daemon's serving path platform-independent:
  - UDS is served only on Unix, the Named Pipe only on Windows, TCP and WebSocket everywhere.
  - A configured transport this platform cannot serve is skipped with a `warn` log.
  - If nothing servable is left, startup fails.
- Named Pipe code cannot run on this Linux machine, so it is checked two ways:
  - locally, by a cross-target clippy script (`scripts/check-windows.sh`);
  - on real Windows, by a new CI job that runs the Named Pipe tests.

## Boundaries & Constraints

**Always:**
- **The adapter (`crates/hexput-transport/src/named_pipe.rs`, `#[cfg(windows)] pub mod named_pipe;`).**
  - `NamedPipeListener::bind(path: &str) -> Result<NamedPipeListener, BindError>` must be called within a Tokio runtime. It creates the first instance with `ServerOptions::new()`, `first_pipe_instance(true)`, `reject_remote_clients(true)` and `pipe_mode(PipeMode::Byte)`. Every other option keeps tokio's default, which includes unlimited instances and the system's default security descriptor.
  - With `first_pipe_instance(true)`, an existing pipe of that name (another Daemon, or a squatter) fails the first instance with `PermissionDenied`. That error maps to `BindErrorKind::InUse`, and any other error to `BindErrorKind::Io { action: "create the pipe", error }`.
  - `BindError { path: String, kind }` is the module's own error type. Its `Display` gives:
    - `{path}: pipe already in use: another process is serving it`
    - `{path}: cannot {action}: {error}`
  - `accept(&mut self) -> io::Result<NamedPipePort>` is cancel-safe. Tokio documents `NamedPipeServer::connect` as cancel-safe.
    1. It awaits `connect()` on the current instance.
    2. It immediately creates the next instance with the same options but `first_pipe_instance(false)`, and returns the connected one.
    3. If creating the next instance fails, the connected client is still returned, and the listener holds no instance (`Option<NamedPipeServer>`).
    4. The next `accept` first retries the create. An error then is that accept's `Err`, and the listener stays usable.
  - A connected instance that `connect` reports as already connected (`ERROR_PIPE_CONNECTED`, which tokio treats as success) is served the same way.
  - `path(&self) -> &str`. There is no `close`: dropping the listener closes the pending instance, and there is no file to remove.
  - `NamedPipePort { pipe: NamedPipeServer }` implements `Port`. Its `split` uses `tokio::io::split` into `FramedInbound`/`FramedOutbound`, like `tcp_tls`. The aliases are `NamedPipeInbound` and `NamedPipeOutbound`.
  - A client disconnect is read as `Ok(0)`, because mio maps `ERROR_BROKEN_PIPE` to EOF. It is therefore `Received::Closed(None)`, as on UDS.
  - The module doc states this policy: byte mode with the 4-byte prefix, local only, the default security descriptor, first instance only, no TLS. It also says a client must retry `ERROR_PIPE_BUSY` (231) in the instant between two instances.
- **System Config (`hexput-config`).**
  - `RawTransports` gains `named_pipe: Option<RawNamedPipe { path: Spanned<String> }>` (`deny_unknown_fields`). `Transports` gains `pub named_pipe: Option<NamedPipeTransport { pub path: String }>`, marked `#[non_exhaustive]` and documented.
  - The "at least one transport" check counts it.
  - The path is validated on every platform as `transport.named_pipe.path`. It must start with `\\.\pipe\`, with `pipe` matched ASCII case-insensitively.
  - The path must also have a non-empty name after that prefix, contain no `\` in the name, and be at most 256 characters in all.
  - Otherwise the error is `InvalidValue` with the position and the message `expected a pipe path such as '\\.\pipe\hexput' (at most 256 characters, no '\' after '\\.\pipe\'), got "<text>"`.
  - Update the module-doc TOML sample and `config.example.toml`. The example adds a documented `[transport.named_pipe]` section with `path = '\\.\pipe\hexput'` (a TOML literal string), and its comment says the section is Windows-only and ignored with a warning elsewhere.
  - Mark `[transport.uds]` as Unix-only in the same way.
- **Platform rule (Daemon).** Each configured transport is served or skipped by platform:

  | Transport | Unix | Windows |
  |---|---|---|
  | UDS | served | skipped |
  | Named Pipe | skipped | served |
  | TCP | served | served |
  | WebSocket | served | served |

  - A skipped section logs `warn` right after "System Config loaded", before anything loads or binds:
    - Named Pipe: `"[transport.named_pipe] is Windows-only; not serving it on this platform"`, with field `path`.
    - UDS: `"[transport.uds] is Unix-only; not serving it on this platform"`, with field `path`.
  - If no configured transport is servable, startup fails, before any identity loads, with the exit code `1` line `error: <config path>: no configured transport is served on this platform ([transport.named_pipe] is Windows-only)`. On Windows, the parenthetical reads `[transport.uds] is Unix-only`.
- **The Daemon (`hexput-daemon`).**
  - `serve`, `listen`, `accept`, `Listeners`, `ACCEPT_BACKOFF` and `registry` become platform-independent. Only the UDS pieces are `#[cfg(unix)]`, and only the Named Pipe pieces are `#[cfg(windows)]`.
  - The `#[cfg(not(unix))]` refusing `serve` is deleted. The in-crate `registry` test may drop its `unix` cfg.
  - Startup order:
    1. Load the TCP identity.
    2. Load the WebSocket identity.
    3. Bind the local transport: UDS on Unix, the Named Pipe on Windows.
    4. Bind TCP.
    5. Bind WebSocket.
  - Named Pipe logging and failures:
    - Once bound, the Daemon logs `info` `"listening on a Named Pipe"`, with field `path`.
    - Accept failures log `warn` with `transport = "named_pipe"`, followed by `ACCEPT_BACKOFF`.
    - A bind failure is `error: <BindError>`. A later failure drops the pipe listener with the others.
  - The accept `select!` gets a Named Pipe branch that spawns `hexput_connection::serve` directly, like UDS, since there is no handshake. The generic `accept` helper is given a `&mut` form, or the listener is borrowed mutably, so that `accept(&mut self)` fits.
  - Update the module doc's Contract and Serving sections.
- **Windows lint cleanliness.** `scripts/check-windows.sh --release` must pass with `-D warnings` for the whole workspace and all targets. Today it reports these, which must be fixed:
  - `Poll` is unused in `hexput-daemon`.
  - In `tests/daemon.rs`, `JsonEvent` and `json_events` are dead off Unix, so they need `#[cfg(unix)]`.
- **`scripts/check-windows.sh`** (new, executable, `set -euo pipefail`) is a compile-only cross-check for `x86_64-pc-windows-msvc`. It needs `rustup target add x86_64-pc-windows-msvc`, `clang-cl` and `llvm-lib`, and if any is missing it says what to install and exits `2`.
  - It writes stub `assert.h`, `string.h`, `stdlib.h` and `malloc.h` into a `mktemp -d` directory. `ring` and `alloca` include them; see Design Notes.
  - It runs `cargo clippy --locked --target x86_64-pc-windows-msvc --workspace --all-targets "$@" -- -D warnings`, with these environment variables:
    - `CC_x86_64_pc_windows_msvc` = clang-cl
    - `AR_x86_64_pc_windows_msvc` = llvm-lib
    - `CFLAGS_x86_64_pc_windows_msvc="/imsvc <stubs>"`
  - It removes the stub directory on exit. It is not a CI step.
- **CI (`.github/workflows/ci.yml`).** Add a job `windows` (`runs-on: windows-latest`), with checkout, toolchain and rust-cache as in `check`. It runs:
  1. `cargo clippy --workspace --all-targets --locked -- -D warnings`
  2. `cargo test --locked -p hexput-tests --test transport --test daemon --test config -- named_pipe`
- **Tests.**
  - Windows tests sit under `#[cfg(windows)] mod named_pipe` (adapter, in `tests/transport.rs`) and `#[cfg(windows)] mod named_pipe_serving` (Daemon, in `tests/daemon.rs`), so the CI filter `named_pipe` selects them.
  - Each test uses its own pipe, `\\.\pipe\hexput-test-<pid>-<n>`.
  - The clients use `std::fs::OpenOptions` read+write, retrying `raw_os_error() == Some(231)` for up to 10 s. A blocking client runs in Daemon tests, and tokio's `ClientOptions` runs in adapter tests.
- **No new dependency or feature.** Tokio's `net` feature already provides `tokio::net::windows::named_pipe`. `check-crate-graph.py` is unchanged, and `hexput-transport` stays `{hexput-port}`.
- **Disk:** every cargo command on this machine runs with `--release`.

**Never:**
- TLS, a credential, or an authorization check on the pipe. Authentication is init/reconnect, the same on every transport (AD-2).
- Message-mode pipes, or a framing different from UDS's.
- Accepting remote (SMB) clients, `max_instances` limits, or a custom security descriptor or DACL option (see Design Notes).
- Any crate but `hexput-daemon` naming the adapter, or `hexput-connection`/`hexput-session` learning the transport (AD-1).
- Failing startup because a configured transport is unservable while another is servable.
- A change to the `Port` traits, a connection cap, reconnect, or TTL (Stories 5.5–5.8). The shared conformance suite is Story 5.4.
- Changing the Linux `check` CI job.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Pipe round trip (Windows) | Daemon with `[transport.named_pipe] path = '\\.\pipe\hexput-test-…'`. A client sends `Init`, then `ExecutionStart {source:"return 1 + 2;", variables:{}}` | `Result {client_id}`, then `Result {value: 3}`, as length-prefixed frames identical to UDS. The log has `"listening on a Named Pipe"` with the path | — |
| Concurrent clients (Windows) | three clients connected at once, each with Init + a Script | each is answered on its own pipe instance | — |
| Pipe already served (Windows) | another `NamedPipeListener` holds the name | adapter: `BindErrorKind::InUse`. Daemon: exit 1, `error: \\.\pipe\…: pipe already in use: another process is serving it`, and nothing is left listening | — |
| Abrupt client exit (Windows) | a client sends Init, then its handle is dropped | `Closed(None)`, then "connection detached from its Session". A new client is still served | — |
| Malformed frame (Windows) | a frame holding non-MessagePack bytes | `Error` `protocol.malformed_frame`, and the connection keeps serving, as on UDS | — |
| Shutdown (Windows) | the shutdown future completes | exit 0. Opening the pipe afterwards fails with `NotFound` | — |
| Pipe and TCP on Windows | `[transport.named_pipe]` + `[transport.tcp]` | both are served by one Daemon | — |
| Pipe ignored (non-Windows) | `[transport.uds]` + `[transport.named_pipe]` on Linux | starts, logs `warn` "[transport.named_pipe] is Windows-only; not serving it on this platform" with `path`, and serves UDS (a round trip works) | — |
| Pipe only (non-Windows) | only `[transport.named_pipe]` on Linux | exit 1, `error: <config path>: no configured transport is served on this platform ([transport.named_pipe] is Windows-only)`. Nothing is bound | — |
| UDS only (Windows) | only `[transport.uds]` | exit 1 with the `[transport.uds] is Unix-only` variant | — |
| Bad pipe path | `"/tmp/x"`, `'\\.\pipe\'`, `'\\.\pipe\a\b'`, a 257-character path | `InvalidValue` for `transport.named_pipe.path`, with line/column and the expected-form message | exit 1 in the Daemon |
| Accepted path forms | `'\\.\pipe\hexput'`, `'\\.\PIPE\hexput'`, exactly 256 characters | parsed, with `path` kept verbatim | — |
| Missing or unknown key | `[transport.named_pipe]` with no `path`, or with `mode = "0660"` | serde's missing-field or unknown-field error, with its position | — |

</intent-contract>

## Code Map

- `crates/hexput-transport/src/lib.rs` -- the module doc already names `named_pipe.rs`. Add `#[cfg(windows)] pub mod named_pipe;` and update "Today: … The Named Pipe arrives later".
- `crates/hexput-transport/src/uds.rs` -- the model for the listener's shape (`bind`, `accept`, `BindError`/`BindErrorKind` with `#[non_exhaustive]` and `Display`). The pipe gets its own simpler `BindError`, because UDS's is `cfg(unix)` and has `PathBuf`.
- `crates/hexput-transport/src/framed.rs` -- `FramedInbound::new`/`FramedOutbound::new` (`pub(crate)`). Reuse them unchanged: `Ok(0)` is already a clean close.
- `crates/hexput-transport/src/tcp_tls.rs` -- the `tokio::io::split` + `FramedInbound` pattern to copy for `NamedPipePort::split`.
- Tokio 1.53.1 `src/net/windows/named_pipe.rs` -- `ServerOptions::{new, first_pipe_instance, reject_remote_clients (default true), pipe_mode, create}`, `NamedPipeServer::connect` (cancel-safe), `ClientOptions::open`. In mio 1.2.3 `sys/windows/named_pipe.rs:573`, `ERROR_BROKEN_PIPE` is read as `Ok(0)`.
- `crates/hexput-config/src/file.rs` -- `RawTransports`, `Validator::transports` (the no-transport check and per-section build), `invalid`, and `path`. Add a `pipe_path` validator.
- `crates/hexput-config/src/lib.rs` -- the module-doc TOML sample, `Transports`, and `UdsTransport` (add `NamedPipeTransport` beside it).
- `crates/hexput-config/config.example.toml` -- add the section, and mark UDS Unix-only. `the_example_file_parses_and_documents_every_field` in `tests/config.rs` must assert the pipe path.
- `crates/hexput-daemon/src/lib.rs` -- the code to change:
  - `serve` (`#[cfg(unix)]`, ~line 211), and the `cfg(not(unix))` `serve` (~line 340), which is to be deleted.
  - `accept` helper (~328), `Listeners`/`close`/`close_uds` (~350), and `listen` (~380).
  - The `cfg(unix)` imports at the top (lines 53–68), and `registry` (~203).
  - The module doc (lines 1–43).
  - The platform skip and the "nothing servable" check go at the top of `listen`, or in a helper it calls, before the identity loads.
- `crates/hexput-tests/tests/daemon.rs` -- the pieces to reuse and fix:
  - `Sandbox`, `SharedLog`, `Run`, `daemon()`, `nothing()` are platform-independent. Reuse them for the Windows harness.
  - `Serving` (~480, `cfg(unix)`) is the model for a `cfg(windows)` `PipeServing`.
  - `mod wire` (~544) holds frame helpers, which a Windows module may copy or un-gate.
  - `JsonEvent`/`json_events` (~1123–1165) need `#[cfg(unix)]`.
  - The non-Windows tests (pipe ignored, pipe only) go beside `every_startup_failure_exits_one_naming_the_cause_and_touches_nothing` (~1016), as `#[cfg(unix)]` tests.
- `crates/hexput-tests/tests/transport.rs` -- the existing UDS adapter tests are the model for the `cfg(windows) mod named_pipe` adapter tests.
- `crates/hexput-tests/tests/config.rs` -- add the pipe parse and validation tests, and extend the full-parse and example tests.
- `.github/workflows/ci.yml` -- the existing `check` job is the template for the `windows` job.
- `scripts/check-crate-graph.py` -- read-only, and must still pass.
- `_bmad-output/planning-artifacts/architecture/architecture-hexput-2026-09-18/ARCHITECTURE-SPINE.md` -- the place for a dated amendment after the Story 5.2 one (~line 157).

## Tasks & Acceptance

**Execution:**
- `crates/hexput-transport/src/{lib.rs,named_pipe.rs}` -- add the adapter and its doc per Boundaries (FR-9, AD-1).
- `crates/hexput-config/src/{file.rs,lib.rs}`, `crates/hexput-config/config.example.toml` -- add `NamedPipeTransport`, the path validation, the docs and the example.
- `crates/hexput-daemon/src/lib.rs` -- make serving platform-independent:
  - add the platform skip warnings and the nothing-servable error;
  - add the Named Pipe bind, log and accept branch;
  - delete the `cfg(not(unix))` refusal;
  - fix the Windows unused import;
  - update the module doc.
- `crates/hexput-tests/tests/config.rs` -- tests for the Bad pipe path, Accepted path forms and Missing/unknown key rows. A pipe-only file now parses, because it counts as a transport.
- `crates/hexput-tests/tests/daemon.rs` -- `#[cfg(unix)]` tests for "Pipe ignored" (warn line + UDS round trip) and "Pipe only" (exit 1, exact line). Add a `#[cfg(windows)] mod named_pipe_serving` with:
  - the round trip;
  - concurrent clients;
  - the pipe already served;
  - the abrupt exit;
  - the malformed frame;
  - shutdown;
  - pipe + TCP;
  - UDS only on Windows.

  Gate `JsonEvent`/`json_events` with `#[cfg(unix)]`.
- `crates/hexput-tests/tests/transport.rs` -- a `#[cfg(windows)] mod named_pipe` with these adapter tests:
  - a round trip through the `Port`;
  - `InUse` on a second bind;
  - a client drop being `Closed(None)`;
  - two instances served at once;
  - `accept` being cancel-safe: drop one pending accept, then the next accept still gets the client.
- `scripts/check-windows.sh` (new, `chmod +x`) -- the cross-target clippy per Boundaries.
- `.github/workflows/ci.yml` -- add the `windows` job.
- `ARCHITECTURE-SPINE.md` -- add a dated amendment (`[Amended 2026-10-22, Epic 5 Story 5.3]`, after Story 5.2's). It records:
  - the pipe policy (byte mode with the shared prefix framing, first instance only, local only, the default security descriptor, no TLS, no new dependency);
  - the platform rule (UDS Unix-only, the pipe Windows-only, an unservable section skipped with a warning, nothing servable is a startup error);
  - the Windows CI job;
  - no edge change.
- `AGENTS.md` -- Project Status: add Story 5.3 to the Epic 5 paragraph, update the "Next step" and the test count, and mention `scripts/check-windows.sh` under Build, Lint, Test.
- `_bmad-output/implementation-artifacts/sprint-status.yaml` -- `5-3-…` → `review` at finalization.

**Acceptance Criteria:**
- Given a Windows host and a System Config with `[transport.named_pipe]`, when the Daemon starts, then it creates that pipe, logs `"listening on a Named Pipe"` with its path, and serves it until shutdown (FR-9).
- Given a Backend on the Named Pipe, when it runs a Script, then every reply matches what UDS returns for the same request, byte for byte as frames. No TLS is involved, and `hexput_connection::serve` runs unchanged (FR-9, AD-1).
- Given a non-Windows host with `[transport.uds]` and `[transport.named_pipe]`, when the Daemon starts, then it does not fail. It warns that the pipe is Windows-only, and UDS serves the local role.
- Given the workspace, when `python3 scripts/check-crate-graph.py` and `scripts/check-windows.sh --release` run, then both pass. `hexput-transport` still depends on `hexput-port` alone, and no crate but `hexput-daemon` names `named_pipe`.

## Implementation Notes

- Tests pre-written by the test agent in `crates/hexput-tests/tests/transport.rs` (`mod named_pipe`: a framed round trip with byte-level framing, path reported, `InUse` on a second bind, client drop as `Closed(None)`, two instances at once, cancel-safe accept), `crates/hexput-tests/tests/config.rs` (`a_pipe_is_a_transport_so_a_pipe_only_file_parses`, `a_bad_pipe_path_names_the_field_its_position_and_the_expected_form`, `accepted_pipe_path_forms_are_kept_verbatim`, `a_pipe_section_without_a_path_or_with_an_unknown_key_is_rejected`, plus the extended `a_file_with_every_field_parses_into_every_field` and `the_example_file_parses_and_documents_every_field`) and `crates/hexput-tests/tests/daemon.rs` (`a_pipe_on_a_non_windows_host_is_warned_about_and_uds_still_serves` and `a_pipe_only_config_on_a_non_windows_host_exits_one_and_binds_nothing` on unix; `mod named_pipe_serving` on Windows: `the_pipe_round_trip_answers_like_the_wire_and_logs_its_path`, `shutdown_ends_the_daemon_and_the_pipe_is_gone`, `three_clients_at_once_are_each_answered_on_their_own_instance`, `a_pipe_already_served_exits_one_naming_it_and_listens_on_nothing`, `a_client_leaving_abruptly_is_detached_and_a_new_client_is_still_served`, `a_malformed_frame_is_answered_and_the_connection_keeps_serving`, `a_pipe_and_tcp_are_both_served_by_one_daemon`, `uds_only_on_windows_exits_one_naming_the_unix_only_section`). The implementer makes them compile and pass; it may correct a test only where it contradicts this spec, never to weaken it.
- Test-file scaffolding for the Windows build: `transport.rs` lost its file-level `#![cfg(unix)]`. Its UDS items (imports, `Sandbox::socket`/`entries`, `is_socket`, `pair`, the UDS tests) are now individually `cfg(unix)`, and `Sandbox::new` uses `std::env::temp_dir()` off Unix, so the TCP, TLS and WebSocket tests compile on Windows too. `daemon.rs`'s `JsonEvent`/`json_events` are `cfg(unix)`, per the spec.
- The Windows tests were written against the spec's assumed log text (`path={pipe}` unquoted, as `listening on a Unix Domain Socket path={}` is logged) and the `WARN` level word. They cannot run on this Linux machine; the Windows CI job is what executes them.
- Implementation (step 3): the Daemon's only platform-specific code is a private `local` module per platform (`cfg(unix)` UDS, `cfg(windows)` Named Pipe) exposing `Listener`, `TRANSPORT`, `configured`, `bind`, `accept(&mut Listener)` and `close`, so the one `select!` and `Listeners` are platform-independent (`tokio::select!` cannot `cfg` a branch). The generic `accept` helper now takes `Option<R>` for any borrow `R`. Any other platform is a `compile_error!`. The pipe path's invalid-value message quotes the text verbatim (`got "<text>"`), not Rust-escaped, so a pipe path's backslashes are not doubled. `scripts/check-windows.sh` also finds a versioned `clang-cl-<n>` / `llvm-lib-<n>` when the plain name is not on PATH (this machine has only `clang-cl-18`).
- Matrix audit decision (unattended): the Windows-only rows (pipe round trip, concurrent clients, pipe already served, abrupt exit, malformed frame, shutdown, pipe + TCP, UDS only on Windows) are covered by tests in `mod named_pipe` / `mod named_pipe_serving` that compile and lint clean under `scripts/check-windows.sh --release` but cannot run on this Linux machine; per Design Notes 4 they run in CI's new `windows` job, which this run could not execute. The non-Windows rows (pipe ignored, pipe only, bad path, accepted forms, missing/unknown key) ran and passed. A reviewer should confirm the `windows` CI job is green before accepting.
- Unrelated flake seen once under full-suite load: `tests/script.rs` `a_removed_handle_is_unknown_and_its_program_is_released_at_once` (Story 4.3, AST cache release); it passed on five reruns and in the final full run. Not touched by this story.

## Spec Change Log

## Review Triage Log

### 2026-10-22 — Review pass
- verdicts: 29 findings — high 2, medium 9, low 11, false 7, maybe-false 0
- findings:
  - `[high]` `[patch]` blind-hunter: `accept` keeps an instance whose `connect` failed and retries it forever (`ERROR_NO_DATA` after a client opens and closes early wedges the pipe) — on a `connect` error the instance is now replaced by a fresh one (created before the old one drops, so the name never lapses) and the error returned; test `a_client_leaving_before_the_accept_does_not_wedge_the_listener` added.
  - `[medium]` `[patch]` blind-hunter: a NUL in the pipe path is not rejected, so Windows would serve a truncated name — `pipe_path` now refuses any control character in the name; covered by `a_named_pipe_path_with_a_control_character_or_too_many_utf16_units_is_rejected`.
  - `[low]` `[patch]` blind-hunter: the 256 limit counts chars, Windows counts UTF-16 units — the check now uses `encode_utf16().count()`; non-BMP rejection and fit are tested.
  - `[low]` `[patch]` blind-hunter: the CI filter `--test config -- named_pipe` selects no config test — the five config pipe tests are renamed to contain `named_pipe`, so the comment in `ci.yml` is now true.
  - `[medium]` `[patch]` blind-hunter: the default descriptor lets Everyone/anonymous open read-only, which still connects an instance, and the doc claims otherwise — the module doc now names the exposure (idle connections from any local account, as on loopback TCP/WebSocket, until a connection cap); a custom descriptor is excluded by the intent's Never list, and the unbounded-connections concern is the cross-transport connection cap already recorded in deferred-work.md.
  - `[medium]` `[defer]` blind-hunter: no way to let a Backend's account connect, which Story 7.6's Windows service install will need — the intent excludes a DACL option; deferred, tied to Story 7.6.
  - `[low]` `[patch]` blind-hunter: the invalid-path message embeds the text unescaped, so a control character breaks the one-line stderr contract — control characters are now escaped (`escape_default`) while backslashes stay as typed; the test asserts the message has no newline.
  - `[false]` `[reject]` blind-hunter: a failure to create the next instance is silently dropped — the Daemon's `select!` polls `accept` again at once after spawning the connection, and that accept retries the create and logs its error at `warn` with `ACCEPT_BACKOFF`, so the operator does see it.
  - `[medium]` `[patch]` blind-hunter: the adapter tests miss the risky paths (cancel with a client arriving, connect error, failed next-instance create) — the first two are now tested (`a_client_arriving_while_a_dropped_accept_was_pending_is_the_next_accept`, `a_client_leaving_before_the_accept_does_not_wedge_the_listener`); the third shares the verification-gap row below (rejected: needs a fault-injection seam).
  - `[low]` `[reject]` blind-hunter: Windows coverage of TCP/WebSocket is thin (CI filter, pipe+TCP test only connects, free-port race) — the TCP/TLS/WebSocket adapters are platform-independent Rust already exercised on Linux; a Windows-only divergence is unlikely and covering it means a TLS round trip harness in the Windows job plus widening its filter to tests never run on Windows.
  - `[low]` `[defer]` blind-hunter: `epic-5-context.md` records no Story 5.3 facts — an agent-context file; deferred to Story 5.4's planning.
  - `[false]` `[reject]` blind-hunter: `PermissionDenied` is always reported as "pipe already in use" — with `FILE_FLAG_FIRST_PIPE_INSTANCE`, `ERROR_ACCESS_DENIED` is the documented failure for a name that already exists, and a pipe name exists only while some process holds an instance of it, so "another process is serving it" is accurate; a fresh name is creatable by any account in the `\\.\pipe\` namespace.
  - `[medium]` `[reject]` blind-hunter: the story is marked `review` though no Windows acceptance criterion has run — true, but the fix is the spec's follow-up flag and an open item, an edit to this build's spec; `followup_review_recommended` is `true` and Auto Run Result names the risk.
  - `[high]` `[patch]` edge-case-hunter: `connect` failure keeps the broken instance — same root cause as the first blind-hunter row; fixed there.
  - `[low]` `[reject]` edge-case-hunter: after a failed next-instance create, if every connected client then leaves the name lapses and a squatter can claim it — needs a create failure on a name this process owns, all clients gone and a racing squatter; unlikely, and guarding it means tracking live instances (new state). The connect-error path no longer opens such a gap: the replacement is created before the failed instance drops.
  - `[false]` `[reject]` edge-case-hunter: `PermissionDenied` mapped to `InUse` for non-instance failures — same refutation as the blind-hunter row.
  - `[low]` `[patch]` edge-case-hunter: the length check counts chars, not UTF-16 units — same as the blind-hunter row; fixed there.
  - `[medium]` `[patch]` edge-case-hunter: an interior NUL truncates the name — same as the blind-hunter row; fixed there.
  - `[low]` `[patch]` edge-case-hunter: the CI comment claims config tests run on Windows but none match — same as the blind-hunter row; fixed by renaming the tests.
  - `[medium]` `[patch]` verification-gap: `dropping_a_pending_accept_loses_no_client` cannot detect a non-cancel-safe `accept` — added `a_client_arriving_while_a_dropped_accept_was_pending_is_the_next_accept`: the accept is polled once, a client connects and writes a frame, the accept is dropped unpolled, and the next accept must return that client's frame (the golden shape would time out).
  - `[low]` `[reject]` verification-gap: no test for the `pending == None` retry and the failed next-instance create — forcing `CreateNamedPipe` to fail on a name the listener owns needs a fault-injection seam (new surface); the branch is two lines and its contract is in the doc comment.
  - `[low]` `[patch]` verification-gap (other): the CI comment and filter select zero config tests — same root cause as the blind-hunter CI row; fixed there.
  - `[medium]` `[reject]` verification-gap (other): the Windows tests have never run and some assertions rest on assumed log formatting — the formatting was checked: the Daemon logs `path = %…` (Display, unquoted) exactly as the UDS line the Linux tests already assert, and the Linux pipe-ignored test asserts the same `WARN` word and passes; `NotFound` is Windows' `ERROR_FILE_NOT_FOUND` for a pipe with no instance. What remains is executing them, which the follow-up flag records (a spec edit, so rejected here).
  - `[low]` `[reject]` verification-gap (other): `a_pipe_and_tcp_are_both_served_by_one_daemon` only connects to TCP — same reasoning as the blind-hunter coverage row.
  - `[false]` `[reject]` intent-alignment: the diff goes past "Halt after planning" — that instruction governed the planning stage; this run's orchestrator then assigned implementation and review, and the spec's `<intent-contract>` (the approved intent) calls for the adapter, config, Daemon and CI changes.
  - `[medium]` `[reject]` intent-alignment: two of three ACs were never executed on Windows — same as the blind-hunter "marked review" row; recorded through the follow-up flag.
  - `[false]` `[reject]` intent-alignment: "identical to UDS" narrowed to byte-equal frames — the intent's own AC states byte-for-byte frames with `serve` unchanged, and the epics assign the wider equivalence to Story 5.4's conformance suite.
  - `[false]` `[reject]` intent-alignment: surfaces the Story never asked for (TCP/WebSocket and the UDS rule on Windows, pipe security policy, CI tooling, Spine) — all are in the intent contract's Approach and Boundaries; the Daemon could not start on Windows at all without the platform rule.
  - `[false]` `[reject]` intent-alignment: the CI job uses non-release cargo against the user's `--release` request — the request is about this machine's disk; CI runs on GitHub runners.

## Design Notes

These decisions were taken unattended, as the most defensible readings:

1. **Unservable sections are skipped, not refused.** The AC requires that the pipe be "cleanly absent rather than failing startup". One System Config may be shared across platforms. A host that can serve nothing configured still fails, because a Daemon listening nowhere is the failure 2.1 already forbids. UDS on Windows is treated the same way: tokio's `UnixListener` is Unix-only.
2. **No `mode` or DACL option.** The pipe's default security descriptor gives full control to LocalSystem, Administrators and the creator owner, and read-only access to Everyone. A Hexput client must open the pipe read/write, so only the Daemon's own account and administrators can connect. That is the UDS `0700`-like default. `first_pipe_instance(true)` stops another process from pre-creating the name and impersonating the Daemon. `reject_remote_clients(true)` keeps the pipe local, which is why it needs no TLS. A configurable DACL can come later, if an operator needs another account.
3. **Byte mode with the 4-byte prefix.** This is one framing for every byte stream (Story 5.1's `framed.rs`). Message mode would add a second framing for no benefit.
4. **Verification without Windows.** `scripts/check-windows.sh` proves that the `cfg(windows)` code compiles and lints. It was probed during planning, and the whole workspace cross-clippies in about 15 s once its stub C headers are present: `ring` needs `assert.h`/`string.h`/`stdlib.h` (`memcpy`, `memset`, `memmove`, `memcmp`, `abort`, `_byteswap_ulong`/`_byteswap_uint64`/`_byteswap_ushort` declarations), and `alloca` needs `malloc.h` (`void *_alloca(size_t)`). The stubs only let the C build scripts finish; nothing is linked or run. The Windows CI job is what executes the pipe tests. It cannot be run from this machine, and the implementation notes must say so.

Golden shape of `accept`:

```rust
pub async fn accept(&mut self) -> io::Result<NamedPipePort> {
    let server = match self.pending.take() { Some(s) => s, None => self.options(false).create(&self.path)? };
    if let Err(e) = server.connect().await { self.pending = Some(server); return Err(e); }
    self.pending = self.options(false).create(&self.path).ok(); // retried on the next accept
    Ok(NamedPipePort { pipe: server })
}
```

This is not cancel-safe as written, because a dropped future loses `server`. Keep the instance in `self.pending` and borrow it for `connect()`. Take it only once `connect` has returned `Ok`.

## Verification

**Commands:**
- `cargo fmt --all --check` -- expected: clean
- `cargo clippy --workspace --all-targets --locked --release -- -D warnings` -- expected: no warnings
- `python3 scripts/check-crate-graph.py` -- expected: passes unchanged
- `scripts/check-windows.sh --release` -- expected: no warnings or errors for `x86_64-pc-windows-msvc`
- `cargo build --workspace --all-targets --locked --release` -- expected: success
- `cargo test --workspace --locked --release` -- expected: all pass, including the new config and non-Windows daemon tests
- `git diff --stat Cargo.lock` -- expected: no change

## Auto Run Result

**Summary.** Story 5.3 adds the Windows Named Pipe adapter (`hexput_transport::named_pipe`), the `[transport.named_pipe] path` System Config section validated on every platform, and a platform-independent Daemon serving path (UDS Unix-only, the pipe Windows-only, TCP and WebSocket everywhere; an unservable section is skipped with a `warn`, nothing servable exits `1`). It also adds `scripts/check-windows.sh` (cross-target clippy) and a `windows` CI job.

**Files changed.**
- `crates/hexput-transport/src/named_pipe.rs` -- the adapter: bind, cancel-safe accept (a failed instance replaced), `NamedPipePort`, `BindError`.
- `crates/hexput-transport/src/lib.rs` -- `#[cfg(windows)] pub mod named_pipe;`, module doc.
- `crates/hexput-config/src/{file.rs,lib.rs}`, `config.example.toml` -- `NamedPipeTransport`, pipe path validation (control characters refused, UTF-16 length), docs, example.
- `crates/hexput-daemon/src/lib.rs` -- platform-independent serving via a cfg'd `local` module, skip warnings, nothing-servable error, pipe bind/accept.
- `crates/hexput-tests/tests/{config,daemon,transport}.rs` -- config pipe tests (names contain `named_pipe`), non-Windows Daemon tests, Windows-only adapter and Daemon tests.
- `scripts/check-windows.sh` -- compile-only Windows cross-lint with stub C headers.
- `.github/workflows/ci.yml` -- the `windows` job.
- `ARCHITECTURE-SPINE.md`, `AGENTS.md`, `epic-5-context.md`, `sprint-status.yaml` -- amendment, status, tracking.

**Review findings.** 29 findings: 13 rows patched (entries: 1 high — the wedged accept; 3 medium — NUL in the path, the read-only-open doc, the cancel-safety/connect-error test gaps; 3 low — UTF-16 length, CI filter, one-line message), 2 deferred (DACL option for Story 7.6; epic-5-context refresh), 14 rejected: 7 false (silent next-create failure, `PermissionDenied` wording x2, "halt after planning", byte-equal narrowing, extra surfaces, CI `--release`), 4 low not worth the complexity (TCP/WebSocket Windows coverage x2, squatter after a failed create, fault-injection for the create-failure branch), 3 whose fix is this spec's follow-up flag (Windows ACs never executed x2, unexecuted Windows assertions).

**Follow-up review recommended: true.** A `high` entry was patched (first pass). Named risk: every Windows-only test (the adapter's `mod named_pipe`, including the two new accept tests, and the Daemon's `mod named_pipe_serving`) has only been cross-compiled and linted, never executed; a reviewer must see CI's `windows` job green before accepting. Patched counts by verdict: high 1, medium 3, low 3.

**Verification.** All with `--release` on this Linux machine: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --locked --release -- -D warnings` clean; `python3 scripts/check-crate-graph.py` OK; `scripts/check-windows.sh --release` clean for `x86_64-pc-windows-msvc`; `cargo build --workspace --all-targets --locked --release` OK; `cargo test --workspace --locked --release --no-fail-fast` 1002 passed, 0 failed (an earlier fail-fast run hit the known Story 4.3 flake `a_removed_handle_is_unknown_and_its_program_is_released_at_once`, which passed on the rerun); `Cargo.lock` unchanged.

**Residual risks.** The Windows tests' runtime behaviour (pipe round trip, busy retry, `NotFound` after shutdown, the two new accept tests) is unverified until the `windows` CI job runs. Any local account can hold idle read-only pipe connections until a connection cap lands (as on loopback TCP/WebSocket). Design Note 2's wording ("only the Daemon's own account and administrators can connect") is superseded by the adapter's module doc: they are the only accounts that can send a request.
