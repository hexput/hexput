//! Adapters: uds.rs, named_pipe.rs, tcp_tls.rs, websocket.rs — one Port implementation each.
//! No transport-specific behavior crosses into the core. Only hexput-daemon depends on this
//! crate, which is what makes AD-1 a compile-time property rather than a review convention.
//!
//! Today: the Unix Domain Socket adapter ([`uds`], Unix only). TCP+TLS, WebSocket and the
//! Named Pipe arrive with Epic 5.
//!
//! Binds: AD-1.

#[cfg(unix)]
pub mod uds;
