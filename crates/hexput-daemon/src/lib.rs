//! Wiring root: composes every daemon crate into one running Daemon. The only crate that depends
//! on hexput-transport, which is what makes AD-1's transport-agnostic core a compile-time
//! property.
//!
//! Binds: AD-1, AD-7, FR-24.
//!
//! # Shape
//!
//! Only `hexput-bin` owns a `fn main()` or ends the process: [`run`] and [`run_with`] return an
//! [`ExitCode`] and write through sinks, and the wait for a shutdown signal is a future passed
//! in, so the whole startup path is exercised in-process by a test.
//!
//! # Contract
//!
//! * `hexput-daemon [--config <path>]` — the System Config file is resolved by AD-7's precedence
//!   (see `hexput-config`): the flag, then `HEXPUT_CONFIG`, then the fixed per-OS default path.
//! * `0` — the Daemon started, logged the resolved System Config path and the source that
//!   supplied it, and later shut down cleanly on SIGINT/SIGTERM (Ctrl-C on Windows); or help or
//!   the version was explicitly requested (printed to stdout).
//! * `2` — the invocation was wrong (an unknown flag, `--config` without a value).
//! * `1` — the System Config could not be loaded (missing, unreadable, malformed, invalid), it
//!   names a transport no adapter serves yet, the socket could not be bound, or the runtime
//!   could not start. The reason is one line on stderr naming the file or the socket path.
//!
//! # Serving
//!
//! The Daemon listens on the configured Unix Domain Socket (`hexput-transport`), binding it
//! before it announces that it started. Each accepted connection is its own Tokio task running
//! `hexput_connection::serve`, which is generic over the Port and never learns the transport
//! (AD-1); a connection that fails, disconnects abruptly or panics ends only its own task. On
//! shutdown the Daemon stops accepting and removes the socket file it created.
//!
//! TCP+TLS and WebSocket have no adapter until Epic 5: a System Config naming either is refused
//! at startup, so the Daemon never appears to listen where it does not.

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::task::Poll;
#[cfg(unix)]
use std::time::Duration;

use clap::Parser;
use hexput_config::{Loaded, LogLevel};
use tracing_subscriber::filter::LevelFilter;

// Re-exported so a caller of `run_with` (and `hexput-bin`) can name these without a direct
// dependency edge on `hexput-config`, which is not in the Spine's graph for `hexput-bin`.
pub use hexput_config::{ConfigError, ConfigSource, SystemConfig};

/// The invocation was wrong — as distinct from the System Config being wrong.
const USAGE: u8 = 2;
/// The System Config could not be loaded, or the Daemon could not start.
const FAILURE: u8 = 1;

/// The Hexput scripting runtime daemon.
#[derive(Debug, Parser)]
#[command(name = "hexput-daemon", version)]
struct Args {
    /// Path to the System Config file. Overrides the HEXPUT_CONFIG environment variable and the
    /// default path.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

/// Where the System Config is looked for when `--config` is absent: the value of
/// `HEXPUT_CONFIG` and the default path. A parameter of [`run_with`] so tests never touch the
/// process environment.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Discovery {
    /// The value of [`hexput_config::ENV_VAR`], if set.
    pub env: Option<OsString>,
    /// The fixed default path.
    pub default_path: PathBuf,
}

impl Discovery {
    /// The real process's discovery: its own `HEXPUT_CONFIG` and the per-OS default path.
    pub fn from_process() -> Self {
        Self {
            env: std::env::var_os(hexput_config::ENV_VAR),
            default_path: hexput_config::default_path(),
        }
    }

    /// An explicit discovery, for a caller that is not the real process.
    pub fn new(env: Option<OsString>, default_path: impl Into<PathBuf>) -> Self {
        Self {
            env,
            default_path: default_path.into(),
        }
    }
}

/// Run the Daemon over `args` (including the program name, as `std::env::args_os` yields it),
/// with the process's environment, stdout and stderr, until SIGINT or SIGTERM.
///
/// Log lines go to stderr. Never ends the process: `hexput-bin`'s `main` returns the
/// [`ExitCode`].
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    // Unlocked handles, unlike `hexput-cli-core`: `err` lives for the Daemon's whole run, and
    // holding stderr's lock that long would block every log line a runtime worker thread writes.
    run_with(
        args,
        &Discovery::from_process(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        std::io::stderr(),
        shutdown_signal(),
    )
}

/// [`run`] against a caller-supplied discovery, sinks, log writer and shutdown future.
///
/// `out` receives explicitly requested help/version text, `err` usage and startup errors, and
/// `log` every `tracing` event once the System Config is loaded. The Daemon runs until
/// `shutdown` completes; it is polled on the Daemon's own Tokio runtime, so it may use Tokio
/// facilities such as `tokio::signal`.
pub fn run_with<I, T, L, S>(
    args: I,
    discovery: &Discovery,
    out: &mut dyn Write,
    err: &mut dyn Write,
    log: L,
    shutdown: S,
) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    L: Write + Send + 'static,
    S: Future<Output = ()>,
{
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            // `use_stderr` is false only for an explicit `--help`/`--version`: what was asked
            // for, so it goes to stdout and is a success.
            return if error.use_stderr() {
                let _ = write!(err, "{}", error.render());
                ExitCode::from(USAGE)
            } else {
                let _ = write!(out, "{}", error.render());
                ExitCode::SUCCESS
            };
        }
    };

    let loaded = match hexput_config::resolve(
        args.config.as_deref(),
        discovery.env.as_deref().map(OsStr::new),
        &discovery.default_path,
    ) {
        Ok(loaded) => loaded,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return ExitCode::from(FAILURE);
        }
    };

    let dispatch = logging(loaded.config.log_level, log);
    let runtime = {
        // Every runtime worker thread gets the same subscriber for its whole life. The guard is
        // leaked on purpose: dropping it would restore the previous default the moment the
        // thread-start hook returns, and the thread-local it sets is freed with the thread.
        let worker_dispatch = dispatch.clone();
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("hexput-daemon")
            .on_thread_start(move || {
                std::mem::forget(tracing::dispatcher::set_default(&worker_dispatch));
            })
            .build()
    };
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(err, "error: cannot start the Tokio runtime: {error}");
            return ExitCode::from(FAILURE);
        }
    };

    // Scoped, not global: a test runs several Daemons in one process, each with its own log.
    let served =
        tracing::dispatcher::with_default(&dispatch, || runtime.block_on(serve(&loaded, shutdown)));
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            ExitCode::from(FAILURE)
        }
    }
}

/// How long the accept loop pauses after a failed accept, so a persistent failure (out of file
/// descriptors) cannot spin a core.
#[cfg(unix)]
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The Daemon proper: announce the System Config, bind the socket, serve connections until
/// `shutdown` completes, then remove the socket. `Err` is the one-line startup failure.
#[cfg(unix)]
async fn serve<S: Future<Output = ()>>(loaded: &Loaded, shutdown: S) -> Result<(), String> {
    tracing::info!(
        path = %loaded.path.display(),
        source = %loaded.source.as_str(),
        "System Config loaded",
    );
    let listener = listen(loaded)?;
    tracing::info!(path = %listener.path().display(), "listening on a Unix Domain Socket");

    // Poll once before announcing readiness: a signal future installs its handlers on first
    // poll, so once "waiting" is logged a signal is certain to be caught rather than meeting the
    // default disposition.
    let mut shutdown = std::pin::pin!(shutdown);
    let armed = std::future::poll_fn(|cx| Poll::Ready(shutdown.as_mut().poll(cx))).await;
    if armed.is_pending() {
        tracing::info!("daemon started; waiting for a shutdown signal");
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                () = shutdown.as_mut() => break,
                Some(joined) = connections.join_next() => {
                    if let Err(error) = joined
                        && error.is_panic()
                    {
                        tracing::error!("a connection task panicked; other connections are unaffected");
                    }
                }
                accepted = listener.accept() => match accepted {
                    Ok(port) => {
                        tracing::debug!("connection accepted");
                        connections.spawn(hexput_connection::serve(port));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "cannot accept a connection");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }
        // Connections end with the runtime; nothing they hold outlives them yet.
        connections.abort_all();
    }
    tracing::info!("shutdown requested; daemon stopping");
    let path = listener.path().to_path_buf();
    if let Err(error) = listener.close() {
        tracing::warn!(path = %path.display(), %error, "cannot remove the socket file");
    }
    Ok(())
}

/// No adapter serves a non-Unix platform until the Named Pipe arrives (Epic 5), so the Daemon
/// refuses to start rather than listen nowhere.
#[cfg(not(unix))]
async fn serve<S: Future<Output = ()>>(loaded: &Loaded, _shutdown: S) -> Result<(), String> {
    Err(format!(
        "{}: no transport is served on this platform yet; the Named Pipe adapter arrives with \
         a later release",
        loaded.path.display()
    ))
}

/// Bind the configured Unix Domain Socket, refusing a System Config that names a transport no
/// adapter serves yet.
#[cfg(unix)]
fn listen(loaded: &Loaded) -> Result<hexput_transport::uds::UdsListener, String> {
    let transports = &loaded.config.transports;
    let unserved: Vec<&str> = [
        (transports.tcp.is_some(), "[transport.tcp]"),
        (transports.websocket.is_some(), "[transport.websocket]"),
    ]
    .into_iter()
    .filter_map(|(present, name)| present.then_some(name))
    .collect();
    if !unserved.is_empty() {
        return Err(format!(
            "{}: {} not supported yet; only [transport.uds] is served in this version",
            loaded.path.display(),
            unserved.join(" and "),
        ));
    }
    // A parsed System Config has at least one transport, and only UDS remains.
    let Some(uds) = transports.uds.as_ref() else {
        return Err(format!("{}: no transport to serve", loaded.path.display()));
    };
    let listener =
        hexput_transport::uds::UdsListener::bind(&uds.path, uds.mode).map_err(|e| e.to_string())?;
    if listener.replaced_stale() {
        tracing::info!(
            path = %uds.path.display(),
            "removed a stale socket file left by an unclean shutdown",
        );
    }
    Ok(listener)
}

/// A plain-text `fmt` subscriber writing to `log`, filtered at the System Config's `log_level`.
/// Structured JSON output is Story 2.8's.
fn logging<L: Write + Send + 'static>(level: LogLevel, log: L) -> tracing::Dispatch {
    let filter = match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    };
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(filter)
        .with_writer(Mutex::new(log))
        .finish();
    tracing::Dispatch::new(subscriber)
}

/// Completes on SIGINT or SIGTERM (Ctrl-C on Windows). The signal handlers are registered when
/// the future is first polled, on the Daemon's runtime.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{Signal, SignalKind, signal};
        // Keep whichever handler installed. Tokio never unregisters a handler, so dropping a
        // working stream would swallow that signal for good instead of restoring its default.
        let install = |kind: SignalKind, name: &str| -> Option<Signal> {
            signal(kind)
                .inspect_err(|error| tracing::warn!("cannot install the {name} handler: {error}"))
                .ok()
        };
        let mut interrupt = install(SignalKind::interrupt(), "SIGINT");
        let mut terminate = install(SignalKind::terminate(), "SIGTERM");
        if interrupt.is_none() && terminate.is_none() {
            // Neither handler installed, so both signals keep their default disposition and
            // still end the process; there is nothing to wait on.
            return std::future::pending().await;
        }
        std::future::poll_fn(|cx| {
            let mut fired = |stream: &mut Option<Signal>| {
                stream
                    .as_mut()
                    .is_some_and(|stream| stream.poll_recv(cx).is_ready())
            };
            if fired(&mut interrupt) || fired(&mut terminate) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            tracing::warn!("cannot install the Ctrl-C handler; relying on default signal handling");
            std::future::pending::<()>().await;
        }
    }
}
