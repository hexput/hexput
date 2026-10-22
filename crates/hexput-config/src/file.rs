//! The file's TOML shape and its validation into a [`SystemConfig`].
//!
//! Two layers, deliberately. `serde` with `deny_unknown_fields` handles the *shape* — syntax,
//! unknown keys, missing required fields, wrong types — and reports each with its position. The
//! *values* are then checked by hand, because a derived deserializer's message for `"loud"` or
//! `0` would not name the field or list what is accepted. Every value is read as a `Spanned` so
//! that second layer can point at it too.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use toml::Spanned;

use crate::{
    DEFAULT_AST_CACHE_CAPACITY, DEFAULT_SESSION_TTL, DEFAULT_TCP_PORT, DEFAULT_WEBSOCKET_PORT,
    Location, LogFormat, LogLevel, Problem, SystemConfig, TcpTransport, Tls, Transports,
    UdsTransport, WebSocketTransport,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    log_level: Option<Spanned<String>>,
    log_format: Option<Spanned<String>>,
    session_ttl_secs: Option<Spanned<i64>>,
    ast_cache_capacity: Option<Spanned<i64>>,
    transport: Option<RawTransports>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransports {
    uds: Option<RawUds>,
    tcp: Option<RawTcp>,
    websocket: Option<RawWebSocket>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUds {
    path: Spanned<String>,
    mode: Option<Spanned<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTcp {
    bind: Spanned<String>,
    tls_cert: Spanned<String>,
    tls_key: Spanned<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWebSocket {
    bind: Spanned<String>,
    tls_cert: Spanned<String>,
    tls_key: Spanned<String>,
}

/// Parse System Config text. Pure: no I/O, no environment. [`crate::resolve`] is this plus
/// finding and reading the file, and wraps a failure with the file's path.
pub fn parse(text: &str) -> Result<SystemConfig, Problem> {
    let raw: RawFile = toml::from_str(text).map_err(|error| Problem::Parse {
        location: error.span().map(|span| locate(text, span.start)),
        message: error.message().trim_end().to_owned(),
    })?;
    Validator { text }.system_config(raw)
}

/// Turns the raw shape into a [`SystemConfig`], pointing every rejected value at its position.
struct Validator<'t> {
    text: &'t str,
}

impl Validator<'_> {
    fn system_config(&self, raw: RawFile) -> Result<SystemConfig, Problem> {
        let log_level = match raw.log_level {
            None => LogLevel::Info,
            Some(level) => self.log_level(&level)?,
        };
        let log_format = match raw.log_format {
            None => LogFormat::Text,
            Some(format) => self.log_format(&format)?,
        };
        let session_ttl = match raw.session_ttl_secs {
            None => DEFAULT_SESSION_TTL,
            Some(ttl) => self.session_ttl(&ttl)?,
        };
        let ast_cache_capacity = match raw.ast_cache_capacity {
            None => DEFAULT_AST_CACHE_CAPACITY,
            Some(capacity) => self.ast_cache_capacity(&capacity)?,
        };
        let transports = match raw.transport {
            None => return Err(Problem::NoTransport),
            Some(transports) => self.transports(transports)?,
        };
        Ok(SystemConfig {
            transports,
            log_level,
            log_format,
            session_ttl,
            ast_cache_capacity,
        })
    }

    fn log_level(&self, level: &Spanned<String>) -> Result<LogLevel, Problem> {
        LogLevel::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == level.get_ref())
            .ok_or_else(|| {
                let accepted: Vec<_> = LogLevel::ALL.iter().map(|l| l.as_str()).collect();
                self.invalid(
                    "log_level",
                    level,
                    format!(
                        "expected one of {}, got {:?}",
                        accepted.join(", "),
                        level.get_ref()
                    ),
                )
            })
    }

    fn log_format(&self, format: &Spanned<String>) -> Result<LogFormat, Problem> {
        LogFormat::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == format.get_ref())
            .ok_or_else(|| {
                let accepted: Vec<_> = LogFormat::ALL.iter().map(|f| f.as_str()).collect();
                self.invalid(
                    "log_format",
                    format,
                    format!(
                        "expected one of {}, got {:?}",
                        accepted.join(", "),
                        format.get_ref()
                    ),
                )
            })
    }

    fn session_ttl(&self, ttl: &Spanned<i64>) -> Result<Duration, Problem> {
        match u64::try_from(*ttl.get_ref()) {
            Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
            _ => Err(self.invalid(
                "session_ttl_secs",
                ttl,
                format!(
                    "expected a whole number of seconds, at least 1, got {}",
                    ttl.get_ref()
                ),
            )),
        }
    }

    fn ast_cache_capacity(&self, capacity: &Spanned<i64>) -> Result<u64, Problem> {
        match u64::try_from(*capacity.get_ref()) {
            Ok(entries) if entries > 0 => Ok(entries),
            _ => Err(self.invalid(
                "ast_cache_capacity",
                capacity,
                format!(
                    "expected a whole number of entries, at least 1, got {}",
                    capacity.get_ref()
                ),
            )),
        }
    }

    fn transports(&self, raw: RawTransports) -> Result<Transports, Problem> {
        if raw.uds.is_none() && raw.tcp.is_none() && raw.websocket.is_none() {
            return Err(Problem::NoTransport);
        }
        let uds = match raw.uds {
            None => None,
            Some(uds) => Some(UdsTransport {
                path: self.path("transport.uds.path", &uds.path)?,
                mode: match uds.mode {
                    None => None,
                    Some(mode) => Some(self.mode("transport.uds.mode", &mode)?),
                },
            }),
        };
        let tcp = match raw.tcp {
            None => None,
            Some(tcp) => Some(TcpTransport {
                bind: self.bind("transport.tcp.bind", &tcp.bind, DEFAULT_TCP_PORT)?,
                tls: Tls {
                    cert: self.path("transport.tcp.tls_cert", &tcp.tls_cert)?,
                    key: self.path("transport.tcp.tls_key", &tcp.tls_key)?,
                },
            }),
        };
        // Story 5.2: WebSocket always uses TLS too, so both paths are required, and its port
        // defaults to 7478 (FR-29).
        let websocket = match raw.websocket {
            None => None,
            Some(websocket) => Some(WebSocketTransport {
                bind: self.bind(
                    "transport.websocket.bind",
                    &websocket.bind,
                    DEFAULT_WEBSOCKET_PORT,
                )?,
                tls: Tls {
                    cert: self.path("transport.websocket.tls_cert", &websocket.tls_cert)?,
                    key: self.path("transport.websocket.tls_key", &websocket.tls_key)?,
                },
            }),
        };
        Ok(Transports {
            uds,
            tcp,
            websocket,
        })
    }

    /// A listener's address. The port may be left out — a bare IP (`0.0.0.0`, `::`, `::1` or
    /// `[::1]`) listens on `default_port`. Port `0` (an ephemeral port) is valid.
    fn bind(
        &self,
        field: &str,
        bind: &Spanned<String>,
        default_port: u16,
    ) -> Result<SocketAddr, Problem> {
        let text = bind.get_ref();
        if let Ok(addr) = text.parse::<SocketAddr>() {
            return Ok(addr);
        }
        let bare = text
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(text);
        if let Ok(ip) = bare.parse::<IpAddr>() {
            // `[...]` brackets only an IPv6 address.
            if ip.is_ipv6() || bare.len() == text.len() {
                return Ok(SocketAddr::new(ip, default_port));
            }
        }
        let port = default_port;
        let message = format!(
            "expected an IP address with an optional port such as \"0.0.0.0\", \
             \"127.0.0.1:{port}\" or \"[::1]:{port}\" (the port is optional and defaults \
             to {port}), got {text:?}"
        );
        Err(self.invalid(field, bind, message))
    }

    /// A permission mode written as a string of three or four octal digits — `"660"` or
    /// `"0660"` — never above `0777`. A string, not a TOML integer, because TOML has no octal
    /// literal an operator would recognise (`0o660`) and a bare `660` would silently be decimal.
    fn mode(&self, field: &str, mode: &Spanned<String>) -> Result<u32, Problem> {
        let text = mode.get_ref();
        let octal = (3..=4).contains(&text.len()) && text.bytes().all(|b| matches!(b, b'0'..=b'7'));
        let parsed = octal
            .then(|| u32::from_str_radix(text, 8).ok())
            .flatten()
            .filter(|bits| *bits <= 0o777);
        parsed.ok_or_else(|| {
            self.invalid(
                field,
                mode,
                format!(
                    "expected three or four octal digits no greater than \"0777\", \
                     such as \"0660\", got {text:?}"
                ),
            )
        })
    }

    fn path(&self, field: &str, path: &Spanned<String>) -> Result<PathBuf, Problem> {
        if path.get_ref().is_empty() {
            return Err(self.invalid(field, path, "expected a file path, got \"\"".to_owned()));
        }
        Ok(PathBuf::from(path.get_ref()))
    }

    fn invalid<T>(&self, field: &str, value: &Spanned<T>, message: String) -> Problem {
        Problem::InvalidValue {
            field: field.to_owned(),
            location: Some(locate(self.text, value.span().start)),
            message,
        }
    }
}

/// The 1-based line and character column of byte `offset` in `text`.
fn locate(text: &str, offset: usize) -> Location {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Location {
        line: before.matches('\n').count() + 1,
        column: before[line_start..].chars().count() + 1,
    }
}
