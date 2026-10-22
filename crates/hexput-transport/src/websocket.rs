//! The WebSocket adapter (FR-9, AD-1): a TCP listener whose every connection is a rustls server
//! session upgraded to a WebSocket, and a [`Port`] per completed upgrade.
//!
//! # TLS policy
//!
//! * **TLS on every connection, exactly as on TCP.** Every accepted connection completes a rustls
//!   server handshake through the same [`TlsIdentity`] as the TCP+TLS adapter ([`crate::tcp_tls`]),
//!   and only then the HTTP upgrade. There is no `ws://` mode and no option to disable TLS:
//!   a cleartext upgrade request fails the TLS handshake and is never answered.
//! * **The handshake belongs to the connection, not to the accept.**
//!   [`WebSocketListener::accept`] returns a [`WebSocketHandshake`] as soon as the TCP connection
//!   is accepted; the caller finishes it ([`WebSocketHandshake::finish`]) in the connection's own
//!   task. The TLS handshake and the
//!   upgrade are bounded together by [`HANDSHAKE_TIMEOUT`] (10 s), after which `finish` fails with
//!   [`io::ErrorKind::TimedOut`].
//! * Accepted sockets get `TCP_NODELAY`.
//!
//! # Upgrade policy
//!
//! Any request path is accepted. No subprotocol is negotiated, no `Origin` is checked (a Backend
//! proves itself in-band through init/reconnect, never through cookies), and no extension
//! (permessage-deflate) is offered. Incoming messages and frames are bounded by
//! [`MAX_FRAME_LEN`].
//!
//! # Message policy
//!
//! One binary message carries exactly one MessagePack envelope, with no length prefix: WebSocket
//! frames messages itself. A text message is `protocol.malformed_frame` (not fatal), a message
//! over the limit `protocol.frame_too_large` (fatal). Ping and Pong are the transport's own
//! business and never reach the core. A peer's Close frame is answered — its reply flushed —
//! before the half reports a clean close; a TCP stream that ends without a Close frame is a clean
//! close too, so the core detaches through its one exit path, as on every transport. The
//! Daemon never sends a Close frame of its own: the `Port` has no close hook.
//!
//! # Replies still owed when the peer stops
//!
//! Unlike a byte stream, a WebSocket ends both directions together, so a Backend must not stop
//! while it still expects replies:
//!
//! * After the peer's Close frame, tungstenite has answered with its own Close, and RFC 6455
//!   allows no data message after that: every later [`WebSocketOutbound::send`] fails, so a reply
//!   still owed is lost and the core abandons what was in flight.
//! * A TCP stream ended *cleanly* — a TLS `close_notify`, then the end — leaves tungstenite's
//!   connection terminated, and every later `send` fails the same way.
//! * A stream that ends without `close_notify` (an intermediary dropping it) leaves the writing
//!   half usable: a reply is written if the peer's socket still takes it.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use hexput_port::{
    Envelope, Inbound, MAX_FRAME_LEN, Outbound, Port, ProtocolCode, ProtocolError, ProtocolFailure,
    Received, Value, decode, encode,
};
use rustls::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::ProtocolError as WsProtocolError;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::tcp_tls::{HANDSHAKE_TIMEOUT, TlsIdentity};

/// The stream a [`WebSocketPort`] speaks over: a WebSocket on a server-side TLS session.
type Stream = WebSocketStream<TlsStream<TcpStream>>;

/// A listening TCP socket whose connections are served as WebSockets over TLS only.
#[derive(Debug)]
pub struct WebSocketListener {
    listener: TcpListener,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
}

impl WebSocketListener {
    /// Listen on `addr`, serving `identity`. Must be called within a Tokio runtime.
    ///
    /// # Errors
    ///
    /// The OS refused the bind: the address is in use, not local, or needs privileges.
    pub async fn bind(addr: SocketAddr, identity: TlsIdentity) -> io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(addr).await?,
            config: identity.server_config(),
            handshake_timeout: HANDSHAKE_TIMEOUT,
        })
    }

    /// Bound the TLS handshake and upgrade by `timeout` instead of [`HANDSHAKE_TIMEOUT`].
    #[must_use]
    pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// The address actually listened on — with the OS-chosen port when bound to port `0`.
    ///
    /// # Errors
    ///
    /// The OS could not report it.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Wait for the next TCP connection and return it with its TLS handshake and upgrade still
    /// to do. Never waits on the peer beyond the TCP accept. Cancel-safe: dropping the future
    /// loses no connection.
    ///
    /// # Errors
    ///
    /// The OS refused this one accept. The listener stays usable.
    pub async fn accept(&self) -> io::Result<WebSocketHandshake> {
        let (stream, peer) = self.listener.accept().await?;
        // A socket that cannot take the option is failing anyway, and its handshake will say so.
        let _ = stream.set_nodelay(true);
        Ok(WebSocketHandshake {
            stream,
            peer,
            config: Arc::clone(&self.config),
            timeout: self.handshake_timeout,
        })
    }
}

/// An accepted TCP connection whose TLS handshake and WebSocket upgrade have not run yet.
#[derive(Debug)]
pub struct WebSocketHandshake {
    stream: TcpStream,
    peer: SocketAddr,
    config: Arc<ServerConfig>,
    timeout: Duration,
}

impl WebSocketHandshake {
    /// The peer's address.
    #[must_use]
    pub const fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Run the TLS server handshake and then the WebSocket upgrade, together bounded by the
    /// listener's handshake timeout. On failure the socket is closed and nothing it sent was read
    /// as a message.
    ///
    /// # Errors
    ///
    /// The TLS handshake failed (cleartext, another protocol, a client that rejected the
    /// certificate), the upgrade failed (not an upgrade request, a reset), or the two did not
    /// complete in time ([`io::ErrorKind::TimedOut`]).
    pub async fn finish(self) -> io::Result<WebSocketPort> {
        let accepted = async {
            let tls = TlsAcceptor::from(self.config).accept(self.stream).await?;
            tokio_tungstenite::accept_async_with_config(tls, Some(ws_config()))
                .await
                .map_err(to_io)
        };
        match tokio::time::timeout(self.timeout, accepted).await {
            Ok(result) => result.map(|stream| WebSocketPort { stream }),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the TLS handshake and WebSocket upgrade did not complete within {} ms",
                    self.timeout.as_millis()
                ),
            )),
        }
    }
}

/// tungstenite's settings: a message and a frame are bounded like a length-prefixed frame.
fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_LEN))
        .max_frame_size(Some(MAX_FRAME_LEN))
}

/// A tungstenite error as an `io::Error`, keeping an I/O error's kind.
fn to_io(error: WsError) -> io::Error {
    match error {
        WsError::Io(error) => error,
        error => io::Error::other(error),
    }
}

/// One TCP connection past its TLS handshake and WebSocket upgrade.
#[derive(Debug)]
pub struct WebSocketPort {
    stream: Stream,
}

impl Port for WebSocketPort {
    type Inbound = WebSocketInbound;
    type Outbound = WebSocketOutbound;

    fn split(self) -> (WebSocketInbound, WebSocketOutbound) {
        let (sink, stream) = self.stream.split();
        (
            WebSocketInbound {
                stream,
                state: ReadState::Open,
            },
            WebSocketOutbound { sink },
        )
    }
}

/// Where a [`WebSocketInbound`] is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadState {
    /// Reading messages.
    Open,
    /// The peer sent Close; the stream is driven until tungstenite has flushed the reply.
    Closing,
    /// Reported closed; every later `recv` is `Closed(None)`.
    Closed,
}

/// The reading half of a [`WebSocketPort`].
#[derive(Debug)]
pub struct WebSocketInbound {
    stream: SplitStream<Stream>,
    state: ReadState,
}

impl Inbound for WebSocketInbound {
    /// Cancel-safe: a message is only taken from the stream when it is returned, and the
    /// closing state survives a dropped call.
    async fn recv(&mut self) -> Received {
        loop {
            if self.state == ReadState::Closed {
                return Received::Closed(None);
            }
            let next = self.stream.next().await;
            if self.state == ReadState::Closing {
                // Anything after the peer's Close only drives the close reply out; the half
                // ends once tungstenite reports the stream closed, however it ends.
                if matches!(next, None | Some(Err(_))) {
                    self.state = ReadState::Closed;
                    return Received::Closed(None);
                }
                continue;
            }
            match next {
                Some(Ok(Message::Binary(bytes))) => {
                    return match decode(&bytes) {
                        Ok(envelope) => Received::Message(envelope),
                        Err(failure) => Received::Malformed(failure),
                    };
                }
                Some(Ok(Message::Text(_))) => {
                    return Received::Malformed(ProtocolFailure {
                        id: None,
                        error: ProtocolError::new(
                            ProtocolCode::MalformedFrame,
                            "a WebSocket text message; Hexput frames are binary messages",
                        ),
                    });
                }
                // tungstenite answers a Ping itself; neither reaches the core.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_))) => self.state = ReadState::Closing,
                None => {
                    self.state = ReadState::Closed;
                    return Received::Closed(None);
                }
                Some(Err(error)) => {
                    self.state = ReadState::Closed;
                    return closed_by(error);
                }
            }
        }
    }
}

/// What a read error means for the core.
fn closed_by(error: WsError) -> Received {
    match error {
        // A message over the limit: refused before its payload was read, and fatal.
        WsError::Capacity(error) => Received::Malformed(ProtocolFailure {
            id: None,
            error: ProtocolError::new(ProtocolCode::FrameTooLarge, error.to_string()),
        }),
        // A peer (or an intermediary) that ended the TCP stream without a Close frame — and,
        // over TLS, without `close_notify` — has stopped sending, exactly like a socket peer
        // that closed. The same clean close as on every transport (AD-1).
        WsError::ConnectionClosed
        | WsError::AlreadyClosed
        | WsError::Protocol(WsProtocolError::ResetWithoutClosingHandshake) => {
            Received::Closed(None)
        }
        WsError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Received::Closed(None)
        }
        error => Received::Closed(Some(to_io(error))),
    }
}

/// The writing half of a [`WebSocketPort`]. Besides a failed write, a send fails with
/// `InvalidInput` when the envelope's encoding is longer than [`MAX_FRAME_LEN`]; nothing was
/// written then.
#[derive(Debug)]
pub struct WebSocketOutbound {
    sink: SplitSink<Stream, Message>,
}

impl Outbound for WebSocketOutbound {
    /// # Errors
    ///
    /// Besides a failed write, `InvalidInput` when the envelope cannot be encoded within
    /// [`MAX_FRAME_LEN`]; nothing was written then.
    async fn send(&mut self, envelope: Envelope<Value>) -> io::Result<()> {
        let body = encode(&envelope)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if body.len() > MAX_FRAME_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                hexput_port::EncodeError::FrameTooLarge { len: body.len() },
            ));
        }
        // `send` writes the message and flushes it.
        self.sink
            .send(Message::Binary(body.into()))
            .await
            .map_err(to_io)
    }
}
