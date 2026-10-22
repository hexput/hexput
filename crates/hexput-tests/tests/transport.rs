//! Story 2.3: the Unix Domain Socket adapter — the socket file's lifecycle, and the Port it
//! hands the core for each accepted connection, over a real socket in a temporary directory.
#![cfg(unix)]

use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use hexput_port::{
    CorrelationId, Envelope, Inbound, MAX_FRAME_LEN, MessageType, Outbound, Port, ProtocolCode,
    Received, Value, decode, encode, encode_frame,
};
use hexput_transport::tcp_tls::{
    HANDSHAKE_TIMEOUT, TcpTlsListener, TcpTlsPort, TlsError, TlsIdentity,
};
use hexput_transport::uds::{BindErrorKind, UdsListener, UdsPort};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // Directly under /tmp: a socket path has a ~108-byte limit, so keep it short.
        let dir = PathBuf::from("/tmp").join(format!(
            "hx-uds-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("a temporary directory");
        Self { dir }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("s.sock")
    }

    /// Everything in the sandbox directory, by name.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(&self.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn is_socket(path: &std::path::Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

fn framed(envelope: &Envelope<Value>) -> Vec<u8> {
    encode_frame(&encode(envelope).unwrap()).unwrap()
}

fn request(id: u64) -> Envelope<Value> {
    Envelope::new(
        CorrelationId(id),
        MessageType::ExecutionStart,
        Value::from(id),
    )
}

/// A connected client and the server-side Port for it.
async fn pair(listener: &UdsListener) -> (UnixStream, UdsPort) {
    let (client, server) = tokio::join!(UnixStream::connect(listener.path()), listener.accept());
    (client.unwrap(), server.unwrap())
}

// --- the socket file ---

#[tokio::test]
async fn bind_creates_the_socket_and_close_removes_it() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).expect("binds");
    assert!(is_socket(&sandbox.socket()));
    assert!(!listener.replaced_stale());
    listener.close().expect("closes");
    assert!(sandbox.entries().is_empty(), "{:?}", sandbox.entries());
}

#[tokio::test]
async fn a_stale_socket_is_replaced() {
    let sandbox = Sandbox::new();
    // A socket file nobody listens on: what an unclean shutdown leaves behind.
    drop(std::os::unix::net::UnixListener::bind(sandbox.socket()).unwrap());
    assert!(is_socket(&sandbox.socket()));

    let listener = UdsListener::bind(&sandbox.socket(), None).expect("replaces the stale socket");
    assert!(listener.replaced_stale());
    let (_client, _server) = pair(&listener).await;
}

#[tokio::test]
async fn a_live_socket_is_in_use_and_left_alone() {
    let sandbox = Sandbox::new();
    let live = std::os::unix::net::UnixListener::bind(sandbox.socket()).unwrap();
    let error = UdsListener::bind(&sandbox.socket(), None).expect_err("in use");
    assert!(matches!(error.kind, BindErrorKind::InUse), "{error:?}");
    let text = error.to_string();
    assert!(
        text.starts_with(&sandbox.socket().display().to_string()),
        "{text}"
    );
    assert!(text.contains("already in use"), "{text}");
    // The live socket still works.
    std::os::unix::net::UnixStream::connect(sandbox.socket()).expect("still listening");
    drop(live);
}

#[tokio::test]
async fn something_that_is_not_a_socket_is_never_deleted() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.socket(), "precious").unwrap();
    let error = UdsListener::bind(&sandbox.socket(), None).expect_err("not a socket");
    assert!(matches!(error.kind, BindErrorKind::NotASocket), "{error:?}");
    assert!(error.to_string().contains("is not a socket"), "{error}");
    assert_eq!(fs::read_to_string(sandbox.socket()).unwrap(), "precious");

    let directory = sandbox.dir.join("dir");
    fs::create_dir(&directory).unwrap();
    let error = UdsListener::bind(&directory, None).expect_err("a directory");
    assert!(matches!(error.kind, BindErrorKind::NotASocket), "{error:?}");
    assert!(directory.is_dir());
}

#[tokio::test]
async fn a_missing_parent_or_an_overlong_path_names_the_path() {
    let sandbox = Sandbox::new();
    let orphan = sandbox.dir.join("absent").join("s.sock");
    let error = UdsListener::bind(&orphan, None).expect_err("no parent");
    assert!(matches!(error.kind, BindErrorKind::Io { .. }), "{error:?}");
    assert!(
        error.to_string().starts_with(&orphan.display().to_string()),
        "{error}"
    );

    // With and without a mode: a mode binds at a shorter temporary name first, which must not
    // let an unreachable overlong path through.
    let long = sandbox.dir.join("x".repeat(200));
    for mode in [None, Some(0o660)] {
        let error = UdsListener::bind(&long, mode).expect_err("too long");
        assert!(
            error.to_string().starts_with(&long.display().to_string()),
            "{error}"
        );
        assert!(
            sandbox.entries().is_empty(),
            "{mode:?}: {:?}",
            sandbox.entries()
        );
    }
    let error = UdsListener::bind(&orphan, Some(0o660)).expect_err("no parent, with a mode");
    assert!(
        error.to_string().starts_with(&orphan.display().to_string()),
        "{error}"
    );
}

#[tokio::test]
async fn a_mode_leaves_a_live_socket_alone_and_nothing_behind() {
    let sandbox = Sandbox::new();
    let live = std::os::unix::net::UnixListener::bind(sandbox.socket()).unwrap();
    let error = UdsListener::bind(&sandbox.socket(), Some(0o600)).expect_err("in use");
    assert!(matches!(error.kind, BindErrorKind::InUse), "{error:?}");
    assert_eq!(
        sandbox.entries(),
        ["s.sock"],
        "no private directory is left behind"
    );
    std::os::unix::net::UnixStream::connect(sandbox.socket()).expect("the live socket survives");
    drop(live);
}

#[tokio::test]
async fn a_mode_works_with_a_stale_socket_and_a_relative_path() {
    let sandbox = Sandbox::new();
    drop(std::os::unix::net::UnixListener::bind(sandbox.socket()).unwrap());
    let listener = UdsListener::bind(&sandbox.socket(), Some(0o640)).expect("replaces it");
    assert!(listener.replaced_stale());
    let bits = fs::metadata(sandbox.socket()).unwrap().permissions().mode() & 0o777;
    assert_eq!(bits, 0o640);
    assert_eq!(sandbox.entries(), ["s.sock"]);
    listener.close().unwrap();

    // A relative path (resolved from the working directory, which a test must not change).
    let cwd = std::env::current_dir().unwrap();
    let relative = std::iter::repeat_n("..", cwd.components().count() - 1)
        .collect::<PathBuf>()
        .join(sandbox.socket().strip_prefix("/").unwrap());
    let listener = UdsListener::bind(&relative, Some(0o600)).expect("a relative path");
    let (_client, _server) = pair(&listener).await;
    assert_eq!(sandbox.entries(), ["s.sock"]);
    listener.close().unwrap();
    assert!(sandbox.entries().is_empty());
}

#[tokio::test]
async fn a_mode_is_applied_before_the_socket_appears_and_leaves_nothing_behind() {
    for mode in [0o660, 0o600, 0o777] {
        let sandbox = Sandbox::new();
        let listener = UdsListener::bind(&sandbox.socket(), Some(mode)).expect("binds");
        let bits = fs::metadata(sandbox.socket()).unwrap().permissions().mode() & 0o777;
        assert_eq!(bits, mode, "{mode:o}");
        assert_eq!(sandbox.entries(), ["s.sock"], "no temporary file is left");
        let (_client, _server) = pair(&listener).await;
        listener.close().unwrap();
        assert!(sandbox.entries().is_empty());
    }
}

#[tokio::test]
async fn close_leaves_a_socket_that_replaced_ours_alone() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).expect("binds");
    fs::remove_file(sandbox.socket()).unwrap();
    let successor = std::os::unix::net::UnixListener::bind(sandbox.socket()).unwrap();
    listener.close().expect("nothing of ours to remove");
    assert!(
        is_socket(&sandbox.socket()),
        "the successor's socket survives"
    );
    drop(successor);
}

// --- the Port ---

#[tokio::test]
async fn a_framed_message_arrives_as_an_envelope_and_a_reply_goes_back() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).unwrap();
    let (mut client, server) = pair(&listener).await;
    let (mut inbound, mut outbound) = server.split();

    // Two frames in one write, the second split across two.
    let (first, second) = (framed(&request(1)), framed(&request(2)));
    let mut bytes = first.clone();
    bytes.extend_from_slice(&second[..3]);
    client.write_all(&bytes).await.unwrap();
    match inbound.recv().await {
        Received::Message(envelope) => assert_eq!(envelope, request(1)),
        other => panic!("{other:?}"),
    }
    client.write_all(&second[3..]).await.unwrap();
    match inbound.recv().await {
        Received::Message(envelope) => assert_eq!(envelope, request(2)),
        other => panic!("{other:?}"),
    }

    let reply = Envelope::new(CorrelationId(2), MessageType::Result, Value::from("ok"));
    outbound.send(reply.clone()).await.unwrap();
    let mut prefix = [0; 4];
    client.read_exact(&mut prefix).await.unwrap();
    let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
    client.read_exact(&mut body).await.unwrap();
    assert_eq!(decode(&body).unwrap(), reply);
}

#[tokio::test]
async fn a_malformed_frame_is_reported_and_the_next_one_still_arrives() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).unwrap();
    let (mut client, server) = pair(&listener).await;
    let (mut inbound, _outbound) = server.split();

    let mut bytes = encode_frame(&[0xc1]).unwrap(); // 0xc1 is never valid MessagePack
    bytes.extend(framed(&request(5)));
    client.write_all(&bytes).await.unwrap();
    match inbound.recv().await {
        Received::Malformed(failure) => {
            assert_eq!(failure.error.code, ProtocolCode::MalformedFrame);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(5)));
}

#[tokio::test]
async fn an_oversized_prefix_is_fatal_and_the_half_then_stays_closed() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).unwrap();
    let (mut client, server) = pair(&listener).await;
    let (mut inbound, _outbound) = server.split();

    let too_long = u32::try_from(MAX_FRAME_LEN + 1).unwrap().to_be_bytes();
    client.write_all(&too_long).await.unwrap();
    client.write_all(&framed(&request(1))).await.unwrap();
    match inbound.recv().await {
        Received::Malformed(failure) => {
            assert_eq!(failure.error.code, ProtocolCode::FrameTooLarge);
            assert!(failure.error.is_fatal());
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(inbound.recv().await, Received::Closed(None)));
    assert!(matches!(inbound.recv().await, Received::Closed(None)));
}

#[tokio::test]
async fn a_peer_leaving_mid_frame_is_a_clean_close() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).unwrap();
    let (mut client, server) = pair(&listener).await;
    let (mut inbound, _outbound) = server.split();

    let frame = framed(&request(1));
    client.write_all(&frame[..frame.len() - 1]).await.unwrap();
    drop(client);
    assert!(matches!(inbound.recv().await, Received::Closed(None)));
}

#[tokio::test]
async fn an_envelope_too_large_to_frame_is_refused_without_writing() {
    let sandbox = Sandbox::new();
    let listener = UdsListener::bind(&sandbox.socket(), None).unwrap();
    let (mut client, server) = pair(&listener).await;
    let (_inbound, mut outbound) = server.split();

    let huge = Value::Binary(vec![0; MAX_FRAME_LEN]);
    let error = outbound
        .send(Envelope::new(CorrelationId(1), MessageType::Result, huge))
        .await
        .expect_err("too large");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    // The connection is still usable: a normal reply follows and is the first thing read.
    outbound.send(request(2)).await.unwrap();
    let mut prefix = [0; 4];
    client.read_exact(&mut prefix).await.unwrap();
    let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
    client.read_exact(&mut body).await.unwrap();
    assert_eq!(decode(&body).unwrap(), request(2));
}

// --- Story 5.1: the TCP+TLS adapter ---

impl Sandbox {
    /// A file with `text` in this sandbox.
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }
}

/// The TLS fixtures under `crates/hexput-tests/fixtures/tls/`, generated once with the system
/// `openssl`, as follows (the CA key, the signing request, the extension file and the serial
/// file are scratch material and are not checked in; every certificate lasts 36500 days):
///
/// ```text
/// # ca.pem: a self-signed CA, ECDSA P-256
/// openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca.key
/// openssl req -new -x509 -key ca.key -days 36500 -subj "/CN=Hexput Test CA" \
///   -addext "basicConstraints=critical,CA:true" \
///   -addext "keyUsage=critical,keyCertSign,cRLSign" -out ca.pem
/// # server.pem / server.key: a leaf signed by that CA, never a CA itself
/// openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out server.key # PKCS#8
/// openssl req -new -key server.key -subj "/CN=localhost" -out server.csr
/// printf 'basicConstraints=critical,CA:false\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n\
/// extendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature\n' > server.ext
/// openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 36500 \
///   -extfile server.ext -out server.pem
/// # other.key: an unrelated PKCS#8 P-256 key
/// openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out other.key
/// ```
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("tls")
        .join(name)
}

/// The leaf identity every adapter test serves with.
fn identity() -> TlsIdentity {
    TlsIdentity::load(&fixture("server.pem"), &fixture("server.key"))
        .unwrap_or_else(|error| panic!("the fixture identity: {error}"))
}

/// A listener on an ephemeral loopback port.
async fn listen() -> TcpTlsListener {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    TcpTlsListener::bind(addr, identity()).await.expect("binds")
}

/// A client that trusts only the fixture CA, as a Backend would.
fn trusting_client_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_file_iter(fixture("ca.pem")).unwrap() {
        roots.add(certificate.unwrap()).unwrap();
    }
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

/// A TLS client connected to `addr` that trusts the fixture CA, for the name `localhost`.
async fn tls_connect(addr: SocketAddr) -> TlsStream<TcpStream> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    TlsConnector::from(trusting_client_config())
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .expect("the handshake succeeds")
}

/// A TLS client and the server-side Port for it, both past the handshake.
async fn tls_pair(listener: &TcpTlsListener) -> (TlsStream<TcpStream>, TcpTlsPort) {
    let addr = listener.local_addr().unwrap();
    let (client, server) = tokio::join!(tls_connect(addr), async {
        listener.accept().await.unwrap().finish().await.unwrap()
    });
    (client, server)
}

/// The next frame read from `stream`, decoded.
async fn read_frame<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> Envelope<Value> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).await.unwrap();
    let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
    stream.read_exact(&mut body).await.unwrap();
    decode(&body).unwrap()
}

/// Standard base64 with padding, for building one malformed-on-purpose key below.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The loader's error, which a test expects.
fn load_error(cert: &Path, key: &Path) -> TlsError {
    match TlsIdentity::load(cert, key) {
        Ok(_) => panic!("{} and {} load", cert.display(), key.display()),
        Err(error) => error,
    }
}

/// The error names `path` first and states `problem`.
fn assert_names(error: &TlsError, path: &Path, problem: &str) {
    let text = error.to_string();
    assert!(
        text.starts_with(&path.display().to_string()),
        "names {}: {text}",
        path.display()
    );
    assert!(text.contains(problem), "states `{problem}`: {text}");
}

#[test]
fn the_handshake_timeout_is_ten_seconds() {
    assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(10));
}

#[test]
fn a_complete_identity_loads_and_a_chain_is_accepted_in_file_order() {
    TlsIdentity::load(&fixture("server.pem"), &fixture("server.key"))
        .expect("the leaf and its key");

    // The leaf followed by the CA: a chain, kept in file order.
    let sandbox = Sandbox::new();
    let chain = sandbox.dir.join("chain.pem");
    fs::write(
        &chain,
        format!(
            "{}{}",
            fs::read_to_string(fixture("server.pem")).unwrap(),
            fs::read_to_string(fixture("ca.pem")).unwrap()
        ),
    )
    .unwrap();
    TlsIdentity::load(&chain, &fixture("server.key")).expect("a chain loads");
}

#[test]
fn every_identity_failure_names_its_file_and_the_problem() {
    let sandbox = Sandbox::new();
    let (cert, key) = (fixture("server.pem"), fixture("server.key"));

    // Not found.
    let absent = sandbox.dir.join("absent.pem");
    assert_names(&load_error(&absent, &key), &absent, "not found");
    assert_names(&load_error(&cert, &absent), &absent, "not found");

    // Unreadable: a directory where a file should be, with the OS error.
    let directory = sandbox.dir.join("a-directory");
    fs::create_dir(&directory).unwrap();
    assert_names(&load_error(&directory, &key), &directory, "unreadable");
    assert_names(&load_error(&cert, &directory), &directory, "unreadable");

    // A certificate file with no certificate in it, and a key file with no key in it.
    let key_text = fs::read_to_string(&key).unwrap();
    let cert_text = fs::read_to_string(&cert).unwrap();
    let no_certificate = sandbox.file("no-certificate.pem", &key_text);
    assert_names(
        &load_error(&no_certificate, &key),
        &no_certificate,
        "contains no certificate",
    );
    let no_key = sandbox.file("no-key.pem", &cert_text);
    assert_names(
        &load_error(&cert, &no_key),
        &no_key,
        "contains no private key",
    );

    // Malformed PEM: a certificate block whose body is not base64.
    let malformed = sandbox.file(
        "malformed.pem",
        "-----BEGIN CERTIFICATE-----\n!!! not base64 !!!\n-----END CERTIFICATE-----\n",
    );
    assert_names(&load_error(&malformed, &key), &malformed, "malformed PEM");
    let malformed_key = sandbox.file(
        "malformed.key",
        "-----BEGIN PRIVATE KEY-----\n!!! not base64 !!!\n-----END PRIVATE KEY-----\n",
    );
    assert_names(
        &load_error(&cert, &malformed_key),
        &malformed_key,
        "malformed PEM",
    );

    // A key that is well formed but belongs to another certificate.
    let other = fixture("other.key");
    assert_names(
        &load_error(&cert, &other),
        &other,
        "does not match the certificate",
    );
}

#[test]
fn a_key_rustls_cannot_use_names_its_file() {
    let sandbox = Sandbox::new();
    // A well-formed PKCS#8 wrapper around an Ed448 key (OID 1.3.101.113), which the ring
    // provider cannot sign with: the PEM parses, the key does not load.
    let mut der = vec![0x30, 0x2e, 0x02, 0x01, 0x00];
    der.extend([0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x71]);
    der.extend([0x04, 0x22, 0x04, 0x20]);
    der.extend([0x11; 32]);
    let key = sandbox.file(
        "ed448.key",
        &format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64(&der)
        ),
    );
    let error = load_error(&fixture("server.pem"), &key);
    assert!(
        error.to_string().starts_with(&key.display().to_string()),
        "{error}"
    );
}

#[tokio::test]
async fn a_framed_message_crosses_tls_and_a_reply_comes_back_over_it() {
    let listener = listen().await;
    let (mut client, server) = tls_pair(&listener).await;
    let (mut inbound, mut outbound) = server.split();

    client.write_all(&framed(&request(1))).await.unwrap();
    match inbound.recv().await {
        Received::Message(envelope) => assert_eq!(envelope, request(1)),
        other => panic!("{other:?}"),
    }

    let reply = Envelope::new(CorrelationId(1), MessageType::Result, Value::from(3));
    outbound.send(reply.clone()).await.unwrap();
    assert_eq!(read_frame(&mut client).await, reply);
}

#[tokio::test]
async fn a_malformed_frame_over_tls_is_reported_and_the_next_one_still_arrives() {
    let listener = listen().await;
    let (mut client, server) = tls_pair(&listener).await;
    let (mut inbound, _outbound) = server.split();

    let mut bytes = encode_frame(&[0xc1]).unwrap(); // 0xc1 is never valid MessagePack
    bytes.extend(framed(&request(5)));
    client.write_all(&bytes).await.unwrap();
    match inbound.recv().await {
        Received::Malformed(failure) => {
            assert_eq!(failure.error.code, ProtocolCode::MalformedFrame);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(5)));
}

#[tokio::test]
async fn a_plaintext_client_is_never_answered_and_its_socket_is_closed() {
    let listener = listen().await;
    let addr = listener.local_addr().unwrap();
    let mut plain = TcpStream::connect(addr).await.unwrap();
    plain.write_all(&framed(&request(1))).await.unwrap();

    // The handshake fails; no Port is ever handed to the core.
    let handshake = listener.accept().await.unwrap();
    assert!(handshake.finish().await.is_err(), "plaintext is refused");

    // The socket is closed. Whatever the server sent back is at most a TLS alert record
    // (content type 0x15), never a Hexput frame.
    let mut bytes = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(10), plain.read_to_end(&mut bytes)).await;
    assert!(read.is_ok(), "the server closes the socket");
    assert!(bytes.is_empty() || bytes[0] == 0x15, "{bytes:02x?}");

    // Other connections are unaffected.
    let (mut client, server) = tls_pair(&listener).await;
    let (mut inbound, _outbound) = server.split();
    client.write_all(&framed(&request(2))).await.unwrap();
    assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(2)));
}

#[tokio::test]
async fn a_client_that_does_not_trust_the_certificate_gets_no_session() {
    let listener = listen().await;
    let addr = listener.local_addr().unwrap();
    let distrusting = Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth(),
    );
    let tcp = TcpStream::connect(addr).await.unwrap();
    let client =
        TlsConnector::from(distrusting).connect(ServerName::try_from("localhost").unwrap(), tcp);
    let server = async { listener.accept().await.unwrap().finish().await };
    let (client, server) = tokio::join!(client, server);
    assert!(client.is_err(), "the client rejects the certificate");
    assert!(server.is_err(), "the server gets no session");
}

#[tokio::test]
async fn a_silent_peer_times_out_with_timed_out_and_nothing_is_read() {
    let listener = listen()
        .await
        .with_handshake_timeout(Duration::from_millis(200));
    let addr = listener.local_addr().unwrap();
    // Connected, sending nothing.
    let _silent = TcpStream::connect(addr).await.unwrap();

    let handshake = listener.accept().await.unwrap();
    let started = Instant::now();
    let error = handshake
        .finish()
        .await
        .expect_err("the handshake times out");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
    assert!(started.elapsed() < Duration::from_secs(5), "bounded");
}

#[tokio::test]
async fn a_silent_peer_never_stalls_the_next_accept() {
    // `accept` returns before any handshake; the handshake belongs to the connection's task.
    let listener = listen().await;
    let addr = listener.local_addr().unwrap();
    let _silent = TcpStream::connect(addr).await.unwrap();
    let _pending = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("accepted without a handshake")
        .unwrap();

    let (client, server) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(tls_connect(addr), async {
            listener.accept().await.unwrap().finish().await.unwrap()
        })
    })
    .await
    .expect("the next peer is served while the silent one is pending");
    let (mut inbound, _outbound) = server.split();
    let mut client = client;
    client.write_all(&framed(&request(3))).await.unwrap();
    assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(3)));
}

#[tokio::test]
async fn a_tls_client_closing_with_close_notify_is_a_clean_close() {
    let listener = listen().await;
    let (mut client, server) = tls_pair(&listener).await;
    let (mut inbound, _outbound) = server.split();

    client.write_all(&framed(&request(1))).await.unwrap();
    // Sends `close_notify`, then shuts down the TCP write side.
    client.shutdown().await.unwrap();
    assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(1)));
    assert!(matches!(inbound.recv().await, Received::Closed(None)));
}

#[tokio::test]
async fn a_tls_client_ending_tcp_without_close_notify_is_a_clean_close_too() {
    // Like a Unix Domain Socket peer that stops sending: the connection still owes replies, so
    // the end is `Closed(None)`, never a lost stream — on every transport alike (AD-1).
    let listener = listen().await;
    let (mut client, server) = tls_pair(&listener).await;
    let (mut inbound, mut outbound) = server.split();

    client.write_all(&framed(&request(1))).await.unwrap();
    client.flush().await.unwrap();
    // A bare TCP FIN: the TLS layer is bypassed, so no `close_notify` is sent.
    client.get_mut().0.shutdown().await.unwrap();
    assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(1)));
    assert!(matches!(inbound.recv().await, Received::Closed(None)));
    assert!(matches!(inbound.recv().await, Received::Closed(None)));

    // The half-closed client still reads the reply it is owed.
    let reply = Envelope::new(CorrelationId(1), MessageType::Result, Value::from(3));
    outbound.send(reply.clone()).await.unwrap();
    assert_eq!(read_frame(&mut client).await, reply);
}

#[tokio::test]
async fn frames_larger_than_a_tls_record_and_back_to_back_cross_tls_whole() {
    // A TLS record holds at most 16 KiB: a frame of several records, then two more right after
    // it, each delivered without waiting for a later write (the send flushes).
    let listener = listen().await;
    let (mut client, server) = tls_pair(&listener).await;
    let (mut inbound, mut outbound) = server.split();

    let big = Envelope::new(
        CorrelationId(7),
        MessageType::Result,
        Value::from("x".repeat(100 * 1024)),
    );
    let replies = [
        big,
        Envelope::new(CorrelationId(8), MessageType::Result, Value::from(8)),
        Envelope::new(CorrelationId(9), MessageType::Result, Value::from(9)),
    ];
    // Sent and read concurrently, so a full socket buffer never deadlocks the test.
    let send = async {
        for reply in &replies {
            outbound.send(reply.clone()).await.unwrap();
        }
    };
    let read = async {
        for reply in &replies {
            let read = tokio::time::timeout(Duration::from_secs(10), read_frame(&mut client))
                .await
                .expect("each frame arrives without a later write");
            assert_eq!(&read, reply);
        }
    };
    tokio::join!(send, read);

    // And inbound alike: a large request and two after it in one write.
    let mut bytes = framed(&replies[0]);
    bytes.extend(framed(&request(10)));
    bytes.extend(framed(&request(11)));
    let write = async { client.write_all(&bytes).await.unwrap() };
    let receive = async {
        assert!(matches!(inbound.recv().await, Received::Message(e) if e == replies[0]));
        assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(10)));
        assert!(matches!(inbound.recv().await, Received::Message(e) if e == request(11)));
    };
    tokio::join!(write, receive);
}
