//! The `Port`: one connection as the core sees it, whatever carries it (AD-1).
//!
//! A Transport adapter owns the socket, the bytes and the framing; the core sees envelopes going
//! in and out and nothing else. A connection splits into two halves up front so the core can
//! read and write independently — a response written while the next request is being read —
//! without a lock around the connection.
//!
//! The traits use `async fn`-in-trait style with explicit `Send` futures, so the core stays
//! generic over the adapter (static dispatch) and its tasks can run on any runtime worker. This
//! crate still performs no I/O and depends on no async runtime: it only states the contract.

use std::future::Future;
use std::io;

use hexput_shared::wire::Envelope;
use rmpv::Value;

use crate::codec::ProtocolFailure;

/// One accepted connection, before it is split into its two directions.
pub trait Port: Send + 'static {
    /// What the core reads from.
    type Inbound: Inbound;
    /// What the core writes to.
    type Outbound: Outbound;

    /// Split the connection into its reading and writing halves. Dropping both closes it.
    fn split(self) -> (Self::Inbound, Self::Outbound);
}

/// The reading half of a [`Port`].
pub trait Inbound: Send + 'static {
    /// The next thing the peer sent. After [`Received::Closed`], or after a
    /// [`Received::Malformed`] whose error [is fatal](crate::ProtocolError::is_fatal), the half
    /// yields `Closed` forever.
    ///
    /// Must be cancel-safe: dropping the future before it completes loses nothing the peer sent,
    /// so the core may race it in a `select!`.
    fn recv(&mut self) -> impl Future<Output = Received> + Send;
}

/// The writing half of a [`Port`].
pub trait Outbound: Send + 'static {
    /// Write one envelope to the peer, completely, before returning.
    ///
    /// An error of kind [`io::ErrorKind::InvalidInput`] means the envelope could not be framed
    /// (too large) and nothing was written, so the connection is still usable. Any other error
    /// means the connection is unusable.
    ///
    /// Not cancel-safe: dropping the future part-way may leave half a frame on the stream, so the
    /// core always drives a send to completion.
    fn send(&mut self, envelope: Envelope<Value>) -> impl Future<Output = io::Result<()>> + Send;
}

/// What [`Inbound::recv`] yields.
#[derive(Debug)]
pub enum Received {
    /// A well-formed envelope.
    Message(Envelope<Value>),
    /// A frame the codec rejected. The core answers it with
    /// [`ProtocolFailure::to_response`], then keeps reading unless the error is fatal.
    Malformed(ProtocolFailure),
    /// The connection ended: `None` when the peer closed it cleanly (including mid-frame, as a
    /// peer that has left is owed no reply), `Some` when reading failed.
    Closed(Option<io::Error>),
}
