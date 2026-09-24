//! Stories 2.4 + 2.5: decoding an `Init` payload, and the Session registry it creates into.

use std::collections::HashSet;

use hexput_port::Value;
use hexput_session::{ClientId, ConnectionId, InitRequest, Sessions};

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
            "`config` accepts no keys yet; found `timeout`",
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

// --- the registry ---

/// A Connection opening and completing init: what `hexput-connection` does, in that order.
fn create(sessions: &Sessions, init: InitRequest) -> (ClientId, ConnectionId) {
    let connection = sessions.connect();
    (sessions.create(init, connection), connection)
}

#[test]
fn a_connection_id_is_issued_before_init_and_becomes_its_attachment() {
    let sessions = Sessions::new();
    let first = sessions.connect();
    let second = sessions.connect();
    assert_ne!(first, second);
    // Connecting creates no Session.
    assert!(sessions.is_empty());
    let id = sessions.create(valid(&[]), second);
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
    // A config with a great many keys lists a few and counts the rest.
    let keys: Vec<(Value, Value)> = (0..100_000).map(|i| (Value::from(i), Value::Nil)).collect();
    let message = refusal(&payload(Value::Map(keys), named(&[])));
    assert!(message.contains("and 99997 more"), "{message}");
    assert!(message.len() < 300, "{} bytes", message.len());

    // A huge key, string or not, is cut short.
    let long = "k".repeat(1_000_000);
    let message = refusal(&payload(map(vec![(&long, Value::Nil)]), named(&[])));
    assert!(
        message.contains('…') && message.len() < 300,
        "{} bytes",
        message.len()
    );
    let huge = Value::Array(vec![Value::from(1); 1_000_000]);
    let message = refusal(&payload(Value::Map(vec![(huge, Value::Nil)]), named(&[])));
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
