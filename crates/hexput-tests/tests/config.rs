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
    ConfigError, ConfigSource, DEFAULT_AST_CACHE_CAPACITY, DEFAULT_SESSION_TTL,
    DEFAULT_WEBSOCKET_PORT, ENV_VAR, Location, LogFormat, LogLevel, Problem, parse, resolve,
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
log_format = "json"
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

[transport.named_pipe]
path = '\\.\pipe\hexput'
"#,
    )
    .expect("a valid file");

    assert_eq!(config.log_level, LogLevel::Debug);
    assert_eq!(config.log_format, LogFormat::Json);
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
    // Story 5.2: TLS is required for WebSocket, so the pair is always present.
    let tls = websocket.tls;
    assert_eq!(tls.cert, Path::new("/tls/ws-cert.pem"));
    assert_eq!(tls.key, Path::new("/tls/ws-key.pem"));
    // Story 5.3: the Named Pipe is a transport on every platform, its path kept verbatim.
    let pipe = config
        .transports
        .named_pipe
        .expect("a Named Pipe transport");
    assert_eq!(pipe.path, r"\\.\pipe\hexput");
}

#[test]
fn log_level_and_session_ttl_have_documented_defaults() {
    let config = parse(MINIMAL).expect("a minimal file");
    assert_eq!(config.log_level, LogLevel::Info);
    assert_eq!(
        config.log_format,
        LogFormat::Text,
        "plain text is the default"
    );
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
fn every_log_format_is_accepted() {
    for format in LogFormat::ALL {
        let config = parse(&format!("log_format = \"{format}\"\n{MINIMAL}")).expect("a format");
        assert_eq!(config.log_format, format);
        assert_eq!(format.to_string(), format.as_str());
    }
    assert_eq!(
        LogFormat::ALL.map(LogFormat::as_str),
        ["text", "json"],
        "the accepted values, in the order errors list them"
    );
}

#[test]
fn a_websocket_transport_must_give_tls_and_a_missing_pair_is_refused() {
    // Story 5.2: `tls_cert` and `tls_key` are required, as for TCP. Leaving the pair out is
    // refused at parse time, never listened on in cleartext.
    for (text, field) in [
        (
            "[transport.websocket]\nbind = \"127.0.0.1:7401\"\n",
            "tls_cert",
        ),
        (
            "[transport.websocket]\nbind = \"127.0.0.1:7401\"\ntls_cert = \"c\"\n",
            "tls_key",
        ),
        (
            "[transport.websocket]\nbind = \"127.0.0.1:7401\"\ntls_key = \"k\"\n",
            "tls_cert",
        ),
    ] {
        match parse_err(text) {
            Problem::Parse {
                message, location, ..
            } => {
                assert!(
                    message.contains(&format!("missing field `{field}`")),
                    "{text:?}: {message}"
                );
                assert!(location.is_some(), "{text:?} gives its position");
            }
            other => panic!("{text:?}: expected a missing-field error, got {other:?}"),
        }
    }
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
    // Story 5.3: the example documents the Named Pipe, parsed on every platform.
    let pipe = config
        .transports
        .named_pipe
        .as_ref()
        .expect("the example sets the Named Pipe");
    assert_eq!(pipe.path, r"\\.\pipe\hexput");
    assert!(
        text.contains("Windows-only"),
        "the pipe is documented as Windows-only"
    );
    assert!(text.contains("Unix-only"), "UDS is documented as Unix-only");
    let websocket = config
        .transports
        .websocket
        .expect("the example sets WebSocket");
    assert_eq!(
        websocket.bind,
        "0.0.0.0:7478".parse::<std::net::SocketAddr>().unwrap(),
        "the example's bare-IP WebSocket bind takes the default port"
    );
    assert_eq!(websocket.tls.cert, Path::new("/etc/hexput/tls/cert.pem"));
    assert_eq!(websocket.tls.key, Path::new("/etc/hexput/tls/key.pem"));
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
        (
            "[transport.websocket]\nbind = \"127.0.0.1:7401\"\ntls_key = \"k\"\n",
            "tls_cert",
        ),
        (
            "[transport.websocket]\nbind = \"127.0.0.1:7401\"\ntls_cert = \"c\"\n",
            "tls_key",
        ),
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
fn a_websocket_section_without_its_tls_pair_is_rejected_with_the_field_and_its_position() {
    // Story 5.2: the half-pair check is gone; serde's missing-field error names the field and
    // gives the line and column, as every other System Config error does.
    let (path, text) = rendered_error(
        "log_level = \"info\"\n[transport.websocket]\nbind = \"127.0.0.1:1\"\ntls_cert = \"c\"\n",
    );
    assert!(
        text.starts_with(&format!("{}:", path.display())),
        "names the file: {text}"
    );
    assert!(text.contains("missing field `tls_key`"), "{text}");
    match parse_err("[transport.websocket]\nbind = \"127.0.0.1:1\"\ntls_key = \"k\"\n") {
        Problem::Parse { message, .. } => assert!(message.contains("missing field `tls_cert`")),
        other => panic!("expected a missing-field error, got {other:?}"),
    }
}

// --- invalid values ---

#[test]
fn an_unknown_log_format_names_the_field_and_the_accepted_values() {
    let (path, text) = rendered_error(&format!("log_format = \"xml\"\n{MINIMAL}"));
    assert_eq!(
        text,
        format!(
            "{}:1:14: invalid value for `log_format`: expected one of text, json, got \"xml\"",
            path.display()
        )
    );
    // A non-string is a shape error from the parser, naming the field.
    match parse_err(&format!("log_format = 1\n{MINIMAL}")) {
        Problem::Parse { message, .. } => assert!(message.contains("string"), "{message}"),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

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
    match parse_err(
        "[transport.websocket]\nbind = \"localhost\"\ntls_cert = \"c\"\ntls_key = \"k\"\n",
    ) {
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
            "[transport.websocket]\nbind = \"127.0.0.1:1\"\ntls_cert = \"c\"\ntls_key = \"k\"\ntls = true\n",
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

// --- Story 4.3: the AST cache's capacity ---

#[test]
fn the_ast_cache_capacity_defaults_to_ten_thousand_entries() {
    let config = parse(MINIMAL).expect("a minimal file");
    assert_eq!(config.ast_cache_capacity, 10_000);
    assert_eq!(DEFAULT_AST_CACHE_CAPACITY, 10_000);
}

#[test]
fn the_ast_cache_capacity_is_read_from_the_file() {
    let config = parse(&format!("ast_cache_capacity = 5\n{MINIMAL}")).expect("a valid file");
    assert_eq!(config.ast_cache_capacity, 5);
}

#[test]
fn a_zero_or_negative_ast_cache_capacity_is_an_error_naming_the_field_and_position() {
    for value in ["0", "-1"] {
        let (path, text) = rendered_error(&format!("ast_cache_capacity = {value}\n{MINIMAL}"));
        assert!(
            text.starts_with(&format!("{}:1:22: ", path.display())),
            "{text}"
        );
        assert!(text.contains("`ast_cache_capacity`"), "{text}");
        assert!(text.contains("at least 1"), "{text}");
        assert!(text.contains(&format!("got {value}")), "{text}");
    }
}

#[test]
fn an_ast_cache_capacity_of_the_wrong_type_is_rejected() {
    assert!(matches!(
        parse_err(&format!("ast_cache_capacity = \"x\"\n{MINIMAL}")),
        Problem::Parse {
            location: Some(Location { line: 1, .. }),
            ..
        }
    ));
}

#[test]
fn the_example_file_documents_the_ast_cache_capacity() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hexput-config/config.example.toml");
    let text = std::fs::read_to_string(&path).expect("the example file");
    assert!(
        text.contains("ast_cache_capacity"),
        "the example documents it"
    );
}

// --- Story 5.1: the TCP bind may omit its port (FR-29: 7476) ---

/// A `[transport.tcp]` section whose only variable is the bind value.
fn tcp_bind(bind: &str) -> String {
    format!("[transport.tcp]\nbind = \"{bind}\"\ntls_cert = \"/c.pem\"\ntls_key = \"/k.pem\"\n")
}

#[test]
fn the_default_tcp_port_is_7476() {
    assert_eq!(hexput_config::DEFAULT_TCP_PORT, 7476);
}

#[test]
fn a_bare_ip_bind_gets_the_default_tcp_port() {
    for (bind, expected) in [
        ("0.0.0.0", "0.0.0.0:7476"),
        ("::", "[::]:7476"),
        ("::1", "[::1]:7476"),
        ("[::1]", "[::1]:7476"),
        ("[::]", "[::]:7476"),
        ("127.0.0.1", "127.0.0.1:7476"),
    ] {
        let config = parse(&tcp_bind(bind)).unwrap_or_else(|error| panic!("{bind}: {error:?}"));
        let tcp = config.transports.tcp.expect("a TCP transport");
        assert_eq!(
            tcp.bind,
            expected.parse::<SocketAddr>().unwrap(),
            "{bind} means {expected}"
        );
    }
}

#[test]
fn an_explicit_tcp_port_is_kept_and_port_zero_stays_valid() {
    for (bind, expected) in [
        ("0.0.0.0:7400", "0.0.0.0:7400"),
        ("[::1]:7400", "[::1]:7400"),
        ("127.0.0.1:0", "127.0.0.1:0"),
        ("[::]:0", "[::]:0"),
    ] {
        let config = parse(&tcp_bind(bind)).unwrap_or_else(|error| panic!("{bind}: {error:?}"));
        let tcp = config.transports.tcp.expect("a TCP transport");
        assert_eq!(tcp.bind, expected.parse::<SocketAddr>().unwrap());
    }
}

#[test]
fn an_unparsable_tcp_bind_names_the_field_and_says_the_port_is_optional() {
    for bind in [
        "not-an-address",
        "localhost",
        "127.0.0.1:notaport",
        "999.1.1.1",
        // Brackets hold only an IPv6 address.
        "[127.0.0.1]",
        "[0.0.0.0]",
    ] {
        match parse_err(&tcp_bind(bind)) {
            Problem::InvalidValue { field, message, .. } => {
                assert_eq!(field, "transport.tcp.bind", "{bind}");
                assert!(
                    message.to_lowercase().contains("optional"),
                    "{bind}: the message says the port is optional: {message}"
                );
            }
            other => panic!("{bind}: expected an invalid bind, got {other:?}"),
        }
    }
}

#[test]
fn a_bare_ip_websocket_bind_listens_on_the_default_port_7478() {
    // Story 5.2: the WebSocket default port is 7478, as FR-29 fixes it.
    assert_eq!(DEFAULT_WEBSOCKET_PORT, 7478);
    for (bind, expected) in [("127.0.0.1", "127.0.0.1:7478"), ("::1", "[::1]:7478")] {
        let config = parse(&format!(
            "[transport.websocket]\nbind = \"{bind}\"\ntls_cert = \"c\"\ntls_key = \"k\"\n"
        ))
        .unwrap_or_else(|error| panic!("{bind}: {error:?}"));
        let websocket = config.transports.websocket.expect("a WebSocket transport");
        assert_eq!(
            websocket.bind,
            expected.parse::<SocketAddr>().unwrap(),
            "{bind}"
        );
    }
    // A port given explicitly still wins over the default.
    let config = parse(
        "[transport.websocket]\nbind = \"127.0.0.1:7401\"\ntls_cert = \"c\"\ntls_key = \"k\"\n",
    )
    .expect("valid");
    assert_eq!(
        config.transports.websocket.unwrap().bind,
        "127.0.0.1:7401".parse::<SocketAddr>().unwrap()
    );
}

// --- Story 5.3: the Named Pipe transport ---

/// A file whose only transport is the Named Pipe at `path`, which is a TOML literal string.
fn pipe_only(path: &str) -> String {
    format!("[transport.named_pipe]\npath = '{path}'\n")
}

/// The expected form, verbatim, as the message names it.
const PIPE_EXPECTED: &str = r"expected a pipe path such as '\\.\pipe\hexput' (at most 256 characters, no '\' after '\\.\pipe\')";

#[test]
fn a_named_pipe_is_a_transport_so_a_pipe_only_file_parses() {
    let config = parse(&pipe_only(r"\\.\pipe\hexput")).expect("a pipe-only file is valid");
    let pipe = config
        .transports
        .named_pipe
        .expect("a Named Pipe transport");
    assert_eq!(pipe.path, r"\\.\pipe\hexput");
    assert!(config.transports.uds.is_none());
    assert!(config.transports.tcp.is_none());
    assert!(config.transports.websocket.is_none());
}

#[test]
fn a_bad_named_pipe_path_names_the_field_its_position_and_the_expected_form() {
    let long = format!(r"\\.\pipe\{}", "a".repeat(248));
    assert_eq!(long.chars().count(), 257);
    for path in ["/tmp/x", r"\\.\pipe\", r"\\.\pipe\a\b", long.as_str()] {
        let text = pipe_only(path);
        match parse_err(&text) {
            Problem::InvalidValue {
                field,
                location,
                message,
            } => {
                assert_eq!(field, "transport.named_pipe.path", "{path:?}");
                assert_eq!(location, Some(Location { line: 2, column: 8 }), "{path:?}");
                assert!(message.contains(PIPE_EXPECTED), "{path:?}: {message}");
            }
            other => panic!("{path:?}: expected an invalid pipe path, got {other:?}"),
        }
    }
}

#[test]
fn accepted_named_pipe_path_forms_are_kept_verbatim() {
    let exactly_256 = format!(r"\\.\pipe\{}", "a".repeat(247));
    assert_eq!(exactly_256.chars().count(), 256);
    for path in [r"\\.\pipe\hexput", r"\\.\PIPE\hexput", exactly_256.as_str()] {
        let config = parse(&pipe_only(path)).unwrap_or_else(|error| panic!("{path:?}: {error:?}"));
        assert_eq!(
            config.transports.named_pipe.expect("a pipe").path,
            path,
            "{path:?} is kept verbatim"
        );
    }
}

/// A NUL would end the name Windows sees early, so the Daemon would serve another pipe; any
/// control character is refused, and the message escapes it so it stays one line. The length is
/// Windows' own count, in UTF-16 units: a character outside the BMP is two.
#[test]
fn a_named_pipe_path_with_a_control_character_or_too_many_utf16_units_is_rejected() {
    // TOML basic strings, so `\u0000` and `\n` reach the validator as the characters.
    for (path, shown) in [
        (r"\\\\.\\pipe\\a\u0000b", r"\\.\pipe\a\u{0}b"),
        (r"\\\\.\\pipe\\a\nb", r"\\.\pipe\a\nb"),
    ] {
        let text = format!("[transport.named_pipe]\npath = \"{path}\"\n");
        match parse_err(&text) {
            Problem::InvalidValue { field, message, .. } => {
                assert_eq!(field, "transport.named_pipe.path", "{path:?}");
                assert!(message.contains(PIPE_EXPECTED), "{path:?}: {message}");
                assert!(message.contains(shown), "{path:?}: {message}");
                assert!(!message.contains('\n'), "one line: {message:?}");
            }
            other => panic!("{path:?}: expected an invalid pipe path, got {other:?}"),
        }
    }

    // 129 non-BMP characters: 138 characters, but 267 UTF-16 units.
    let wide = format!(r"\\.\pipe\{}", "\u{1F600}".repeat(129));
    assert!(wide.chars().count() <= 256 && wide.encode_utf16().count() > 256);
    match parse_err(&pipe_only(&wide)) {
        Problem::InvalidValue { field, .. } => assert_eq!(field, "transport.named_pipe.path"),
        other => panic!("expected an invalid pipe path, got {other:?}"),
    }
    // 123 of them, 255 units, fit.
    let fits = format!(r"\\.\pipe\{}", "\u{1F600}".repeat(123));
    assert_eq!(fits.encode_utf16().count(), 255);
    assert!(parse(&pipe_only(&fits)).is_ok());
}

#[test]
fn a_named_pipe_section_without_a_path_or_with_an_unknown_key_is_rejected() {
    match parse_err("[transport.named_pipe]\n") {
        Problem::Parse { location, message } => {
            assert!(message.contains("missing field `path`"), "{message}");
            assert!(location.is_some(), "the missing field has a position");
        }
        other => panic!("expected a missing-field error, got {other:?}"),
    }
    match parse_err("[transport.named_pipe]\npath = '\\\\.\\pipe\\hexput'\nmode = \"0660\"\n") {
        Problem::Parse { location, message } => {
            assert!(message.contains("unknown field `mode`"), "{message}");
            assert!(location.is_some(), "the unknown field has a position");
        }
        other => panic!("expected an unknown-field error, got {other:?}"),
    }
}
