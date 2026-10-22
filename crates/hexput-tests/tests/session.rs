//! Stories 2.4 + 2.5: decoding an `Init` payload, and the Session registry it creates into.
//! Stories 3.7 + 3.8: the Config's execution limits, and replacing the Config at runtime.
//! Story 5.5: the reconnect credential issued at init, and verifying a presented secret.
//! Story 5.6: reconnecting a Connection to a live Session, and decoding a `Reconnect` payload.
//! Story 5.8: a Session outlives its last Connection by the TTL, and expires only if still unattached.

use std::collections::HashSet;
use std::sync::Barrier;
use std::time::Duration;

use hexput_port::Value;
use hexput_session::{
    ClientId, Config, ConnectionId, Expiry, InitRequest, ReconnectRequest, ReconnectSecret,
    Sessions, Setting, Settings,
};

fn s(text: &str) -> Value {
    Value::from(text)
}

fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}

fn entry(name: Value) -> Value {
    map(vec![("name", name)])
}

fn payload(config: Value, registrations: Value) -> Value {
    map(vec![("config", config), ("registrations", registrations)])
}

/// Registrations, each stating its blanket grant explicitly.
fn named(registrations: &[(&str, bool)]) -> Value {
    Value::Array(
        registrations
            .iter()
            .map(|(name, blanket)| map(vec![("name", s(name)), ("blanket", Value::from(*blanket))]))
            .collect(),
    )
}

fn valid(registrations: &[(&str, bool)]) -> InitRequest {
    InitRequest::from_value(&payload(Value::Map(vec![]), named(registrations))).unwrap()
}

/// A Session's registrations as `(name, blanket)` pairs.
fn registrations(sessions: &Sessions, id: ClientId) -> Option<Vec<(String, bool)>> {
    sessions.registrations(id).map(|registered| {
        registered
            .iter()
            .map(|r| (r.name().to_owned(), r.blanket()))
            .collect()
    })
}

/// The refusal message for `value`, which must be refused.
fn refusal(value: &Value) -> String {
    InitRequest::from_value(value)
        .expect_err("the payload is refused")
        .to_string()
}

// --- decoding ---

#[test]
fn a_valid_init_keeps_its_registrations_in_order() {
    let init = valid(&[("getUser", true), ("sendMail", true)]);
    let names: Vec<_> = init.registrations().iter().map(|r| r.name()).collect();
    assert_eq!(names, ["getUser", "sendMail"]);
}

#[test]
fn a_registration_carries_its_blanket_grant_and_absent_means_none() {
    let init = InitRequest::from_value(&payload(
        Value::Map(vec![]),
        Value::Array(vec![
            map(vec![("name", s("granted")), ("blanket", Value::from(true))]),
            map(vec![
                ("name", s("withheld")),
                ("blanket", Value::from(false)),
            ]),
            // No `blanket` key at all: no blanket grant.
            entry(s("unstated")),
            // Key order does not matter.
            map(vec![("blanket", Value::from(true)), ("name", s("first"))]),
        ]),
    ))
    .unwrap();
    let grants: Vec<_> = init
        .registrations()
        .iter()
        .map(|r| (r.name(), r.blanket()))
        .collect();
    assert_eq!(
        grants,
        [
            ("granted", true),
            ("withheld", false),
            ("unstated", false),
            ("first", true)
        ]
    );
}

#[test]
fn a_blanket_grant_that_is_not_a_boolean_is_refused_by_index() {
    for bad in [
        s("yes"),
        Value::from(1),
        Value::Nil,
        Value::Array(vec![]),
        Value::Map(vec![]),
    ] {
        let message = refusal(&payload(
            Value::Map(vec![]),
            Value::Array(vec![
                map(vec![("name", s("a")), ("blanket", Value::from(true))]),
                map(vec![("name", s("b")), ("blanket", bad.clone())]),
            ]),
        ));
        assert_eq!(
            message, "`registrations[1].blanket` is not a boolean",
            "{bad:?}"
        );
    }
    let message = refusal(&payload(
        Value::Map(vec![]),
        Value::Array(vec![map(vec![
            ("name", s("a")),
            ("blanket", Value::from(true)),
            ("blanket", Value::from(false)),
        ])]),
    ));
    assert_eq!(message, "`registrations[0]` repeats the key `blanket`");
}

#[test]
fn grants_belong_to_their_session() {
    let sessions = Sessions::new();
    let (a, _) = create(&sessions, valid(&[("getOrder", true)]));
    let (b, _) = create(&sessions, valid(&[("getOrder", false)]));
    let (c, _) = create(&sessions, valid(&[]));
    assert_eq!(
        registrations(&sessions, a),
        Some(vec![("getOrder".to_owned(), true)])
    );
    assert_eq!(
        registrations(&sessions, b),
        Some(vec![("getOrder".to_owned(), false)])
    );
    assert_eq!(registrations(&sessions, c), Some(vec![]));
}

#[test]
fn no_registrations_is_empty_not_missing() {
    assert!(valid(&[]).registrations().is_empty());
}

#[test]
fn a_missing_part_is_named_and_both_are_named_together() {
    let both = "the `Init` payload is missing `config` and `registrations`";
    let cases = [
        (Value::Nil, both),
        (Value::Map(vec![]), both),
        (payload(Value::Nil, Value::Nil), both),
        (
            map(vec![("registrations", named(&[]))]),
            "the `Init` payload is missing `config`",
        ),
        (
            payload(Value::Nil, named(&[])),
            "the `Init` payload is missing `config`",
        ),
        (
            map(vec![("config", Value::Map(vec![]))]),
            "the `Init` payload is missing `registrations`",
        ),
    ];
    for (value, expected) in cases {
        assert_eq!(refusal(&value), expected, "{value}");
    }
}

#[test]
fn a_malformed_part_names_the_offending_key_or_index() {
    let empty = || Value::Map(vec![]);
    let cases = [
        (s("init"), "the `Init` payload must be a map"),
        (
            map(vec![
                ("config", empty()),
                ("registrations", named(&[])),
                ("extra", Value::Nil),
            ]),
            "unknown key `extra`",
        ),
        (
            Value::Map(vec![(Value::from(1), Value::Nil)]),
            "unknown key 1 (not a string)",
        ),
        (
            map(vec![
                ("config", empty()),
                ("config", empty()),
                ("registrations", named(&[])),
            ]),
            "repeats the key `config`",
        ),
        (
            payload(Value::Array(vec![]), named(&[])),
            "`config` is not a map",
        ),
        (
            payload(map(vec![("timeout", Value::from(5))]), named(&[])),
            "`config.timeout` is not a known setting",
        ),
        (
            payload(
                map(vec![(
                    "budget",
                    map(vec![("memory_bytes", Value::from(1_u64 << 40))]),
                )]),
                named(&[]),
            ),
            "`config.budget.memory_bytes` must be an integer from 1024 to 1073741824; found \
             1099511627776",
        ),
        (payload(empty(), empty()), "`registrations` is not an array"),
        (
            payload(empty(), Value::Array(vec![s("getUser")])),
            "`registrations[0]` is not a map",
        ),
        (
            payload(empty(), Value::Array(vec![entry(s("a")), empty()])),
            "`registrations[1].name` is missing",
        ),
        (
            payload(empty(), Value::Array(vec![entry(Value::from(7))])),
            "`registrations[0].name` is not a string",
        ),
        (
            payload(empty(), Value::Array(vec![entry(s(""))])),
            "`registrations[0].name` is empty",
        ),
        (
            payload(
                empty(),
                Value::Array(vec![map(vec![("name", s("a")), ("grants", Value::Nil)])]),
            ),
            "`registrations[0]` has an unknown key `grants`",
        ),
        (
            payload(
                empty(),
                Value::Array(vec![map(vec![("name", s("a")), ("name", s("b"))])]),
            ),
            "`registrations[0]` repeats the key `name`",
        ),
    ];
    for (value, expected) in cases {
        let message = refusal(&value);
        assert!(message.contains(expected), "{message:?} names {expected:?}");
    }
}

#[test]
fn a_duplicate_registration_is_named() {
    let message = refusal(&payload(
        Value::Map(vec![]),
        named(&[("getUser", true), ("sendMail", true), ("getUser", true)]),
    ));
    assert_eq!(
        message,
        "`getUser` is registered twice, at `registrations[0]` and `registrations[2]`"
    );
}

// --- Story 3.12: Registered Methods ---

#[test]
fn a_registration_with_a_key_is_a_method_under_it() {
    let init = InitRequest::from_value(&payload(
        Value::Map(vec![]),
        Value::Array(vec![
            map(vec![
                ("name", s("save")),
                ("key", s("User")),
                ("blanket", Value::from(true)),
            ]),
            map(vec![("name", s("save"))]),
            map(vec![("key", s("Order")), ("name", s("save"))]),
        ]),
    ))
    .unwrap();
    let listed: Vec<_> = init
        .registrations()
        .iter()
        .map(|r| (r.key(), r.name(), r.blanket()))
        .collect();
    // A function and methods under two keys share the name `save`.
    assert_eq!(
        listed,
        vec![
            (Some("User"), "save", true),
            (None, "save", false),
            (Some("Order"), "save", false),
        ]
    );
}

#[test]
fn a_key_that_is_empty_or_not_a_string_is_refused_by_index() {
    for (bad, problem) in [
        (s(""), "is empty"),
        (Value::from(1), "is not a string"),
        (Value::Nil, "is not a string"),
        (Value::Boolean(true), "is not a string"),
    ] {
        let message = refusal(&payload(
            Value::Map(vec![]),
            Value::Array(vec![
                map(vec![("name", s("a"))]),
                map(vec![("name", s("b")), ("key", bad)]),
            ]),
        ));
        assert_eq!(message, format!("`registrations[1].key` {problem}"));
    }
    let message = refusal(&payload(
        Value::Map(vec![]),
        Value::Array(vec![map(vec![
            ("name", s("a")),
            ("key", s("User")),
            ("key", s("Order")),
        ])]),
    ));
    assert_eq!(message, "`registrations[0]` repeats the key `key`");
}

#[test]
fn a_method_named_secret_is_refused_and_a_function_so_named_is_not() {
    let message = refusal(&payload(
        Value::Map(vec![]),
        Value::Array(vec![
            map(vec![("name", s("save")), ("key", s("User"))]),
            map(vec![("name", s("__secret")), ("key", s("User"))]),
        ]),
    ));
    assert_eq!(
        message,
        "`registrations[1].name` cannot be `__secret` for a method: a Script can never call it"
    );
    // A bare-name function is reachable by a call, so its name is the Backend's business.
    InitRequest::from_value(&payload(
        Value::Map(vec![]),
        Value::Array(vec![map(vec![("name", s("__secret"))])]),
    ))
    .unwrap();
}

#[test]
fn a_duplicate_method_under_one_key_is_named() {
    let message = refusal(&payload(
        Value::Map(vec![]),
        Value::Array(vec![
            map(vec![("name", s("save")), ("key", s("User"))]),
            map(vec![("name", s("save")), ("key", s("Order"))]),
            map(vec![("name", s("save")), ("key", s("User"))]),
        ]),
    ));
    assert_eq!(
        message,
        "`save` under key `User` is registered twice, at `registrations[0]` and \
         `registrations[2]`"
    );
}

// --- the registry ---

/// A Connection opening and completing init: what `hexput-connection` does, in that order.
fn create(sessions: &Sessions, init: InitRequest) -> (ClientId, ConnectionId) {
    let connection = sessions.connect();
    let (id, _secret) = sessions.create(init, connection);
    (id, connection)
}

#[test]
fn a_connection_id_is_issued_before_init_and_becomes_its_attachment() {
    let sessions = Sessions::new();
    let first = sessions.connect();
    let second = sessions.connect();
    assert_ne!(first, second);
    // Connecting creates no Session.
    assert!(sessions.is_empty());
    let (id, _secret) = sessions.create(valid(&[]), second);
    assert_eq!(sessions.attached(id), Some(1));
    // Detaching under the id it was created with is what removes it.
    sessions.detach(id, first);
    assert!(sessions.contains(id));
    sessions.detach(id, second);
    assert!(!sessions.contains(id));
}

#[test]
fn create_issues_a_client_id_with_its_creator_attached() {
    let sessions = Sessions::new();
    let (id, _) = create(&sessions, valid(&[("getUser", true)]));
    assert!(sessions.contains(id));
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions.attached(id), Some(1));
    assert_eq!(
        registrations(&sessions, id),
        Some(vec![("getUser".to_owned(), true)])
    );
    let text = id.to_string();
    assert_eq!(text.len(), ClientId::TEXT_LEN);
    assert_eq!(text.parse::<ClientId>().unwrap(), id);
}

#[test]
fn client_ids_and_attachments_are_unique() {
    let sessions = Sessions::new();
    let created: Vec<_> = (0..1000).map(|_| create(&sessions, valid(&[]))).collect();
    let ids: HashSet<_> = created.iter().map(|(id, _)| *id).collect();
    let connections: HashSet<_> = created.iter().map(|(_, c)| *c).collect();
    assert_eq!(ids.len(), 1000);
    assert_eq!(connections.len(), 1000);
    assert_eq!(sessions.len(), 1000);
}

#[test]
fn detaching_the_last_connection_tears_the_session_down() {
    let sessions = Sessions::new();
    let (id, connection) = create(&sessions, valid(&[("getUser", true)]));
    let (other, _) = create(&sessions, valid(&[("other", true)]));
    sessions.detach(id, connection);
    assert!(!sessions.contains(id));
    assert_eq!(sessions.attached(id), None);
    assert_eq!(registrations(&sessions, id), None);
    let late = sessions.connect();
    assert!(!sessions.attach(id, late), "a torn-down Session is gone");
    assert_eq!(
        sessions.attached(id),
        None,
        "a refused attach creates nothing"
    );
    assert!(sessions.contains(other), "no other Session is touched");
}

#[test]
fn detaching_one_of_two_connections_keeps_the_session() {
    let sessions = Sessions::new();
    let (id, first) = create(&sessions, valid(&[("getUser", true)]));
    let second = sessions.connect();
    assert!(sessions.attach(id, second), "the Session is live");
    assert_ne!(first, second);
    assert_eq!(sessions.attached(id), Some(2));

    sessions.detach(id, first);
    assert!(sessions.contains(id));
    assert_eq!(sessions.attached(id), Some(1));
    assert_eq!(
        registrations(&sessions, id),
        Some(vec![("getUser".to_owned(), true)])
    );

    sessions.detach(id, second);
    assert!(!sessions.contains(id));
}

#[test]
fn detaching_what_is_not_attached_changes_nothing() {
    let sessions = Sessions::new();
    let (id, connection) = create(&sessions, valid(&[]));
    let (other, stranger) = create(&sessions, valid(&[]));
    // A connection of another Session.
    sessions.detach(id, stranger);
    assert_eq!(sessions.attached(id), Some(1));
    // A Session that does not exist.
    sessions.detach(ClientId::from_bytes([0; 16]), connection);
    assert_eq!(sessions.len(), 2);
    // Twice: the second is a no-op on a Session that is gone.
    sessions.detach(id, connection);
    sessions.detach(id, connection);
    assert!(sessions.contains(other));
    assert_eq!(sessions.len(), 1);
}

#[test]
fn concurrent_creates_and_detaches_leave_nothing_behind() {
    let sessions = Sessions::new();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..200 {
                    let (id, first) = create(&sessions, valid(&[("f", true)]));
                    let second = sessions.connect();
                    assert!(sessions.attach(id, second));
                    sessions.detach(id, first);
                    assert!(sessions.contains(id));
                    sessions.detach(id, second);
                    assert!(!sessions.contains(id));
                }
            });
        }
    });
    assert!(sessions.is_empty());
}

#[test]
fn a_refusal_echoes_a_bounded_amount_of_backend_input() {
    // A config with a great many unknown keys names the first alone (Story 3.7).
    let keys: Vec<(Value, Value)> = (0..100_000)
        .map(|i| (Value::from(format!("k{i}")), Value::Nil))
        .collect();
    let message = refusal(&payload(Value::Map(keys), named(&[])));
    assert_eq!(message, "`config.k0` is not a known setting");

    // A huge key, string or not, is cut short.
    let long = "k".repeat(1_000_000);
    let message = refusal(&payload(map(vec![(&long, Value::Nil)]), named(&[])));
    assert!(
        message.contains('…') && message.len() < 300,
        "{} bytes",
        message.len()
    );
    // A huge non-string key is not echoed at all.
    let huge = Value::Array(vec![Value::from(1); 1_000_000]);
    let message = refusal(&payload(
        Value::Map(vec![(huge.clone(), Value::Nil)]),
        named(&[]),
    ));
    assert_eq!(message, "`config` has a key that is not a string");
    // One at the `Init` payload's top level is echoed, bounded.
    let message = refusal(&Value::Map(vec![(huge, Value::Nil)]));
    assert!(
        message.contains('…') && message.len() < 300,
        "{} bytes",
        message.len()
    );

    // So is a huge duplicate name.
    let message = refusal(&payload(
        Value::Map(vec![]),
        named(&[(long.as_str(), true), (long.as_str(), true)]),
    ));
    assert!(
        message.contains('…') && message.len() < 300,
        "{} bytes",
        message.len()
    );
    assert!(
        message.contains("`registrations[0]` and `registrations[1]`"),
        "{message}"
    );
}

// --- Story 3.7: Config holds the execution limits ---

/// An `Init` whose `config` is `config`, registering `getOrder` with a blanket grant.
fn configured(config: Value) -> InitRequest {
    InitRequest::from_value(&payload(config, named(&[("getOrder", true)]))).unwrap()
}

#[test]
fn an_empty_config_sets_nothing() {
    let init = valid(&[]);
    assert_eq!(init.config().settings(), Settings::new());
    let sessions = Sessions::new();
    let (id, _) = create(&sessions, init);
    assert_eq!(sessions.settings(id), Some(Settings::new()));
}

#[test]
fn the_session_keeps_the_config_settings_and_reads_them_with_its_registrations() {
    let init = configured(map(vec![
        ("budget", map(vec![("rpc_calls", Value::from(2))])),
        ("authorization_timeout_ms", Value::from(100)),
    ]));
    let mut expected = Settings::new();
    expected.set(Setting::RpcCalls, 2).unwrap();
    expected.set(Setting::AuthorizationTimeoutMs, 100).unwrap();
    assert_eq!(init.config().settings(), expected);

    let sessions = Sessions::new();
    let (id, connection) = create(&sessions, init);
    assert_eq!(sessions.settings(id), Some(expected));
    let (registered, settings) = sessions.for_execution(id).unwrap();
    assert_eq!(settings, expected);
    let names: Vec<_> = registered.iter().map(|r| (r.name(), r.blanket())).collect();
    assert_eq!(names, [("getOrder", true)]);

    sessions.detach(id, connection);
    assert_eq!(sessions.settings(id), None);
    assert!(sessions.for_execution(id).is_none());
}

#[test]
fn a_config_out_of_range_or_mistyped_is_refused_naming_its_path() {
    let budget = |key: &str, value: Value| map(vec![("budget", map(vec![(key, value)]))]);
    let cases = [
        (
            budget("rpc_calls", Value::from(200_000)),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found 200000",
        ),
        (
            budget("rpc_calls", s("5")),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a string",
        ),
        (
            budget("rpc_calls", Value::F64(1.0)),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a float",
        ),
        (
            budget("rpc_calls", Value::from(-1)),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found -1",
        ),
        (
            budget("cpu", Value::from(1)),
            "`config.budget.cpu` is not a known setting",
        ),
        (
            map(vec![("speed", Value::from(1))]),
            "`config.speed` is not a known setting",
        ),
    ];
    for (config, expected) in cases {
        assert_eq!(
            refusal(&payload(config, named(&[]))),
            expected,
            "a refused Init creates no Session: nothing to create from"
        );
    }
}

// --- Story 3.8: replacing the Config at runtime ---

/// A `ConfigUpdate` payload carrying `config`.
fn update(config: Value) -> Value {
    map(vec![("config", config)])
}

/// The Config a valid `ConfigUpdate` payload decodes to.
fn updated(config: Value) -> Config {
    Config::from_update_payload(&update(config)).unwrap()
}

/// The refusal message for a `ConfigUpdate` payload, which must be refused.
fn update_refusal(payload: &Value) -> String {
    Config::from_update_payload(payload)
        .expect_err("the payload is refused")
        .to_string()
}

fn rpc_calls(value: u64) -> Value {
    map(vec![(
        "budget",
        map(vec![("rpc_calls", Value::from(value))]),
    )])
}

#[test]
fn an_update_replaces_the_config_whole_and_a_left_out_setting_is_back_to_its_default() {
    let sessions = Sessions::new();
    let (id, _) = create(
        &sessions,
        configured(map(vec![
            ("budget", map(vec![("rpc_calls", Value::from(1))])),
            ("argument_depth", Value::from(2)),
        ])),
    );
    assert!(sessions.update_config(id, updated(rpc_calls(3))));

    let settings = sessions.settings(id).unwrap();
    let mut expected = Settings::new();
    expected.set(Setting::RpcCalls, 3).unwrap();
    assert_eq!(settings, expected, "replace, never patch");
    assert_eq!(settings.get(Setting::ArgumentDepth), None);
    assert_eq!(settings.effective(Setting::ArgumentDepth), 12);

    // `config: {}` resets every setting.
    assert!(sessions.update_config(id, updated(Value::Map(vec![]))));
    assert_eq!(sessions.settings(id), Some(Settings::new()));
    // The registrations are untouched.
    assert_eq!(
        registrations(&sessions, id),
        Some(vec![("getOrder".to_owned(), true)])
    );
}

#[test]
fn an_update_of_no_session_changes_nothing() {
    let sessions = Sessions::new();
    let (id, connection) = create(&sessions, valid(&[]));
    sessions.detach(id, connection);
    assert!(!sessions.update_config(id, updated(rpc_calls(3))));
    assert!(sessions.is_empty());
}

#[test]
fn a_refused_update_leaves_the_stored_config_as_it_was() {
    let sessions = Sessions::new();
    let (id, _) = create(&sessions, configured(rpc_calls(1)));
    let before = sessions.settings(id);

    let cases = [
        (
            update(map(vec![(
                "budget",
                map(vec![("rpc_calls", Value::from(-1))]),
            )])),
            "`config.budget.rpc_calls` must be an integer from 0 to 100000; found -1",
        ),
        (Value::Nil, "the `ConfigUpdate` payload is missing `config`"),
        (
            Value::Map(vec![]),
            "the `ConfigUpdate` payload is missing `config`",
        ),
        (
            map(vec![("config", Value::Nil)]),
            "the `ConfigUpdate` payload is missing `config`",
        ),
        (update(Value::from(1)), "`config` is not a map"),
        (
            map(vec![("config", Value::Map(vec![])), ("x", Value::from(1))]),
            "the `ConfigUpdate` payload has an unknown key `x`",
        ),
        (
            map(vec![
                ("config", Value::Map(vec![])),
                ("config", Value::Map(vec![])),
            ]),
            "the `ConfigUpdate` payload repeats the key `config`",
        ),
        (
            Value::from(1),
            "the `ConfigUpdate` payload must be a map with `config`",
        ),
    ];
    for (payload, expected) in cases {
        assert_eq!(update_refusal(&payload), expected);
    }
    // Nothing refused ever reached the registry.
    assert_eq!(sessions.settings(id), before);
}

#[test]
fn an_update_is_seen_by_every_attached_connection() {
    let sessions = Sessions::new();
    let (id, first) = create(&sessions, configured(rpc_calls(1)));
    let second = sessions.connect();
    assert!(sessions.attach(id, second));

    // The update is made on the Session, and both attached Connections read it.
    assert!(sessions.update_config(id, updated(rpc_calls(3))));
    let mut expected = Settings::new();
    expected.set(Setting::RpcCalls, 3).unwrap();
    // One Session, one Config: whichever Connection dispatches next reads the new one.
    let (_, settings) = sessions.for_execution(id).unwrap();
    assert_eq!(settings, expected);

    // Still there after the updating Connection detaches.
    sessions.detach(id, second);
    let (_, settings) = sessions.for_execution(id).unwrap();
    assert_eq!(settings, expected);
    sessions.detach(id, first);
    assert!(sessions.is_empty());
}

// --- Story 4.3: the teardown hook ---

/// Records every Client ID the teardown hook is called with, in order.
type Torn = std::sync::Arc<std::sync::Mutex<Vec<ClientId>>>;

/// A registry whose teardown hook records the Client IDs it is called with.
fn recording(torn: &Torn) -> Sessions {
    let torn = std::sync::Arc::clone(torn);
    Sessions::new().with_teardown(move |id| torn.lock().unwrap().push(id))
}

#[test]
fn the_hook_is_called_once_with_the_client_id_when_the_last_connection_detaches() {
    let torn = Torn::default();
    let sessions = recording(&torn);
    let (id, first) = create(&sessions, valid(&[]));
    let second = sessions.connect();
    assert!(sessions.attach(id, second));

    sessions.detach(id, first);
    assert!(
        torn.lock().unwrap().is_empty(),
        "not while another connection is still attached"
    );
    assert!(sessions.contains(id));

    sessions.detach(id, second);
    assert_eq!(*torn.lock().unwrap(), vec![id]);
    assert!(!sessions.contains(id));
}

#[test]
fn the_hook_runs_after_the_session_is_gone_and_outside_any_lock() {
    use std::sync::{Mutex, OnceLock, Weak};

    // The hook reads the registry it is part of, so it would deadlock if the registry's locks were
    // still held, and it would see the Session if the Session were still there.
    let slot: std::sync::Arc<OnceLock<Weak<Sessions>>> = std::sync::Arc::default();
    let seen: std::sync::Arc<Mutex<Vec<(ClientId, bool, usize)>>> = std::sync::Arc::default();
    let hook_slot = std::sync::Arc::clone(&slot);
    let hook_seen = std::sync::Arc::clone(&seen);
    let sessions = std::sync::Arc::new(Sessions::new().with_teardown(move |id| {
        let registry = hook_slot
            .get()
            .unwrap()
            .upgrade()
            .expect("the registry is alive");
        hook_seen
            .lock()
            .unwrap()
            .push((id, registry.contains(id), registry.len()));
    }));
    slot.set(std::sync::Arc::downgrade(&sessions)).unwrap();

    let (id, connection) = create(&sessions, valid(&[("getOrder", true)]));
    sessions.detach(id, connection);
    assert_eq!(*seen.lock().unwrap(), vec![(id, false, 0)]);
}

#[test]
fn a_registry_without_a_hook_tears_down_quietly() {
    let sessions = Sessions::new();
    let (id, connection) = create(&sessions, valid(&[]));
    sessions.detach(id, connection);
    assert!(sessions.is_empty());
}

#[test]
fn the_hook_is_called_for_each_session_that_ends_and_only_those() {
    let torn = Torn::default();
    let sessions = recording(&torn);
    let (a, first) = create(&sessions, valid(&[]));
    let (b, second) = create(&sessions, valid(&[]));
    sessions.detach(a, first);
    assert_eq!(*torn.lock().unwrap(), vec![a]);
    sessions.detach(b, second);
    assert_eq!(*torn.lock().unwrap(), vec![a, b]);
}

#[test]
fn a_detach_of_an_unknown_session_does_not_call_the_hook() {
    let torn = Torn::default();
    let sessions = recording(&torn);
    let connection = sessions.connect();
    sessions.detach(ClientId::from_bytes([9; 16]), connection);
    assert!(torn.lock().unwrap().is_empty());
}

// --- Story 5.5: the reconnect credential issued at init ---

/// A Session created with its Connection attached, and the reconnect secret issued with it.
fn created(sessions: &Sessions) -> (ClientId, ReconnectSecret) {
    let connection = sessions.connect();
    sessions.create(valid(&[]), connection)
}

/// Whether `text` is exactly 64 lowercase hex characters.
fn is_lower_hex_64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn a_created_session_issues_a_reconnect_secret_of_64_lowercase_hex_characters() {
    let sessions = Sessions::new();
    let (_, secret) = created(&sessions);
    let text = secret.to_hex();
    assert!(is_lower_hex_64(&text), "{text:?}");
    // The text form is the secret itself: parsing it back gives the same text.
    let parsed = ReconnectSecret::from_hex(&text).expect("its own text parses");
    assert_eq!(parsed.to_hex(), text);
}

#[test]
fn two_sessions_get_different_client_ids_and_different_secrets() {
    let sessions = Sessions::new();
    let (first_id, first) = created(&sessions);
    let (second_id, second) = created(&sessions);
    assert_ne!(first_id, second_id);
    assert_ne!(first.to_hex(), second.to_hex());
}

#[test]
fn verify_reconnect_accepts_exactly_the_issued_secret_for_its_client_id() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    // A presented secret arrives as text, so the parsed form must verify as well.
    let presented = ReconnectSecret::from_hex(&secret.to_hex()).expect("parses");
    assert!(sessions.verify_reconnect(id, &secret));
    assert!(sessions.verify_reconnect(id, &presented));
}

#[test]
fn verify_reconnect_refuses_a_secret_that_differs_in_one_character() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let text = secret.to_hex();
    let other = if text.ends_with('0') { '1' } else { '0' };
    let altered = format!("{}{other}", &text[..63]);
    let altered = ReconnectSecret::from_hex(&altered).expect("still 64 lowercase hex");
    assert!(!sessions.verify_reconnect(id, &altered));
    assert!(sessions.verify_reconnect(id, &secret));
}

#[test]
fn verify_reconnect_refuses_another_sessions_secret() {
    let sessions = Sessions::new();
    let (first_id, first) = created(&sessions);
    let (second_id, second) = created(&sessions);
    assert!(!sessions.verify_reconnect(first_id, &second));
    assert!(!sessions.verify_reconnect(second_id, &first));
    assert!(sessions.verify_reconnect(first_id, &first));
    assert!(sessions.verify_reconnect(second_id, &second));
}

#[test]
fn verify_reconnect_refuses_an_unknown_client_id_for_any_secret() {
    let sessions = Sessions::new();
    let (_, secret) = created(&sessions);
    assert!(!sessions.verify_reconnect(ClientId::from_bytes([9; 16]), &secret));
}

#[test]
fn a_torn_down_session_no_longer_verifies_its_secret() {
    let sessions = Sessions::new();
    let connection = sessions.connect();
    let (id, secret) = sessions.create(valid(&[]), connection);
    assert!(sessions.verify_reconnect(id, &secret));
    sessions.detach(id, connection);
    assert!(!sessions.verify_reconnect(id, &secret));
}

#[test]
fn a_secret_parses_only_as_exactly_64_lowercase_hex_characters() {
    let good = "0123456789abcdef".repeat(4);
    let parsed = ReconnectSecret::from_hex(&good).expect("64 lowercase hex parses");
    assert_eq!(parsed.to_hex(), good);
    let refused: Vec<(String, &str)> = vec![
        (good.to_uppercase(), "uppercase"),
        (good[..63].to_owned(), "63 characters"),
        (format!("{good}0"), "65 characters"),
        (format!("{}g", &good[..63]), "a non-hex character"),
        (format!("{} ", &good[..63]), "a space"),
        ("é".repeat(32), "64 bytes that are not hex"),
        (String::new(), "empty"),
    ];
    for (text, why) in refused {
        assert!(
            ReconnectSecret::from_hex(&text).is_none(),
            "refused ({why}): {text:?}"
        );
    }
}

#[test]
fn the_secret_debug_and_the_sessions_debug_never_show_the_secret() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let text = secret.to_hex();
    let shown = format!("{secret:?}");
    assert!(!shown.contains(&text), "{shown}");
    assert!(!shown.contains(&text[..16]), "{shown}");
    let registry = format!("{sessions:?}");
    assert!(!registry.contains(&text), "{registry}");
    assert!(!registry.contains(&text[..16]), "{registry}");
    // Formatting changes nothing: the secret still verifies.
    assert!(sessions.verify_reconnect(id, &secret));
}

// --- Story 5.6: reconnecting a Connection to a live Session ---

/// The `ReconnectRequest` refusal message for `value`, which must be refused.
fn reconnect_refusal(value: &Value) -> String {
    match ReconnectRequest::from_value(value) {
        Ok(_) => panic!("the payload is accepted: {value:?}"),
        Err(error) => error.to_string(),
    }
}

/// A well-formed `Reconnect` payload: `client_id` then `reconnect_secret`, as text.
fn reconnect_payload(client_id: &str, secret: &str) -> Value {
    map(vec![
        ("client_id", s(client_id)),
        ("reconnect_secret", s(secret)),
    ])
}

/// `secret` with its last character changed, still 64 lowercase hex characters.
fn altered(secret: &ReconnectSecret) -> ReconnectSecret {
    let text = secret.to_hex();
    let other = if text.ends_with('0') { '1' } else { '0' };
    ReconnectSecret::from_hex(&format!("{}{other}", &text[..63])).expect("still 64 lowercase hex")
}

#[test]
fn reconnect_attaches_a_second_connection_with_the_issued_secret() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let second = sessions.connect();
    assert!(sessions.reconnect(id, &secret, second));
    assert_eq!(sessions.attached(id), Some(2));
    assert!(sessions.contains(id));
}

#[test]
fn reconnect_refuses_a_wrong_secret_and_changes_no_attachment() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let before = registrations(&sessions, id);
    let stranger = sessions.connect();
    assert!(!sessions.reconnect(id, &altered(&secret), stranger));
    assert_eq!(
        sessions.attached(id),
        Some(1),
        "the attachment count is unchanged"
    );
    assert_eq!(registrations(&sessions, id), before);
    // The refused connection is not attached: detaching it changes nothing.
    sessions.detach(id, stranger);
    assert_eq!(sessions.attached(id), Some(1));
    // The correct secret still works afterwards: a refusal is not a lockout.
    let retry = sessions.connect();
    assert!(sessions.reconnect(id, &secret, retry));
    assert_eq!(sessions.attached(id), Some(2));
}

#[test]
fn reconnect_refuses_another_sessions_secret() {
    let sessions = Sessions::new();
    let (first_id, _first) = created(&sessions);
    let (_second_id, second) = created(&sessions);
    let stranger = sessions.connect();
    assert!(!sessions.reconnect(first_id, &second, stranger));
    assert_eq!(sessions.attached(first_id), Some(1));
}

#[test]
fn reconnect_refuses_an_unknown_client_id_and_creates_nothing() {
    let sessions = Sessions::new();
    let (_, secret) = created(&sessions);
    let stranger = sessions.connect();
    let unknown = ClientId::from_bytes([9; 16]);
    assert!(!sessions.reconnect(unknown, &secret, stranger));
    assert!(!sessions.contains(unknown));
    assert_eq!(sessions.len(), 1);
}

#[test]
fn reconnect_refuses_a_torn_down_session_like_an_unknown_one() {
    let sessions = Sessions::new();
    let connection = sessions.connect();
    let (id, secret) = sessions.create(valid(&[]), connection);
    sessions.detach(id, connection);
    let stranger = sessions.connect();
    assert!(!sessions.reconnect(id, &secret, stranger));
    assert!(!sessions.contains(id));
    assert!(sessions.is_empty());
}

#[test]
fn a_reconnected_session_outlives_the_connection_that_created_it() {
    let sessions = Sessions::new();
    let creator = sessions.connect();
    let (id, secret) = sessions.create(valid(&[("getUser", true)]), creator);
    let joiner = sessions.connect();
    assert!(sessions.reconnect(id, &secret, joiner));
    sessions.detach(id, creator);
    assert!(sessions.contains(id), "the reconnected Connection keeps it");
    assert_eq!(sessions.attached(id), Some(1));
    assert_eq!(
        registrations(&sessions, id),
        Some(vec![("getUser".to_owned(), true)])
    );
    sessions.detach(id, joiner);
    assert!(!sessions.contains(id));
}

#[test]
fn a_reconnect_racing_the_last_detach_either_tears_down_first_or_attaches() {
    // Each round: the only other Connection detaches while a reconnect is in flight. Either the
    // detach tears the Session down first (the reconnect is denied) or the reconnect lands
    // first (the Session survives the detach with the new Connection). Never a mix of both.
    let sessions = Sessions::new();
    for _ in 0..500 {
        let creator = sessions.connect();
        let (id, secret) = sessions.create(valid(&[]), creator);
        let joiner = sessions.connect();
        let barrier = Barrier::new(2);
        let attached = std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                sessions.detach(id, creator);
            });
            barrier.wait();
            sessions.reconnect(id, &secret, joiner)
        });
        if attached {
            assert!(sessions.contains(id), "the reconnect won the race");
            assert_eq!(sessions.attached(id), Some(1));
            sessions.detach(id, joiner);
        } else {
            assert!(!sessions.contains(id), "the detach won the race");
            assert_eq!(sessions.attached(id), None);
        }
        assert!(!sessions.contains(id));
    }
    assert!(sessions.is_empty());
}

#[test]
fn reconnect_request_accepts_exactly_a_client_id_and_a_secret() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let payload = reconnect_payload(&id.to_string(), &secret.to_hex());
    assert!(ReconnectRequest::from_value(&payload).is_ok());
}

#[test]
fn reconnect_request_names_a_carried_config_or_registrations_and_says_what_resumes() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let id_text = id.to_string();
    let secret_text = secret.to_hex();
    for key in ["config", "registrations", "stray"] {
        let mut fields = vec![
            ("client_id", s(&id_text)),
            ("reconnect_secret", s(&secret_text)),
        ];
        fields.push((key, Value::Map(vec![])));
        let message = reconnect_refusal(&map(fields));
        assert!(message.contains(&format!("`{key}`")), "{message}");
        assert!(!message.contains(&id_text), "{message}");
        assert!(!message.contains(&secret_text), "{message}");
    }
    let message = reconnect_refusal(&map(vec![
        ("client_id", s(&id_text)),
        ("reconnect_secret", s(&secret_text)),
        ("config", Value::Map(vec![])),
    ]));
    assert!(message.contains("ConfigUpdate"), "{message}");
}

#[test]
fn reconnect_request_names_missing_parts_together() {
    let text = reconnect_refusal(&Value::Nil);
    assert!(
        text.contains("client_id") && text.contains("reconnect_secret"),
        "{text}"
    );
    let text = reconnect_refusal(&map(vec![("client_id", s(&"0".repeat(32)))]));
    assert!(text.contains("reconnect_secret"), "{text}");
    assert!(!text.contains("`client_id`"), "{text}");
    let text = reconnect_refusal(&map(vec![("reconnect_secret", s(&"0".repeat(64)))]));
    assert!(text.contains("client_id"), "{text}");
    assert!(!text.contains("`reconnect_secret`"), "{text}");
}

#[test]
fn reconnect_request_refuses_a_non_map_a_repeated_key_and_malformed_values() {
    let id = "0123456789abcdef0123456789abcdef";
    let secret = "0123456789abcdef".repeat(4);
    let refused = [
        Value::from(7u64),
        Value::Array(vec![]),
        map(vec![
            ("client_id", s(id)),
            ("client_id", s(id)),
            ("reconnect_secret", s(&secret)),
        ]),
        // Uppercase hex, a short or long ID or secret, a non-string, each a malformed value.
        reconnect_payload(&id.to_uppercase(), &secret),
        reconnect_payload(&id[..31], &secret),
        reconnect_payload(id, &secret.to_uppercase()),
        reconnect_payload(id, &format!("{secret}0")),
        map(vec![
            ("client_id", s(id)),
            ("reconnect_secret", Value::from(1u64)),
        ]),
        map(vec![
            ("client_id", Value::from(1u64)),
            ("reconnect_secret", s(&secret)),
        ]),
    ];
    for payload in &refused {
        let message = reconnect_refusal(payload);
        assert!(!message.contains(id), "{message}");
        assert!(!message.contains(&secret), "{message}");
        assert!(!message.contains(&id.to_uppercase()), "{message}");
        assert!(!message.contains(&secret.to_uppercase()), "{message}");
    }
}

#[test]
fn reconnect_refusals_never_echo_the_presented_values() {
    let sessions = Sessions::new();
    let (id, secret) = created(&sessions);
    let text = secret.to_hex();
    let refused = [
        reconnect_payload(&id.to_string().to_uppercase(), &text),
        reconnect_payload(&id.to_string(), &format!("{}G", &text[..63])),
        map(vec![
            ("client_id", s(&id.to_string())),
            ("reconnect_secret", s(&text)),
            ("config", s(&text)),
        ]),
        // A credential sent as a key — a wrongly serialized struct — is not echoed either.
        map(vec![
            ("client_id", s(&id.to_string())),
            ("reconnect_secret", s(&text)),
            (text.as_str(), Value::Boolean(true)),
        ]),
        map(vec![(id.to_string().as_str(), s(&text))]),
    ];
    for payload in &refused {
        let message = reconnect_refusal(payload);
        assert!(!message.contains(&text[..16]), "{message}");
        assert!(!message.contains(&id.to_string()), "{message}");
    }
}

// --- Story 5.7: several Connections attached to one Session ---

/// Three Connections on one Session: the creator, then two that reconnect with the issued secret.
fn three_attached(sessions: &Sessions) -> (ClientId, [ConnectionId; 3]) {
    let creator = sessions.connect();
    let (id, secret) = sessions.create(valid(&[("getUser", true), ("ask", false)]), creator);
    let second = sessions.connect();
    assert!(sessions.reconnect(id, &secret, second));
    let third = sessions.connect();
    assert!(sessions.reconnect(id, &secret, third));
    (id, [creator, second, third])
}

#[test]
fn three_connections_on_one_session_count_up_and_down_in_any_order() {
    let sessions = Sessions::new();
    let (id, [creator, second, third]) = three_attached(&sessions);
    assert_eq!(sessions.attached(id), Some(3));
    assert_eq!(sessions.len(), 1, "three Connections are one Session");

    // Detaching the middle one leaves the other two.
    sessions.detach(id, second);
    assert_eq!(sessions.attached(id), Some(2));
    assert!(sessions.contains(id));

    // Detaching the creator, which created the Session, leaves the reconnected one.
    sessions.detach(id, creator);
    assert_eq!(sessions.attached(id), Some(1));
    assert!(sessions.contains(id));

    sessions.detach(id, third);
    assert!(!sessions.contains(id));
    assert_eq!(sessions.attached(id), None);
    assert!(sessions.is_empty());
}

#[test]
fn a_session_stays_live_until_its_last_connection_leaves_in_every_order() {
    // Each permutation of detaching three Connections: the Session survives every detach but the last.
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for order in orders {
        let sessions = Sessions::new();
        let (id, connections) = three_attached(&sessions);
        for (step, index) in order.iter().enumerate() {
            sessions.detach(id, connections[*index]);
            let remaining = 3 - step - 1;
            if remaining == 0 {
                assert!(
                    !sessions.contains(id),
                    "order {order:?}: last detach tears down"
                );
            } else {
                assert!(
                    sessions.contains(id),
                    "order {order:?}: Session outlives detach {step}"
                );
                assert_eq!(sessions.attached(id), Some(remaining));
            }
        }
        assert!(sessions.is_empty());
    }
}

#[test]
fn the_state_every_connection_reads_is_the_sessions_and_outlives_its_creator() {
    let sessions = Sessions::new();
    let (id, [creator, second, _third]) = three_attached(&sessions);
    // `for_execution` takes the Client ID alone, so no Connection can hold a view of its own: what
    // one changes, every other reads. The creator leaving takes nothing of the Session's with it,
    // and an update made after it left is what the remaining Connection reads.
    sessions.detach(id, creator);
    assert!(sessions.update_config(id, updated(rpc_calls(0))));
    sessions.detach(id, second);

    let (registered, settings) = sessions.for_execution(id).expect("the Session is live");
    assert_eq!(
        registered
            .iter()
            .map(|r| (r.name().to_owned(), r.blanket()))
            .collect::<Vec<_>>(),
        [("getUser".to_owned(), true), ("ask".to_owned(), false)],
        "the registrations made at `Init` serve the reconnected Connection"
    );
    let mut expected = Settings::new();
    expected.set(Setting::RpcCalls, 0).unwrap();
    assert_eq!(settings, expected, "the update made after the creator left");
}

#[test]
fn a_connection_of_another_session_never_attaches_here() {
    let sessions = Sessions::new();
    let (id, _connections) = three_attached(&sessions);
    let (other, other_secret) = {
        let connection = sessions.connect();
        sessions.create(valid(&[]), connection)
    };
    let stranger = sessions.connect();
    assert!(!sessions.reconnect(id, &other_secret, stranger));
    assert_eq!(sessions.attached(id), Some(3));
    assert_eq!(sessions.attached(other), Some(1));
}

// --- Story 5.8: a Session outlives its last Connection by the TTL ---

/// The TTL these tests configure. Nothing sleeps: the registry tests call `expire` themselves.
const TTL: Duration = Duration::from_secs(300);

/// A registry with [`TTL`] whose teardown hook records the Client IDs it is called with.
fn expiring(torn: &Torn) -> Sessions {
    let torn = std::sync::Arc::clone(torn);
    Sessions::new()
        .with_ttl(TTL)
        .with_teardown(move |id| torn.lock().unwrap().push(id))
}

#[test]
fn a_detach_with_another_connection_still_attached_starts_no_countdown() {
    // Matrix: busy Session, and a non-last Connection leaving.
    let torn = Torn::default();
    let sessions = expiring(&torn);
    let (id, first) = create(&sessions, valid(&[("getUser", true)]));
    let second = sessions.connect();
    assert!(sessions.attach(id, second));

    assert!(
        sessions.detach(id, first).is_none(),
        "not the last Connection"
    );
    assert!(sessions.contains(id));
    assert_eq!(sessions.attached(id), Some(1));
    assert_eq!(sessions.expiring(id), Some(false), "no countdown ever runs");
    assert!(torn.lock().unwrap().is_empty());

    let expiry = sessions
        .detach(id, second)
        .expect("the last Connection leaves");
    assert_eq!(expiry.client_id(), id);
}

#[test]
fn the_last_detach_returns_an_expiry_and_starts_the_countdown_without_tearing_down() {
    let torn = Torn::default();
    let sessions = expiring(&torn);
    let (id, connection) = create(&sessions, valid(&[("getUser", true)]));

    let expiry = sessions
        .detach(id, connection)
        .expect("the last Connection leaves");
    assert_eq!(expiry.client_id(), id);
    assert_eq!(expiry.ttl(), TTL);
    assert!(sessions.contains(id), "the Session lives until the TTL");
    assert_eq!(sessions.expiring(id), Some(true));
    assert_eq!(sessions.attached(id), Some(0));
    assert!(torn.lock().unwrap().is_empty(), "no teardown yet");
    assert_eq!(
        registrations(&sessions, id),
        Some(vec![("getUser".to_owned(), true)]),
        "its state is kept for a reconnect"
    );
}

#[test]
fn expire_tears_the_session_down_once_through_the_hook_and_a_second_expire_is_false() {
    let torn = Torn::default();
    let sessions = expiring(&torn);
    let (id, connection) = create(&sessions, valid(&[]));
    let expiry = sessions.detach(id, connection).expect("an expiry");

    assert!(sessions.expire(&expiry), "true: torn down now");
    assert!(!sessions.contains(id));
    assert_eq!(sessions.expiring(id), None);
    assert_eq!(sessions.attached(id), None);
    assert_eq!(registrations(&sessions, id), None);
    assert_eq!(*torn.lock().unwrap(), vec![id], "the hook is called once");

    assert!(!sessions.expire(&expiry), "a second expire does nothing");
    assert_eq!(*torn.lock().unwrap(), vec![id]);
}

#[test]
fn a_reconnect_during_the_countdown_attaches_stops_it_and_keeps_the_state() {
    let torn = Torn::default();
    let sessions = expiring(&torn);
    let creator = sessions.connect();
    let (id, secret) = sessions.create(configured(rpc_calls(7)), creator);
    let settings_before = sessions.settings(id).unwrap();
    let registrations_before = registrations(&sessions, id);
    let expiry = sessions.detach(id, creator).expect("an expiry");

    let joiner = sessions.connect();
    assert!(sessions.reconnect(id, &secret, joiner), "a valid reconnect");
    assert_eq!(sessions.expiring(id), Some(false), "the countdown stopped");
    assert_eq!(sessions.attached(id), Some(1));
    assert_eq!(sessions.settings(id), Some(settings_before), "same Config");
    assert_eq!(registrations(&sessions, id), registrations_before);
    assert!(
        sessions.verify_reconnect(id, &secret),
        "same reconnect secret"
    );

    assert!(!sessions.expire(&expiry), "the old countdown does nothing");
    assert!(sessions.contains(id));
    assert!(torn.lock().unwrap().is_empty());
    sessions.detach(id, joiner);
}

#[test]
fn a_stale_expiry_from_an_earlier_orphaning_does_nothing_and_the_newer_one_expires() {
    let sessions = expiring(&Torn::default());
    let creator = sessions.connect();
    let (id, secret) = sessions.create(valid(&[]), creator);
    let first = sessions.detach(id, creator).expect("the first orphaning");

    let joiner = sessions.connect();
    assert!(sessions.reconnect(id, &secret, joiner));
    let second = sessions.detach(id, joiner).expect("the second orphaning");

    assert!(!sessions.expire(&first), "a stale generation is refused");
    assert!(sessions.contains(id), "Session live");
    assert_eq!(
        sessions.expiring(id),
        Some(true),
        "the newer countdown runs"
    );
    assert!(sessions.expire(&second), "the current generation expires");
    assert!(!sessions.contains(id));
}

#[test]
fn a_wrong_secret_during_the_countdown_is_refused_and_the_countdown_keeps_running() {
    let sessions = expiring(&Torn::default());
    let creator = sessions.connect();
    let (id, secret) = sessions.create(valid(&[]), creator);
    let expiry = sessions.detach(id, creator).expect("an expiry");

    let stranger = sessions.connect();
    assert!(!sessions.reconnect(id, &altered(&secret), stranger));
    assert_eq!(
        sessions.expiring(id),
        Some(true),
        "the countdown is untouched"
    );
    assert_eq!(sessions.attached(id), Some(0));
    assert!(sessions.expire(&expiry), "the Session still expires");
    assert!(!sessions.contains(id));
}

#[test]
fn a_session_expired_by_its_ttl_is_refused_a_reconnect_exactly_like_an_unknown_one() {
    let sessions = expiring(&Torn::default());
    let creator = sessions.connect();
    let (id, secret) = sessions.create(valid(&[]), creator);
    let expiry = sessions.detach(id, creator).expect("an expiry");
    assert!(sessions.expire(&expiry));

    let stranger = sessions.connect();
    assert!(!sessions.reconnect(id, &secret, stranger));
    assert!(!sessions.verify_reconnect(id, &secret));
    assert!(!sessions.reconnect(ClientId::from_bytes([9; 16]), &secret, stranger));
    assert!(sessions.is_empty());
}

#[test]
fn a_zero_ttl_tears_down_on_the_last_detach_at_once_as_before() {
    for sessions in [Sessions::new(), Sessions::new().with_ttl(Duration::ZERO)] {
        let (id, connection) = create(&sessions, valid(&[]));
        assert!(
            sessions.detach(id, connection).is_none(),
            "no countdown, no Expiry"
        );
        assert!(!sessions.contains(id));
        assert_eq!(sessions.expiring(id), None);
    }
}

#[test]
fn with_ttl_and_with_teardown_chain_in_either_order() {
    let torn = Torn::default();
    let hook = std::sync::Arc::clone(&torn);
    let sessions = Sessions::new()
        .with_teardown(move |id| hook.lock().unwrap().push(id))
        .with_ttl(TTL);
    let (id, connection) = create(&sessions, valid(&[]));
    let expiry = sessions.detach(id, connection).expect("an expiry");
    assert_eq!(expiry.ttl(), TTL);
    assert!(torn.lock().unwrap().is_empty());
    assert!(sessions.expire(&expiry));
    assert_eq!(*torn.lock().unwrap(), vec![id]);
}

#[test]
fn concurrent_reconnects_and_expiries_never_tear_down_an_attached_session() {
    // Each round: the orphaned Session's countdown fires while a reconnect is in flight. Exactly
    // one lands: the reconnect (the Session survives, attached) or the expiry (the reconnect is
    // refused). Never both, never neither, and never a torn-down Session that is attached.
    let sessions = expiring(&Torn::default());
    for _ in 0..300 {
        let creator = sessions.connect();
        let (id, secret) = sessions.create(valid(&[]), creator);
        let expiry: Expiry = sessions.detach(id, creator).expect("an expiry");
        let joiner = sessions.connect();
        let barrier = Barrier::new(2);
        let (attached, expired) = std::thread::scope(|scope| {
            let expirer = scope.spawn(|| {
                barrier.wait();
                sessions.expire(&expiry)
            });
            barrier.wait();
            let attached = sessions.reconnect(id, &secret, joiner);
            (attached, expirer.join().unwrap())
        });
        assert!(
            attached != expired,
            "exactly one of the reconnect and the expiry lands"
        );
        if attached {
            assert!(sessions.contains(id), "the reconnect won: Session live");
            assert_eq!(sessions.attached(id), Some(1));
            let last = sessions.detach(id, joiner).expect("an expiry");
            assert!(sessions.expire(&last));
        }
        assert!(!sessions.contains(id));
    }
    assert!(sessions.is_empty());
}

#[test]
fn an_attach_during_the_countdown_stops_it_too() {
    let torn = Torn::default();
    let sessions = expiring(&torn);
    let (id, creator) = create(&sessions, valid(&[]));
    let expiry = sessions.detach(id, creator).expect("an expiry");

    let joiner = sessions.connect();
    assert!(sessions.attach(id, joiner));
    assert_eq!(sessions.expiring(id), Some(false), "the countdown stopped");
    assert!(!sessions.expire(&expiry), "the old countdown does nothing");
    assert!(sessions.contains(id));
    assert!(torn.lock().unwrap().is_empty());

    let last = sessions.detach(id, joiner).expect("a new countdown");
    assert!(sessions.expire(&last));
}

#[test]
fn concurrent_detaches_and_stale_expiries_never_tear_down_by_the_old_countdown() {
    // Each round: a stale countdown (its Session since reconnected) fires while the reconnected
    // Connection detaches, orphaning the Session again. The stale one never lands, whichever runs
    // first; only the new orphaning's countdown tears the Session down, exactly once.
    let torn = Torn::default();
    let sessions = expiring(&torn);
    for round in 0..300 {
        let creator = sessions.connect();
        let (id, secret) = sessions.create(valid(&[]), creator);
        let stale: Expiry = sessions.detach(id, creator).expect("the first orphaning");
        let joiner = sessions.connect();
        assert!(sessions.reconnect(id, &secret, joiner));
        let barrier = Barrier::new(2);
        let (fresh, expired) = std::thread::scope(|scope| {
            let expirer = scope.spawn(|| {
                barrier.wait();
                sessions.expire(&stale)
            });
            barrier.wait();
            let fresh = sessions.detach(id, joiner);
            (fresh, expirer.join().unwrap())
        });
        assert!(!expired, "a stale countdown never tears down");
        assert!(
            sessions.contains(id),
            "the Session waits on its new countdown"
        );
        assert_eq!(torn.lock().unwrap().len(), round);
        assert!(sessions.expire(&fresh.expect("the second orphaning")));
        assert_eq!(torn.lock().unwrap().len(), round + 1);
        assert!(!sessions.contains(id));
    }
    assert!(sessions.is_empty());
}
