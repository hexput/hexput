//! The wire codec every Transport adapter wraps (AD-1): stream framing, envelope encoding and
//! decoding, and the one error shape a Backend ever receives.
//!
//! Sans-IO by construction — no sockets, no tasks, no async. An adapter owns the bytes; this
//! crate turns them into [`Envelope`]s and back:
//!
//! * [`FrameDecoder`] splits a byte stream into frames (a 4-byte big-endian length, then that
//!   many bytes), however the stream arrives — byte by byte or several frames per chunk.
//!   [`encode_frame`] is its inverse. A message-oriented transport (WebSocket, Epic 5) skips the
//!   length prefix and hands each message to [`decode`] directly.
//! * [`decode`] turns one frame's bytes into an [`Envelope`] or a [`ProtocolFailure`], which
//!   carries the request's correlation id whenever the bytes let it be read and becomes the
//!   error response through [`ProtocolFailure::to_response`]. [`encode`] is its inverse.
//! * [`ErrorBody`] is the single error payload on the wire, built from a language
//!   [`Diagnostic`](hexput_shared::diagnostics::Diagnostic) or a [`ProtocolError`];
//!   [`error_response`] wraps one in an `Error` envelope.
//!
//! The async `Port` trait a connection adapter implements arrives with the first adapter
//! (Story 2.3), wrapping this codec. Health/metrics (FR-11) will ride the same Port, exempted
//! from the init-handshake gate rather than given a separate listener.
//!
//! Binds: AD-1.

mod codec;
mod error;
mod frame;

pub use codec::{EncodeError, MAX_NESTING_DEPTH, ProtocolFailure, decode, encode};
pub use error::{ErrorBody, ProtocolCode, ProtocolError, WireSpan, error_response};
pub use frame::{FrameDecoder, LENGTH_PREFIX_LEN, MAX_FRAME_LEN, encode_frame};

pub use hexput_shared::wire::{CorrelationId, Envelope, MessageType};
/// The untyped MessagePack value the Port uses as its payload type. Re-exported so consumers
/// reach the same pinned `rmpv` without depending on it themselves.
pub use rmpv::{self, Value};
