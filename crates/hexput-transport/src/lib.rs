//! Adapters: uds.rs, named_pipe.rs, tcp_tls.rs, websocket.rs — one Port implementation each.
//! No transport-specific behavior crosses into the core. Only hexput-daemon depends on this
//! crate, which is what makes AD-1 a compile-time property rather than a review convention.
//!
//! Today: the Unix Domain Socket adapter ([`uds`], Unix only) and the TCP+TLS adapter
//! ([`tcp_tls`], Story 5.1), both over one private length-prefixed framing, and the WebSocket
//! adapter ([`websocket`], Story 5.2), one envelope per binary message over the same TLS. The
//! Named Pipe arrives later in Epic 5.
//!
//! Binds: AD-1, FR-9.

mod framed;
pub mod tcp_tls;
#[cfg(unix)]
pub mod uds;
pub mod websocket;
