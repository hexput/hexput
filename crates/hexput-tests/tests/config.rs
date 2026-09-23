//! Story 2.1: System Config — AD-7 discovery precedence, parsing, and errors that name the file
//! and the problem.
//!
//! The environment variable and the default path are parameters of `resolve`, so nothing here
//! touches the process environment.

use std::ffi::OsStr;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hexput_config::{
    ConfigError, ConfigSource, DEFAULT_SESSION_TTL, ENV_VAR, Location, LogLevel, Problem, parse,
    resolve,
};

/// A temporary directory, removed when the test ends.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hexput-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        Self { dir }
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, text).expect("a temporary file");
        path
    }

    fn missing(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const MINIMAL: &str = "[transport.uds]\npath = \"/run/hexput.sock\"\n";

/// A config whose `log_level` marks which file it is, so precedence tests can tell them apart.
fn marked(level: &str) -> String {
    format!("log_level = \"{level}\"\n{MINIMAL}")
}

fn parse_err(text: &str) -> Problem {
    parse(text).expect_err("the text is rejected")
}

/// Resolve `text` through a real file and return the rendered error.
fn rendered_error(text: &str) -> (PathBuf, String) {
    let sandbox = Sandbox::new();
    let path = sandbox.file("config.toml", text);
    let error = resolve(Some(&path), None, Path::new("/unused")).expect_err("the file is rejected");
    assert_eq!(error.path, path);
    (path, error.to_string())
}

// --- a complete file ---

#[test]
fn a_file_with_every_field_parses_into_every_field() {
    let config = parse(
        r#"
log_level = "debug"
session_ttl_secs = 42

[transport.uds]
path = "/run/hexput/hexput.sock"
mode = "0660"

[transport.tcp]
bind = "0.0.0.0:7400"
tls_cert = "/tls/tcp-cert.pem"
tls_key = "/tls/tcp-key.pem"

[transport.websocket]
bind = "[::1]:7401"
tls_cert = "/tls/ws-cert.pem"
tls_key = "/tls/ws-key.pem"
"#,
    )
    .expect("a valid file");

    assert_eq!(config.log_level, LogLevel::Debug);
    assert_eq!(config.session_ttl, Duration::from_secs(42));
    let uds = config.transports.uds.expect("a UDS transport");
    assert_eq!(uds.path, Path::new("/run/hexput/hexput.sock"));
    assert_eq!(uds.mode, Some(0o660));
    let tcp = config.transports.tcp.expect("a TCP transport");
    assert_eq!(tcp.bind, "0.0.0.0:7400".parse::<SocketAddr>().unwrap());
    assert_eq!(tcp.tls.cert, Path::new("/tls/tcp-cert.pem"));
    assert_eq!(tcp.tls.key, Path::new("/tls/tcp-key.pem"));
    let websocket = config.transports.websocket.expect("a WebSocket transport");
    assert_eq!(websocket.bind, "[::1]:7401".parse::<SocketAddr>().unwrap());
    let tls = websocket.tls.expect("WebSocket TLS");
    assert_eq!(tls.cert, Path::new("/tls/ws-cert.pem"));
    assert_eq!(tls.key, Path::new("/tls/ws-key.pem"));
}

#[test]
fn log_level_and_session_ttl_have_documented_defaults() {
    let config = parse(MINIMAL).expect("a minimal file");
    assert_eq!(config.log_level, LogLevel::Info);
    assert_eq!(config.session_ttl, Duration::from_secs(300));
    assert_eq!(DEFAULT_SESSION_TTL, Duration::from_secs(300));
    assert!(config.transports.tcp.is_none());
    assert!(config.transports.websocket.is_none());
    let uds = config.transports.uds.expect("a UDS transport");
    assert_eq!(uds.mode, None, "no mode means the umask decides");
}

#[test]
fn every_log_level_is_accepted() {
    for level in LogLevel::ALL {
        let config = parse(&marked(level.as_str())).expect("a valid level");
        assert_eq!(config.log_level, level);
    }
}

#[test]
fn a_websocket_transport_may_omit_tls() {
    let config = parse("[transport.websocket]\nbind = \"127.0.0.1:7401\"\n").expect("valid");
    let websocket = config.transports.websocket.expect("a WebSocket transport");
    assert!(websocket.tls.is_none());
}

/// The documentation cannot drift from the parser: the shipped example must parse, set every
/// field, and state exactly the defaults the parser applies.
#[test]
fn the_example_file_parses_and_documents_every_field() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hexput-config/config.example.toml");
    let text = std::fs::read_to_string(&path).expect("the example file");
    let config = parse(&text).expect("the example file parses");

    let uds = config.transports.uds.expect("the example sets UDS");
    assert!(uds.mode.is_some(), "the example documents the socket mode");
    assert!(config.transports.tcp.is_some());
    let websocket = config
        .transports
        .websocket
        .expect("the example sets WebSocket");
    assert!(websocket.tls.is_some(), "the example sets WebSocket TLS");
    assert_eq!(config.log_level, LogLevel::Info);
    assert_eq!(config.session_ttl, DEFAULT_SESSION_TTL);
    assert!(text.contains("defaults to \"info\""));
    assert!(text.contains("defaults to 300"));
    assert!(text.contains(ENV_VAR));
    assert!(text.contains(hexput_config::DEFAULT_PATH) || cfg!(windows));

    let loaded = resolve(Some(&path), None, Path::new("/unused")).expect("resolves too");
    assert_eq!(loaded.source, ConfigSource::Flag);
}

// --- precedence (AD-7) ---

struct ThreeFiles {
    _sandbox: Sandbox,
    flag: PathBuf,
    env: PathBuf,
    default: PathBuf,
}

fn three_files() -> ThreeFiles {
    let sandbox = Sandbox::new();
    ThreeFiles {
        flag: sandbox.file("flag.toml", &marked("error")),
        env: sandbox.file("env.toml", &marked("warn")),
        default: sandbox.file("default.toml", &marked("trace")),
        _sandbox: sandbox,
    }
}

#[test]
fn the_flag_beats_the_env_var_and_the_default() {
    let files = three_files();
    let loaded = resolve(
        Some(&files.flag),
        Some(files.env.as_os_str()),
        &files.default,
    )
    .expect("loads");
    assert_eq!(loaded.source, ConfigSource::Flag);
    assert_eq!(loaded.path, files.flag);
    assert_eq!(loaded.config.log_level, LogLevel::Error);
}

#[test]
fn without_the_flag_the_env_var_beats_the_default() {
    let files = three_files();
    let loaded = resolve(None, Some(files.env.as_os_str()), &files.default).expect("loads");
    assert_eq!(loaded.source, ConfigSource::Env);
    assert_eq!(loaded.path, files.env);
    assert_eq!(loaded.config.log_level, LogLevel::Warn);
}

#[test]
fn without_the_flag_or_the_env_var_the_default_is_used() {
    let files = three_files();
    let loaded = resolve(None, None, &files.default).expect("loads");
    assert_eq!(loaded.source, ConfigSource::Default);
    assert_eq!(loaded.path, files.default);
    assert_eq!(loaded.config.log_level, LogLevel::Trace);
}

#[test]
fn the_flag_beats_the_default_without_an_env_var() {
    let files = three_files();
    let loaded = resolve(Some(&files.flag), None, &files.default).expect("loads");
    assert_eq!(loaded.source, ConfigSource::Flag);
    assert_eq!(loaded.config.log_level, LogLevel::Error);
}

#[test]
fn an_empty_env_var_counts_as_unset() {
    let files = three_files();
    let loaded = resolve(None, Some(OsStr::new("")), &files.default).expect("loads");
    assert_eq!(loaded.source, ConfigSource::Default);
}

#[test]
fn an_empty_flag_is_not_found_and_does_not_fall_through() {
    let files = three_files();
    let error = resolve(
        Some(Path::new("")),
        Some(files.env.as_os_str()),
        &files.default,
    )
    .expect_err("an empty flag names no file");
    assert_eq!(error.source, ConfigSource::Flag);
    assert_eq!(error.path, Path::new(""));
    assert!(matches!(error.problem, Problem::NotFound), "{error:?}");
}

// --- no fallthrough ---

#[test]
fn a_flag_naming_a_missing_file_does_not_fall_through_to_the_env_var() {
    let files = three_files();
    let error = resolve(
        Some(Path::new("/nope.toml")),
        Some(files.env.as_os_str()),
        &files.default,
    )
    .expect_err("the flag's file is missing");
    assert_eq!(error.path, Path::new("/nope.toml"));
    assert_eq!(error.source, ConfigSource::Flag);
    assert!(matches!(error.problem, Problem::NotFound));
    assert_eq!(
        error.to_string(),
        "/nope.toml: System Config file not found (named by the --config flag)"
    );
}

#[test]
fn an_env_var_naming_a_missing_file_does_not_fall_through_to_the_default() {
    let files = three_files();
    let missing = files._sandbox.missing("absent.toml");
    let error = resolve(None, Some(missing.as_os_str()), &files.default)
        .expect_err("the env var's file is missing");
    assert_eq!(error.path, missing);
    assert_eq!(error.source, ConfigSource::Env);
    assert_eq!(
        error.to_string(),
        format!(
            "{}: System Config file not found (named by the HEXPUT_CONFIG environment variable)",
            missing.display()
        )
    );
}

#[test]
fn a_missing_default_file_names_the_path_and_the_overrides() {
    let sandbox = Sandbox::new();
    let default = sandbox.missing("config.toml");
    let error = resolve(None, None, &default).expect_err("nothing to load");
    assert_eq!(error.source, ConfigSource::Default);
    assert_eq!(
        error.to_string(),
        format!(
            "{}: no System Config file at the default path; pass --config <path> or set \
             HEXPUT_CONFIG to use another file",
            default.display()
        )
    );
}

#[test]
fn a_directory_is_unreadable_not_missing() {
    let sandbox = Sandbox::new();
    let error = resolve(Some(&sandbox.dir), None, Path::new("/unused")).expect_err("a directory");
    assert!(matches!(error.problem, Problem::Unreadable(_)));
    let text = error.to_string();
    assert!(
        text.starts_with(&format!(
            "{}: cannot read System Config file (named by the --config flag): ",
            sandbox.dir.display()
        )),
        "{text}"
    );
    // The I/O error is in the text; `source()` does not repeat it for a chained reporter.
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn a_file_that_is_not_utf8_is_unreadable() {
    let sandbox = Sandbox::new();
    let path = sandbox.dir.join("binary.toml");
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    let error = resolve(Some(&path), None, Path::new("/unused")).expect_err("not UTF-8");
    assert!(matches!(error.problem, Problem::Unreadable(_)));
}

// --- malformed ---

#[test]
fn malformed_toml_names_the_file_line_and_column() {
    let (path, text) = rendered_error("log_level = \"info\"\n[transport.uds\npath = \"x\"\n");
    assert!(
        text.starts_with(&format!("{}:2:", path.display())),
        "expected the file and line 2: {text}"
    );
    match parse_err("log_level = \"info\"\n[transport.uds\npath = \"x\"\n") {
        Problem::Parse {
            location: Some(Location { line: 2, column }),
            ..
        } => assert!(column > 1, "a column inside the header, got {column}"),
        other => panic!("expected a located parse error, got {other:?}"),
    }
}

#[test]
fn a_duplicate_key_is_a_parse_error() {
    assert!(matches!(
        parse_err("log_level = \"info\"\nlog_level = \"warn\"\n[transport.uds]\npath = \"x\"\n"),
        Problem::Parse {
            location: Some(Location { line: 2, .. }),
            ..
        }
    ));
}

// --- missing required field ---

#[test]
fn a_tcp_transport_without_tls_cert_names_the_file_and_the_field() {
    let (path, text) =
        rendered_error("[transport.tcp]\nbind = \"0.0.0.0:7400\"\ntls_key = \"/k.pem\"\n");
    assert!(text.starts_with(&format!("{}:", path.display())), "{text}");
    assert!(text.contains("missing field `tls_cert`"), "{text}");
}

#[test]
fn every_field_of_a_present_transport_is_required() {
    for (text, field) in [
        ("[transport.uds]\n", "path"),
        (
            "[transport.tcp]\ntls_cert = \"c\"\ntls_key = \"k\"\n",
            "bind",
        ),
        (
            "[transport.tcp]\nbind = \"0.0.0.0:1\"\ntls_cert = \"c\"\n",
            "tls_key",
        ),
        ("[transport.websocket]\n", "bind"),
    ] {
        match parse_err(text) {
            Problem::Parse { message, .. } => assert!(
                message.contains(&format!("missing field `{field}`")),
                "{text:?}: {message}"
            ),
            other => panic!("{text:?}: expected a missing-field error, got {other:?}"),
        }
    }
}

#[test]
fn half_a_websocket_tls_pair_is_rejected_naming_the_missing_half() {
    let (path, text) = rendered_error(
        "log_level = \"info\"\n[transport.websocket]\nbind = \"127.0.0.1:1\"\ntls_cert = \"c\"\n",
    );
    assert_eq!(
        text,
        format!(
            "{}:2:1: invalid value for `transport.websocket`: tls_cert and tls_key must be \
             given together or not at all; `tls_cert` is set but `tls_key` is missing",
            path.display()
        )
    );
    match parse_err("[transport.websocket]\nbind = \"127.0.0.1:1\"\ntls_key = \"k\"\n") {
        Problem::InvalidValue { message, .. } => {
            assert!(message.ends_with("`tls_key` is set but `tls_cert` is missing"))
        }
        other => panic!("expected an invalid value, got {other:?}"),
    }
}

// --- invalid values ---

#[test]
fn an_unknown_log_level_names_the_field_and_the_accepted_values() {
    let (path, text) = rendered_error(&format!("log_level = \"loud\"\n{MINIMAL}"));
    assert_eq!(
        text,
        format!(
            "{}:1:13: invalid value for `log_level`: expected one of error, warn, info, debug, \
             trace, got \"loud\"",
            path.display()
        )
    );
}

#[test]
fn a_zero_or_negative_session_ttl_is_an_error_not_a_default() {
    for (value, column) in [("0", 20), ("-5", 20)] {
        let (path, text) = rendered_error(&format!("session_ttl_secs = {value}\n{MINIMAL}"));
        assert_eq!(
            text,
            format!(
                "{}:1:{column}: invalid value for `session_ttl_secs`: expected a whole number of \
                 seconds, at least 1, got {value}",
                path.display()
            )
        );
    }
}

#[test]
fn a_session_ttl_of_the_wrong_type_is_rejected() {
    assert!(matches!(
        parse_err(&format!("session_ttl_secs = \"300\"\n{MINIMAL}")),
        Problem::Parse {
            location: Some(Location { line: 1, .. }),
            ..
        }
    ));
}

#[test]
fn a_bind_that_is_not_an_address_and_port_is_rejected() {
    match parse_err("[transport.websocket]\nbind = \"localhost\"\n") {
        Problem::InvalidValue {
            field,
            location,
            message,
        } => {
            assert_eq!(field, "transport.websocket.bind");
            assert_eq!(location, Some(Location { line: 2, column: 8 }));
            assert!(message.contains("got \"localhost\""), "{message}");
        }
        other => panic!("expected an invalid bind, got {other:?}"),
    }
}

#[test]
fn columns_count_characters_not_bytes() {
    // "üé" is four bytes but two characters, so a byte count would land two columns late.
    let line = r#"transport.websocket = { tls_cert = "üé", tls_key = "k", bind = "bad" }"#;
    let byte_offset = line.find(r#""bad""#).unwrap();
    let column = line[..byte_offset].chars().count() + 1;
    assert_eq!(column, byte_offset + 1 - 2);
    match parse_err(line) {
        Problem::InvalidValue {
            field, location, ..
        } => {
            assert_eq!(field, "transport.websocket.bind");
            assert_eq!(location, Some(Location { line: 1, column }));
        }
        other => panic!("expected an invalid bind, got {other:?}"),
    }
}

#[test]
fn an_empty_path_is_rejected() {
    match parse_err("[transport.uds]\npath = \"\"\n") {
        Problem::InvalidValue { field, .. } => assert_eq!(field, "transport.uds.path"),
        other => panic!("expected an invalid path, got {other:?}"),
    }
}

#[test]
fn a_socket_mode_is_three_or_four_octal_digits() {
    for (text, bits) in [("660", 0o660), ("0660", 0o660), ("0777", 0o777), ("000", 0)] {
        let config = parse(&format!(
            "[transport.uds]\npath = \"x\"\nmode = \"{text}\"\n"
        ))
        .expect("a valid mode");
        assert_eq!(config.transports.uds.unwrap().mode, Some(bits), "{text}");
    }
}

#[test]
fn an_invalid_socket_mode_names_the_field_and_the_value() {
    for text in [
        "",
        "66",
        "06600",
        "0668",
        "1777",
        "rw-rw----",
        "+660",
        " 660",
    ] {
        let file = format!("[transport.uds]\npath = \"x\"\nmode = \"{text}\"\n");
        match parse_err(&file) {
            Problem::InvalidValue {
                field,
                location,
                message,
            } => {
                assert_eq!(field, "transport.uds.mode", "{text:?}");
                assert_eq!(location.map(|l| l.line), Some(3), "{text:?}");
                assert!(
                    message.contains(&format!("{text:?}")),
                    "{text:?}: {message}"
                );
            }
            other => panic!("{text:?}: expected an invalid mode, got {other:?}"),
        }
    }
}

#[test]
fn a_socket_mode_must_be_a_string() {
    // A bare `660` would be decimal 660 (0o1224), not the octal an operator means.
    match parse_err("[transport.uds]\npath = \"x\"\nmode = 660\n") {
        Problem::Parse { message, .. } => assert!(message.contains("string"), "{message}"),
        other => panic!("expected a type error, got {other:?}"),
    }
}

// --- unknown keys ---

#[test]
fn an_unknown_top_level_key_is_named() {
    let (path, text) = rendered_error(&format!("log_levle = \"info\"\n{MINIMAL}"));
    assert!(
        text.starts_with(&format!("{}:1:1:", path.display())),
        "{text}"
    );
    assert!(text.contains("unknown field `log_levle`"), "{text}");
}

#[test]
fn unknown_keys_are_rejected_at_every_level() {
    for (text, key) in [
        (
            "[transport.uds]\npath = \"x\"\nowner = \"hexput\"\n",
            "owner",
        ),
        ("[transport.pipe]\nname = \"x\"\n", "pipe"),
        (
            "[transport.websocket]\nbind = \"127.0.0.1:1\"\ntls = true\n",
            "tls",
        ),
    ] {
        match parse_err(text) {
            Problem::Parse { message, .. } => assert!(
                message.contains(&format!("unknown field `{key}`")),
                "{text:?}: {message}"
            ),
            other => panic!("{text:?}: expected an unknown-key error, got {other:?}"),
        }
    }
}

// --- no transport ---

#[test]
fn a_file_with_no_transport_is_rejected() {
    for text in ["", "log_level = \"info\"\n", "[transport]\n"] {
        assert!(
            matches!(parse_err(text), Problem::NoTransport),
            "{text:?} has no transport"
        );
    }
    let (path, text) = rendered_error("log_level = \"info\"\n");
    assert_eq!(
        text,
        format!(
            "{}: no transport configured; at least one of [transport.uds], [transport.tcp] or \
             [transport.websocket] is required",
            path.display()
        )
    );
}

// --- the public surface ---

#[test]
fn the_default_path_and_env_var_are_the_documented_ones() {
    assert_eq!(ENV_VAR, "HEXPUT_CONFIG");
    #[cfg(unix)]
    {
        assert_eq!(hexput_config::DEFAULT_PATH, "/etc/hexput/config.toml");
        assert_eq!(
            hexput_config::default_path(),
            Path::new("/etc/hexput/config.toml")
        );
    }
}

#[test]
fn config_error_is_a_std_error() {
    fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
    assert_error::<ConfigError>();
}
