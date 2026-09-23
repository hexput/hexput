//! The Unix Domain Socket adapter: a listener that owns its socket file, and a [`Port`] per
//! accepted connection speaking `hexput-port`'s length-prefixed frames.
//!
//! # The socket file's lifecycle
//!
//! * [`UdsListener::bind`] creates the socket at the configured path. A socket file already
//!   there that nothing listens on — left by an unclean shutdown — is removed and replaced, and
//!   [`UdsListener::replaced_stale`] reports it. Anything else at the path is never deleted: a
//!   socket something is listening on is [`BindErrorKind::InUse`], and a regular file,
//!   directory or symlink is [`BindErrorKind::NotASocket`].
//! * With a `mode`, the socket is bound inside a private (`0700`) temporary directory beside
//!   the path, given its permission bits, then linked onto the path. No other user can reach it
//!   before the mode is in force, and the link fails instead of replacing anything that appeared
//!   at the path in the meantime.
//! * [`UdsListener::close`] stops listening and removes the file — only if the file at the
//!   path is still the one this listener created, so a later daemon's socket is never removed.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hexput_port::{
    Envelope, FrameDecoder, Inbound, Outbound, Port, Received, Value, decode, encode, encode_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

/// How many bytes one read from the socket asks for.
const READ_CHUNK: usize = 8 * 1024;

/// A listening Unix Domain Socket and the file it owns.
#[derive(Debug)]
pub struct UdsListener {
    listener: UnixListener,
    path: PathBuf,
    /// The socket file's `(device, inode)`, so [`UdsListener::close`] removes only its own file.
    identity: (u64, u64),
    replaced_stale: bool,
}

impl UdsListener {
    /// Create the socket at `path` and listen on it; with `mode`, the socket file has exactly
    /// those permission bits before any peer can connect to it.
    ///
    /// Must be called within a Tokio runtime. Blocks briefly: probing an existing socket file
    /// is a synchronous local connect.
    ///
    /// # Errors
    ///
    /// A [`BindError`] naming `path`: something is listening there, something that is not a
    /// socket is there, or the OS refused (missing parent directory, path too long,
    /// permissions).
    pub fn bind(path: &Path, mode: Option<u32>) -> Result<Self, BindError> {
        let fail = |kind| BindError {
            path: path.to_path_buf(),
            kind,
        };
        // Checked up front: with a `mode` the socket is bound at a shorter temporary name and
        // linked onto `path`, which no `sun_path` limit applies to — an overlong path would then
        // bind "successfully" to a name no peer can connect to.
        std::os::unix::net::SocketAddr::from_pathname(path)
            .map_err(|error| fail(io_kind("use this path for a socket", error)))?;
        let replaced_stale = clear_stale(path).map_err(fail)?;
        let listener = match mode {
            None => UnixListener::bind(path).map_err(|error| fail(io_kind("bind", error)))?,
            Some(mode) => bind_with_mode(path, mode).map_err(fail)?,
        };
        let meta = fs::symlink_metadata(path).map_err(|error| fail(io_kind("inspect", error)))?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            identity: (meta.dev(), meta.ino()),
            replaced_stale,
        })
    }

    /// The socket file's path, as given to [`UdsListener::bind`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether binding removed a stale socket file left by an unclean shutdown.
    #[must_use]
    pub const fn replaced_stale(&self) -> bool {
        self.replaced_stale
    }

    /// Wait for the next connection. Cancel-safe: dropping the future loses no connection.
    ///
    /// # Errors
    ///
    /// The OS refused this one accept — out of file descriptors, most often. The listener stays
    /// usable.
    pub async fn accept(&self) -> io::Result<UdsPort> {
        let (stream, _) = self.listener.accept().await?;
        Ok(UdsPort { stream })
    }

    /// Stop listening and remove the socket file, if it is still the one this listener
    /// created. A file that is already gone or was replaced is left alone and is not an error.
    ///
    /// # Errors
    ///
    /// The file is ours but could not be removed.
    pub fn close(self) -> io::Result<()> {
        drop(self.listener);
        match fs::symlink_metadata(&self.path) {
            Ok(meta) if (meta.dev(), meta.ino()) == self.identity => fs::remove_file(&self.path),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Make room at `path`: nothing there, or a socket nobody listens on (removed). `Ok(true)` when a
/// stale socket was removed.
fn clear_stale(path: &Path) -> Result<bool, BindErrorKind> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_kind("inspect", error)),
    };
    if !meta.file_type().is_socket() {
        return Err(BindErrorKind::NotASocket);
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(BindErrorKind::InUse),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            fs::remove_file(path).map_err(|error| io_kind("remove the stale socket", error))?;
            Ok(true)
        }
        Err(error) => Err(io_kind("probe the existing socket", error)),
    }
}

/// Distinguishes temporary names when one process binds more than one socket (tests do).
static TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Bind inside a private (`0700`) temporary directory beside `path`, set the mode, then link the
/// socket onto `path`. Nobody but the Daemon's own user can reach the socket before the mode is
/// set — the directory cannot be traversed — and linking, unlike renaming, fails rather than
/// replacing anything that appeared at `path` since it was checked.
fn bind_with_mode(path: &Path, mode: u32) -> Result<UnixListener, BindErrorKind> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let private = parent.join(format!(
        ".hexput-{}-{}",
        std::process::id(),
        TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&private)
        .map_err(|error| io_kind("create a private directory to bind in", error))?;
    let temporary = private.join("s");
    let placed = UnixListener::bind(&temporary)
        .map_err(|error| io_kind("bind the socket", error))
        .and_then(|listener| {
            fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
                .map_err(|error| io_kind("set the socket's mode", error))?;
            fs::hard_link(&temporary, path).map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => BindErrorKind::InUse,
                _ => io_kind("move the socket into place", error),
            })?;
            Ok(listener)
        });
    let _ = fs::remove_file(&temporary);
    let _ = fs::remove_dir(&private);
    placed
}

fn io_kind(action: &'static str, error: io::Error) -> BindErrorKind {
    BindErrorKind::Io { action, error }
}

/// Why [`UdsListener::bind`] failed. Its `Display` names the path.
#[derive(Debug)]
#[non_exhaustive]
pub struct BindError {
    /// The configured socket path.
    pub path: PathBuf,
    /// What went wrong.
    pub kind: BindErrorKind,
}

/// What [`BindError`] reports.
#[derive(Debug)]
#[non_exhaustive]
pub enum BindErrorKind {
    /// A process is listening on the socket at the path.
    InUse,
    /// Something that is not a socket is at the path.
    NotASocket,
    /// The OS refused `action`.
    Io {
        /// What was being attempted, for the message: "bind", "inspect", ...
        action: &'static str,
        /// The OS's reason.
        error: io::Error,
    },
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = self.path.display();
        match &self.kind {
            BindErrorKind::InUse => write!(
                f,
                "{path}: socket already in use: another process is listening on it"
            ),
            BindErrorKind::NotASocket => write!(
                f,
                "{path}: exists and is not a socket; refusing to replace it"
            ),
            BindErrorKind::Io { action, error } => write!(f, "{path}: cannot {action}: {error}"),
        }
    }
}

impl std::error::Error for BindError {}

/// One accepted Unix Domain Socket connection.
#[derive(Debug)]
pub struct UdsPort {
    stream: UnixStream,
}

impl Port for UdsPort {
    type Inbound = UdsInbound;
    type Outbound = UdsOutbound;

    fn split(self) -> (UdsInbound, UdsOutbound) {
        let (read, write) = self.stream.into_split();
        (
            UdsInbound {
                read,
                decoder: FrameDecoder::new(),
                buffer: vec![0; READ_CHUNK].into_boxed_slice(),
                closed: false,
            },
            UdsOutbound { write },
        )
    }
}

/// The reading half of a [`UdsPort`].
#[derive(Debug)]
pub struct UdsInbound {
    read: OwnedReadHalf,
    decoder: FrameDecoder,
    buffer: Box<[u8]>,
    closed: bool,
}

impl Inbound for UdsInbound {
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
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.closed = true;
                    return Received::Closed(Some(error));
                }
            }
        }
    }
}

/// The writing half of a [`UdsPort`].
#[derive(Debug)]
pub struct UdsOutbound {
    write: OwnedWriteHalf,
}

impl Outbound for UdsOutbound {
    /// # Errors
    ///
    /// Besides a failed write, `InvalidInput` when the envelope cannot be encoded as a frame
    /// (too large); nothing was written then.
    async fn send(&mut self, envelope: Envelope<Value>) -> io::Result<()> {
        let frame = encode(&envelope)
            .and_then(|body| encode_frame(&body))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        self.write.write_all(&frame).await
    }
}
