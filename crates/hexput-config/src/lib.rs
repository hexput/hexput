//! System Config: the Daemon's own operational settings, read once at startup from one TOML file.
//!
//! This is **not** per-backend Config. A Backend's Config arrives inline over the socket, lives in
//! `hexput-session`, and never touches disk; System Config configures the Daemon itself and is
//! never changed by a Backend (AD-5). The two crates have no dependency on each other, which
//! `scripts/check-crate-graph.py` asserts in both directions.
//!
//! Binds: AD-5, AD-7, FR-24.
//!
//! # Where the file is (AD-7)
//!
//! [`resolve`] picks the file by one fixed precedence, identical for systemd, Docker or anything
//! else — packaging only sets the environment variable or mounts the default path:
//!
//! 1. the `--config <path>` flag, if given;
//! 2. otherwise the [`ENV_VAR`] (`HEXPUT_CONFIG`) environment variable, if set and non-empty;
//! 3. otherwise the fixed per-OS default path, [`default_path`].
//!
//! The resolver **never falls through**: a flag or environment variable naming a file that does
//! not exist is an error about *that* file, not a reason to try the next source. An operator who
//! named a file meant that file.
//!
//! # What the file holds
//!
//! ```toml
//! log_level = "info"          # optional: error | warn | info | debug | trace — default "info"
//! session_ttl_secs = 300      # optional: a whole number of seconds, at least 1 — default 300
//!
//! [transport.uds]             # at least one [transport.*] section is required
//! path = "/run/hexput/hexput.sock"
//!
//! [transport.tcp]             # TCP always uses TLS: both paths are required
//! bind = "0.0.0.0:7400"
//! tls_cert = "/etc/hexput/tls/cert.pem"
//! tls_key = "/etc/hexput/tls/key.pem"
//!
//! [transport.websocket]       # TLS optional, but tls_cert and tls_key come together or not at all
//! bind = "0.0.0.0:7401"
//! ```
//!
//! `crates/hexput-config/config.example.toml` documents every field, and a test parses it so the
//! example cannot drift from this parser. Every field inside a present transport section is
//! required; only `log_level` and `session_ttl_secs` have defaults, and both are stated above.
//! Unknown keys are rejected, so a typo cannot silently drop a setting. Paths are kept exactly as
//! written; a relative path is relative to the Daemon's working directory, not to the file.
//!
//! # Loaded once
//!
//! The file is read once, at startup. There is deliberately no reload, watch or write function:
//! changing System Config means restarting the Daemon.

mod file;

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use file::parse;

/// The environment variable that names the System Config file when no `--config` flag is given.
pub const ENV_VAR: &str = "HEXPUT_CONFIG";

/// The command-line flag that names the System Config file, as an operator types it.
pub const FLAG: &str = "--config";

/// The default System Config path when neither the flag nor [`ENV_VAR`] names one.
///
/// On Windows this is the fallback used only when the `ProgramData` environment variable is
/// unset; [`default_path`] is what the Daemon actually uses.
#[cfg(not(windows))]
pub const DEFAULT_PATH: &str = "/etc/hexput/config.toml";

/// The default System Config path when neither the flag nor [`ENV_VAR`] names one.
///
/// On Windows this is the fallback used only when the `ProgramData` environment variable is
/// unset; [`default_path`] is what the Daemon actually uses.
#[cfg(windows)]
pub const DEFAULT_PATH: &str = r"C:\ProgramData\hexput\config.toml";

/// `session_ttl_secs` when the file does not set it.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(300);

/// The fixed per-OS default path: `/etc/hexput/config.toml` on Unix,
/// `%ProgramData%\hexput\config.toml` on Windows (falling back to [`DEFAULT_PATH`],
/// `C:\ProgramData\hexput\config.toml`, only when `ProgramData` is unset).
pub fn default_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(program_data) = std::env::var_os("ProgramData").filter(|v| !v.is_empty()) {
            return PathBuf::from(program_data)
                .join("hexput")
                .join("config.toml");
        }
    }
    PathBuf::from(DEFAULT_PATH)
}

/// The Daemon's operational settings: transports, log level, default Session TTL.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SystemConfig {
    /// Where the Daemon listens. At least one transport is always present.
    pub transports: Transports,
    /// The least severe log event the Daemon emits. Defaults to [`LogLevel::Info`].
    pub log_level: LogLevel,
    /// How long a Session outlives its last Connection. Never zero; defaults to
    /// [`DEFAULT_SESSION_TTL`] (300 seconds).
    pub session_ttl: Duration,
}

/// The configured transports. A parsed [`SystemConfig`] always has at least one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Transports {
    /// `[transport.uds]`: a Unix Domain Socket.
    pub uds: Option<UdsTransport>,
    /// `[transport.tcp]`: TCP, always with TLS.
    pub tcp: Option<TcpTransport>,
    /// `[transport.websocket]`: WebSocket, with or without TLS.
    pub websocket: Option<WebSocketTransport>,
}

/// `[transport.uds]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UdsTransport {
    /// The socket file's path.
    pub path: PathBuf,
}

/// `[transport.tcp]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TcpTransport {
    /// The address and port to listen on.
    pub bind: SocketAddr,
    /// The certificate and key. Required: TCP is never plaintext.
    pub tls: Tls,
}

/// `[transport.websocket]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WebSocketTransport {
    /// The address and port to listen on.
    pub bind: SocketAddr,
    /// The certificate and key, when the WebSocket listener uses TLS.
    pub tls: Option<Tls>,
}

/// A TLS certificate and private key, always given together.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Tls {
    /// `tls_cert`: the certificate chain file.
    pub cert: PathBuf,
    /// `tls_key`: the private key file.
    pub key: PathBuf,
}

/// `log_level`: the least severe event the Daemon emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Every level, most severe first — also the order the accepted values are listed in errors.
    pub const ALL: [LogLevel; 5] = [
        LogLevel::Error,
        LogLevel::Warn,
        LogLevel::Info,
        LogLevel::Debug,
        LogLevel::Trace,
    ];

    /// The level as it is written in the file.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which AD-7 source supplied the System Config path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigSource {
    /// The `--config` command-line flag.
    Flag,
    /// The [`ENV_VAR`] environment variable.
    Env,
    /// The fixed default path.
    Default,
}

impl ConfigSource {
    /// A short machine-readable name — `flag`, `env` or `default` — for structured log fields.
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigSource::Flag => "flag",
            ConfigSource::Env => "env",
            ConfigSource::Default => "default",
        }
    }
}

/// The human form, as used in error messages: "the --config flag", "the HEXPUT_CONFIG
/// environment variable", "the default path".
impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigSource::Flag => write!(f, "the {FLAG} flag"),
            ConfigSource::Env => write!(f, "the {ENV_VAR} environment variable"),
            ConfigSource::Default => f.write_str("the default path"),
        }
    }
}

/// A successfully resolved and parsed System Config, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Loaded {
    /// The parsed settings.
    pub config: SystemConfig,
    /// The file's path, exactly as the chosen source gave it.
    pub path: PathBuf,
    /// Which source supplied [`Loaded::path`].
    pub source: ConfigSource,
}

/// A 1-based position in the System Config file. `column` counts characters, not bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// What went wrong with the System Config file.
#[derive(Debug)]
#[non_exhaustive]
pub enum Problem {
    /// No file exists at the path.
    NotFound,
    /// The file exists but could not be read (permissions, a directory, not UTF-8, ...).
    Unreadable(io::Error),
    /// The TOML parser rejected the file: malformed TOML, an unknown key, a missing required
    /// field or a value of the wrong type. `message` names the key or field concerned.
    Parse {
        location: Option<Location>,
        message: String,
    },
    /// A field is well-formed TOML but its value is not acceptable. `field` is the dotted key,
    /// `message` says what is accepted.
    InvalidValue {
        field: String,
        location: Option<Location>,
        message: String,
    },
    /// The file configures no transport at all.
    NoTransport,
}

/// Why the System Config could not be loaded. Always names the resolved path, the source that
/// supplied it, and the specific [`Problem`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ConfigError {
    /// The resolved path, exactly as its source gave it.
    pub path: PathBuf,
    /// Which source supplied [`ConfigError::path`].
    pub source: ConfigSource,
    /// What went wrong.
    pub problem: Problem,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        let at = |f: &mut fmt::Formatter<'_>, location: &Option<Location>| match location {
            Some(Location { line, column }) => write!(f, "{path}:{line}:{column}"),
            None => write!(f, "{path}"),
        };
        match &self.problem {
            Problem::NotFound => match self.source {
                ConfigSource::Default => write!(
                    f,
                    "{path}: no System Config file at the default path; pass {FLAG} <path> or \
                     set {ENV_VAR} to use another file"
                ),
                source => write!(
                    f,
                    "{path}: System Config file not found (named by {source})"
                ),
            },
            Problem::Unreadable(error) => write!(
                f,
                "{path}: cannot read System Config file (named by {}): {error}",
                self.source
            ),
            Problem::Parse { location, message } => {
                at(f, location)?;
                write!(f, ": {message}")
            }
            Problem::InvalidValue {
                field,
                location,
                message,
            } => {
                at(f, location)?;
                write!(f, ": invalid value for `{field}`: {message}")
            }
            Problem::NoTransport => write!(
                f,
                "{path}: no transport configured; at least one of [transport.uds], \
                 [transport.tcp] or [transport.websocket] is required"
            ),
        }
    }
}

/// No `source()`: an [`Problem::Unreadable`] I/O error is already part of the `Display` text,
/// and returning it again would make a chained error reporter print it twice.
impl std::error::Error for ConfigError {}

/// Resolve the System Config file by AD-7's precedence and parse it.
///
/// `flag` is the `--config` value, `env` the value of [`ENV_VAR`], `default_path` the fixed
/// default ([`default_path`] in the Daemon). All three are parameters so the one resolution rule
/// is testable without touching the process environment. An empty `env` counts as unset; an
/// empty `flag` does not, and fails as a file that cannot be found.
///
/// Never falls through: the first source present is the only one tried.
pub fn resolve(
    flag: Option<&Path>,
    env: Option<&OsStr>,
    default_path: &Path,
) -> Result<Loaded, ConfigError> {
    let (path, source) = if let Some(flag) = flag {
        (flag.to_path_buf(), ConfigSource::Flag)
    } else if let Some(env) = env.filter(|value| !value.is_empty()) {
        (PathBuf::from(env), ConfigSource::Env)
    } else {
        (default_path.to_path_buf(), ConfigSource::Default)
    };

    let fail = |problem| ConfigError {
        path: path.clone(),
        source,
        problem,
    };
    let text = std::fs::read_to_string(&path).map_err(|error| {
        fail(match error.kind() {
            io::ErrorKind::NotFound => Problem::NotFound,
            _ => Problem::Unreadable(error),
        })
    })?;
    let config = parse(&text).map_err(fail)?;
    Ok(Loaded {
        config,
        path,
        source,
    })
}
