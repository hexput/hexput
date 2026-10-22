//! The one length-prefixed framing every byte-stream adapter shares (Story 5.1): a reading half
//! that pushes bytes into `hexput-port`'s [`FrameDecoder`] and pulls envelopes out, and a writing
//! half that encodes an envelope and writes its frame. Generic over the stream's halves, so the
//! Unix Domain Socket (owned `into_split` halves) and TCP+TLS (`tokio::io::split` halves) behave
//! byte for byte alike — AD-1's "identical protocol behaviour" is one implementation, not two.

use std::io;

use hexput_port::{
    Envelope, FrameDecoder, Inbound, Outbound, Received, Value, decode, encode, encode_frame,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// How many bytes one read from the stream asks for.
const READ_CHUNK: usize = 8 * 1024;

/// The reading half of a framed byte stream.
#[derive(Debug)]
pub struct FramedInbound<R> {
    read: R,
    decoder: FrameDecoder,
    buffer: Box<[u8]>,
    closed: bool,
}

impl<R> FramedInbound<R> {
    /// Frames read from `read`.
    pub(crate) fn new(read: R) -> Self {
        Self {
            read,
            decoder: FrameDecoder::new(),
            buffer: vec![0; READ_CHUNK].into_boxed_slice(),
            closed: false,
        }
    }
}

impl<R: AsyncRead + Unpin + Send + 'static> Inbound for FramedInbound<R> {
    /// Cancel-safe: a frame is only taken out of the decoder when it is returned, and a read
    /// that is dropped before it completes has consumed nothing.
    async fn recv(&mut self) -> Received {
        loop {
            if self.closed {
                return Received::Closed(None);
            }
            match self.decoder.next_frame() {
                Ok(Some(frame)) => {
                    return match decode(&frame) {
                        Ok(envelope) => Received::Message(envelope),
                        Err(failure) => Received::Malformed(failure),
                    };
                }
                Ok(None) => {}
                Err(failure) => {
                    // Only an oversized frame fails here, and the stream cannot resync after it.
                    self.closed = true;
                    return Received::Malformed(failure);
                }
            }
            match self.read.read(&mut self.buffer).await {
                // A peer that closed mid-frame has left; it is owed no reply.
                Ok(0) => {
                    self.closed = true;
                    return Received::Closed(None);
                }
                Ok(read) => self.decoder.push(&self.buffer[..read]),
                // A TLS stream reports a TCP end without `close_notify` as `UnexpectedEof`. It is
                // the same peer close that a socket reports as `Ok(0)`, and it is read the same
                // way, so a Backend that stops sending is answered on every transport (AD-1).
                // A plain socket never returns `UnexpectedEof` from `read`.
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    self.closed = true;
                    return Received::Closed(None);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.closed = true;
                    return Received::Closed(Some(error));
                }
            }
        }
    }
}

/// The writing half of a framed byte stream.
#[derive(Debug)]
pub struct FramedOutbound<W> {
    write: W,
}

impl<W> FramedOutbound<W> {
    /// Frames written to `write`.
    pub(crate) const fn new(write: W) -> Self {
        Self { write }
    }
}

impl<W: AsyncWrite + Unpin + Send + 'static> Outbound for FramedOutbound<W> {
    /// # Errors
    ///
    /// Besides a failed write, `InvalidInput` when the envelope cannot be encoded as a frame
    /// (too large); nothing was written then.
    async fn send(&mut self, envelope: Envelope<Value>) -> io::Result<()> {
        let frame = encode(&envelope)
            .and_then(|body| encode_frame(&body))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.write.write_all(&frame).await?;
        // A no-op on a socket; a TLS stream may hold the last record until flushed.
        self.write.flush().await
    }
}
