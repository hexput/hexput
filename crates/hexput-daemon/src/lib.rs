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
//! * `1` — the System Config could not be loaded (missing, unreadable, malformed, invalid —
//!   including a TCP or WebSocket section without `tls_cert`/`tls_key`), it configures no
//!   transport this platform serves, a TLS certificate or key could not be loaded (missing,
//!   unreadable, malformed, or a key that does not match the certificate), a listener could not
//!   be bound (including a Named Pipe another process already serves), or the runtime could not
//!   start. The reason is one line on stderr naming the file, the socket path, the pipe or the
//!   TCP/WebSocket address; nothing is left listening or on disk.
//!
//! # Serving
//!
//! The Daemon serves every configured transport from one accept loop (`hexput-transport`): the
//! local transport — the Unix Domain Socket on Unix, the Named Pipe on Windows (Story 5.3) —,
//! TCP with mandatory TLS (Story 5.1) and WebSocket with mandatory TLS (Story 5.2), the last two
//! on every platform. A configured section this platform cannot serve (`[transport.uds]` off
//! Unix, `[transport.named_pipe]` off Windows) is skipped with a `warn` naming it, logged right
//! after the System Config, so one file can serve hosts of both kinds; only a System Config with
//! nothing servable here fails to start. Startup then runs in a fixed order — load the TCP TLS
//! identity, load the WebSocket TLS identity (separately, even when it names the same files),
//! bind the local transport, bind the TCP listener, bind the WebSocket listener — so a bad
//! certificate binds nothing, and a bind failure closes every listener bound before it.
//! Everything is bound before the Daemon announces that it started; the socket's or pipe's path
//! is logged (`"listening on a Unix Domain Socket"` / `"listening on a Named Pipe"`, field
//! `path`), and each network listener's actual address (`"listening on TCP with TLS"` /
//! `"listening on WebSocket with TLS"`, field `address`). Each accepted connection is its own
//! Tokio task running `hexput_connection::serve`, which is generic over the Port and never
//! learns the transport (AD-1); a connection that fails, disconnects abruptly or panics ends
//! only its own task. A TCP connection's TLS handshake, and a WebSocket
//! connection's TLS handshake and upgrade, run inside that task, bounded by
//! `hexput_transport::tcp_tls::HANDSHAKE_TIMEOUT`, so a slow peer never stalls the accept loop;
//! a failed handshake closes that socket and is logged at `debug` with the peer's address
//! (`"TLS handshake failed"` / `"WebSocket handshake failed"`). Every connection shares the run's
//! one Session registry (`hexput-session`), which is where an `Init` creates a Session. On
//! shutdown the Daemon stops accepting on every listener and removes the socket file it created;
//! a Named Pipe has no file and is gone once its listener is dropped.

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use clap::Parser;
use hexput_config::{Loaded, LogFormat, LogLevel};
use hexput_session::Sessions;
use hexput_transport::tcp_tls::{TcpTlsListener, TlsIdentity};
use hexput_transport::websocket::WebSocketListener;
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

    let dispatch = logging(loaded.config.log_level, loaded.config.log_format, log);
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
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The Daemon's Session registry over `cache`: a Session's teardown releases its cache entries
/// through the registry's hook, so `hexput-session` never reaches `hexput-script` (Story 4.3). The
/// one place this pairing is made — the connection opens a Session in the cache at init, and this
/// hook closes it — so a test can check the Daemon's own wiring.
fn registry(cache: &Arc<hexput_script::AstCache>) -> Arc<Sessions> {
    let released = Arc::clone(cache);
    Arc::new(Sessions::new().with_teardown(move |client_id| released.close(client_id)))
}

/// The Daemon proper: announce the System Config, bind every listener, serve connections until
/// `shutdown` completes, then stop listening and remove the socket. `Err` is the one-line
/// startup failure.
async fn serve<S: Future<Output = ()>>(loaded: &Loaded, shutdown: S) -> Result<(), String> {
    tracing::info!(
        path = %loaded.path.display(),
        source = %loaded.source.as_str(),
        "System Config loaded",
    );
    servable(loaded)?;
    let mut listeners = listen(loaded).await?;

    // Poll once before announcing readiness: a signal future installs its handlers on first
    // poll, so once "waiting" is logged a signal is certain to be caught rather than meeting the
    // default disposition.
    let mut shutdown = std::pin::pin!(shutdown);
    let armed = std::future::poll_fn(|cx| Poll::Ready(shutdown.as_mut().poll(cx))).await;
    if armed.is_pending() {
        tracing::info!("daemon started; waiting for a shutdown signal");
        // One AST Cache per Daemon run (Story 4.1): its handles come from one counter, and it
        // holds at most System Config's capacity of entries (Story 4.3).
        let cache = Arc::new(hexput_script::AstCache::with_capacity(
            loaded.config.ast_cache_capacity,
        ));
        // One Session registry per Daemon run beside it, shared by every connection.
        let sessions = registry(&cache);
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
                // The local transport — the Unix Domain Socket on Unix, the Named Pipe on Windows —
                // has no handshake, so its connection is served directly.
                accepted = accept(listeners.local.as_mut(), local::accept) => match accepted {
                    Ok(port) => {
                        // `serve` logs the connection's opening inside its own span.
                        connections.spawn(hexput_connection::serve(
                            port,
                            Arc::clone(&sessions),
                            Arc::clone(&cache),
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(
                            transport = local::TRANSPORT,
                            %error,
                            "cannot accept a connection",
                        );
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                accepted = accept(listeners.tcp.as_ref(), TcpTlsListener::accept) => match accepted {
                    Ok(handshake) => {
                        let (sessions, cache) = (Arc::clone(&sessions), Arc::clone(&cache));
                        // The handshake runs in the connection's own task, never here, so a slow
                        // or silent peer cannot stall the next accept.
                        connections.spawn(async move {
                            let peer = handshake.peer_addr();
                            match handshake.finish().await {
                                Ok(port) => hexput_connection::serve(port, sessions, cache).await,
                                Err(error) => {
                                    tracing::debug!(%peer, %error, "TLS handshake failed");
                                }
                            }
                        });
                    }
                    Err(error) => {
                        tracing::warn!(transport = "tcp", %error, "cannot accept a connection");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
                accepted = accept(listeners.websocket.as_ref(), WebSocketListener::accept) => match accepted {
                    Ok(handshake) => {
                        let (sessions, cache) = (Arc::clone(&sessions), Arc::clone(&cache));
                        // The TLS handshake and the upgrade run in the connection's own task.
                        connections.spawn(async move {
                            let peer = handshake.peer_addr();
                            match handshake.finish().await {
                                Ok(port) => hexput_connection::serve(port, sessions, cache).await,
                                Err(error) => {
                                    tracing::debug!(%peer, %error, "WebSocket handshake failed");
                                }
                            }
                        });
                    }
                    Err(error) => {
                        tracing::warn!(transport = "websocket", %error, "cannot accept a connection");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }
        // Aborted connection tasks never reach their detach, so no Session is torn down one by
        // one here: the registry, and every Session in it, is dropped with this run. Nothing a
        // Session holds is on disk or outlives the process, so there is nothing to release.
        connections.abort_all();
    }
    tracing::info!("shutdown requested; daemon stopping");
    listeners.close();
    Ok(())
}

/// The next connection on `listener`; never ready when the transport is not configured, so an
/// absent listener's `select!` branch simply never fires. `R` is a shared or a mutable borrow of
/// the listener, whichever its `accept` takes.
async fn accept<R, F, T>(listener: Option<R>, accept: impl FnOnce(R) -> F) -> T
where
    F: Future<Output = T>,
{
    match listener {
        Some(listener) => accept(listener).await,
        None => std::future::pending().await,
    }
}

/// The platform rule (Story 5.3): the Unix Domain Socket is served only on Unix and the Named
/// Pipe only on Windows; TCP and WebSocket everywhere. A configured section this platform cannot
/// serve is skipped with a warning, and the Daemon fails to start only when nothing configured is
/// served here — a Daemon listening nowhere is not running. Runs before any identity loads or
/// anything binds.
fn servable(loaded: &Loaded) -> Result<(), String> {
    let transports = &loaded.config.transports;
    let mut skipped = Vec::new();
    if !cfg!(unix)
        && let Some(uds) = &transports.uds
    {
        tracing::warn!(
            path = %uds.path.display(),
            "[transport.uds] is Unix-only; not serving it on this platform",
        );
        skipped.push("[transport.uds] is Unix-only");
    }
    if !cfg!(windows)
        && let Some(pipe) = &transports.named_pipe
    {
        tracing::warn!(
            path = %pipe.path,
            "[transport.named_pipe] is Windows-only; not serving it on this platform",
        );
        skipped.push("[transport.named_pipe] is Windows-only");
    }
    let served =
        local::configured(transports) || transports.tcp.is_some() || transports.websocket.is_some();
    if served {
        Ok(())
    } else {
        Err(format!(
            "{}: no configured transport is served on this platform ({})",
            loaded.path.display(),
            skipped.join("; ")
        ))
    }
}

/// The local transport on Unix: the Unix Domain Socket.
#[cfg(unix)]
mod local {
    use std::io;

    use hexput_config::Transports;
    use hexput_transport::uds::{UdsListener, UdsPort};

    /// The local listener's type.
    pub(crate) type Listener = UdsListener;

    /// The `transport` field of an accept failure's log line.
    pub(crate) const TRANSPORT: &str = "uds";

    /// Whether the System Config names the local transport this platform serves.
    pub(crate) const fn configured(transports: &Transports) -> bool {
        transports.uds.is_some()
    }

    /// Bind the Unix Domain Socket, if configured.
    pub(crate) fn bind(transports: &Transports) -> Result<Option<Listener>, String> {
        let Some(uds) = transports.uds.as_ref() else {
            return Ok(None);
        };
        let listener = UdsListener::bind(&uds.path, uds.mode).map_err(|e| e.to_string())?;
        if listener.replaced_stale() {
            tracing::info!(
                path = %uds.path.display(),
                "removed a stale socket file left by an unclean shutdown",
            );
        }
        tracing::info!(path = %listener.path().display(), "listening on a Unix Domain Socket");
        Ok(Some(listener))
    }

    /// The next connection on the socket.
    pub(crate) async fn accept(listener: &mut Listener) -> io::Result<UdsPort> {
        listener.accept().await
    }

    /// Stop listening and remove the socket file.
    pub(crate) fn close(listener: Listener) {
        let path = listener.path().to_path_buf();
        if let Err(error) = listener.close() {
            tracing::warn!(path = %path.display(), %error, "cannot remove the socket file");
        }
    }
}

/// The local transport on Windows: the Named Pipe (Story 5.3).
#[cfg(windows)]
mod local {
    use std::io;

    use hexput_config::Transports;
    use hexput_transport::named_pipe::{NamedPipeListener, NamedPipePort};

    /// The local listener's type.
    pub(crate) type Listener = NamedPipeListener;

    /// The `transport` field of an accept failure's log line.
    pub(crate) const TRANSPORT: &str = "named_pipe";

    /// Whether the System Config names the local transport this platform serves.
    pub(crate) const fn configured(transports: &Transports) -> bool {
        transports.named_pipe.is_some()
    }

    /// Create the Named Pipe's first instance, if configured.
    pub(crate) fn bind(transports: &Transports) -> Result<Option<Listener>, String> {
        let Some(pipe) = transports.named_pipe.as_ref() else {
            return Ok(None);
        };
        let listener = NamedPipeListener::bind(&pipe.path).map_err(|e| e.to_string())?;
        tracing::info!(path = %listener.path(), "listening on a Named Pipe");
        Ok(Some(listener))
    }

    /// The next client on the pipe.
    pub(crate) async fn accept(listener: &mut Listener) -> io::Result<NamedPipePort> {
        listener.accept().await
    }

    /// Stop serving the pipe: dropping the listener closes its waiting instance, and there is
    /// no file to remove.
    pub(crate) fn close(listener: Listener) {
        drop(listener);
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("hexput-daemon serves Unix and Windows hosts only");

/// Every listener the System Config names, each present only when configured and served on this
/// platform.
struct Listeners {
    /// The Unix Domain Socket on Unix, the Named Pipe on Windows.
    local: Option<local::Listener>,
    tcp: Option<TcpTlsListener>,
    websocket: Option<WebSocketListener>,
}

impl Listeners {
    /// Stop accepting on every listener and remove the socket file.
    fn close(self) {
        drop(self.websocket);
        drop(self.tcp);
        if let Some(listener) = self.local {
            local::close(listener);
        }
    }
}

/// Bind every configured listener, in the fixed startup order: load the TCP TLS identity, load
/// the WebSocket TLS identity, bind the local transport (the Unix Domain Socket on Unix, the
/// Named Pipe on Windows), bind the TCP listener, bind the WebSocket listener. A failure after a
/// listener is bound closes every listener bound before it, so a failed start leaves nothing
/// listening or on disk.
async fn listen(loaded: &Loaded) -> Result<Listeners, String> {
    let transports = &loaded.config.transports;
    // 1–2. The TLS identities, before anything is bound: a bad certificate binds nothing. The
    // WebSocket identity is loaded on its own even when it names the TCP files.
    let tcp = match transports.tcp.as_ref() {
        None => None,
        Some(tcp) => Some((
            tcp.bind,
            TlsIdentity::load(&tcp.tls.cert, &tcp.tls.key).map_err(|error| error.to_string())?,
        )),
    };
    let websocket = match transports.websocket.as_ref() {
        None => None,
        Some(websocket) => Some((
            websocket.bind,
            TlsIdentity::load(&websocket.tls.cert, &websocket.tls.key)
                .map_err(|error| error.to_string())?,
        )),
    };
    // 3. The local transport.
    let local = local::bind(transports)?;
    // 4. The TCP listener.
    let tcp = match tcp {
        None => None,
        Some((bind, identity)) => match TcpTlsListener::bind(bind, identity).await {
            Ok(listener) => {
                let address = listener.local_addr().unwrap_or(bind);
                tracing::info!(%address, "listening on TCP with TLS");
                Some(listener)
            }
            Err(error) => {
                if let Some(listener) = local {
                    local::close(listener);
                }
                return Err(format!("{bind}: cannot listen for TCP: {error}"));
            }
        },
    };
    // 5. The WebSocket listener.
    let websocket = match websocket {
        None => None,
        Some((bind, identity)) => match WebSocketListener::bind(bind, identity).await {
            Ok(listener) => {
                let address = listener.local_addr().unwrap_or(bind);
                tracing::info!(%address, "listening on WebSocket with TLS");
                Some(listener)
            }
            Err(error) => {
                Listeners {
                    local,
                    tcp,
                    websocket: None,
                }
                .close();
                return Err(format!("{bind}: cannot listen for WebSocket: {error}"));
            }
        },
    };
    Ok(Listeners {
        local,
        tcp,
        websocket,
    })
}

/// A `fmt` subscriber writing to `log` in the System Config's `log_format`, filtered at its
/// `log_level`. Plain text shows a connection's and a request's span fields in each line's
/// context prefix; JSON writes one object per line, the fields of the innermost span under
/// `span` and those of every enclosing span under `spans`. The level filters events only in
/// effect: `hexput-connection` creates its spans at `ERROR`, so they pass any level.
fn logging<L: Write + Send + 'static>(
    level: LogLevel,
    format: LogFormat,
    log: L,
) -> tracing::Dispatch {
    let filter = match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    };
    let builder = tracing_subscriber::fmt()
        .with_max_level(filter)
        .with_writer(Mutex::new(log));
    match format {
        LogFormat::Text => tracing::Dispatch::new(builder.finish()),
        LogFormat::Json => tracing::Dispatch::new(
            builder
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .finish(),
        ),
    }
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

// In-crate rather than in `hexput-tests`: `registry` is private to the Daemon's composition root,
// and the wiring it makes is not observable over the socket before Epic 5 lets a Client ID be
// reattached (Story 4.3).
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hexput_script::AstCache;
    use hexput_session::{InitRequest, Settings};
    use rmpv::Value;

    use super::registry;

    fn map(fields: Vec<(&str, Value)>) -> Value {
        Value::Map(
            fields
                .into_iter()
                .map(|(key, value)| (Value::from(key), value))
                .collect(),
        )
    }

    #[test]
    fn the_daemons_registry_releases_a_sessions_entries_when_it_is_torn_down() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let register = |cache: &Arc<AstCache>, client_id| {
            runtime.block_on(hexput_script::code_register(
                map(vec![("source", Value::from("return 1;"))]),
                client_id,
                Arc::clone(cache),
                Vec::new(),
                Settings::default(),
            ))
        };
        let cache = Arc::new(AstCache::with_capacity(16));
        let sessions = registry(&cache);
        let init = map(vec![
            ("config", map(Vec::new())),
            ("registrations", Value::Array(Vec::new())),
        ]);
        let mut sessions_made = Vec::new();
        for _ in 0..2 {
            let connection = sessions.connect();
            let client_id = sessions.create(InitRequest::from_value(&init).unwrap(), connection);
            // As `hexput-connection`'s init does.
            cache.open(client_id);
            register(&cache, client_id).unwrap();
            sessions_made.push((client_id, connection));
        }
        let [(ended, connection), (kept, _)] = sessions_made[..] else {
            unreachable!("two Sessions were made");
        };
        sessions.detach(ended, connection);
        assert!(cache.handles(ended).is_empty());
        assert_eq!(cache.handles(kept).len(), 1);
        assert_eq!(cache.entry_count(), 1);
        // And a registration arriving after the teardown leaves nothing behind.
        let late = register(&cache, ended).unwrap_err();
        assert_eq!(late.code, "protocol.init_not_completed");
        assert_eq!(cache.entry_count(), 1);
    }
}
