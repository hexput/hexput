//! Length-prefixed stream framing: a 4-byte big-endian length, then that many bytes of one
//! MessagePack value.

use crate::codec::EncodeError;
use crate::codec::ProtocolFailure;
use crate::error::{ProtocolCode, ProtocolError};

/// Size of the length prefix, in bytes.
pub const LENGTH_PREFIX_LEN: usize = 4;

/// The largest frame body accepted or produced: 16 MiB. A length prefix above it is
/// `protocol.frame_too_large`, and the decoder that saw it is poisoned.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Prefix `body` with its length, producing one frame.
///
/// # Errors
///
/// [`EncodeError::FrameTooLarge`] when `body` is longer than [`MAX_FRAME_LEN`].
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let len = match u32::try_from(body.len()) {
        Ok(len) if body.len() <= MAX_FRAME_LEN => len,
        _ => return Err(EncodeError::FrameTooLarge { len: body.len() }),
    };
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_LEN + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}

/// Splits a byte stream into frame bodies. Push bytes as they arrive with [`push`](Self::push);
/// pull complete frames with [`next_frame`](Self::next_frame) until it returns `Ok(None)`.
///
/// Memory grows only with bytes actually pushed, never with a length a prefix merely claims:
/// a prefix is checked against [`MAX_FRAME_LEN`] the moment its four bytes are present.
///
/// Once a prefix is oversized the decoder is **poisoned**: the stream has no trustworthy frame
/// boundary any more, so every later [`next_frame`](Self::next_frame) returns the same failure
/// and pushed bytes are discarded. The adapter writes the error response and closes.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    /// Bytes at the front of `buffer` already handed out as frames.
    consumed: usize,
    poisoned: Option<usize>,
}

impl FrameDecoder {
    /// A decoder with nothing buffered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes received from the stream. Discarded once the decoder is poisoned.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.poisoned.is_some() {
            return;
        }
        if self.consumed > 0 {
            // Reclaim space from frames already returned before growing.
            self.buffer.drain(..self.consumed);
            self.consumed = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete frame body, `Ok(None)` if more bytes are needed.
    ///
    /// # Errors
    ///
    /// `protocol.frame_too_large` (with a nil id — no body was read) when a length prefix
    /// exceeds [`MAX_FRAME_LEN`], then on every later call.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, ProtocolFailure> {
        if let Some(len) = self.poisoned {
            return Err(too_large(len));
        }
        let pending = &self.buffer[self.consumed..];
        let Some((prefix, rest)) = pending.split_first_chunk::<LENGTH_PREFIX_LEN>() else {
            return Ok(None);
        };
        let claimed = u32::from_be_bytes(*prefix);
        let len = usize::try_from(claimed).unwrap_or(usize::MAX);
        if len > MAX_FRAME_LEN {
            self.poisoned = Some(len);
            self.buffer = Vec::new();
            self.consumed = 0;
            return Err(too_large(len));
        }
        let Some(body) = rest.get(..len) else {
            return Ok(None);
        };
        let body = body.to_vec();
        self.consumed += LENGTH_PREFIX_LEN + len;
        Ok(Some(body))
    }

    /// Whether an oversized frame has made this stream unreadable.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }

    /// Bytes pushed but not yet returned as a frame.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len() - self.consumed
    }
}

fn too_large(len: usize) -> ProtocolFailure {
    ProtocolFailure {
        id: None,
        error: ProtocolError::new(
            ProtocolCode::FrameTooLarge,
            format!("frame length {len} exceeds the maximum of {MAX_FRAME_LEN} bytes"),
        ),
    }
}
