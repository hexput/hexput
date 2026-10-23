//! Story 6.2 and Story 6.3: registering a Plugin's source and its Event declarations, and holding
//! them per Client ID, driven directly through `hexput_plugin`'s registration phases (`prepare`,
//! `initialize`, `finish`; Story 6.4) with a stub (or a real Session's) reservation.
//!
//! The wire rows and the Connection's routing live in `connection.rs`; the Session's name set in
//! `session.rs`; the Plugin-only findings of the check in `check.rs`. Nothing here touches a
//! socket.

use std::cell::Cell;

use hexput_exec::Registration;
use hexput_exec::wire::{MAX_RESULT_DEPTH, bounded};
use hexput_parser::parse_shape;
use hexput_plugin::{Plugins, Refusal, Reservation, finish, initialize, prepare};
use hexput_port::{CheckMode, ErrorBody, Feature, ProtocolCode, Settings, Value};
use hexput_port::{Envelope, MessageType};
use hexput_rpc::Calls;
use hexput_script::MAX_FINDINGS;
use hexput_session::ClientId;
use hexput_shared::ids::PluginId;

fn s(text: &str) -> Value {
    Value::from(text)
}

fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}

/// The `events` map of a `PluginRegister` payload, from `(name, declaration)` pairs in order.
fn events_map(entries: Vec<(&str, Value)>) -> Value {
    map(entries)
}

/// One Event declaration: `{returns}`.
fn declare(returns: &str) -> Value {
    map(vec![("returns", s(returns))])
}

/// A `{source, events}` payload.
fn payload_with(source: &str, events: Value) -> Value {
    map(vec![("source", s(source)), ("events", events)])
}

/// A payload declaring no custom Events: `events: {}`.
fn payload(source: &str) -> Value {
    payload_with(source, events_map(vec![]))
}

/// A Client ID with `n` as its low byte, without a Session behind it.
fn client(n: u8) -> ClientId {
    format!("{:032x}", n).parse().unwrap()
}

/// A valid Plugin named `name` with one `@Event` handler, for `Ping`.
fn named(name: &str) -> String {
    format!("plugin {{ name = \"{name}\" }}\n@Event(Ping) fn h(p) {{ return 1; }}")
}

/// A reservation that always succeeds with `id`.
fn reserving(id: u64) -> impl FnOnce(&str) -> Reservation {
    move |_| Reservation::Reserved(PluginId::new(id))
}

/// A reservation that records being called in `called`, then succeeds with `id`.
fn noting(called: &Cell<bool>, id: u64) -> impl FnOnce(&str) -> Reservation + '_ {
    move |_| {
        called.set(true);
        Reservation::Reserved(PluginId::new(id))
    }
}

/// The refusal `result` must be, as its `ErrorBody`.
fn refused(result: Result<Value, Box<ErrorBody>>) -> Box<ErrorBody> {
    match result {
        Ok(value) => panic!("registration accepted: {value:?}"),
        Err(body) => body,
    }
}

/// `register` with the Session's Registered Function names as `callables` and `settings` as the
/// Session's live Settings. This is the one place these tests depend on `register`'s parameters.
fn register_under(
    payload: &Value,
    client: ClientId,
    plugins: &Plugins,
    reserve: impl FnOnce(&str) -> Reservation,
    callables: &[&str],
    settings: &Settings,
) -> Result<Value, Box<ErrorBody>> {
    let callables: Vec<String> = callables.iter().map(|name| (*name).to_owned()).collect();
    // Story 6.4: the three phases, run as a registration runs them. These Plugins have no init
    // handler, so `initialize` makes no call; the Calls table is dropped unanswered.
    let prepared = prepare(payload, reserve, &callables, settings).map_err(Refusal::into_body)?;
    let (_calls, caller) = Calls::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(initialize(&prepared, Vec::new(), caller, settings))?;
    finish(prepared, client, plugins).map_err(Refusal::into_body)
}

/// [`register_under`] with no callable names and every Setting at its default (check `off`).
fn reg(
    payload: &Value,
    client: ClientId,
    plugins: &Plugins,
    reserve: impl FnOnce(&str) -> Reservation,
) -> Result<Value, Box<ErrorBody>> {
    register_under(payload, client, plugins, reserve, &[], &Settings::new())
}

/// Settings whose static check mode is `mode`.
fn checked(mode: CheckMode) -> Settings {
    let mut settings = Settings::new();
    settings.set_check(mode);
    settings
}

/// The `findings` of a successful reply, if it carries any.
fn findings_of(reply: &Value) -> Option<Vec<ErrorBody>> {
    let Value::Map(fields) = reply else {
        panic!("a Result payload is a map: {reply:?}");
    };
    let (_, findings) = fields
        .iter()
        .find(|(key, _)| key.as_str() == Some("findings"))?;
    Some(rmpv::ext::from_value(findings.clone()).expect("findings are error bodies"))
}

/// The codes of `findings`, in order.
fn codes(findings: &[ErrorBody]) -> Vec<&str> {
    findings.iter().map(|f| f.code.as_str()).collect()
}

// --- the reply and the holding ---

#[test]
fn a_registration_answers_its_name_and_holds_the_plugin_under_its_id() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let reply = reg(&payload(&named("x")), c, &plugins, reserving(1)).unwrap();
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
        let reply = reg(&payload(&named(name)), c, &plugins, reserving(id)).unwrap();
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
    reg(&payload(&named("x")), first, &plugins, reserving(1)).unwrap();
    reg(&payload(&named("x")), second, &plugins, reserving(2)).unwrap();
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
    reg(&payload(&named("a")), gone, &plugins, reserving(1)).unwrap();
    reg(&payload(&named("b")), kept, &plugins, reserving(2)).unwrap();
    plugins.close(gone);
    assert!(plugins.ids(gone).is_empty());
    assert!(plugins.get(gone, PluginId::new(1)).is_none());
    assert_eq!(plugins.ids(kept), vec![PluginId::new(2)]);
    assert_eq!(plugins.len(), 1);
}

// --- Story 6.2 refusals: the wire's `protocol.invalid_payload`, naming the key ---

#[test]
fn a_payload_that_is_not_exactly_source_and_events_is_refused_naming_the_key_and_stores_nothing() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = named("x");
    let invalid_utf8 =
        // A two-byte MessagePack str (fixstr 0xa2) whose bytes are not UTF-8.
        rmpv::decode::read_value(&mut &[0xa2_u8, 0xff, 0xfe][..])
            .expect("a str with invalid UTF-8 decodes");
    let cases: Vec<(Value, Option<&str>)> = vec![
        // Both keys missing, so the message names them together.
        (Value::Nil, Some("source")),
        (map(vec![]), Some("source")),
        (
            map(vec![
                ("source", Value::from(1)),
                ("events", events_map(vec![])),
            ]),
            Some("source"),
        ),
        // Story 6.3: `events` is required, so a `{source}` payload is refused naming it.
        (map(vec![("source", s(&source))]), Some("events")),
        (
            map(vec![("source", s(&source)), ("events", Value::Nil)]),
            Some("events"),
        ),
        (
            map(vec![
                ("source", s(&source)),
                ("events", Value::Array(vec![])),
            ]),
            Some("events"),
        ),
        // There is no `overrides` key on `PluginRegister`; an unknown key stays refused.
        (
            map(vec![
                ("source", s(&source)),
                ("events", events_map(vec![])),
                ("overrides", map(vec![])),
            ]),
            Some("overrides"),
        ),
        (
            Value::Map(vec![
                (s("source"), s(&source)),
                (s("source"), s(&source)),
                (s("events"), events_map(vec![])),
            ]),
            Some("source"),
        ),
        // A `source` whose string is not valid UTF-8 says so, naming `source`.
        (
            map(vec![
                ("source", invalid_utf8),
                ("events", events_map(vec![])),
            ]),
            Some("valid UTF-8"),
        ),
        // Not a map at all, and a non-string key: refused by code alone.
        (Value::from(1), None),
        (Value::Map(vec![(Value::from(1), s(&source))]), None),
    ];
    for (value, key) in cases {
        let body = refused(reg(&value, c, &plugins, reserving(1)));
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
fn a_payload_with_bad_events_is_refused_before_its_source_is_parsed() {
    // The payload is decoded fully before the source is parsed: this source does not parse, yet
    // the refusal is the payload's, not a `syntax` diagnostic.
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let called = Cell::new(false);
    let body = refused(reg(
        &payload_with("let x = 1;", Value::from(1)),
        c,
        &plugins,
        noting(&called, 1),
    ));
    assert_eq!(body.code, ProtocolCode::InvalidPayload.as_str());
    assert!(body.message.contains("events"), "{}", body.message);
    assert!(!called.get());
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_parse_failure_is_the_parsers_diagnostic_with_its_span_and_reserves_nothing() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let reserved = Cell::new(false);
    let body = refused(reg(&payload("let x = 1;"), c, &plugins, |_| {
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
    reg(&payload(&named("x")), c, &plugins, reserving(1)).unwrap();
    let body = refused(reg(&payload(&named("x")), c, &plugins, |_| {
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
    let body = refused(reg(&payload(&named("x")), c, &plugins, |_| {
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
    let body = refused(reg(&payload(&named("x")), never, &plugins, reserving(1)));
    assert_eq!(body.code, ProtocolCode::InitNotCompleted.as_str());
    assert!(plugins.ids(never).is_empty());

    let closed = client(2);
    plugins.open(closed);
    plugins.close(closed);
    let body = refused(reg(&payload(&named("x")), closed, &plugins, reserving(2)));
    assert_eq!(body.code, ProtocolCode::InitNotCompleted.as_str());
    assert!(plugins.ids(closed).is_empty());
    assert_eq!(plugins.len(), 0);
}

#[test]
fn a_registration_whose_client_closes_before_its_insert_leaves_nothing_behind() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let body = refused(reg(&payload(&named("x")), c, &plugins, |_| {
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

// --- Story 6.3: `events` is required, and every declaration is refused naming its path ---

#[test]
fn a_declared_event_is_held_with_its_parsed_shape_and_keyed_flag() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = named("x");
    let events = events_map(vec![
        (
            "OrderPlaced",
            map(vec![
                ("returns", s("{ points: 0, tags: [\"\"] }")),
                ("keyed", Value::Boolean(true)),
            ]),
        ),
        ("Ping", declare("null")),
    ]);
    let reply = reg(&payload_with(&source, events), c, &plugins, reserving(1)).unwrap();
    assert_eq!(reply, map(vec![("name", s("x"))]), "exactly `name`");
    let plugin = plugins.get(c, PluginId::new(1)).unwrap();
    let declared = plugin.events();
    assert_eq!(declared.len(), 2, "every declared Event is held");
    assert_eq!(&*declared[0].name, "OrderPlaced", "in payload order");
    assert_eq!(&*declared[1].name, "Ping");
    assert!(declared[0].keyed, "`keyed: true` is recorded");
    assert!(!declared[1].keyed, "absent `keyed` is false");
    assert_eq!(
        declared[0].returns,
        parse_shape("{ points: 0, tags: [\"\"] }").unwrap(),
        "the shape is the parsed Hexput literal"
    );
}

#[test]
fn no_custom_events_registers_and_every_fire_is_refused() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    reg(&payload(&named("x")), c, &plugins, reserving(1)).unwrap();
    let plugin = plugins.get(c, PluginId::new(1)).unwrap();
    assert!(plugin.events().is_empty());
    let body = match plugin.event("X") {
        Ok(_) => panic!("an undeclared Event resolved"),
        Err(body) => body,
    };
    assert_eq!(body.code, "protocol.undeclared_event");
}

#[test]
fn missing_events_are_refused_naming_events_and_nothing_is_reserved() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = named("x");
    let cases = [
        map(vec![("source", s(&source))]),
        map(vec![("source", s(&source)), ("events", Value::Nil)]),
    ];
    for value in cases {
        let called = Cell::new(false);
        let body = refused(reg(&value, c, &plugins, noting(&called, 1)));
        assert_eq!(
            body.code,
            ProtocolCode::InvalidPayload.as_str(),
            "{value:?}"
        );
        assert!(body.message.contains("events"), "{}", body.message);
        assert!(!called.get(), "no name is reserved for {value:?}");
    }
    assert_eq!(plugins.len(), 0);
}

#[test]
fn bad_event_declarations_are_refused_naming_their_path_and_nothing_is_registered() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = named("x");
    let cases: Vec<(Value, &str)> = vec![
        // `events` is not a map.
        (Value::Array(vec![]), "events"),
        (s("OrderPlaced"), "events"),
        // Names: not a §2 identifier, repeated, or the reserved one.
        (
            events_map(vec![("not an id", declare("null"))]),
            "`events.not an id`",
        ),
        (events_map(vec![("1x", declare("null"))]), "`events.1x`"),
        (
            Value::Map(vec![
                (s("OrderPlaced"), declare("null")),
                (s("OrderPlaced"), declare("null")),
            ]),
            "events.OrderPlaced",
        ),
        (
            events_map(vec![("BackendRegisteredInit", declare("null"))]),
            "BackendRegisteredInit",
        ),
        // A declaration that is not a map.
        (
            events_map(vec![("OrderPlaced", Value::from(1))]),
            "events.OrderPlaced",
        ),
        // `returns` missing, not a string, or nil (which reads as missing).
        (
            events_map(vec![("OrderPlaced", map(vec![]))]),
            "events.OrderPlaced.returns",
        ),
        (
            events_map(vec![(
                "OrderPlaced",
                map(vec![("returns", Value::from(1))]),
            )]),
            "events.OrderPlaced.returns",
        ),
        (
            events_map(vec![("OrderPlaced", map(vec![("returns", Value::Nil)]))]),
            "events.OrderPlaced.returns",
        ),
        // `keyed` not a bool.
        (
            events_map(vec![(
                "OrderPlaced",
                map(vec![("returns", s("null")), ("keyed", Value::from(1))]),
            )]),
            "events.OrderPlaced.keyed",
        ),
        // Unknown, repeated and non-string keys.
        (
            events_map(vec![(
                "OrderPlaced",
                map(vec![("returns", s("null")), ("extra", Value::from(1))]),
            )]),
            "events.OrderPlaced",
        ),
        (
            events_map(vec![(
                "OrderPlaced",
                Value::Map(vec![(s("returns"), s("null")), (s("returns"), s("null"))]),
            )]),
            "events.OrderPlaced",
        ),
        (
            events_map(vec![(
                "OrderPlaced",
                Value::Map(vec![(Value::from(1), s("null"))]),
            )]),
            "events.OrderPlaced",
        ),
    ];
    for (events, needle) in cases {
        let called = Cell::new(false);
        let value = payload_with(&source, events.clone());
        refused_naming(
            reg(&value, c, &plugins, noting(&called, 1)),
            needle,
            &events,
        );
        assert!(!called.get(), "no name is reserved for {events:?}");
    }
    assert!(plugins.ids(c).is_empty());
    assert_eq!(plugins.len(), 0);
}

/// A `protocol.invalid_payload` refusal whose message contains `needle`, with the offending
/// `events` named in the failure message.
#[track_caller]
fn refused_naming(result: Result<Value, Box<ErrorBody>>, needle: &str, events: &Value) {
    let body = refused(result);
    assert_eq!(
        body.code,
        ProtocolCode::InvalidPayload.as_str(),
        "{events:?}: {}",
        body.message
    );
    assert!(
        body.message.contains(needle),
        "{events:?}: expected `{needle}` in: {}",
        body.message
    );
}

#[test]
fn keyed_nil_reads_as_false_and_keyed_true_is_recorded() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let events = events_map(vec![
        (
            "Tick",
            map(vec![
                ("returns", s("null")),
                ("keyed", Value::Boolean(true)),
            ]),
        ),
        (
            "Quiet",
            map(vec![("returns", s("null")), ("keyed", Value::Nil)]),
        ),
    ]);
    reg(
        &payload_with(&named("x"), events),
        c,
        &plugins,
        reserving(1),
    )
    .unwrap();
    let plugin = plugins.get(c, PluginId::new(1)).unwrap();
    assert!(
        plugin.event("Tick").unwrap().keyed,
        "Tick records keyed == true"
    );
    assert!(!plugin.event("Quiet").unwrap().keyed, "nil keyed is false");
}

#[test]
fn bad_return_shapes_are_refused_naming_the_path_and_the_parser_message() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = named("x");
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    let shapes = ["{ a: f() }", "[1, 2]", "x", "{ a: 1 } 2", "{ a: 1, a: 2 }"];
    for shape in shapes {
        let called = Cell::new(false);
        let value = payload_with(&source, events_map(vec![("X", declare(shape))]));
        let body = refused(reg(&value, c, &plugins, noting(&called, 1)));
        assert_eq!(body.code, ProtocolCode::InvalidPayload.as_str(), "{shape}");
        assert!(
            body.message
                .contains("`events.X.returns` is not a return shape:"),
            "{shape}: {}",
            body.message
        );
        assert!(
            body.message.contains("(line ") && body.message.contains(", column "),
            "{shape}: {}",
            body.message
        );
        assert!(!called.get(), "{shape}: nothing reserved");
    }
    // Nested deeper than the result depth limit: no result could ever match it.
    let too_deep = deep(MAX_RESULT_DEPTH + 1);
    let called = Cell::new(false);
    let body = refused(reg(
        &payload_with(&source, events_map(vec![("X", declare(&too_deep))])),
        c,
        &plugins,
        noting(&called, 1),
    ));
    assert_eq!(body.code, ProtocolCode::InvalidPayload.as_str());
    assert!(
        body.message
            .contains("`events.X.returns` is not a return shape:"),
        "{}",
        body.message
    );
    assert!(!called.get());
    assert!(plugins.ids(c).is_empty());

    // Exactly at the limit is accepted.
    reg(
        &payload_with(
            &source,
            events_map(vec![("X", declare(&deep(MAX_RESULT_DEPTH)))]),
        ),
        c,
        &plugins,
        reserving(2),
    )
    .expect("a shape at the depth limit is accepted");
    assert_eq!(plugins.ids(c), vec![PluginId::new(2)]);
}

#[test]
fn an_event_is_resolved_only_when_declared_and_the_reserved_name_says_so() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let events = events_map(vec![("OrderPlaced", declare("{ points: 0 }"))]);
    reg(
        &payload_with(&named("x"), events),
        c,
        &plugins,
        reserving(1),
    )
    .unwrap();
    let plugin = plugins.get(c, PluginId::new(1)).unwrap();

    let declared = plugin
        .event("OrderPlaced")
        .expect("OrderPlaced is declared");
    assert!(!declared.keyed);
    assert_eq!(&*declared.name, "OrderPlaced");

    let refund = match plugin.event("Refund") {
        Ok(_) => panic!("Refund is not declared"),
        Err(body) => body,
    };
    assert_eq!(refund.code, "protocol.undeclared_event");
    assert!(refund.message.contains("Refund"), "{}", refund.message);

    let reserved = match plugin.event("BackendRegisteredInit") {
        Ok(_) => panic!("the reserved name is never fired"),
        Err(body) => body,
    };
    assert_eq!(reserved.code, "protocol.undeclared_event");
    assert_eq!(
        reserved.message,
        "`BackendRegisteredInit` is reserved; a Backend cannot fire it"
    );
}

#[test]
fn an_undeclared_event_name_in_an_error_is_bounded() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    reg(&payload(&named("x")), c, &plugins, reserving(1)).unwrap();
    let plugin = plugins.get(c, PluginId::new(1)).unwrap();
    let long = "Z".repeat(600);
    let body = match plugin.event(&long) {
        Ok(_) => panic!("an undeclared Event resolved"),
        Err(body) => body,
    };
    assert_eq!(body.code, "protocol.undeclared_event");
    assert!(
        body.message.contains(&bounded(&long)),
        "the name is echoed bounded: {}",
        body.message
    );
    assert!(
        !body.message.contains(&long),
        "the full name is never echoed"
    );
}

// --- Story 6.3: the static check at Plugin registration (AD-8, FR-26) ---

#[test]
fn check_off_registers_a_handler_for_an_undeclared_event_with_no_findings() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\n@Event(Typo) fn h(p) { return 1; }";
    let reply = reg(&payload(source), c, &plugins, reserving(1)).unwrap();
    assert_eq!(reply, map(vec![("name", s("x"))]), "no `findings` key");
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)]);
}

#[test]
fn check_warn_registers_with_a_reference_undeclared_event_finding_spanned_on_its_name() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\n@Event(Typo) fn h(p) { return 1; }";
    let typo = source.find("Typo").unwrap() as u64;
    let events = events_map(vec![("Ping", declare("null"))]);
    let reply = register_under(
        &payload_with(source, events),
        c,
        &plugins,
        reserving(1),
        &[],
        &checked(CheckMode::Warn),
    )
    .expect("a warning never rejects");
    let findings = findings_of(&reply).expect("the warning travels with the reply");
    assert_eq!(codes(&findings), ["reference.undeclared_event"]);
    assert_eq!(findings[0].severity, "warning");
    assert_eq!(findings[0].span.unwrap().offset, typo);
    assert_eq!(
        findings[0].message,
        "`Typo` is not an Event the Backend declared; this handler never runs"
    );
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)], "registered");
}

#[test]
fn check_error_rejects_with_every_finding_and_reserves_nothing_until_an_off_registration_succeeds()
{
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { let unused = 1; return g(); }";
    // `Ping` is declared, so the undeclared-Event warning stays out of the findings asserted.
    let declared = payload_with(source, events_map(vec![("Ping", declare("null"))]));
    let called = Cell::new(false);
    let body = refused(register_under(
        &declared,
        c,
        &plugins,
        noting(&called, 1),
        &[],
        &checked(CheckMode::Error),
    ));
    assert_eq!(body.code, "capability.unknown_function");
    assert_eq!(body.severity, "error");
    assert_eq!(
        codes(&body.findings),
        ["reference.unused_variable", "capability.unknown_function"],
        "every finding, warnings too, in source order"
    );
    assert!(!called.get(), "a rejected Plugin reserves nothing");
    assert!(plugins.ids(c).is_empty());

    // The same source is accepted once the check is off: the rejection changed nothing.
    reg(&payload(source), c, &plugins, reserving(1))
        .expect("an identical registration under check `off` succeeds");
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)]);
}

#[test]
fn a_listed_callable_is_silent_and_an_unlisted_one_is_the_error() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { return g(); }";
    // `Ping` is declared, so a clean pass carries no undeclared-Event warning either.
    let declared = payload_with(source, events_map(vec![("Ping", declare("null"))]));
    let listed = register_under(
        &declared,
        c,
        &plugins,
        reserving(1),
        &["g"],
        &checked(CheckMode::Error),
    )
    .expect("g is a Registered Function of the Session");
    assert_eq!(listed, map(vec![("name", s("x"))]));
}

#[test]
fn a_construct_the_sessions_features_disable_is_the_checks_error() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { while (false) {}; return 1; }";
    let declared = payload_with(source, events_map(vec![("Ping", declare("null"))]));
    let mut settings = checked(CheckMode::Error);
    settings.set_feature(Feature::Loops, false);
    let called = Cell::new(false);
    let body = refused(register_under(
        &declared,
        c,
        &plugins,
        noting(&called, 1),
        &[],
        &settings,
    ));
    assert_eq!(body.code, "policy.construct_disabled");
    assert!(!called.get(), "a rejected Plugin reserves nothing");
}

#[test]
fn declared_events_and_the_reserved_handler_are_silent_under_warn() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    // A declared Event is no finding, and `BackendRegisteredInit` is never reported.
    let source = "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { return 1; }\n\
                  @Event(BackendRegisteredInit) fn boot(p) { return 2; }";
    let events = events_map(vec![("Ping", declare("null"))]);
    let reply = register_under(
        &payload_with(source, events),
        c,
        &plugins,
        reserving(1),
        &[],
        &checked(CheckMode::Warn),
    )
    .unwrap();
    assert!(findings_of(&reply).is_none(), "nothing to report");
}

#[test]
fn under_warn_empty_events_leave_every_ordinary_handler_undeclared_and_never_report_the_reserved_one()
 {
    // An empty `events` map declares no Event at all, so an ordinary handler is undeclared, but
    // the reserved name is never reported.
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\n@Event(Ping) fn h(p) { return 1; }\n\
                  @Event(BackendRegisteredInit) fn boot(p) { return 2; }";
    let reply = register_under(
        &payload(source),
        c,
        &plugins,
        reserving(1),
        &[],
        &checked(CheckMode::Warn),
    )
    .unwrap();
    let findings = findings_of(&reply).expect("Ping is undeclared");
    assert_eq!(codes(&findings), ["reference.undeclared_event"]);
    assert!(
        findings[0].message.contains("`Ping`"),
        "{}",
        findings[0].message
    );
}

#[test]
fn under_warn_globals_and_handlers_are_never_unused_but_a_helper_is() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\nlet a = 1;\n@Event(Ping) fn h(p) { return 1; };\n\
                  fn helper() { return 2; }";
    let events = events_map(vec![("Ping", declare("null"))]);
    let reply = register_under(
        &payload_with(source, events),
        c,
        &plugins,
        reserving(1),
        &[],
        &checked(CheckMode::Warn),
    )
    .unwrap();
    let findings = findings_of(&reply).expect("the helper is unused");
    assert_eq!(
        codes(&findings),
        ["reference.unused_variable"],
        "only `helper`: `a` and `h` are exempt"
    );
}

#[test]
fn a_handler_reading_a_global_is_clean() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = "plugin { name = \"x\" }\nlet total = 1;\n@Event(Ping) fn h(p) { return total; }";
    let events = events_map(vec![("Ping", declare("null"))]);
    let reply = register_under(
        &payload_with(source, events),
        c,
        &plugins,
        reserving(1),
        &[],
        &checked(CheckMode::Warn),
    )
    .unwrap();
    assert!(
        findings_of(&reply).is_none(),
        "a Global read by a handler is clean"
    );
}

#[test]
fn findings_are_bounded_to_max_findings_in_source_order_and_a_rejection_keeps_its_error_last() {
    assert_eq!(MAX_FINDINGS, 100);
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let unused = |tail: &str| {
        let mut body: String = (0..150).map(|i| format!("let u{i} = 1; ")).collect();
        body.push_str(tail);
        format!("plugin {{ name = \"x\" }}\n@Event(Ping) fn h(p) {{ {body} }}")
    };
    let events = events_map(vec![("Ping", declare("null"))]);

    let reply = register_under(
        &payload_with(&unused("return 1;"), events.clone()),
        c,
        &plugins,
        reserving(1),
        &[],
        &checked(CheckMode::Warn),
    )
    .unwrap();
    let findings = findings_of(&reply).expect("findings");
    assert_eq!(findings.len(), MAX_FINDINGS);
    let offsets: Vec<u64> = findings.iter().map(|f| f.span.unwrap().offset).collect();
    assert!(offsets.windows(2).all(|w| w[0] < w[1]), "source order");

    // Under error, the rejection's first error lies past the cap and is kept as the last finding.
    let body = refused(register_under(
        &payload_with(&unused("return g();"), events),
        c,
        &plugins,
        reserving(2),
        &[],
        &checked(CheckMode::Error),
    ));
    assert_eq!(body.code, "capability.unknown_function");
    assert_eq!(body.findings.len(), MAX_FINDINGS);
    let last = body.findings.last().unwrap();
    assert_eq!(
        (&last.code, &last.message, last.span),
        (&body.code, &body.message, body.span)
    );
    assert!(
        body.findings[..MAX_FINDINGS - 1]
            .iter()
            .all(|f| f.code == "reference.unused_variable")
    );
    assert_eq!(
        plugins.ids(c),
        vec![PluginId::new(1)],
        "the rejection held nothing"
    );
}

// --- Story 6.4: prepare, initialize, finish — a Plugin is ready only after its init completed ---

/// A Plugin named `x` whose `BackendRegisteredInit` handlers are `handlers`, given as
/// `(function name, body)` in source order.
fn initialized(handlers: &[(&str, &str)]) -> String {
    let mut source = String::from("plugin { name = \"x\" }\n");
    for (name, body) in handlers {
        source.push_str(&format!(
            "@Event(BackendRegisteredInit) fn {name}(p) {{ {body} }}\n"
        ));
    }
    source
}

#[test]
fn a_prepared_plugin_is_not_held_until_it_is_finished() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let prepared = prepare(&payload(&named("x")), reserving(1), &[], &Settings::new()).unwrap();
    assert!(plugins.ids(c).is_empty(), "a prepared Plugin is not ready");
    assert!(plugins.get(c, PluginId::new(1)).is_none());
    assert_eq!(plugins.len(), 0);
    let reply = finish(prepared, c, &plugins)
        .map_err(Refusal::into_body)
        .unwrap();
    assert_eq!(reply, map(vec![("name", s("x"))]));
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)]);
}

#[test]
fn a_prepared_plugin_dropped_before_it_is_finished_is_never_held() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let prepared = prepare(&payload(&named("x")), reserving(1), &[], &Settings::new()).unwrap();
    drop(prepared);
    assert!(plugins.ids(c).is_empty());
    assert!(plugins.is_empty());
}

#[test]
fn a_plugin_is_not_ready_while_its_init_handler_is_pending_and_is_ready_once_it_completes() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = initialized(&[("boot", "return log(1);")]);
    let prepared = prepare(&payload(&source), reserving(1), &[], &Settings::new()).unwrap();
    let (mut calls, caller) = Calls::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let init = initialize(
            &prepared,
            vec![Registration::from(("log", true))],
            caller,
            &Settings::new(),
        );
        let answer = async {
            let call = calls.submitted().await.expect("the init handler calls log");
            assert!(
                plugins.ids(c).is_empty() && plugins.get(c, PluginId::new(1)).is_none(),
                "no Event can run ahead of init: the Plugin is not ready yet"
            );
            let envelope: Envelope<Value> = calls.issue(call).expect("the init waits for it");
            calls
                .complete(Envelope::new(
                    envelope.id.unwrap(),
                    MessageType::Result,
                    map(vec![("value", Value::Nil)]),
                ))
                .unwrap();
        };
        let (outcome, ()) = tokio::join!(init, answer);
        outcome.unwrap();
    });
    assert!(
        plugins.ids(c).is_empty(),
        "initialize alone does not make it ready"
    );
    let reply = finish(prepared, c, &plugins)
        .map_err(Refusal::into_body)
        .unwrap();
    assert_eq!(reply, map(vec![("name", s("x"))]));
    assert_eq!(plugins.ids(c), vec![PluginId::new(1)]);
}

#[test]
fn a_failing_init_handler_yields_its_diagnostic_and_nothing_is_held() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = initialized(&[("boot", "return f();"), ("later", "return 2;")]);
    let prepared = prepare(&payload(&source), reserving(1), &[], &Settings::new()).unwrap();
    let (_calls, caller) = Calls::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let body = runtime
        .block_on(initialize(&prepared, Vec::new(), caller, &Settings::new()))
        .expect_err("the unregistered call fails the init");
    assert_eq!(body.code, "capability.unknown_function");
    drop(prepared);
    assert!(plugins.ids(c).is_empty());
    assert!(plugins.is_empty());
}

#[test]
fn an_init_handler_whose_budget_is_spent_yields_its_diagnostic_and_nothing_is_held() {
    let plugins = Plugins::new();
    let c = client(1);
    plugins.open(c);
    let source = initialized(&[("boot", "while (true) {}")]);
    let prepared = prepare(&payload(&source), reserving(1), &[], &Settings::new()).unwrap();
    let (_calls, caller) = Calls::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let body = runtime
        .block_on(initialize(&prepared, Vec::new(), caller, &Settings::new()))
        .expect_err("the runaway init is stopped");
    assert_eq!(body.code, "budget.cpu_time_exceeded");
    drop(prepared);
    assert!(plugins.ids(c).is_empty());
}
