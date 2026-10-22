//! The TCP+TLS adapter (FR-9, AD-1): a TCP listener whose every connection is a rustls server
//! session, and a [`Port`] per completed handshake speaking the same length-prefixed frames as
//! the Unix Domain Socket.
//!
//! # TLS policy
//!
//! * **TLS on every connection, without exception.** There is no plaintext mode and no option
//!   to disable TLS: a connection becomes a [`TcpTlsPort`] only through a completed handshake,
//!   so not one byte of a frame is read before it. Plaintext, a wrong protocol, a client that
//!   does not trust the certificate — any failed handshake ends that socket alone.
//! * **The handshake belongs to the connection, not to the accept.** [`TcpTlsListener::accept`]
//!   returns a [`TlsHandshake`] as soon as the TCP connection is accepted; the caller finishes
//!   it ([`TlsHandshake::finish`]) inside the connection's own task, so a slow or silent peer
//!   never stalls the next accept. The handshake is bounded by [`HANDSHAKE_TIMEOUT`] (10 s),
//!   after which it fails with [`io::ErrorKind::TimedOut`].
//! * **rustls with the `ring` provider**, its safe default protocol versions (TLS 1.2 and 1.3).
//!   No client authentication — a Backend proves itself through the init/reconnect protocol,
//!   which is the same on every transport (AD-2) — and no ALPN.
//! * **The identity is loaded once, at startup** ([`TlsIdentity::load`]): a PEM certificate
//!   chain, leaf first, and the leaf's private key. Restarting the Daemon is the reload.
//! * Accepted sockets get `TCP_NODELAY`: a frame is a whole request or reply, never worth
//!   holding back for coalescing.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hexput_port::Port;
use rustls::ServerConfig;
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::framed::{FramedInbound, FramedOutbound};

/// How long a peer has to complete the TLS handshake after its TCP connection is accepted.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A loaded certificate chain and private key, ready to serve TLS.
#[derive(Debug, Clone)]
pub struct TlsIdentity {
    config: Arc<ServerConfig>,
}

impl TlsIdentity {
    /// Read the PEM certificate chain at `cert` (leaf first, kept in file order) and the PEM
    /// private key at `key` (PKCS#8, PKCS#1 or SEC1), and build the TLS server configuration.
    ///
    /// # Errors
    ///
    /// A [`TlsError`] naming the offending file: not found, unreadable, malformed PEM, no
    /// certificate, no private key, a key rustls cannot use, or a key that does not match the
    /// leaf certificate.
    pub fn load(cert: &Path, key: &Path) -> Result<Self, TlsError> {
        let cert_fail = |kind| TlsError::new(cert, kind);
        let key_fail = |kind| TlsError::new(key, kind);

        let chain = CertificateDer::pem_slice_iter(&read(cert)?)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| cert_fail(malformed(error)))?;
        if chain.is_empty() {
            return Err(cert_fail(TlsErrorKind::NoCertificate));
        }
        let private_key = match PrivateKeyDer::from_pem_slice(&read(key)?) {
            Ok(private_key) => private_key,
            Err(pem::Error::NoItemsFound) => return Err(key_fail(TlsErrorKind::NoPrivateKey)),
            Err(error) => return Err(key_fail(malformed(error))),
        };

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        // Loaded once on its own first, so a key the provider cannot use is told apart from a
        // certificate it cannot use when the configuration below is built.
        provider
            .key_provider
            .load_private_key(private_key.clone_key())
            .map_err(|error| key_fail(TlsErrorKind::UnusableKey(error)))?;
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| cert_fail(TlsErrorKind::UnusableCertificate(error)))?
            .with_no_client_auth()
            // Checks that the key belongs to the leaf certificate.
            .with_single_cert(chain, private_key)
            .map_err(|error| match error {
                rustls::Error::InconsistentKeys(_) => key_fail(TlsErrorKind::KeyMismatch),
                error => cert_fail(TlsErrorKind::UnusableCertificate(error)),
            })?;
        Ok(Self {
            config: Arc::new(config),
        })
    }

    /// The TLS server configuration, shared with the WebSocket adapter (Story 5.2), which serves
    /// the very same TLS before its upgrade.
    pub(crate) fn server_config(self) -> Arc<ServerConfig> {
        self.config
    }
}

/// The whole file at `path`, or why it could not be read.
fn read(path: &Path) -> Result<Vec<u8>, TlsError> {
    std::fs::read(path).map_err(|error| {
        TlsError::new(
            path,
            if error.kind() == io::ErrorKind::NotFound {
                TlsErrorKind::NotFound
            } else {
                TlsErrorKind::Unreadable(error)
            },
        )
    })
}

fn malformed(error: pem::Error) -> TlsErrorKind {
    TlsErrorKind::MalformedPem(error.to_string())
}

/// Why [`TlsIdentity::load`] failed. Its `Display` starts with the offending file's path.
#[derive(Debug)]
#[non_exhaustive]
pub struct TlsError {
    /// The certificate or key file at fault.
    pub path: PathBuf,
    /// What is wrong with it.
    pub kind: TlsErrorKind,
}

impl TlsError {
    fn new(path: &Path, kind: TlsErrorKind) -> Self {
        Self {
            path: path.to_path_buf(),
            kind,
        }
    }
}

/// What [`TlsError`] reports.
#[derive(Debug)]
#[non_exhaustive]
pub enum TlsErrorKind {
    /// No file is at the path.
    NotFound,
    /// The file exists but could not be read (a directory, no permission).
    Unreadable(io::Error),
    /// The file is not well-formed PEM.
    MalformedPem(String),
    /// The certificate file holds no `CERTIFICATE` section.
    NoCertificate,
    /// The key file holds no private key section.
    NoPrivateKey,
    /// The private key does not belong to the leaf certificate.
    KeyMismatch,
    /// rustls cannot use the private key (an unsupported algorithm or a malformed key).
    UnusableKey(rustls::Error),
    /// rustls cannot use the certificate.
    UnusableCertificate(rustls::Error),
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        match &self.kind {
            TlsErrorKind::NotFound => write!(f, "{path}: not found"),
            TlsErrorKind::Unreadable(error) => write!(f, "{path}: unreadable: {error}"),
            TlsErrorKind::MalformedPem(detail) => write!(f, "{path}: malformed PEM: {detail}"),
            TlsErrorKind::NoCertificate => write!(f, "{path}: contains no certificate"),
            TlsErrorKind::NoPrivateKey => write!(
                f,
                "{path}: contains no private key (PKCS#8, PKCS#1 or SEC1 PEM expected)"
            ),
            TlsErrorKind::KeyMismatch => {
                write!(f, "{path}: the private key does not match the certificate")
            }
            TlsErrorKind::UnusableKey(error) => {
                write!(f, "{path}: a private key rustls cannot use: {error}")
            }
            TlsErrorKind::UnusableCertificate(error) => {
                write!(f, "{path}: a certificate rustls cannot use: {error}")
            }
        }
    }
}

impl std::error::Error for TlsError {}

/// A listening TCP socket whose connections are served over TLS only.
#[derive(Debug)]
pub struct TcpTlsListener {
    listener: TcpListener,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
}

impl TcpTlsListener {
    /// Listen on `addr`, serving `identity`. Must be called within a Tokio runtime.
    ///
    /// # Errors
    ///
    /// The OS refused the bind: the address is in use, not local, or needs privileges.
    pub async fn bind(addr: SocketAddr, identity: TlsIdentity) -> io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(addr).await?,
            config: identity.config,
            handshake_timeout: HANDSHAKE_TIMEOUT,
        })
    }

    /// Bound handshakes by `timeout` instead of [`HANDSHAKE_TIMEOUT`].
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

    /// Wait for the next TCP connection and return it with its handshake still to do. Never
    /// waits on the peer beyond the TCP accept. Cancel-safe: dropping the future loses no
    /// connection.
    ///
    /// # Errors
    ///
    /// The OS refused this one accept — out of file descriptors, most often. The listener stays
    /// usable.
    pub async fn accept(&self) -> io::Result<TlsHandshake> {
        let (stream, peer) = self.listener.accept().await?;
        // A socket that cannot take the option is failing anyway, and its handshake will say so.
        let _ = stream.set_nodelay(true);
        Ok(TlsHandshake {
            stream,
            peer,
            config: Arc::clone(&self.config),
            timeout: self.handshake_timeout,
        })
    }
}

/// An accepted TCP connection whose TLS handshake has not run yet.
#[derive(Debug)]
pub struct TlsHandshake {
    stream: TcpStream,
    peer: SocketAddr,
    config: Arc<ServerConfig>,
    timeout: Duration,
}

impl TlsHandshake {
    /// The peer's address.
    #[must_use]
    pub const fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Run the server handshake, bounded by the listener's handshake timeout. On failure the
    /// socket is closed and nothing it sent was read as a frame.
    ///
    /// # Errors
    ///
    /// The handshake failed (plaintext, another protocol, a client that rejected the
    /// certificate, a reset), or did not complete in time ([`io::ErrorKind::TimedOut`]).
    pub async fn finish(self) -> io::Result<TcpTlsPort> {
        match tokio::time::timeout(
            self.timeout,
            TlsAcceptor::from(self.config).accept(self.stream),
        )
        .await
        {
            Ok(Ok(stream)) => Ok(TcpTlsPort { stream }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the TLS handshake did not complete within {} ms",
                    self.timeout.as_millis()
                ),
            )),
        }
    }
}

/// One TCP connection past its TLS handshake.
#[derive(Debug)]
pub struct TcpTlsPort {
    stream: TlsStream<TcpStream>,
}

impl Port for TcpTlsPort {
    type Inbound = TcpTlsInbound;
    type Outbound = TcpTlsOutbound;

    fn split(self) -> (TcpTlsInbound, TcpTlsOutbound) {
        let (read, write) = tokio::io::split(self.stream);
        (FramedInbound::new(read), FramedOutbound::new(write))
    }
}

/// The reading half of a [`TcpTlsPort`]: the same framing as the Unix Domain Socket's.
pub type TcpTlsInbound = FramedInbound<ReadHalf<TlsStream<TcpStream>>>;

/// The writing half of a [`TcpTlsPort`]. Besides a failed write, a send fails with
/// `InvalidInput` when the envelope cannot be encoded as a frame (too large); nothing was
/// written then.
pub type TcpTlsOutbound = FramedOutbound<WriteHalf<TlsStream<TcpStream>>>;
