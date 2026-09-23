//! One frame's bytes to an [`Envelope`] and back.
//!
//! Decoding is two-phase. First the bytes become an untyped [`Value`] through `rmp-serde`, whose
//! nesting-depth limit keeps recursion bounded and whose containers grow with the elements
//! actually present, never with a count a header claims. Then the map is validated by hand, so
//! every failure gets its exact `protocol.*` code and the correlation id is recovered whenever it
//! is readable.

use core::fmt;

use hexput_shared::wire::{CorrelationId, Envelope, MessageType};
use rmpv::Value;
use serde::Deserialize;

use crate::error::{ErrorBody, ProtocolCode, ProtocolError, error_response};

/// The deepest container nesting a frame may have, counting the envelope map itself as one
/// level: a payload may nest `MAX_NESTING_DEPTH - 1` arrays, maps or extension values deep.
/// Past it the frame is `protocol.malformed_frame` — decoding recurses once per level, and this
/// bound is what keeps that from ever overflowing the stack.
pub const MAX_NESTING_DEPTH: usize = 128;

/// A frame [`decode`] rejected: the error, and the request's id if the bytes let it be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolFailure {
    /// The request's correlation id, `None` when it could not be read.
    pub id: Option<CorrelationId>,
    /// What was wrong.
    pub error: ProtocolError,
}

impl ProtocolFailure {
    /// The `Error` response to send back for this failure.
    #[must_use]
    pub fn to_response(&self) -> Envelope<Value> {
        error_response(self.id, &ErrorBody::from(&self.error))
    }
}

impl fmt::Display for ProtocolFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.id {
            Some(id) => write!(f, "request {id}: {}", self.error),
            None => fmt::Display::fmt(&self.error, f),
        }
    }
}

impl core::error::Error for ProtocolFailure {}

/// An envelope that could not be encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncodeError {
    /// The encoded body is longer than [`MAX_FRAME_LEN`](crate::MAX_FRAME_LEN).
    FrameTooLarge {
        /// The body's length in bytes.
        len: usize,
    },
    /// The MessagePack serializer refused the value.
    Serialize(String),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { len } => write!(
                f,
                "frame length {len} exceeds the maximum of {} bytes",
                crate::MAX_FRAME_LEN
            ),
            Self::Serialize(message) => write!(f, "could not encode envelope: {message}"),
        }
    }
}

impl core::error::Error for EncodeError {}

/// Encode an envelope as one MessagePack map with named fields (`id`, `type`, `payload`) — the
/// body of a frame, without its length prefix.
///
/// # Errors
///
/// [`EncodeError::Serialize`] if the serializer refuses the value.
pub fn encode(envelope: &Envelope<Value>) -> Result<Vec<u8>, EncodeError> {
    rmp_serde::to_vec_named(envelope).map_err(|e| EncodeError::Serialize(e.to_string()))
}

/// Decode one frame body into an envelope.
///
/// # Errors
///
/// A [`ProtocolFailure`] with exactly one `protocol.*` code — see [`ProtocolCode`] — carrying
/// the request's id whenever it could be read.
pub fn decode(bytes: &[u8]) -> Result<Envelope<Value>, ProtocolFailure> {
    let value = parse_value(bytes).map_err(|error| ProtocolFailure { id: None, error })?;
    validate_envelope(value)
}

fn parse_value(bytes: &[u8]) -> Result<Value, ProtocolError> {
    if bytes.is_empty() {
        return Err(ProtocolError::new(
            ProtocolCode::MalformedFrame,
            "empty frame: expected one MessagePack value",
        ));
    }
    // Read through a shrinking slice so the unread remainder is visible afterwards.
    let mut rest = bytes;
    let value = {
        let mut de = rmp_serde::Deserializer::new(&mut rest);
        // rmp-serde fails once its counter reaches zero, so N levels need a budget of N + 1.
        de.set_max_depth(MAX_NESTING_DEPTH + 1);
        Value::deserialize(&mut de).map_err(classify)?
    };
    if !rest.is_empty() {
        return Err(ProtocolError::new(
            ProtocolCode::MalformedFrame,
            format!(
                "{} trailing byte(s) after the MessagePack value",
                rest.len()
            ),
        ));
    }
    Ok(value)
}

fn classify(error: rmp_serde::decode::Error) -> ProtocolError {
    use rmp_serde::decode::Error as E;
    match error {
        E::InvalidMarkerRead(ref io) | E::InvalidDataRead(ref io)
            if io.kind() == std::io::ErrorKind::UnexpectedEof =>
        {
            ProtocolError::new(
                ProtocolCode::TruncatedFrame,
                "the MessagePack value ends before its headers say it does",
            )
        }
        E::DepthLimitExceeded => ProtocolError::new(
            ProtocolCode::MalformedFrame,
            format!("nesting exceeds the limit of {MAX_NESTING_DEPTH} levels"),
        ),
        other => ProtocolError::new(
            ProtocolCode::MalformedFrame,
            format!("not a MessagePack value: {other}"),
        ),
    }
}

fn validate_envelope(value: Value) -> Result<Envelope<Value>, ProtocolFailure> {
    let Value::Map(pairs) = value else {
        return Err(invalid(None, "an envelope must be a map"));
    };

    let mut id_field = None;
    let mut type_field = None;
    let mut payload_field = None;
    let mut problem: Option<String> = None;
    let mut id_repeated = false;

    for (key, value) in pairs {
        let slot = match key.as_str() {
            Some("id") => &mut id_field,
            Some("type") => &mut type_field,
            Some("payload") => &mut payload_field,
            Some(other) => {
                problem.get_or_insert_with(|| format!("unknown envelope field {other:?}"));
                continue;
            }
            None => {
                problem.get_or_insert_with(|| "envelope keys must be strings".to_owned());
                continue;
            }
        };
        if slot.is_some() {
            let name = key.as_str().unwrap_or_default();
            id_repeated |= name == "id";
            problem.get_or_insert_with(|| format!("envelope field {name:?} appears twice"));
        } else {
            *slot = Some(value);
        }
    }

    // The id is echoed whenever it is readable: present once, and a uint64. A repeated id is
    // ambiguous, so it is not echoed.
    let readable_id = if id_repeated {
        None
    } else {
        id_field.as_ref().and_then(Value::as_u64).map(CorrelationId)
    };

    if let Some(problem) = problem {
        return Err(invalid(readable_id, problem));
    }

    // Structure first: an envelope with no usable id is invalid whatever its type says.
    let id_is_nil = match &id_field {
        None => return Err(invalid(None, "envelope is missing \"id\"")),
        Some(Value::Nil) => true,
        Some(_) if readable_id.is_some() => false,
        Some(_) => return Err(invalid(None, "\"id\" must be an unsigned 64-bit integer")),
    };

    // An invalid-UTF-8 MessagePack str decodes as `Value::Binary` (rmp-serde hands it to
    // `visit_bytes`), so it is rejected here as "not a string".
    let message_type = match type_field.as_ref().map(Value::as_str) {
        None => return Err(invalid(readable_id, "envelope is missing \"type\"")),
        Some(Some(name)) => name
            .parse::<MessageType>()
            .map_err(|unknown| ProtocolFailure {
                id: readable_id,
                error: ProtocolError::new(ProtocolCode::UnknownMessageType, unknown.to_string()),
            })?,
        Some(None) => return Err(invalid(readable_id, "\"type\" must be a string")),
    };

    if id_is_nil && message_type != MessageType::Error {
        return Err(invalid(None, "\"id\" may be nil only on an Error response"));
    }

    Ok(Envelope {
        id: readable_id,
        message_type,
        payload: payload_field.unwrap_or(Value::Nil),
    })
}

fn invalid(id: Option<CorrelationId>, message: impl Into<String>) -> ProtocolFailure {
    ProtocolFailure {
        id,
        error: ProtocolError::new(ProtocolCode::InvalidEnvelope, message),
    }
}
