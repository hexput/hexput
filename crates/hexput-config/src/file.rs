//! The file's TOML shape and its validation into a [`SystemConfig`].
//!
//! Two layers, deliberately. `serde` with `deny_unknown_fields` handles the *shape* — syntax,
//! unknown keys, missing required fields, wrong types — and reports each with its position. The
//! *values* are then checked by hand, because a derived deserializer's message for `"loud"` or
//! `0` would not name the field or list what is accepted. Every value is read as a `Spanned` so
//! that second layer can point at it too.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use toml::Spanned;

use crate::{
    DEFAULT_SESSION_TTL, Location, LogLevel, Problem, SystemConfig, TcpTransport, Tls, Transports,
    UdsTransport, WebSocketTransport,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    log_level: Option<Spanned<String>>,
    session_ttl_secs: Option<Spanned<i64>>,
    transport: Option<RawTransports>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransports {
    uds: Option<RawUds>,
    tcp: Option<RawTcp>,
    websocket: Option<Spanned<RawWebSocket>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUds {
    path: Spanned<String>,
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
    tls_cert: Option<Spanned<String>>,
    tls_key: Option<Spanned<String>>,
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
        let session_ttl = match raw.session_ttl_secs {
            None => DEFAULT_SESSION_TTL,
            Some(ttl) => self.session_ttl(&ttl)?,
        };
        let transports = match raw.transport {
            None => return Err(Problem::NoTransport),
            Some(transports) => self.transports(transports)?,
        };
        Ok(SystemConfig {
            transports,
            log_level,
            session_ttl,
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

    fn transports(&self, raw: RawTransports) -> Result<Transports, Problem> {
        if raw.uds.is_none() && raw.tcp.is_none() && raw.websocket.is_none() {
            return Err(Problem::NoTransport);
        }
        let uds = match raw.uds {
            None => None,
            Some(uds) => Some(UdsTransport {
                path: self.path("transport.uds.path", &uds.path)?,
            }),
        };
        let tcp = match raw.tcp {
            None => None,
            Some(tcp) => Some(TcpTransport {
                bind: self.bind("transport.tcp.bind", &tcp.bind)?,
                tls: Tls {
                    cert: self.path("transport.tcp.tls_cert", &tcp.tls_cert)?,
                    key: self.path("transport.tcp.tls_key", &tcp.tls_key)?,
                },
            }),
        };
        let websocket = match raw.websocket {
            None => None,
            Some(websocket) => Some(self.websocket(websocket)?),
        };
        Ok(Transports {
            uds,
            tcp,
            websocket,
        })
    }

    fn websocket(&self, raw: Spanned<RawWebSocket>) -> Result<WebSocketTransport, Problem> {
        let table = raw.span();
        let raw = raw.into_inner();
        let tls = match (raw.tls_cert, raw.tls_key) {
            (None, None) => None,
            (Some(cert), Some(key)) => Some(Tls {
                cert: self.path("transport.websocket.tls_cert", &cert)?,
                key: self.path("transport.websocket.tls_key", &key)?,
            }),
            (Some(_), None) => return Err(self.half_tls_pair(table.start, "tls_cert", "tls_key")),
            (None, Some(_)) => return Err(self.half_tls_pair(table.start, "tls_key", "tls_cert")),
        };
        Ok(WebSocketTransport {
            bind: self.bind("transport.websocket.bind", &raw.bind)?,
            tls,
        })
    }

    fn half_tls_pair(&self, table_start: usize, given: &str, missing: &str) -> Problem {
        Problem::InvalidValue {
            field: "transport.websocket".to_owned(),
            location: Some(locate(self.text, table_start)),
            message: format!(
                "tls_cert and tls_key must be given together or not at all; \
                 `{given}` is set but `{missing}` is missing"
            ),
        }
    }

    fn bind(&self, field: &str, bind: &Spanned<String>) -> Result<SocketAddr, Problem> {
        bind.get_ref().parse().map_err(|_| {
            self.invalid(
                field,
                bind,
                format!(
                    "expected an IP address and port such as \"127.0.0.1:7400\" or \
                     \"[::1]:7400\", got {:?}",
                    bind.get_ref()
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
