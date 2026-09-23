---
title: 'Story 2.1: Start the daemon from a System Config file'
type: 'feature'
created: '2026-09-23'
status: 'done'
baseline_commit: '3474727768e8981dceabc723d3f0dbdaa9362490'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-2-context.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** The daemon cannot start. `SystemConfig` is a field-less stub whose `resolve()` takes no arguments (so AD-7's CLI flag has no channel), `hexput_daemon::run` is a `todo!()`, and the `hexput-daemon` binary parses no arguments. An operator has no documented way to point the daemon at its operational settings, and nothing reports a missing or broken file.

**Approach:** Implement `hexput-config`: a typed `SystemConfig` (transports with bind addresses and TLS paths, log level, default Session TTL) parsed from one file, and a pure resolver applying AD-7's precedence — `--config` flag, then an environment variable, then a fixed per-OS default path — returning the resolved path alongside the config or a `ConfigError` that names the file and the exact problem. Give the daemon a `clap`-parsed `--config` flag and a `run` that resolves, reports failures on stderr with a non-zero exit, and otherwise logs the resolved path at startup.

## Boundaries & Constraints

**Always:** Resolution is one function, identical for every packaging; the env var and default path are passed in as parameters so tests never mutate the process environment (`std::env::set_var` is `unsafe` in edition 2024). The resolver never falls through: a flag or env var naming a missing file is an error for *that* file, not a reason to try the next source. Every `ConfigError` names the resolved path and the specific problem (not found, unreadable, parse error with line/column, missing field, invalid value, unknown key). Unknown keys are rejected so a typo cannot silently drop a setting. The config is loaded once at startup; `hexput-config` exposes no reload, watch, or write API, and gains no edge to `hexput-session` (AD-5, already asserted by `check-crate-graph.py`). Only `hexput-bin` owns `fn main()`; `hexput_daemon::run` returns an `ExitCode` and never calls `process::exit`. Third-party versions are pinned once in `[workspace.dependencies]` and recorded in the Spine's Stack table.

**Never:** No socket, listener, or transport adapter (Story 2.3). No wire envelope (2.2). No JSON log output or Client ID span fields (2.8). No per-backend Config anything. No config hot-reload. No undocumented default for a required field.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Valid file | complete file via `--config` | config loaded; startup log names the resolved path | N/A |
| Precedence | flag, env var and default file all present | flag's file is used; without flag, env var's; without both, default | N/A |
| Flag names missing file | `--config /nope.toml`, env var set | exit non-zero naming `/nope.toml` — no fallthrough to env | stderr, non-zero |
| Nothing supplied, no default file | no flag, no env, default absent | exit non-zero naming the default path and listing the flag and env var that could override it | stderr, non-zero |
| Malformed | invalid TOML | exit non-zero naming the file, line and column | stderr, non-zero |
| Missing required field | e.g. a TCP transport without `tls_cert` | exit non-zero naming the file and the field | stderr, non-zero |
| Invalid value | `log_level = "loud"`, TTL of zero | exit non-zero naming the file, field and accepted values | stderr, non-zero |
| Unknown key | `log_levle = "info"` | exit non-zero naming the key | stderr, non-zero |
| No transport configured | file with no transport section | exit non-zero: at least one transport is required | stderr, non-zero |

## Decisions

1. **Format and dependencies.** TOML, parsed by the `toml` crate through `serde` (`derive`). The startup log is a real `tracing` event emitted through `tracing-subscriber` (plain `fmt` output, filtered at `log_level`); JSON output stays with Story 2.8. `toml`, `tracing-subscriber` and any feature sets are pinned in `[workspace.dependencies]` and added to the Spine's Stack table with a dated amendment note, as `clap` was.
2. **Discovery names.** Environment variable `HEXPUT_CONFIG`. Default path `/etc/hexput/config.toml` on Unix, `%ProgramData%\hexput\config.toml` on Windows (falling back to `C:\ProgramData` only if `ProgramData` is unset is acceptable and documented).
3. **Required vs. defaulted.** At least one transport section is required, and every field inside a present transport section is required (bind address, TLS certificate and key where the transport uses TLS). `log_level` defaults to `info` and `session_ttl_secs` to `300`; both defaults are documented in `hexput-config`'s docs and the example file. A TTL of `0` or an unrecognised level is an error, not a default.
4. **Run behaviour before transports exist.** After a successful load, `run` builds the multi-thread Tokio runtime and waits for SIGINT/SIGTERM (Ctrl-C on Windows), then exits `0`. The wait is a future passed in, so a test drives the success path with an already-ready shutdown future instead of a real signal.

</frozen-after-approval>

## Code Map

- `crates/hexput-config/src/lib.rs` -- stub `SystemConfig` + no-arg `resolve()`; replace wholesale. Its only dependent is `hexput-daemon` (re-exports it).
- `crates/hexput-config/Cargo.toml` -- has a `hexput-shared` edge (Spine: `config --> shared`); keep it, add `serde`, the TOML parser.
- `crates/hexput-daemon/src/lib.rs` -- `pub use hexput_config::SystemConfig` + `run()` `todo!()`. Owns the `clap` args and `run(args) -> ExitCode`, mirroring `hexput-cli-core`'s `run`/`run_with` sink pattern.
- `crates/hexput-bin/src/bin/hexput-daemon.rs` -- becomes `args_os()` hand-off returning `ExitCode`, exactly like `hexput.rs`. `hexput-bin` must not gain a `hexput-config` edge.
- `crates/hexput-cli-core/src/lib.rs` -- reference pattern only: `run`/`run_with(args, out, err) -> ExitCode`, usage exit `2`, failure `1`. Do not change it.
- `Cargo.toml` `[workspace.dependencies]` -- add new pins; hoist feature sets there (deferred-work item from Story 1.1).
- `_bmad-output/planning-artifacts/architecture/.../ARCHITECTURE-SPINE.md` -- Stack table amendment for any new crate, mirroring the `clap` amendment note.
- `scripts/check-crate-graph.py` -- no new workspace edge is expected; `hexput-session`↔`hexput-config` is already forbidden.
- `crates/hexput-tests/Cargo.toml`, `tests/` -- add `hexput-config`, `hexput-daemon` as dev-dependencies; new `tests/config.rs`, `tests/daemon.rs`.
- `_bmad-output/implementation-artifacts/deferred-work.md` -- resolve the Story 1.1 items on `resolve()`'s signature and on feature hoisting.

## Tasks & Acceptance

**Execution:**
- [x] `Cargo.toml` -- pin new third-party crates and their features -- one version, one feature set.
- [x] `ARCHITECTURE-SPINE.md` -- amend Stack table with a dated note -- the table is the one home of a version.
- [x] `crates/hexput-config/src/lib.rs` (+ modules as needed) -- `SystemConfig` with transports (`uds { path }`, `tcp { bind, tls_cert, tls_key }`, `websocket { bind, tls_cert?, tls_key? }` — the two TLS paths both present or both absent), `log_level`, `session_ttl_secs`; `ConfigSource` (Flag/Env/Default) recorded with the path; `resolve(flag, env, default_path) -> Result<Loaded, ConfigError>`; `DEFAULT_PATH` and `ENV_VAR` consts; `ConfigError: Display + Error` naming path and problem -- AD-7, FR-24.
- [x] `crates/hexput-daemon/src/lib.rs` -- `clap` `--config`; `run`/`run_with` returning `ExitCode`; resolve with real env and default; on error render to stderr and exit `1` (usage `2`); on success init logging at `log_level` and emit the resolved path and its source -- FR-24.
- [x] `crates/hexput-bin/src/bin/hexput-daemon.rs` -- thin `args_os()` hand-off.
- [x] `crates/hexput-config/config.example.toml` -- documented example with every field and both defaults; a test parses it -- the documentation cannot drift from the parser.
- [x] `crates/hexput-tests/tests/config.rs`, `tests/daemon.rs` -- every I/O-matrix row, precedence permutations, error text names the path, a fixture exercising every field.
- [x] `AGENTS.md` Project Status, `deferred-work.md` -- keep honest.

**Acceptance Criteria:**
- Given a daemon started with a valid config, when it starts, then the startup log contains the resolved absolute-or-as-given path and which source supplied it.
- Given `hexput-config`'s public API, when inspected, then it has no reload, watch or write function and no `hexput-session` edge (graph check passes).
- Given the five CI commands, when run, then all pass.

## Implementation Notes

## Spec Change Log

## Review Triage Log

| # | Layer | Finding | Verdict | Evidence | Route |
|---|-------|---------|---------|----------|-------|
| 1 | blind, edge | Partial SIGINT/SIGTERM install swallows the installed signal; comment claims default disposition still applies | low | Tokio never unregisters a handler, so dropping the installed stream leaves that signal ignored; rare but the fix is contained | patch |
| 2 | blind, edge | Real-binary signal test has no deadline and orphans the daemon on assertion failure | medium | `read_line` loop is unbounded and `kill` runs only on the happy path | patch |
| 3 | blind | `ConfigError` reports the io error in both `Display` and `source()` | low | Chained reporters print it twice; fix is a deletion | patch |
| 4 | blind | `epic-2-context.md` pinned-stack line lacks `toml`/`tracing-subscriber` | low | Later stories read it as context; one-line fix | patch |
| 5 | verification-gap | Empty flag passed to `resolve` has no no-fallthrough test | low | Pre-verified; only the env side is tested | patch |
| 6 | verification-gap | Character-based column never tested with non-ASCII input | low | Pre-verified; all column assertions are ASCII | patch |
| 7 | verification-gap | `--version` exit contract untested | low | Pre-verified; only `--help` is exercised | patch |
| 8 | verification-gap, blind | Windows default path and Ctrl-C path never run in CI; Windows console close/logoff/shutdown events not handled | medium | `#[cfg(windows)]` code unexercised by the Linux-only CI; `ctrl_close` etc. not listened for | defer |
| 9 | edge | `locate` could panic if a toml error span lands inside a multi-byte char | maybe-false | Probed six non-ASCII malformed inputs through the binary, no panic; spans appear char-aligned. Settled by confirming toml's error spans are always char boundaries, or by `floor_char_boundary` | defer |
| 10 | blind, edge | FIFO / `/dev/zero` / huge file blocks or exhausts memory at startup | low | Real but requires an operator to point the config at a non-file; fix adds guards | reject |
| 11 | edge | Startup log filtered out at `warn`/`error` | false | Decision 1 (frozen) makes the startup log a `tracing` event filtered at `log_level`; an operator choosing `error` chose that | reject |
| 12 | blind | `session_ttl_secs` has no upper bound; `Instant + ttl` may overflow in Epic 5 | low | No consumer exists yet; the Epic 5 TTL countdown must use checked arithmetic | reject |
| 13 | blind | Empty `--config ""` renders a blank-path message | false | clap rejects an empty `PathBuf` with a usage error (exit 2) before `resolve` runs — verified | reject |
| 14 | blind | Public field `source` shadows `Error::source` name | low | Cosmetic API naming; rename churns a public surface | reject |
| 15 | blind | `hexput-config` → `hexput-shared` edge unused yet pinned | low | The Spine's graph lists `config --> shared`; pinning it matches the Spine | reject |
| 16 | blind | Example-file test does not enforce every field is documented | low | A field-list cross-check adds machinery for a rare drift | reject |
| 17 | blind | Invalid tcp bind, empty tls paths, permission-denied not tested | low | Same code paths as tested uds/websocket cases | reject |
| 18 | blind | Duplicate bind address across tcp and websocket accepted | low | Surfaces as a bind error once listeners exist (2.3/Epic 5) | reject |
| 19 | blind | Spec in-review vs sprint in-progress; Code Map said no new edge | false | Status sync is a later workflow step; the new entries are exact-set pins, not new edges | reject |
| 20 | blind | Implementation Notes empty | low | Fix would edit this build's spec | reject |
| 21 | blind | Early-signal race window includes parse and runtime build | low | Nothing to clean up before 2.3; recorded in deferred-work already | reject |

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && python3 scripts/check-crate-graph.py && cargo build --workspace --all-targets --locked && cargo test --workspace --locked` -- expected: all green.
- `cargo run -q --bin hexput-daemon -- --config /nope.toml; echo $?` -- expected: error naming `/nope.toml`, exit 1.
