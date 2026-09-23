//! Story 2.3: the Unix Domain Socket adapter — the socket file's lifecycle, and the Port it
//! hands the core for each accepted connection, over a real socket in a temporary directory.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use hexput_port::{
    CorrelationId, Envelope, Inbound, MAX_FRAME_LEN, MessageType, Outbound, Port, ProtocolCode,
    Received, Value, decode, encode, encode_frame,
};
use hexput_transport::uds::{BindErrorKind, UdsListener, UdsPort};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

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
