//! The Windows Named Pipe adapter (FR-9, AD-1): the Windows-native local transport, the
//! equivalent of the Unix Domain Socket, and a [`Port`] per connected pipe instance speaking the
//! same length-prefixed frames.
//!
//! # Pipe policy
//!
//! * **Byte mode with the shared 4-byte length prefix.** One framing for every byte stream
//!   (`framed.rs`); a message-mode pipe would add a second one for no benefit.
//! * **Local only.** The pipe rejects remote (SMB) clients, which is why it needs no TLS —
//!   like the Unix Domain Socket, a peer here is already on the Daemon's machine.
//! * **The default security descriptor.** It grants full control to LocalSystem,
//!   Administrators and the creator owner, and read-only access to Everyone and the anonymous
//!   account; a Hexput client opens the pipe read/write, so only the Daemon's own account and
//!   administrators can send it a request. A read-only open still connects an instance, though:
//!   any local account can hold connections that never send anything, as it can on a loopback
//!   TCP or WebSocket listener, until a connection cap bounds them. There is no DACL option.
//! * **First instance only.** [`NamedPipeListener::bind`] creates the first instance of its
//!   name, so a name another process already serves — another Daemon, or a squatter waiting to
//!   impersonate this one — is [`BindErrorKind::InUse`] instead of a silent shared name.
//! * **No TLS and no credential on the pipe.** A Backend proves itself through the
//!   init/reconnect protocol, the same on every transport (AD-2).
//!
//! Between connecting one instance and creating the next, there is an instant when no instance
//! is waiting: a client opening the pipe then gets `ERROR_PIPE_BUSY` (231) and must retry.

use std::io;

use hexput_port::Port;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};

use crate::framed::{FramedInbound, FramedOutbound};

/// A served Named Pipe: its name and the instance waiting for the next client.
#[derive(Debug)]
pub struct NamedPipeListener {
    path: String,
    /// The instance the next client connects to. `None` only after creating the next instance
    /// failed; the next [`NamedPipeListener::accept`] retries it.
    pending: Option<NamedPipeServer>,
}

impl NamedPipeListener {
    /// Create the first instance of the pipe named `path` (`\\.\pipe\<name>`).
    ///
    /// Must be called within a Tokio runtime.
    ///
    /// # Errors
    ///
    /// A [`BindError`] naming `path`: another process already serves that name, or the OS
    /// refused to create the pipe.
    pub fn bind(path: &str) -> Result<Self, BindError> {
        let server = options(true).create(path).map_err(|error| BindError {
            path: path.to_owned(),
            kind: if error.kind() == io::ErrorKind::PermissionDenied {
                BindErrorKind::InUse
            } else {
                BindErrorKind::Io {
                    action: "create the pipe",
                    error,
                }
            },
        })?;
        Ok(Self {
            path: path.to_owned(),
            pending: Some(server),
        })
    }

    /// The pipe's name, as given to [`NamedPipeListener::bind`].
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Wait for the next client. Cancel-safe: the waiting instance stays with the listener until
    /// a client has connected to it, so dropping the future loses no client.
    ///
    /// # Errors
    ///
    /// The OS refused this one accept — creating the waiting instance or connecting it (a
    /// client that left before it was connected, say). The listener stays usable: an instance
    /// that failed to connect is replaced by a fresh one.
    pub async fn accept(&mut self) -> io::Result<NamedPipePort> {
        if self.pending.is_none() {
            self.pending = Some(options(false).create(&self.path)?);
        }
        if let Some(server) = &self.pending {
            // Cancel-safe per tokio; a client already connected before this call is a success.
            if let Err(error) = server.connect().await {
                // A failed instance stays failed (a client that left before the connect is
                // `ERROR_NO_DATA` on every later try), so it is replaced — the new one created
                // before the old one closes, so the name never lapses.
                self.pending = options(false).create(&self.path).ok();
                return Err(error);
            }
        }
        let Some(pipe) = self.pending.take() else {
            unreachable!("the waiting instance was created above");
        };
        // The next instance, created at once so the busy instant stays short. A failure here
        // does not cost the connected client: the next accept retries it.
        self.pending = options(false).create(&self.path).ok();
        Ok(NamedPipePort { pipe })
    }
}

/// The options every instance of a pipe is created with; `first` only for the first.
fn options(first: bool) -> ServerOptions {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte);
    options
}

/// Why [`NamedPipeListener::bind`] failed. Its `Display` names the pipe.
#[derive(Debug)]
#[non_exhaustive]
pub struct BindError {
    /// The configured pipe name.
    pub path: String,
    /// What went wrong.
    pub kind: BindErrorKind,
}

/// What [`BindError`] reports.
#[derive(Debug)]
#[non_exhaustive]
pub enum BindErrorKind {
    /// Another process already serves a pipe of that name.
    InUse,
    /// The OS refused `action`.
    Io {
        /// What was being attempted, for the message.
        action: &'static str,
        /// The OS's reason.
        error: io::Error,
    },
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = &self.path;
        match &self.kind {
            BindErrorKind::InUse => write!(
                f,
                "{path}: pipe already in use: another process is serving it"
            ),
            BindErrorKind::Io { action, error } => write!(f, "{path}: cannot {action}: {error}"),
        }
    }
}

impl std::error::Error for BindError {}

/// One connected Named Pipe instance.
#[derive(Debug)]
pub struct NamedPipePort {
    pipe: NamedPipeServer,
}

impl Port for NamedPipePort {
    type Inbound = NamedPipeInbound;
    type Outbound = NamedPipeOutbound;

    fn split(self) -> (NamedPipeInbound, NamedPipeOutbound) {
        let (read, write) = tokio::io::split(self.pipe);
        (FramedInbound::new(read), FramedOutbound::new(write))
    }
}

/// The reading half of a [`NamedPipePort`]: the shared length-prefixed framing. A client that
/// closed its handle reads as the end of the stream (`ERROR_BROKEN_PIPE` is `Ok(0)`), a clean
/// close.
pub type NamedPipeInbound = FramedInbound<ReadHalf<NamedPipeServer>>;

/// The writing half of a [`NamedPipePort`]. Besides a failed write, a send fails with
/// `InvalidInput` when the envelope cannot be encoded as a frame (too large); nothing was
/// written then.
pub type NamedPipeOutbound = FramedOutbound<WriteHalf<NamedPipeServer>>;
