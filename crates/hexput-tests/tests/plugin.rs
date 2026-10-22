//! Story 6.2: registering a Plugin's source and holding it per Client ID, driven directly through
//! `hexput_plugin::register` with a stub (or a real Session's) reservation.
//!
//! The wire rows and the Connection's routing live in `connection.rs`; the Session's name set in
//! `session.rs`. Nothing here touches a socket.

use std::cell::Cell;

use hexput_plugin::{Plugins, Reservation, register};
use hexput_port::{ErrorBody, ProtocolCode, Value};
use hexput_session::ClientId;
use hexput_shared::ids::PluginId;

fn s(text: &str) -> Value {
    Value::from(text)
}

fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}

/// A `{source}` payload.
fn payload(source: &str) -> Value {
    map(vec![("source", s(source))])
}

/// A Client ID with `n` as its low byte, without a Session behind it.
fn client(n: u8) -> ClientId {
    format!("{:032x}", n).parse().unwrap()
}

/// A valid Plugin named `name` with one `@Event` handler.
fn named(name: &str) -> String {
    format!("plugin {{ name = \"{name}\" }}\n@Event(Ping) fn h(p) {{ return 1; }}")
}

/// A reservation that always succeeds with `id`.
fn reserving(id: u64) -> impl FnOnce(&str) -> Reservation {
    move |_| Reservation::Reserved(PluginId::new(id))
}

/// The refusal `result` must be, as its `ErrorBody`.
fn refused(result: Result<Value, Box<ErrorBody>>) -> Box<ErrorBody> {
    match result {
        Ok(value) => panic!("registration accepted: {value:?}"),
        Err(body) => body,
    }
}

// --- the reply and the holding ---

#[test]
fn a_registration_answers_its_name_and_holds_the_plugin_under_its_id() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let reply = register(&payload(&named("x")), c, &plugins, reserving(1)).unwrap();
    assert_eq!(reply, map(vec![("name", s("x"))]));
    let plugin = plugins
        .get(c, PluginId::new(1))
        .expect("the Plugin is held under its id");
    assert_eq!(plugin.name(), "x");
    assert_eq!(plugin.id(), PluginId::new(1));
    assert_eq!(plugin.program().handlers.len(), 1);
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)]);
    assert_eq!(plugins.len(), 1);
}

#[test]
fn several_plugins_are_each_distinct_with_their_own_id_and_program() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    for (n, name) in ["a", "b", "c"].into_iter().enumerate() {
        let id = n as u64 + 1;
        let reply = register(&payload(&named(name)), c, &plugins, reserving(id)).unwrap();
        assert_eq!(reply, map(vec![("name", s(name))]));
    }
    assert_eq!(
        plugins.ids(c),
        vec![PluginId::new(1), PluginId::new(2), PluginId::new(3)]
    );
    assert_eq!(plugins.len(), 3);
    let a = plugins.get(c, PluginId::new(1)).unwrap();
    let b = plugins.get(c, PluginId::new(2)).unwrap();
    let third = plugins.get(c, PluginId::new(3)).unwrap();
    assert_eq!(
        [a.name(), b.name(), third.name()],
        ["a", "b", "c"],
        "each keeps its own name"
    );
    assert_ne!(
        a.program(),
        b.program(),
        "each keeps its own parsed program"
    );
    assert_ne!(b.program(), third.program());
}

#[test]
fn plugins_are_held_per_client_id_and_never_seen_across_clients() {
    let plugins = Plugins::new();
    let (first, second) = (client(1), client(2));
    plugins.open(first);
    plugins.open(second);
    register(&payload(&named("x")), first, &plugins, reserving(1)).unwrap();
    register(&payload(&named("x")), second, &plugins, reserving(2)).unwrap();
    assert_eq!(plugins.ids(first), vec![PluginId::new(1)]);
    assert_eq!(plugins.ids(second), vec![PluginId::new(2)]);
    assert!(plugins.get(second, PluginId::new(1)).is_none());
    assert!(plugins.get(first, PluginId::new(2)).is_none());
    assert_eq!(plugins.len(), 2);
}

#[test]
fn closing_a_client_releases_its_plugins_and_no_other_clients() {
    let plugins = Plugins::new();
    let (gone, kept) = (client(1), client(2));
    plugins.open(gone);
    plugins.open(kept);
    register(&payload(&named("a")), gone, &plugins, reserving(1)).unwrap();
    register(&payload(&named("b")), kept, &plugins, reserving(2)).unwrap();
    plugins.close(gone);
    assert!(plugins.ids(gone).is_empty());
    assert!(plugins.get(gone, PluginId::new(1)).is_none());
    assert_eq!(plugins.ids(kept), vec![PluginId::new(2)]);
    assert_eq!(plugins.len(), 1);
}

// --- refusals: the wire's `protocol.invalid_payload`, naming the key ---

#[test]
fn a_payload_that_is_not_exactly_source_is_refused_naming_the_key_and_stores_nothing() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = named("x");
    let cases: Vec<(Value, Option<&str>)> = vec![
        // nil reads as missing, so the message names `source`.
        (Value::Nil, Some("source")),
        (map(vec![]), Some("source")),
        (map(vec![("source", Value::from(1))]), Some("source")),
        (
            map(vec![
                ("source", s(&source)),
                ("events", Value::Array(vec![])),
            ]),
            Some("events"),
        ),
        (
            Value::Map(vec![(s("source"), s(&source)), (s("source"), s(&source))]),
            Some("source"),
        ),
        // A `source` whose string is not valid UTF-8 says so, naming `source`.
        (
            map(vec![(
                "source",
                // A two-byte MessagePack str (fixstr 0xa2) whose bytes are not UTF-8.
                rmpv::decode::read_value(&mut &[0xa2_u8, 0xff, 0xfe][..])
                    .expect("a str with invalid UTF-8 decodes"),
            )]),
            Some("valid UTF-8"),
        ),
        // Not a map at all, and a non-string key: refused by code alone.
        (Value::from(1), None),
        (Value::Map(vec![(Value::from(1), s(&source))]), None),
    ];
    for (value, key) in cases {
        let body = refused(register(&value, c, &plugins, reserving(1)));
        assert_eq!(
            body.code,
            ProtocolCode::InvalidPayload.as_str(),
            "{value:?}"
        );
        if let Some(key) = key {
            assert!(body.message.contains(key), "{value:?}: {}", body.message);
        }
    }
    assert!(plugins.ids(c).is_empty());
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_parse_failure_is_the_parsers_diagnostic_with_its_span_and_reserves_nothing() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let reserved = Cell::new(false);
    let body = refused(register(&payload("let x = 1;"), c, &plugins, |_| {
        reserved.set(true);
        Reservation::Reserved(PluginId::new(1))
    }));
    assert_eq!(body.code, "syntax.plugin_block_required");
    assert!(body.span.is_some(), "the diagnostic carries its span");
    assert!(
        !reserved.get(),
        "a source that does not parse is never reserved"
    );
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_name_the_session_already_holds_is_refused_naming_it_and_the_first_is_untouched() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    register(&payload(&named("x")), c, &plugins, reserving(1)).unwrap();
    let body = refused(register(&payload(&named("x")), c, &plugins, |_| {
        Reservation::NameTaken
    }));
    assert_eq!(body.code, ProtocolCode::DuplicatePluginName.as_str());
    assert!(
        body.message
            .contains("this Session already has a Plugin named `x`; nothing was registered"),
        "{}",
        body.message
    );
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)]);
    assert_eq!(plugins.get(c, PluginId::new(1)).unwrap().name(), "x");
    assert_eq!(plugins.len(), 1);
}

// --- a Session that ends while a registration is in flight ---

#[test]
fn a_session_that_ended_before_the_reservation_is_init_not_completed_and_holds_nothing() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let body = refused(register(&payload(&named("x")), c, &plugins, |_| {
        Reservation::SessionEnded
    }));
    assert_eq!(body.code, ProtocolCode::InitNotCompleted.as_str());
    assert!(
        body.message
            .contains("the Session has ended; nothing was registered"),
        "{}",
        body.message
    );
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_client_that_was_never_opened_or_is_closed_holds_nothing_and_is_init_not_completed() {
    let plugins = Plugins::new();
    let never = client(1);
    let body = refused(register(
        &payload(&named("x")),
        never,
        &plugins,
        reserving(1),
    ));
    assert_eq!(body.code, ProtocolCode::InitNotCompleted.as_str());
    assert!(plugins.ids(never).is_empty());

    let closed = client(2);
    plugins.open(closed);
    plugins.close(closed);
    let body = refused(register(
        &payload(&named("x")),
        closed,
        &plugins,
        reserving(2),
    ));
    assert_eq!(body.code, ProtocolCode::InitNotCompleted.as_str());
    assert!(plugins.ids(closed).is_empty());
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_registration_whose_client_closes_before_its_insert_leaves_nothing_behind() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let body = refused(register(&payload(&named("x")), c, &plugins, |_| {
        // The Session ends after the reservation and before the insert.
        plugins.close(c);
        Reservation::Reserved(PluginId::new(1))
    }));
    assert_eq!(body.code, ProtocolCode::InitNotCompleted.as_str());
    assert!(
        body.message
            .contains("the Session has ended; nothing was registered"),
        "{}",
        body.message
    );
    assert!(plugins.ids(c).is_empty());
    assert!(plugins.get(c, PluginId::new(1)).is_none());
    assert_eq!(plugins.len(), 0);
}
