//! The one error shape on the wire, and the `protocol.*` failures of the codec itself.

use core::fmt;

use hexput_shared::diagnostics::{Diagnostic, Severity, Span};
use hexput_shared::wire::{CorrelationId, Envelope, MessageType};
use rmpv::Value;
use serde::{Deserialize, Serialize};

/// Which way a frame was broken, or why a well-formed message was refused. Each maps to one
/// stable `protocol.*` code a Backend may match on.
///
/// `protocol` is a **wire-only** category: a protocol failure is never a language failure, so it
/// is deliberately not a [`Category`](hexput_shared::diagnostics::Category) and LANGUAGE-REFERENCE
/// §7 is untouched by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolCode {
    /// Not MessagePack, a value followed by extra bytes, an empty frame, or nesting past
    /// [`MAX_NESTING_DEPTH`](crate::MAX_NESTING_DEPTH).
    MalformedFrame,
    /// The MessagePack value ends before its own headers say it does.
    TruncatedFrame,
    /// Valid MessagePack that is not a valid envelope: not a map, a missing or mistyped `id` or
    /// `type`, an unknown or repeated field.
    InvalidEnvelope,
    /// A well-formed envelope whose `type` names no known message type.
    UnknownMessageType,
    /// A length prefix above [`MAX_FRAME_LEN`](crate::MAX_FRAME_LEN). The stream cannot be
    /// resynchronised after this, so the adapter closes the connection after responding.
    FrameTooLarge,
    /// A request that needs a Session arrived before the connection completed init (FR-1).
    InitNotCompleted,
    /// A well-formed message the Daemon never accepts from a Backend on its own initiative, such
    /// as a `Result` or `Error` answering nothing the Daemon asked.
    UnexpectedMessage,
    /// A message type the Daemon knows but does not serve yet. Temporary: its only use is
    /// `ExecutionStart` on an initialized connection, which Story 2.6 serves, removing this code.
    NotImplemented,
    /// An `Init` payload that is not a valid init: a missing, mistyped or unknown key, a
    /// malformed or duplicate registration. The message names the offending key or index.
    InvalidPayload,
    /// An `Init` on a connection already attached to a Session; that Session is untouched.
    AlreadyInitialized,
}

impl ProtocolCode {
    /// The category every protocol code belongs to.
    pub const CATEGORY: &'static str = "protocol";

    /// Every protocol code, so tests can assert coverage.
    pub const ALL: &'static [Self] = &[
        Self::MalformedFrame,
        Self::TruncatedFrame,
        Self::InvalidEnvelope,
        Self::UnknownMessageType,
        Self::FrameTooLarge,
        Self::InitNotCompleted,
        Self::UnexpectedMessage,
        Self::NotImplemented,
        Self::InvalidPayload,
        Self::AlreadyInitialized,
    ];

    /// The code's stable string form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedFrame => "protocol.malformed_frame",
            Self::TruncatedFrame => "protocol.truncated_frame",
            Self::InvalidEnvelope => "protocol.invalid_envelope",
            Self::UnknownMessageType => "protocol.unknown_message_type",
            Self::FrameTooLarge => "protocol.frame_too_large",
            Self::InitNotCompleted => "protocol.init_not_completed",
            Self::UnexpectedMessage => "protocol.unexpected_message",
            Self::NotImplemented => "protocol.not_implemented",
            Self::InvalidPayload => "protocol.invalid_payload",
            Self::AlreadyInitialized => "protocol.already_initialized",
        }
    }
}

impl fmt::Display for ProtocolCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A frame the codec could not accept: which way it was broken, and a message for a human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    /// The stable code.
    pub code: ProtocolCode,
    /// What exactly was wrong.
    pub message: String,
}

impl ProtocolError {
    /// Build a protocol error.
    #[must_use]
    pub fn new(code: ProtocolCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Whether the stream is unusable after this error, so the adapter must close the
    /// connection once the error response is written. Only an oversized frame is: its
    /// length prefix cannot be trusted to find the next frame.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        matches!(self.code, ProtocolCode::FrameTooLarge)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl core::error::Error for ProtocolError {}

/// A [`Span`] as it appears on the wire: a map of four unsigned integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WireSpan {
    /// 0-based byte offset.
    pub offset: u64,
    /// Length in bytes.
    pub len: u64,
    /// 1-based line.
    pub line: u64,
    /// 1-based column, in Unicode scalar values.
    pub column: u64,
}

impl From<Span> for WireSpan {
    fn from(span: Span) -> Self {
        // `usize` is at most 64 bits on every supported target; saturate rather than panic.
        let wide = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        Self {
            offset: wide(span.offset),
            len: wide(span.len),
            line: wide(span.line),
            column: wide(span.column),
        }
    }
}

/// The single error payload on the wire, carried by every `Error` response.
///
/// Built from a language [`Diagnostic`] (span present) or a [`ProtocolError`] (category
/// `protocol`, severity `error`, no span). Owned strings, so it round-trips: an SDK or a test can
/// decode it back with `rmpv::ext::from_value`. `span` is always present on the wire, nil when
/// absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `error` or `warning`.
    pub severity: String,
    /// A §7 category, or `protocol`.
    pub category: String,
    /// The stable code, e.g. `syntax.expected_syntax` or `protocol.truncated_frame`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Where in the Script the failure is; absent for a protocol failure.
    pub span: Option<WireSpan>,
}

impl ErrorBody {
    /// The body as a payload value, ready for an [`Envelope`].
    #[must_use]
    pub fn to_value(&self) -> Value {
        // Hand-built rather than through `rmpv::ext::to_value`, which is fallible in its
        // signature; every field here is a string, an integer or nil, so this cannot fail.
        let text = |s: &str| Value::from(s);
        let span = self.span.map_or(Value::Nil, |s| {
            Value::Map(vec![
                (text("offset"), Value::from(s.offset)),
                (text("len"), Value::from(s.len)),
                (text("line"), Value::from(s.line)),
                (text("column"), Value::from(s.column)),
            ])
        });
        Value::Map(vec![
            (text("severity"), text(&self.severity)),
            (text("category"), text(&self.category)),
            (text("code"), text(&self.code)),
            (text("message"), text(&self.message)),
            (text("span"), span),
        ])
    }
}

impl From<&Diagnostic> for ErrorBody {
    fn from(diagnostic: &Diagnostic) -> Self {
        Self {
            severity: diagnostic.severity.as_str().to_owned(),
            category: diagnostic.category.as_str().to_owned(),
            code: diagnostic.code.as_str().to_owned(),
            message: diagnostic.message.clone(),
            span: Some(diagnostic.span.into()),
        }
    }
}

impl From<&ProtocolError> for ErrorBody {
    fn from(error: &ProtocolError) -> Self {
        Self {
            severity: Severity::Error.as_str().to_owned(),
            category: ProtocolCode::CATEGORY.to_owned(),
            code: error.code.as_str().to_owned(),
            message: error.message.clone(),
            span: None,
        }
    }
}

/// An `Error` response carrying `body`, addressed to `id` — `None` only when the request's id
/// could not be read.
#[must_use]
pub fn error_response(id: Option<CorrelationId>, body: &ErrorBody) -> Envelope<Value> {
    Envelope {
        id,
        message_type: MessageType::Error,
        payload: body.to_value(),
    }
}
