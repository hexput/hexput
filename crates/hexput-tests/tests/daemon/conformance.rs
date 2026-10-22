//! Story 5.4: the conformance suite. FR-9 promises that the transport is a latency and topology
//! choice, never a semantic one; this file is the proof.
//!
//! # The contract
//!
//! A **case** is one row of the I/O & Edge-Case Matrix in the story spec: a function
//! `fn(&mut dyn Client, &mut Transcript)` that drives one fresh connection to a real Daemon
//! (`run_with`, end to end) and asserts every reply the row names: `message_type`, the correlation
//! id and the payload fields, plus `code` and `span` for an `Error` where the row names one. A case
//! never opens a connection; the runner hands it a fresh one, and every case except `uninitialized`
//! begins with the suite's one `Init` (`begin`).
//!
//! A **transport** is one way of reaching the Daemon. Cases see only `Client`, so none can branch on
//! a transport, and none can skip by one (`no_case_names_a_transport` pins that). Every envelope a
//! case sends or receives is recorded in its `Transcript`, normalized after the case's own
//! assertions have run on the real values:
//!
//! - a `client_id` becomes `<client-id>` (asserted to be 32 lowercase hex characters first);
//! - a `reconnect_secret` becomes `<reconnect-secret>` (asserted to be 64 lowercase hex characters
//!   first; Story 5.5 — the credential is issued identically on every transport);
//! - a `handle` becomes `<handle>` (asserted to be 16 lowercase hex characters first);
//! - a generated Reference ID (`hx:<16 hex>:…`) has its nonce replaced by `*`;
//! - nothing else is rewritten.
//!
//! After a case returns, the runner sends one sentinel request (`quiet`) and asserts its `Result` is
//! the very next message, so nothing a case never read (an extra or repeated message) goes unseen.
//!
//! `every_transport_on_this_platform_gives_the_same_transcript` asserts that the transcripts of
//! every transport the platform serves are byte-identical, so any difference the per-case
//! assertions did not name (an extra field, an ordering, an id, a span) still fails.
//!
//! Each run starts its own Daemon whose System Config holds that transport's section alone, and
//! must stop with exit `0`.
//!
//! # The one known difference
//!
//! Every case keeps the connection open in both directions. A WebSocket ends both directions
//! together, so a reply still owed when the Backend sends a Close frame (or ends TCP after a TLS
//! `close_notify`) cannot be sent, where a byte stream still writes it (Story 5.2; the `websocket`
//! adapter's module doc, its test and deferred-work record it). That is the one documented way the
//! transports differ, and it is why no case here half-closes a connection with work in flight.
//!
//! # Adding a transport
//!
//! Implement [`Transport`] for it (its name, the System Config section that serves it, and how to
//! connect a `Client`), and add one `test => transport` line to `transports!`, with the `cfg` of the
//! platforms that serve it. That line is both its `#[test]` and its place in the transcript
//! comparison. No case changes, and no case may mention a transport.
//!
//! The harness below (fixture, Daemon, TLS client, framing, WebSocket client) is shared with the
//! TLS and WebSocket serving tests in `daemon.rs`, which import it back from here.

use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(windows)]
use std::sync::Mutex;
#[cfg(windows)]
use std::time::{Duration, Instant};

use hexput_port::{CorrelationId, Envelope, MessageType, Value, decode, encode, encode_frame};

use self::harness::{Daemon, Wss, frames, tcp_section, tls, websocket_section};
use super::Sandbox;
#[cfg(windows)]
use super::named_pipe_section;
#[cfg(windows)]
use super::named_pipe_serving::{open_once, pipe_name, retryable};

/// The shared harness: the fixture, a Daemon on its own thread, the blocking TLS client, the
/// framing over any byte stream, and the blocking WebSocket client. `tls_serving` imports it.
pub(super) mod harness {
    use std::ffi::OsString;
    use std::io::{ErrorKind, Read, Write};
    use std::net::{SocketAddr, TcpStream};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use hexput_daemon::run_with;
    #[cfg(unix)]
    use hexput_port::{CorrelationId, MessageType};
    use hexput_port::{Envelope, Value, decode, encode, encode_frame};
    use rustls::pki_types::ServerName;
    use rustls::pki_types::pem::PemObject;

    use crate::{Run, Sandbox, SharedLog, nothing};

    pub fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("tls")
            .join(name)
    }

    /// The `[transport.tcp]` section for `bind`, with the fixture leaf identity.
    pub fn tcp_section(bind: &str) -> String {
        tcp_section_with(bind, &fixture("server.pem"), &fixture("server.key"))
    }

    pub fn tcp_section_with(bind: &str, cert: &Path, key: &Path) -> String {
        format!(
            "[transport.tcp]\nbind = {bind:?}\ntls_cert = {:?}\ntls_key = {:?}\n",
            cert.to_str().unwrap(),
            key.to_str().unwrap()
        )
    }

    /// The `[transport.websocket]` section for `bind`, with the fixture leaf identity (Story 5.2).
    pub fn websocket_section(bind: &str) -> String {
        websocket_section_with(bind, &fixture("server.pem"), &fixture("server.key"))
    }

    pub fn websocket_section_with(bind: &str, cert: &Path, key: &Path) -> String {
        format!(
            "[transport.websocket]\nbind = {bind:?}\ntls_cert = {:?}\ntls_key = {:?}\n",
            cert.to_str().unwrap(),
            key.to_str().unwrap()
        )
    }

    /// A Daemon running on its own thread until `stop`.
    pub struct Daemon {
        /// The socket the Daemon serves its UDS listener on, for the UDS transport to connect to.
        #[cfg(unix)]
        pub socket: PathBuf,
        stop: tokio::sync::oneshot::Sender<()>,
        thread: std::thread::JoinHandle<Run>,
        log: SharedLog,
    }

    impl Daemon {
        pub fn start(sandbox: &Sandbox, config_text: &str) -> Self {
            let config = sandbox.file("config.toml", config_text);
            let discovery = nothing(sandbox);
            let log = SharedLog::default();
            let thread_log = log.clone();
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let thread = std::thread::spawn(move || {
                let argv: Vec<OsString> =
                    vec!["hexput-daemon".into(), "--config".into(), config.into()];
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let code = run_with(
                    argv,
                    &discovery,
                    &mut out,
                    &mut err,
                    thread_log.clone(),
                    async {
                        let _ = stopped.await;
                    },
                );
                Run {
                    code: format!("{code:?}"),
                    stdout: String::from_utf8(out).unwrap(),
                    stderr: String::from_utf8(err).unwrap(),
                    log: thread_log.text(),
                }
            });
            Self {
                stop,
                thread,
                log,
                #[cfg(unix)]
                socket: sandbox.socket(),
            }
        }

        /// The log, once `needle` appears in it; panics if the Daemon stops first.
        pub fn log_with(&self, needle: &str) -> String {
            let started = Instant::now();
            loop {
                let text = self.log.text();
                if text.contains(needle) {
                    return text;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(10) && !self.thread.is_finished(),
                    "never logged `{needle}`; log:\n{text}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// The TCP listener's actual address, read from the startup log line.
        pub fn tcp_address(&self) -> SocketAddr {
            self.bound_address("TCP")
        }

        /// The actual address of the `transport` listener (`TCP`, `WebSocket`), read from its
        /// `listening on <transport> with TLS` startup line.
        pub fn bound_address(&self, transport: &str) -> SocketAddr {
            let needle = format!("listening on {transport} with TLS");
            let text = self.log_with(&needle);
            let line = text
                .lines()
                .find(|line| line.contains(&needle))
                .expect("the line");
            let field = line
                .split("address=")
                .nth(1)
                .expect("an address field")
                .split_whitespace()
                .next()
                .expect("an address value");
            field
                .trim_matches('"')
                .parse()
                .unwrap_or_else(|error| panic!("{field}: {error}"))
        }

        #[cfg(unix)]
        /// A UDS client, waiting until the socket accepts.
        pub fn uds(&self, socket: &Path) -> UnixStream {
            let started = Instant::now();
            loop {
                match UnixStream::connect(socket) {
                    Ok(stream) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(10)))
                            .unwrap();
                        return stream;
                    }
                    Err(error) => {
                        assert!(
                            started.elapsed() < Duration::from_secs(10)
                                && !self.thread.is_finished(),
                            "the socket never accepted: {error}"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }

        pub fn stop(self) -> Run {
            let _ = self.stop.send(());
            self.thread.join().expect("the Daemon thread")
        }
    }

    /// A blocking TLS client that trusts only the fixture CA, for the name `localhost`.
    pub fn tls(addr: SocketAddr) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in rustls::pki_types::CertificateDer::pem_file_iter(fixture("ca.pem"))
            .expect("the CA file")
        {
            roots.add(certificate.unwrap()).unwrap();
        }
        let config = Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        );
        let tcp = TcpStream::connect(addr).expect("connects");
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        // The handshake happens on the first read or write.
        let connection =
            rustls::ClientConnection::new(config, ServerName::try_from("localhost").unwrap())
                .expect("a client connection");
        rustls::StreamOwned::new(connection, tcp)
    }

    /// Framing for any byte stream, UDS or TLS alike.
    pub mod frames {
        use super::*;

        #[cfg(unix)]
        pub fn init(id: u64) -> Envelope<Value> {
            Envelope::new(
                CorrelationId(id),
                MessageType::Init,
                Value::Map(vec![
                    (Value::from("config"), Value::Map(vec![])),
                    (Value::from("registrations"), Value::Array(vec![])),
                ]),
            )
        }

        #[cfg(unix)]
        pub fn request(id: u64, source: &str) -> Envelope<Value> {
            Envelope::new(
                CorrelationId(id),
                MessageType::ExecutionStart,
                Value::Map(vec![
                    (Value::from("source"), Value::from(source)),
                    (Value::from("variables"), Value::Map(vec![])),
                ]),
            )
        }

        pub fn send<S: Write>(stream: &mut S, envelope: &Envelope<Value>) {
            let bytes = encode_frame(&encode(envelope).unwrap()).unwrap();
            stream.write_all(&bytes).unwrap();
        }

        /// The next reply, or `None` once the peer has closed.
        pub fn reply<S: Read>(stream: &mut S) -> Option<Envelope<Value>> {
            let mut prefix = [0; 4];
            match stream.read_exact(&mut prefix) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
                    ) =>
                {
                    return None;
                }
                Err(error) => panic!("no reply: {error}"),
            }
            let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
            stream.read_exact(&mut body).unwrap();
            Some(decode(&body).unwrap())
        }
    }

    // --- Story 5.2: WebSocket ---

    /// A blocking WebSocket client: a current-thread runtime drives tokio-tungstenite over
    /// tokio-rustls, trusting only the fixture CA, for the name `localhost`.
    pub struct Wss {
        runtime: tokio::runtime::Runtime,
        socket: tokio_tungstenite::WebSocketStream<
            tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
        >,
    }

    impl Wss {
        pub fn connect(addr: SocketAddr) -> Self {
            let mut roots = rustls::RootCertStore::empty();
            for certificate in rustls::pki_types::CertificateDer::pem_file_iter(fixture("ca.pem"))
                .expect("the CA file")
            {
                roots.add(certificate.unwrap()).unwrap();
            }
            let config = Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            let socket = runtime.block_on(async {
                let tcp = tokio::net::TcpStream::connect(addr)
                    .await
                    .expect("connects");
                let tls = tokio_rustls::TlsConnector::from(config)
                    .connect(ServerName::try_from("localhost").unwrap(), tcp)
                    .await
                    .expect("the TLS handshake succeeds");
                let (socket, _response) = tokio_tungstenite::client_async("wss://localhost/", tls)
                    .await
                    .expect("the WebSocket upgrade succeeds");
                socket
            });
            Self { runtime, socket }
        }

        /// Send one envelope as one binary message.
        pub fn send(&mut self, envelope: &Envelope<Value>) {
            self.send_bytes(&encode(envelope).unwrap());
        }

        /// Send `bytes` as one binary message, whatever they are (a malformed body included).
        pub fn send_bytes(&mut self, bytes: &[u8]) {
            use futures_util::SinkExt;
            let socket = &mut self.socket;
            self.runtime
                .block_on(socket.send(tokio_tungstenite::tungstenite::Message::Binary(
                    bytes.to_vec().into(),
                )))
                .expect("the message is sent");
        }

        /// The next reply, decoded, or `None` once the Daemon has closed.
        pub fn reply(&mut self) -> Option<Envelope<Value>> {
            use futures_util::StreamExt;
            use tokio_tungstenite::tungstenite::Message;
            let socket = &mut self.socket;
            self.runtime.block_on(async {
                loop {
                    match tokio::time::timeout(Duration::from_secs(10), socket.next())
                        .await
                        .expect("a reply within 10 s")
                    {
                        None | Some(Err(_)) => return None,
                        Some(Ok(Message::Binary(bytes))) => return Some(decode(&bytes).unwrap()),
                        Some(Ok(Message::Close(_))) => return None,
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                        Some(Ok(other)) => panic!("expected a binary message, got {other:?}"),
                    }
                }
            })
        }

        #[cfg(unix)]
        /// Send `text` as one WebSocket text message, which Hexput never accepts as a frame.
        pub fn send_text(&mut self, text: &str) {
            use futures_util::SinkExt;
            let socket = &mut self.socket;
            self.runtime
                .block_on(socket.send(tokio_tungstenite::tungstenite::Message::Text(text.into())))
                .expect("the message is sent");
        }

        #[cfg(unix)]
        /// Drop the TCP stream with no Close frame and no `close_notify` (an intermediary
        /// dropping the connection).
        pub fn vanish(self) {
            let Self { runtime, socket } = self;
            drop(socket);
            drop(runtime);
        }
    }
}

// --- The client every transport supplies ---------------------------------------------------------

/// One connection to the Daemon, as a case sees it: envelopes in and out, in the transport's own
/// framing. A case never learns which transport it is talking through.
trait Client {
    /// Sends `envelope`, framed as the transport frames it.
    fn send(&mut self, envelope: &Envelope<Value>);

    /// Sends one message body in the transport's own framing: a length-prefixed frame on a byte
    /// stream, one binary message on WebSocket.
    fn send_body(&mut self, body: &[u8]);

    /// The next envelope from the Daemon, or `None` once it has closed the connection. Each client
    /// bounds this wait at 10 s where its type allows it; a blocking Named Pipe handle has no read
    /// timeout, which is accepted.
    fn reply(&mut self) -> Option<Envelope<Value>>;
}

// --- Transports ---------------------------------------------------------------------------------
//
// The only code in this file that names a transport. Each one serves the suite through its own
// Daemon, started with its System Config section alone.

/// One way of reaching the Daemon.
trait Transport {
    /// Names the transport in a failure message.
    fn name(&self) -> &'static str;

    /// The System Config section that serves this transport, and nothing else.
    fn section(&self, sandbox: &Sandbox) -> String;

    /// A fresh connection to the running Daemon, ready for a case.
    fn connect(&self, daemon: &Daemon) -> Box<dyn Client>;
}

/// Framing over any byte stream: the UDS socket, a TLS stream, a Named Pipe.
struct Framed<S>(S);

impl<S: Read + Write> Client for Framed<S> {
    fn send(&mut self, envelope: &Envelope<Value>) {
        frames::send(&mut self.0, envelope);
    }

    fn send_body(&mut self, body: &[u8]) {
        self.0
            .write_all(&encode_frame(body).expect("a frame"))
            .expect("the body is sent");
    }

    fn reply(&mut self) -> Option<Envelope<Value>> {
        frames::reply(&mut self.0)
    }
}

impl Client for Wss {
    fn send(&mut self, envelope: &Envelope<Value>) {
        Wss::send(self, envelope);
    }

    fn send_body(&mut self, body: &[u8]) {
        self.send_bytes(body);
    }

    fn reply(&mut self) -> Option<Envelope<Value>> {
        Wss::reply(self)
    }
}

#[cfg(unix)]
struct Uds;

#[cfg(unix)]
impl Transport for Uds {
    fn name(&self) -> &'static str {
        "uds"
    }

    fn section(&self, sandbox: &Sandbox) -> String {
        sandbox.valid()
    }

    fn connect(&self, daemon: &Daemon) -> Box<dyn Client> {
        Box::new(Framed(daemon.uds(&daemon.socket)))
    }
}

struct TcpTls;

impl Transport for TcpTls {
    fn name(&self) -> &'static str {
        "tcp_tls"
    }

    fn section(&self, _sandbox: &Sandbox) -> String {
        tcp_section("127.0.0.1:0")
    }

    fn connect(&self, daemon: &Daemon) -> Box<dyn Client> {
        Box::new(Framed(tls(daemon.tcp_address())))
    }
}

struct WebSocket;

impl Transport for WebSocket {
    fn name(&self) -> &'static str {
        "websocket"
    }

    fn section(&self, _sandbox: &Sandbox) -> String {
        websocket_section("127.0.0.1:0")
    }

    fn connect(&self, daemon: &Daemon) -> Box<dyn Client> {
        Box::new(Wss::connect(daemon.bound_address("WebSocket")))
    }
}

/// The Named Pipe gets a fresh name per run: `section` names it, and `connect` opens it.
#[cfg(windows)]
#[derive(Default)]
struct NamedPipe {
    pipe: Mutex<Option<String>>,
}

#[cfg(windows)]
impl Transport for NamedPipe {
    fn name(&self) -> &'static str {
        "named_pipe"
    }

    fn section(&self, _sandbox: &Sandbox) -> String {
        let pipe = pipe_name();
        *self.pipe.lock().unwrap() = Some(pipe.clone());
        named_pipe_section(&pipe)
    }

    fn connect(&self, _daemon: &Daemon) -> Box<dyn Client> {
        let pipe = self
            .pipe
            .lock()
            .unwrap()
            .clone()
            .expect("the section names the pipe");
        let started = Instant::now();
        loop {
            match open_once(&pipe) {
                Ok(file) => return Box::new(Framed(file)),
                Err(error) if retryable(&error) && started.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("the Daemon never listened on {pipe}: {error}"),
            }
        }
    }
}

/// The one place a transport is listed: each `test => transport` entry (with the `cfg` of the
/// platforms that serve it) becomes the `#[test]` that runs the suite over it and its entry in
/// `served`, the transports `every_transport_on_this_platform_gives_the_same_transcript` compares.
macro_rules! transports {
    ($($(#[$platform:meta])* $test:ident => $transport:expr;)*) => {
        $(
            $(#[$platform])*
            #[test]
            fn $test() {
                run(&$transport);
            }
        )*

        /// Every transport this platform serves.
        // One `push` per entry, each under its entry's `cfg`: a `vec![]` element cannot carry one.
        #[allow(clippy::vec_init_then_push)]
        fn served() -> Vec<Box<dyn Transport>> {
            let mut served: Vec<Box<dyn Transport>> = Vec::new();
            $(
                $(#[$platform])*
                served.push(Box::new($transport));
            )*
            served
        }
    };
}

transports! {
    #[cfg(unix)]
    uds => Uds;
    #[cfg(windows)]
    named_pipe => NamedPipe::default();
    tcp_tls => TcpTls;
    websocket => WebSocket;
}

// --- Transcripts --------------------------------------------------------------------------------

/// Which way a recorded message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Sent,
    Received,
}

/// One recorded message: the case it belongs to, its direction, and its bytes after
/// normalization (an envelope's `encode`, or a raw body exactly as sent).
#[derive(Debug, PartialEq, Eq)]
struct Recorded {
    case: &'static str,
    direction: Direction,
    bytes: Vec<u8>,
}

/// The transcript of one case: every envelope and body it sends or receives, in order.
struct Transcript {
    case: &'static str,
    entries: Vec<Recorded>,
}

impl Transcript {
    /// Sends `envelope`, recording it.
    fn send(&mut self, client: &mut dyn Client, envelope: &Envelope<Value>) {
        self.record(Direction::Sent, envelope);
        client.send(envelope);
    }

    /// Sends a raw message body, recording it as it is.
    fn send_body(&mut self, client: &mut dyn Client, body: &[u8]) {
        self.entries.push(Recorded {
            case: self.case,
            direction: Direction::Sent,
            bytes: body.to_vec(),
        });
        client.send_body(body);
    }

    /// The next reply, recorded. The envelope returned is the Daemon's, unnormalized, so the case
    /// asserts on the real values.
    fn recv(&mut self, client: &mut dyn Client) -> Envelope<Value> {
        let envelope = client
            .reply()
            .expect("a reply: the Daemon closed the connection");
        self.record(Direction::Received, &envelope);
        envelope
    }

    fn record(&mut self, direction: Direction, envelope: &Envelope<Value>) {
        let normal = Envelope {
            id: envelope.id,
            message_type: envelope.message_type,
            payload: normalize(&envelope.payload),
        };
        self.entries.push(Recorded {
            case: self.case,
            direction,
            bytes: encode(&normal).expect("a recorded envelope encodes"),
        });
    }
}

/// The transcript form of a value: a `client_id`, a `reconnect_secret` and a `handle` are checked
/// for shape and replaced by placeholders, and a generated Reference ID's nonce becomes `*`.
/// Nothing else changes.
fn normalize(value: &Value) -> Value {
    match value {
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(key, item)| {
                    let item = match (key.as_str(), item.as_str()) {
                        (Some("client_id"), Some(text)) => {
                            assert_lower_hex(text, 32, "client_id");
                            Value::from("<client-id>")
                        }
                        (Some("reconnect_secret"), Some(text)) => {
                            assert_lower_hex(text, 64, "reconnect_secret");
                            Value::from("<reconnect-secret>")
                        }
                        (Some("handle"), Some(text)) => {
                            assert_lower_hex(text, 16, "handle");
                            Value::from("<handle>")
                        }
                        _ => normalize(item),
                    };
                    (key.clone(), item)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => match other.as_str() {
            Some(text) => Value::from(normalize_reference(text)),
            None => other.clone(),
        },
    }
}

/// `text` with the nonce of a generated Reference ID (`hx:<16 hex>:…`) replaced by `*`.
fn normalize_reference(text: &str) -> String {
    let bytes = text.as_bytes();
    let generated = bytes.len() >= 20
        && text.starts_with("hx:")
        && bytes[3..19]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && bytes[19] == b':';
    if generated {
        format!("hx:*:{}", &text[20..])
    } else {
        text.to_owned()
    }
}

fn assert_lower_hex(text: &str, length: usize, what: &str) {
    assert!(
        text.len() == length
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "`{what}` is {length} lowercase hex characters: {text:?}"
    );
}

// --- Running the suite --------------------------------------------------------------------------

/// One case: it drives a fresh connection and asserts every reply it names.
type Case = fn(&mut dyn Client, &mut Transcript);

/// Runs every case over `transport` in matrix order, each on its own fresh connection, against one
/// Daemon whose System Config holds only that transport. The Daemon must stop with exit `0`.
fn run(transport: &dyn Transport) -> Vec<Recorded> {
    let sandbox = Sandbox::new();
    let daemon = Daemon::start(&sandbox, &transport.section(&sandbox));
    let mut recorded = Vec::new();
    for &(name, case) in CASES {
        let mut client = transport.connect(&daemon);
        let mut transcript = Transcript {
            case: name,
            entries: Vec::new(),
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            case(client.as_mut(), &mut transcript);
            quiet(client.as_mut(), &mut transcript);
        }));
        if let Err(payload) = outcome {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                })
                .unwrap_or_default();
            panic!("case `{name}` failed over {}:\n{message}", transport.name());
        }
        drop(client);
        recorded.extend(transcript.entries);
    }
    daemon.stop().exit(0);
    recorded
}

/// The id of the request that closes every case. No case uses it.
const SENTINEL: u64 = 1_000;

/// After a case's last expected reply, the connection holds nothing more: a sentinel request's
/// `Result` is the very next message, so an extra or repeated message a case never read (a second
/// `Error`, a stray `Call`) fails here, on every transport, instead of passing unseen. Every case
/// ends initialized, so the sentinel always runs.
fn quiet(client: &mut dyn Client, t: &mut Transcript) {
    t.send(client, &run_request(SENTINEL, "return 0;"));
    assert_eq!(
        expect_result(client, t, SENTINEL),
        map(vec![("value", Value::from(0u64))]),
        "nothing follows the case's last reply"
    );
}

/// Renders one recorded message for a divergence message.
fn show(entry: Option<&Recorded>) -> String {
    match entry {
        None => "(nothing: the transcript ends here)".to_owned(),
        Some(entry) => format!("{:?} {:?}", entry.direction, decode(&entry.bytes)),
    }
}

// --- The conformance tests ----------------------------------------------------------------------

// The per-transport tests (`uds`, `named_pipe`, `tcp_tls`, `websocket`) come from `transports!`.

#[test]
fn every_transport_on_this_platform_gives_the_same_transcript() {
    let runs: Vec<(&str, Vec<Recorded>)> = served()
        .iter()
        .map(|transport| (transport.name(), run(transport.as_ref())))
        .collect();
    let (first_name, first) = &runs[0];
    for (name, other) in &runs[1..] {
        if other == first {
            continue;
        }
        let index = first
            .iter()
            .zip(other.iter())
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| first.len().min(other.len()));
        let case = first
            .get(index)
            .or(other.get(index))
            .map_or("(none)", |entry| entry.case);
        panic!(
            "the transports `{first_name}` and `{name}` diverge at case `{case}`, entry {index}:\n  {first_name}: {}\n  {name}: {}",
            show(first.get(index)),
            show(other.get(index)),
        );
    }
}

/// AC4: no case names a transport. The case section below (between its markers) must not mention
/// one, so a case can never branch on, or skip by, transport.
#[test]
fn no_case_names_a_transport() {
    let source = include_str!("conformance.rs");
    // Built from pieces, so this test's own text never contains the markers it looks for.
    let start = source
        .find(concat!("// BEGIN", " CASES"))
        .expect("the case section starts");
    let end = source
        .find(concat!("// END", " CASES"))
        .expect("the case section ends");
    assert!(start < end, "the case section's markers are in order");
    let cases = source[start..end].to_ascii_lowercase();
    for needle in ["const cases", "fn init(", "fn unexpected_reply("] {
        assert!(
            cases.contains(needle),
            "the scanned section holds `{needle}`: the markers enclose every case"
        );
    }
    for word in [
        "uds",
        "unix",
        "socket",
        "tcp",
        "tls",
        "websocket",
        "wss",
        "pipe",
        "rustls",
        "tungstenite",
    ] {
        assert!(
            !cases.contains(word),
            "a case mentions `{word}`: cases are transport-neutral"
        );
    }
}

/// AD-1: `scripts/check-crate-graph.py`'s transport-name guard fails naming the crate, the
/// file:line and AD-1 when a core crate names a transport, in source or in its manifest, and spares
/// the exempt crates. It runs the real script over a throwaway workspace (no dependencies, so
/// `cargo metadata` needs no registry), where the other graph assertions also fail; only the
/// guard's lines are checked.
#[cfg(unix)]
#[test]
fn the_transport_name_guard_names_the_crate_and_line() {
    let sandbox = Sandbox::new();
    let root = &sandbox.dir;
    let package = |name: &str, manifest_extra: &str, source: &str| {
        let dir = root.join("crates").join(name);
        std::fs::create_dir_all(dir.join("src")).expect("a crate directory");
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{manifest_extra}"
            ),
        )
        .expect("a manifest");
        std::fs::write(dir.join("src").join("lib.rs"), source).expect("a source file");
    };
    // A real dependency line (a path one, so `cargo metadata` needs no registry), a comment, and
    // the identifier spellings a transport branch would use.
    package(
        "hexput-session",
        "[dependencies]\nrustls = { package = \"hexput-transport\", path = \"../hexput-transport\" }\n",
        "//! Sessions.\n// A WebSocket peer.\npub struct TcpTlsPort;\npub fn uds_path() {}\n",
    );
    package("hexput-transport", "", "//! The WebSocket adapter.\n");
    package("hexput-config", "", "//! [transport.tcp] sections.\n");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/hexput-session\", \"crates/hexput-transport\", \"crates/hexput-config\"]\n",
    )
    .expect("the workspace manifest");
    let mut lock = String::from("version = 4\n");
    for name in ["hexput-config", "hexput-session", "hexput-transport"] {
        lock.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\n"
        ));
    }
    std::fs::write(root.join("Cargo.lock"), lock).expect("the lock file");

    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-crate-graph.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .current_dir(root)
        .output()
        .expect("python3 runs the crate-graph check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let guard: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("names a transport"))
        .collect();
    let expected = [
        ("rustls", "crates/hexput-session/Cargo.toml:6"),
        ("WebSocket", "crates/hexput-session/src/lib.rs:2"),
        ("Tcp", "crates/hexput-session/src/lib.rs:3"),
        ("Tls", "crates/hexput-session/src/lib.rs:3"),
        ("uds", "crates/hexput-session/src/lib.rs:4"),
    ];
    assert_eq!(guard.len(), expected.len(), "{stderr}");
    for (line, (name, place)) in guard.iter().zip(expected) {
        assert!(
            line.contains(&format!(
                "AD-1: `hexput-session` names a transport (`{name}`) at {place};"
            )),
            "expected `{name}` at {place}:\n{stderr}"
        );
    }
}

// BEGIN CASES --------------------------------------------------------------------------------------
//
// The I/O & Edge-Case Matrix of Story 5.4, one case per row, in matrix order. Every case except
// `uninitialized` starts with the suite's `Init`, sent as id 1 by `begin`.

/// The matrix, in order. Each entry is one fresh connection.
const CASES: &[(&str, Case)] = &[
    ("init", init),
    ("uninitialized", uninitialized),
    ("init_twice", init_twice),
    ("direct", direct),
    ("script_error", script_error),
    ("host_call", host_call),
    ("cached", cached),
    ("config_update", config_update),
    ("capability_unregistered", capability_unregistered),
    ("capability_denied", capability_denied),
    ("budget", budget_case),
    ("invalid_payload", invalid_payload),
    ("malformed", malformed),
    ("unknown_type", unknown_type),
    ("unexpected_reply", unexpected_reply),
];

/// The `Init` the suite sends: no Config, `double` blanket-granted, `ask` not.
fn init_envelope(id: u64) -> Envelope<Value> {
    Envelope::new(
        CorrelationId(id),
        MessageType::Init,
        map(vec![
            ("config", map(vec![])),
            (
                "registrations",
                Value::Array(vec![
                    map(vec![
                        ("name", Value::from("double")),
                        ("blanket", Value::Boolean(true)),
                    ]),
                    map(vec![("name", Value::from("ask"))]),
                ]),
            ),
        ]),
    )
}

/// An `ExecutionStart` for `source`, with the given starting variables and optional overrides.
fn execution(
    id: u64,
    source: &str,
    variables: Vec<(&str, Value)>,
    overrides: Option<Value>,
) -> Envelope<Value> {
    let mut fields = vec![
        ("source", Value::from(source)),
        ("variables", map(variables)),
    ];
    if let Some(overrides) = overrides {
        fields.push(("overrides", overrides));
    }
    Envelope::new(CorrelationId(id), MessageType::ExecutionStart, map(fields))
}

/// `execution` with no variables and no overrides.
fn run_request(id: u64, source: &str) -> Envelope<Value> {
    execution(id, source, vec![], None)
}

/// A Value Secret holder's value; any other value as it is. A scalar argument may arrive as a
/// holder with a generated Reference ID (Story 3.11).
fn unheld(value: &Value) -> Value {
    match (field(value, "__secret"), field(value, "value")) {
        (Some(_), Some(inner)) if value.as_map().is_some_and(|entries| entries.len() == 2) => {
            inner.clone()
        }
        _ => value.clone(),
    }
}

/// A `budget` overrides map with one counted dimension set to `limit`.
fn budget(dimension: &str, limit: u64) -> Value {
    map(vec![("budget", map(vec![(dimension, Value::from(limit))]))])
}

/// `Init` as `id`, answered with `Result {client_id, reconnect_secret}`: the Client ID a string of
/// 32 lowercase hex characters and the reconnect secret one of 64 (Story 5.5); returns the reply's
/// payload.
fn init_result(client: &mut dyn Client, t: &mut Transcript, id: u64) -> Value {
    t.send(client, &init_envelope(id));
    let payload = expect_result(client, t, id);
    assert_lower_hex(text_of(&payload, "client_id"), 32, "client_id");
    assert_lower_hex(
        text_of(&payload, "reconnect_secret"),
        64,
        "reconnect_secret",
    );
    payload
}

/// The suite's `Init` as id 1.
fn begin(client: &mut dyn Client, t: &mut Transcript) -> Value {
    init_result(client, t, 1)
}

/// The next reply, which must be a `Result` to `id`; returns its payload.
fn expect_result(client: &mut dyn Client, t: &mut Transcript, id: u64) -> Value {
    let reply = t.recv(client);
    assert_eq!(
        reply.message_type,
        MessageType::Result,
        "a Result to {id}: {:?}",
        reply.payload
    );
    assert_eq!(reply.id, Some(CorrelationId(id)), "{:?}", reply.payload);
    reply.payload
}

/// The next reply, which must be an `Error` to `id` (`None` for a nil id); returns it.
fn error_of(client: &mut dyn Client, t: &mut Transcript, id: Option<u64>) -> Envelope<Value> {
    let reply = t.recv(client);
    assert_eq!(
        reply.message_type,
        MessageType::Error,
        "an Error to {id:?}: {:?}",
        reply.payload
    );
    assert_eq!(reply.id, id.map(CorrelationId), "{:?}", reply.payload);
    assert_error_shape(&reply);
    reply
}

/// The next reply, which must be `Error` `code` to `id`; returns it.
fn expect_error(
    client: &mut dyn Client,
    t: &mut Transcript,
    id: u64,
    code: &str,
) -> Envelope<Value> {
    let reply = error_of(client, t, Some(id));
    assert_eq!(code_of(&reply), code, "{:?}", reply.payload);
    reply
}

/// Every `Error` carries the severity, category, code and message of the wire form, with the
/// category being the code's prefix.
fn assert_error_shape(reply: &Envelope<Value>) {
    let code = code_of(reply);
    assert_eq!(text_of(&reply.payload, "severity"), "error", "{code}");
    assert_eq!(
        text_of(&reply.payload, "category"),
        code.split('.').next().unwrap_or_default(),
        "{code}"
    );
    assert!(!text_of(&reply.payload, "message").is_empty(), "{code}");
}

fn code_of(reply: &Envelope<Value>) -> String {
    text_of(&reply.payload, "code").to_owned()
}

fn message_of(reply: &Envelope<Value>) -> String {
    text_of(&reply.payload, "message").to_owned()
}

/// The span of an `Error`, checked to be exactly `offset`, `len`, `line` and `column`, with `line`
/// and `column` (1-based) agreeing with `offset`; the text it covers in `source`.
fn span_text<'a>(source: &'a str, reply: &Envelope<Value>) -> &'a str {
    let span = field(&reply.payload, "span").expect("a span");
    assert_keys(span, &["offset", "len", "line", "column"]);
    let offset = field(span, "offset")
        .and_then(Value::as_u64)
        .expect("an offset") as usize;
    let len = field(span, "len").and_then(Value::as_u64).expect("a len") as usize;
    assert!(
        offset + len <= source.len(),
        "the span {offset}+{len} lies outside the source"
    );
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count()
        + 1;
    assert_eq!(
        (
            field(span, "line").and_then(Value::as_u64),
            field(span, "column").and_then(Value::as_u64),
        ),
        (Some(line as u64), Some(column as u64)),
        "the span's line and column agree with its offset {offset}"
    );
    &source[offset..offset + len]
}

/// A request the Daemon sends the Backend: `Call` or `Authorize`.
struct Request {
    id: CorrelationId,
    name: String,
    arguments: Vec<Value>,
    execution: u64,
}

/// The next message, which must be `message_type` with exactly `name`, `arguments` and `execution`.
fn expect_request(
    client: &mut dyn Client,
    t: &mut Transcript,
    message_type: MessageType,
) -> Request {
    let request = t.recv(client);
    assert_eq!(request.message_type, message_type, "{:?}", request.payload);
    assert_keys(&request.payload, &["name", "arguments", "execution"]);
    let Some(Value::Array(arguments)) = field(&request.payload, "arguments") else {
        panic!("`arguments` is an array: {:?}", request.payload);
    };
    Request {
        id: request.id.expect("a Daemon request has an id"),
        name: text_of(&request.payload, "name").to_owned(),
        arguments: arguments.iter().map(unheld).collect(),
        execution: field(&request.payload, "execution")
            .and_then(Value::as_u64)
            .expect("`execution` is a request id"),
    }
}

/// Answers a Backend request under its own id with `Result {value}`.
fn answer(client: &mut dyn Client, t: &mut Transcript, id: CorrelationId, value: Value) {
    t.send(
        client,
        &Envelope {
            id: Some(id),
            message_type: MessageType::Result,
            payload: map(vec![("value", value)]),
        },
    );
}

fn init(client: &mut dyn Client, t: &mut Transcript) {
    let payload = begin(client, t);
    assert_keys(&payload, &["client_id", "reconnect_secret"]);
}

fn uninitialized(client: &mut dyn Client, t: &mut Transcript) {
    t.send(client, &run_request(1, "return 1;"));
    expect_error(client, t, 1, "protocol.init_not_completed");
    let payload = init_result(client, t, 2);
    assert_keys(&payload, &["client_id", "reconnect_secret"]);
}

fn init_twice(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(client, &init_envelope(2));
    expect_error(client, t, 2, "protocol.already_initialized");
}

fn direct(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(
        client,
        &execution(2, "return a + 2;", vec![("a", Value::from(1u64))], None),
    );
    assert_eq!(
        expect_result(client, t, 2),
        map(vec![("value", Value::from(3u64))])
    );
}

fn script_error(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    let source = "return missing;";
    t.send(client, &run_request(2, source));
    let reply = expect_error(client, t, 2, "reference.undeclared_identifier");
    assert_eq!(span_text(source, &reply), "missing");

    let source = "return (;";
    t.send(client, &run_request(3, source));
    let reply = error_of(client, t, Some(3));
    let code = code_of(&reply);
    assert!(code.starts_with("syntax."), "the parser's code: {code}");
    assert_eq!(span_text(source, &reply), ";");
}

fn host_call(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(client, &run_request(2, "return double(21);"));
    let call = expect_request(client, t, MessageType::Call);
    assert_eq!(call.name, "double");
    assert_eq!(call.execution, 2);
    assert_eq!(call.arguments, vec![Value::from(21u64)]);
    answer(client, t, call.id, Value::from(42u64));
    assert_eq!(
        expect_result(client, t, 2),
        map(vec![("value", Value::from(42u64))])
    );
}

fn cached(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(
        client,
        &Envelope::new(
            CorrelationId(2),
            MessageType::CodeRegister,
            map(vec![
                ("source", Value::from("return n * 2;")),
                ("variables", Value::Array(vec![Value::from("n")])),
            ]),
        ),
    );
    let payload = expect_result(client, t, 2);
    assert_keys(&payload, &["handle"]);
    let handle = text_of(&payload, "handle").to_owned();
    assert_eq!(handle.len(), 16, "a 16-character handle");

    let cached_start = |id: u64| {
        Envelope::new(
            CorrelationId(id),
            MessageType::CachedExecutionStart,
            map(vec![
                ("handle", Value::from(handle.as_str())),
                ("variables", map(vec![("n", Value::from(21u64))])),
            ]),
        )
    };
    t.send(client, &cached_start(3));
    assert_eq!(
        expect_result(client, t, 3),
        map(vec![("value", Value::from(42u64))])
    );

    t.send(
        client,
        &Envelope::new(
            CorrelationId(4),
            MessageType::CodeUnregister,
            map(vec![("handle", Value::from(handle.as_str()))]),
        ),
    );
    assert_eq!(expect_result(client, t, 4), map(vec![]));

    t.send(client, &cached_start(5));
    expect_error(client, t, 5, "protocol.unknown_script_handle");
}

fn config_update(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(
        client,
        &Envelope::new(
            CorrelationId(2),
            MessageType::ConfigUpdate,
            map(vec![("config", budget("rpc_calls", 0))]),
        ),
    );
    assert_eq!(expect_result(client, t, 2), map(vec![]));

    // The refusal is the next message: a `Call` written first would arrive before it.
    t.send(client, &run_request(3, "return double(1);"));
    expect_error(client, t, 3, "budget.rpc_calls_exceeded");
}

fn capability_unregistered(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    let source = "return nope(1);";
    t.send(client, &run_request(2, source));
    let reply = expect_error(client, t, 2, "capability.unknown_function");
    assert_eq!(span_text(source, &reply), "nope(1)");
}

fn capability_denied(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(client, &run_request(2, "return ask(1);"));
    let question = expect_request(client, t, MessageType::Authorize);
    assert_eq!(question.name, "ask");
    assert_eq!(question.execution, 2);
    assert_eq!(question.arguments, vec![Value::from(1u64)]);
    answer(client, t, question.id, Value::Boolean(false));
    expect_error(client, t, 2, "capability.unknown_function");
}

fn budget_case(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(
        client,
        &execution(2, "return double(1);", vec![], Some(budget("rpc_calls", 0))),
    );
    expect_error(client, t, 2, "budget.rpc_calls_exceeded");

    t.send(
        client,
        &execution(3, "return [1, 2];", vec![], Some(budget("allocations", 0))),
    );
    expect_error(client, t, 3, "budget.allocations_exceeded");
}

fn invalid_payload(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(
        client,
        &execution(2, "return 1;", vec![], Some(budget("rpc_calls", 200_000))),
    );
    let reply = expect_error(client, t, 2, "protocol.invalid_payload");
    assert!(
        message_of(&reply).contains("`overrides.budget.rpc_calls`"),
        "{}",
        message_of(&reply)
    );

    t.send(
        client,
        &Envelope::new(
            CorrelationId(3),
            MessageType::ExecutionStart,
            map(vec![("source", Value::from(1u64))]),
        ),
    );
    let reply = expect_error(client, t, 3, "protocol.invalid_payload");
    let message = message_of(&reply);
    assert!(
        message.contains("source") || message.contains("variables"),
        "the message names the path: {message}"
    );
}

fn malformed(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send_body(client, b"\xc1");
    let reply = t.recv(client);
    assert_eq!(
        reply.message_type,
        MessageType::Error,
        "{:?}",
        reply.payload
    );
    assert_eq!(reply.id, None, "a nil id: the request was unreadable");
    assert_eq!(code_of(&reply), "protocol.malformed_frame");

    t.send(client, &run_request(3, "return 1;"));
    assert_eq!(
        expect_result(client, t, 3),
        map(vec![("value", Value::from(1u64))])
    );
}

fn unknown_type(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    let body = map(vec![
        ("id", Value::from(4u64)),
        ("type", Value::from("Bogus")),
        ("payload", map(vec![])),
    ]);
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &body).expect("the body encodes");
    t.send_body(client, &bytes);
    expect_error(client, t, 4, "protocol.unknown_message_type");
}

fn unexpected_reply(client: &mut dyn Client, t: &mut Transcript) {
    begin(client, t);
    t.send(
        client,
        &Envelope::new(
            CorrelationId(99),
            MessageType::Result,
            map(vec![("value", Value::from(1u64))]),
        ),
    );
    // The matrix row names id 99, but a stray reply is refused with a nil id: its id is in the
    // Daemon's call-id space, never echoed (Story 3.1, pinned by `tests/connection.rs`). The
    // refusal's code is what this row checks.
    let reply = error_of(client, t, None);
    assert_eq!(code_of(&reply), "protocol.unexpected_message");
}

// --- Payload helpers -----------------------------------------------------------------------------

/// A map value from `(key, value)` pairs.
fn map(entries: Vec<(&str, Value)>) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(key, value)| (Value::from(key), value))
            .collect(),
    )
}

/// The value under `key` in a map, if any.
fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a Value> {
    payload
        .as_map()?
        .iter()
        .find(|(name, _)| name.as_str() == Some(key))
        .map(|(_, value)| value)
}

/// The string under `key`; panics if it is missing or not a string.
fn text_of<'a>(payload: &'a Value, key: &str) -> &'a str {
    field(payload, key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a string `{key}` in {payload:?}"))
}

/// The keys of a map, in order, are exactly `expected`.
fn assert_keys(payload: &Value, expected: &[&str]) {
    let keys: Vec<&str> = payload
        .as_map()
        .unwrap_or_else(|| panic!("a map: {payload:?}"))
        .iter()
        .filter_map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(keys, expected, "in {payload:?}");
}

// END CASES ----------------------------------------------------------------------------------------
