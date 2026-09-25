//! The one wire envelope every Transport adapter speaks, through `hexput-port`'s codec (AD-1).
//!
//! On the wire an envelope is a MessagePack **map** with exactly three keys, encoded with their
//! names so an SDK in any language sees a self-describing value:
//!
//! * `id` — the request's [`CorrelationId`] (a uint64). A response carries its request's id
//!   verbatim; the Daemon treats ids as opaque and never reorders or deduplicates them, so a
//!   Backend matches responses to requests by id and may receive them in any order. Ids are per
//!   direction (Story 3.1): a Backend's `Result`/`Error` always answers a Daemon `Call`, whose id
//!   the Daemon issued, and the Daemon's `Result`/`Error` always answers a Backend request, so the
//!   two id spaces never meet. `id` is nil
//!   only on an [`MessageType::Error`] response whose request id could not be read.
//! * `type` — the [`MessageType`], as its PascalCase name.
//! * `payload` — any value. An absent `payload` reads as nil.
//!
//! [`Envelope`] is generic in its payload: `hexput-port` instantiates it with an untyped
//! MessagePack value and later stories convert that to their own typed payloads. Decoding is the
//! Port's (it validates the map by hand so each failure gets its exact `protocol.*` code), which
//! is why [`Envelope`] only derives `Serialize`.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

/// A request's correlation id, chosen by the Backend and echoed verbatim on its response.
///
/// Opaque to the Daemon: no ordering, uniqueness or density is assumed or enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorrelationId(pub u64);

impl CorrelationId {
    /// The id's integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CorrelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What an envelope is: which request, or which kind of response.
///
/// Names are PascalCase on the wire, matching the PRD's `ExecutionStart`. A response is not
/// per-request-type — a Backend knows what a `Result` answers from its id. `#[non_exhaustive]`
/// because later epics add to this vocabulary (`CodeRegister`, `HealthCheck`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessageType {
    /// Request: the init handshake carrying per-backend Config and registrations (Story 2.4).
    Init,
    /// Request: run a Script (Story 2.6).
    ExecutionStart,
    /// Request: replace the Session's Config with a new one, without reconnecting (Story 3.8).
    /// The payload is `{config}`, a complete Config in exactly `Init.config`'s shape: a setting it
    /// leaves out goes back to the Daemon default. Answered `Result {}` (an empty map); every
    /// attached Connection's later executions run under the new Config.
    ConfigUpdate,
    /// Response: a request succeeded; the payload is its result.
    Result,
    /// Request, from the Daemon to the Backend: a Script called a Registered Function (Story
    /// 3.1). The payload is `{name, arguments}`, plus `execution` — the id of the request that
    /// started the execution making the call — when that execution is named; a later Registered
    /// Method call adds `receiver`.
    /// The Backend answers with `Result {value}` or `Error` under the call's id.
    Call,
    /// Request, from the Daemon to the Backend: may a Script make this call of a Registered
    /// Function granted per call rather than blanket (Story 3.3)? The payload is
    /// `{name, arguments}` (with `execution`), exactly what the `Call` would carry; the Backend's per-call handler
    /// answers `Result {value: <bool>}` or `Error` under the question's id. Only `value: true`
    /// lets the Daemon send the `Call`. Its id comes from the same per-connection counter as a
    /// `Call`'s.
    Authorize,
    /// Response: a request failed; the payload is the one wire error shape.
    Error,
}

impl MessageType {
    /// Every message type, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Init,
        Self::ExecutionStart,
        Self::ConfigUpdate,
        Self::Result,
        Self::Call,
        Self::Authorize,
        Self::Error,
    ];

    /// The type's wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "Init",
            Self::ExecutionStart => "ExecutionStart",
            Self::ConfigUpdate => "ConfigUpdate",
            Self::Result => "Result",
            Self::Call => "Call",
            Self::Authorize => "Authorize",
            Self::Error => "Error",
        }
    }
}

impl fmt::Display for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `type` string that names no [`MessageType`]. Carries the string so the error can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMessageType(pub String);

impl fmt::Display for UnknownMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown message type {:?}", self.0)
    }
}

impl core::error::Error for UnknownMessageType {}

impl FromStr for MessageType {
    type Err = UnknownMessageType;

    /// Exact, case-sensitive match on the wire spelling.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| UnknownMessageType(s.to_owned()))
    }
}

/// One message on the wire. See the module documentation for the encoded shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Envelope<P> {
    /// The request's correlation id; `None` (nil) only on an [`MessageType::Error`] response
    /// whose request id was unreadable.
    pub id: Option<CorrelationId>,
    /// What this message is.
    #[serde(rename = "type")]
    pub message_type: MessageType,
    /// The message body.
    pub payload: P,
}

impl<P> Envelope<P> {
    /// An envelope addressed by `id` — every request, and every response to a readable request.
    #[must_use]
    pub const fn new(id: CorrelationId, message_type: MessageType, payload: P) -> Self {
        Self {
            id: Some(id),
            message_type,
            payload,
        }
    }
}
