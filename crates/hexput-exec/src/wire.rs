//! Hexput values to and from the wire's MessagePack values — for every value that crosses the
//! boundary: a starting variable and a Script result (Story 2.6), a host call's arguments and the
//! Backend's reply (Story 3.1). Moved here from `hexput-script` in Story 3.1, because the Executor
//! converts a host call's values itself; Direct Execution uses the same functions from here.
//!
//! Inbound, a wire value becomes a Hexput value **losslessly or not at all**: every MessagePack
//! shape with no exact Hexput counterpart is refused with the path to it, never rounded, truncated
//! or dropped. Outbound, a value is walked iteratively first ([`measure`]), so one the Daemon could
//! not send — nested past a limit, or certain to exceed a frame — is refused before the recursive
//! conversion and the recursive encoder ever see it.
//!
//! # Value Secrets (Story 3.11)
//!
//! A value carrying a Value Secret travels as a **holder**: a map with exactly the keys `__secret`
//! and `value`, `__secret` itself a map with a string `ref` (the Reference ID, non-empty, at most
//! [`MAX_REFERENCE_LEN`] bytes), an optional string `key`, and any further fields the Backend put
//! there, kept verbatim and in order. Inbound, [`to_hexput`] recognises holders anywhere in a
//! value — a holder around an array or object gives that collection its secret, one around any
//! other value gives the place it arrives in the secret — and refuses, with the path, a malformed
//! `__secret`, a holder directly inside a holder's `value`, and any other map with a `__secret`
//! key, so the key never reaches a Script. The further fields are handed to the interpreter as
//! encoded MessagePack, which it carries and never reads. Outbound, [`to_wire_held`] emits a
//! holder for every value that carries a secret — `ref`, then `key`, then the further fields —
//! and a value without one exactly as before.
//!
//! # Modifications (Story 3.13)
//!
//! A `Call` reply's `modifications` and a Script result's are both an array of maps with exactly
//! the keys `ref` (a Reference ID) and `value` (the whole new value). The value is the value
//! itself — never a holder, since the Reference ID names the place — and everything nested inside
//! it travels as anywhere else. Inbound, [`modifications_to_hexput`] refuses, with the path,
//! anything else. Outbound, [`modifications_to_wire`] emits every nested secret, a generated one
//! included (the Backend saw it in a `Call`), and [`result_payload_size`] counts the whole
//! `{value, modifications}` payload exactly.

use core::fmt::Write as _;
use std::collections::HashSet;
use std::sync::Arc;

use hexput_interpreter::{Array, Held, Modification, Object, Secret, Value as Hexput};
use hexput_rpc::{MAX_FRAME_LEN, MAX_NESTING_DEPTH, Value as Wire, rmpv};

/// The deepest container nesting a Script result may have. The Daemon's decoder accepts
/// [`MAX_NESTING_DEPTH`] levels per frame and the envelope map and the `{value}` payload map take
/// two of them, so a result nested any deeper could not be read back by a peer applying the same
/// limit.
pub const MAX_RESULT_DEPTH: usize = MAX_NESTING_DEPTH - 2;

/// The deepest wire nesting — holders included — one host call argument may have: the envelope,
/// the `Call` payload map and its `arguments` array take three of the frame's
/// [`MAX_NESTING_DEPTH`] levels.
pub const MAX_ARGUMENT_WIRE_DEPTH: usize = MAX_NESTING_DEPTH - 3;

/// The deepest wire nesting — holders included — one modification's value in a Script result may
/// have (Story 3.13): the envelope, the payload map, its `modifications` array and the entry's map
/// take four of the frame's [`MAX_NESTING_DEPTH`] levels.
pub const MAX_MODIFICATION_DEPTH: usize = MAX_NESTING_DEPTH - 4;

/// The longest Reference ID a Backend may supply, in bytes.
pub const MAX_REFERENCE_LEN: usize = 256;

/// The key a holder carries its Value Secret under.
pub const SECRET_KEY: &str = "__secret";
/// The key a holder carries its value under.
const HOLDER_VALUE_KEY: &str = "value";
/// The Reference ID's key inside `__secret`.
const REF_KEY: &str = "ref";
/// The object key's key inside `__secret`.
const KEY_KEY: &str = "key";
/// The payload key a reply's or a result's modifications travel under.
pub const MODIFICATIONS_KEY: &str = "modifications";

/// The largest magnitude an integer may have to be a Hexput `number` exactly: 2^53.
const EXACT_INTEGER: u64 = 1 << 53;

/// The most characters of Backend input one path segment echoes back.
const SEGMENT_LIMIT: usize = 64;

/// The most characters a whole echoed path may have.
const PATH_LIMIT: usize = 256;

/// One step from `variables` down to a nested value.
#[derive(Clone, Copy)]
enum Segment<'a> {
    Key(&'a str),
    Index(usize),
}

/// Where a value sits inside the payload it arrived in, rendered only on failure.
pub struct Path<'a> {
    root: &'static str,
    segments: Vec<Segment<'a>>,
}

impl<'a> Path<'a> {
    /// The path of one starting variable, `variables.<name>`.
    #[must_use]
    pub fn variable(name: &'a str) -> Self {
        Self {
            root: "variables",
            segments: vec![Segment::Key(name)],
        }
    }

    /// The path of a host call's reply value, `value`.
    #[must_use]
    pub const fn reply() -> Self {
        Self {
            root: "value",
            segments: Vec::new(),
        }
    }

    /// The path of a reply's modification at `index`, `modifications[<index>]`.
    fn modification(index: usize) -> Self {
        Self {
            root: MODIFICATIONS_KEY,
            segments: vec![Segment::Index(index)],
        }
    }

    /// `variables.user.tags[2]`, with non-identifier keys quoted and every part bounded.
    fn render(&self) -> String {
        let mut out = format!("`{}", self.root);
        for segment in &self.segments {
            match segment {
                Segment::Key(key) if is_plain_key(key) => {
                    let _ = write!(out, ".{}", bounded(key));
                }
                Segment::Key(key) => {
                    let _ = write!(out, "[{:?}]", bounded(key));
                }
                Segment::Index(index) => {
                    let _ = write!(out, "[{index}]");
                }
            }
        }
        let mut out = bounded_to(&out, PATH_LIMIT);
        out.push('`');
        out
    }
}

/// A key that reads unambiguously after a `.`: ASCII letters, digits and `_`, not starting with a
/// digit. Only for rendering a path — whether a *starting variable's name* is valid is the
/// parser's judgement, not this.
fn is_plain_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `text` cut to [`SEGMENT_LIMIT`] characters, with an ellipsis when anything was cut.
#[must_use]
pub fn bounded(text: &str) -> String {
    bounded_to(text, SEGMENT_LIMIT)
}

fn bounded_to(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Convert a wire value — a starting variable's, or a host call's reply — into a Hexput value,
/// with the secret of the place it arrives in when it is a holder around a string, number, bool
/// or `null` (a holder around an array or object gives the collection its secret instead).
///
/// Recursive, and bounded: a payload the Daemon decoded is at most [`MAX_NESTING_DEPTH`] levels
/// deep, and a value handed in directly deeper than that is refused rather than followed.
///
/// # Errors
///
/// A message naming the path to the first value with no lossless Hexput representation, or to the
/// first malformed holder (see the module documentation).
pub fn to_hexput<'a>(value: &'a Wire, path: &mut Path<'a>) -> Result<Held, String> {
    convert(value, path, None)
}

/// [`to_hexput`] for a value whose holder, if it was one, carried `secret`.
fn convert<'a>(
    value: &'a Wire,
    path: &mut Path<'a>,
    secret: Option<Secret>,
) -> Result<Held, String> {
    if path.segments.len() > MAX_NESTING_DEPTH {
        return Err(format!(
            "{} nests deeper than {MAX_NESTING_DEPTH} levels",
            path.render()
        ));
    }
    let refuse = |path: &Path<'_>, what: &str| Err(format!("{} {what}", path.render()));
    let scalar = |value: Hexput| Ok(Held::new(value, secret.clone()));
    match value {
        Wire::Nil => scalar(Hexput::Null),
        Wire::Boolean(b) => scalar(Hexput::Bool(*b)),
        Wire::Integer(integer) => {
            let exact = match (integer.as_i64(), integer.as_u64()) {
                // Within ±2^53, so either conversion is exact.
                (Some(signed), _) if signed.unsigned_abs() <= EXACT_INTEGER => Some(signed as f64),
                (_, Some(unsigned)) if unsigned <= EXACT_INTEGER => Some(unsigned as f64),
                _ => None,
            };
            match exact {
                Some(number) => scalar(Hexput::Number(number)),
                None => refuse(
                    path,
                    &format!(
                        "is the integer {integer}, outside ±2^53, which a Hexput number cannot \
                         hold exactly"
                    ),
                ),
            }
        }
        Wire::F32(number) => finite(f64::from(*number), path).and_then(scalar),
        Wire::F64(number) => finite(*number, path).and_then(scalar),
        Wire::String(text) => match text.as_str() {
            Some(text) => scalar(Hexput::String(Arc::from(text))),
            None => refuse(path, "is a string that is not valid UTF-8"),
        },
        // An invalid-UTF-8 MessagePack str also arrives as `Binary` (see `hexput-port`'s codec).
        Wire::Binary(_) => refuse(
            path,
            "is binary data (or a string that is not valid UTF-8); Hexput has no binary type",
        ),
        Wire::Ext(tag, _) => refuse(
            path,
            &format!("is a MessagePack extension (type {tag}); Hexput has no such type"),
        ),
        Wire::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                path.segments.push(Segment::Index(index));
                let value = convert(item, path, None)?;
                path.segments.pop();
                values.push(value);
            }
            Ok(Held::plain(Hexput::Array(Array::from_held(values, secret))))
        }
        Wire::Map(fields)
            if fields
                .iter()
                .any(|(key, _)| key.as_str() == Some(SECRET_KEY)) =>
        {
            let holder = match fields.as_slice() {
                [(first, a), (second, b)] => match (first.as_str(), second.as_str()) {
                    (Some(SECRET_KEY), Some(HOLDER_VALUE_KEY)) => Some((a, b)),
                    (Some(HOLDER_VALUE_KEY), Some(SECRET_KEY)) => Some((b, a)),
                    _ => None,
                },
                _ => None,
            };
            let Some((hidden, inner)) = holder else {
                return refuse(
                    path,
                    "has a `__secret` key but is not a Value Secret holder, which has exactly the \
                     keys `__secret` and `value`",
                );
            };
            if secret.is_some() {
                return refuse(
                    path,
                    "is a Value Secret holder directly inside another holder's `value`",
                );
            }
            path.segments.push(Segment::Key(SECRET_KEY));
            let hidden = decode_secret(hidden, path)?;
            path.segments.pop();
            path.segments.push(Segment::Key(HOLDER_VALUE_KEY));
            let held = convert(inner, path, Some(hidden))?;
            path.segments.pop();
            Ok(held)
        }
        Wire::Map(fields) => {
            let mut seen: HashSet<&str> = HashSet::with_capacity(fields.len());
            let mut entries = Vec::with_capacity(fields.len());
            for (key, item) in fields {
                let Some(key) = key.as_str() else {
                    return refuse(
                        path,
                        "has a key that is not a string; object keys are strings",
                    );
                };
                if !seen.insert(key) {
                    return refuse(path, &format!("repeats the key {:?}", bounded(key)));
                }
                path.segments.push(Segment::Key(key));
                let value = convert(item, path, None)?;
                path.segments.pop();
                entries.push((key, value));
            }
            Ok(Held::plain(Hexput::Object(Object::from_held_entries(
                entries, secret,
            ))))
        }
    }
}

/// A `Call` reply's `modifications` as Hexput modifications (Story 3.13, decision 1): an array of
/// maps with exactly the keys `ref` — a non-empty string of at most [`MAX_REFERENCE_LEN`] bytes —
/// and `value`, any wire value but a holder at its top (holders may nest inside it). Converted as
/// a reply's value is.
///
/// # Errors
/// A message naming the path to the first thing wrong; nothing is converted past it.
pub fn modifications_to_hexput(value: &Wire) -> Result<Vec<Modification>, String> {
    let Wire::Array(entries) = value else {
        return Err(format!(
            "`{MODIFICATIONS_KEY}` must be an array of maps with exactly the keys `{REF_KEY}` and \
             `{HOLDER_VALUE_KEY}`"
        ));
    };
    let mut modifications = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let mut path = Path::modification(index);
        let shape = |path: &Path<'_>| {
            format!(
                "{} must be a map with exactly the keys `{REF_KEY}` and `{HOLDER_VALUE_KEY}`",
                path.render()
            )
        };
        let Wire::Map(fields) = entry else {
            return Err(shape(&path));
        };
        let mut reference = None;
        let mut new = None;
        for (key, field) in fields {
            let slot = match key.as_str() {
                Some(REF_KEY) => &mut reference,
                Some(HOLDER_VALUE_KEY) => &mut new,
                _ => return Err(shape(&path)),
            };
            if slot.is_some() {
                return Err(shape(&path));
            }
            *slot = Some(field);
        }
        let (Some(reference), Some(new)) = (reference, new) else {
            return Err(shape(&path));
        };
        let reference = match reference.as_str() {
            Some(text) if !text.is_empty() && text.len() <= MAX_REFERENCE_LEN => text,
            _ => {
                path.segments.push(Segment::Key(REF_KEY));
                return Err(format!(
                    "{} must be a non-empty string of at most {MAX_REFERENCE_LEN} bytes",
                    path.render()
                ));
            }
        };
        path.segments.push(Segment::Key(HOLDER_VALUE_KEY));
        if let Wire::Map(fields) = new
            && fields
                .iter()
                .any(|(key, _)| key.as_str() == Some(SECRET_KEY))
        {
            return Err(format!(
                "{} has a `__secret` key: a modification carries the value itself, never a Value \
                 Secret holder, and its `{REF_KEY}` names the place",
                path.render()
            ));
        }
        let held = convert(new, &mut path, None)?;
        modifications.push(Modification::new(reference, held.value));
    }
    Ok(modifications)
}

/// A holder's `__secret`, at `path`: a map with a string `ref` (non-empty, at most
/// [`MAX_REFERENCE_LEN`] bytes), an optional string `key` and any further fields, every key a
/// string appearing once. The further fields are kept in order, encoded as one MessagePack map.
fn decode_secret(hidden: &Wire, path: &Path<'_>) -> Result<Secret, String> {
    let refuse = |what: &str| Err(format!("{} {what}", path.render()));
    let Wire::Map(fields) = hidden else {
        return refuse("must be a map with a string `ref`");
    };
    let mut seen: HashSet<&str> = HashSet::with_capacity(fields.len());
    let mut reference = None;
    let mut key = None;
    let mut rest = Vec::new();
    for (name, value) in fields {
        let Some(name) = name.as_str() else {
            return refuse("has a key that is not a string");
        };
        if !seen.insert(name) {
            return refuse(&format!("repeats the key {:?}", bounded(name)));
        }
        match name {
            REF_KEY => match value.as_str() {
                Some(text) if !text.is_empty() && text.len() <= MAX_REFERENCE_LEN => {
                    reference = Some(text);
                }
                _ => {
                    return refuse(&format!(
                        "must have a `ref` that is a non-empty string of at most \
                         {MAX_REFERENCE_LEN} bytes"
                    ));
                }
            },
            KEY_KEY => match value.as_str() {
                Some(text) => key = Some(Arc::from(text)),
                None => return refuse("must have a `key` that is a string, when it has one"),
            },
            _ => rest.push((Wire::from(name), value.clone())),
        }
    }
    let Some(reference) = reference else {
        return refuse("must be a map with a string `ref`");
    };
    let extra = if rest.is_empty() {
        Vec::new()
    } else {
        let mut bytes = Vec::new();
        // Writing to a `Vec` cannot fail.
        let _ = rmpv::encode::write_value(&mut bytes, &Wire::Map(rest));
        bytes
    };
    Ok(Secret::new(reference, key, extra))
}

fn finite(number: f64, path: &Path<'_>) -> Result<Hexput, String> {
    if number.is_finite() {
        Ok(Hexput::Number(number))
    } else {
        Err(format!(
            "{} is {number}; a Hexput number is always finite",
            path.render()
        ))
    }
}

/// Why a value cannot be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsendable {
    /// It nests more containers deep than the limit it was measured against.
    TooDeep,
    /// It is certain to encode past [`MAX_FRAME_LEN`] (together with whatever else was measured
    /// against the same budget).
    TooLarge,
    /// It holds a value kind this crate does not know how to put on the wire.
    Unrepresentable,
}

/// Walk a value without recursion and refuse it if it nests more than `max_depth` arrays or
/// objects deep, or if its encoding is certain to take more than the `budget` bytes left — which
/// is then reduced by what the value takes, so several values can share one frame's budget.
///
/// The size check is a lower bound — every value encodes to at least one byte, and a string or
/// key to at least its own bytes — so it never refuses a value that would have fit. It also
/// bounds the walk itself: a value that shares one collection many times over (`[x, x]` nested
/// repeatedly) is small in the execution but exponential on the wire, and the walk stops as soon
/// as the bound is passed instead of expanding it.
///
/// # Errors
/// Which limit the value breaks.
pub fn measure(value: &Hexput, max_depth: usize, budget: &mut usize) -> Result<(), Unsendable> {
    measure_held(value, None, max_depth, budget)
}

/// [`measure`] for a value in a place with the secret `secret` (Story 3.11). The depth counted is
/// the Script's nesting, never the holders around it; the bytes include every holder's.
///
/// # Errors
/// Which limit the value breaks.
pub fn measure_held(
    value: &Hexput,
    secret: Option<&Secret>,
    max_depth: usize,
    budget: &mut usize,
) -> Result<(), Unsendable> {
    measure_as(value, secret, max_depth, budget, Leaving::Call, false)
}

/// Where a value is going: a secret the execution generated travels in a `Call`, never in the
/// Script's result (Story 3.11).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Leaving {
    Call,
    Result,
}

/// `bare`: the value itself travels with no holder around it, whatever it carries — a
/// modification's value (Story 3.13).
fn measure_as(
    value: &Hexput,
    secret: Option<&Secret>,
    max_depth: usize,
    budget: &mut usize,
    leaving: Leaving,
    bare: bool,
) -> Result<(), Unsendable> {
    let mut pending: Vec<(&Hexput, Option<&Secret>, usize)> = vec![(value, secret, 0)];
    while let Some((value, secret, depth)) = pending.pop() {
        let mut bytes: usize = 1;
        // Only the value itself sits at depth 0: everything inside it is at least one deeper.
        let top = bare && depth == 0;
        if !top && let Some(secret) = shown(value, secret, leaving) {
            bytes = bytes.saturating_add(holder_size(secret));
        }
        match value {
            Hexput::Null | Hexput::Bool(_) | Hexput::Number(_) => {}
            Hexput::String(text) => bytes = bytes.saturating_add(text.len()),
            Hexput::Array(array) => {
                let depth = nested(depth, max_depth)?;
                pending.extend(
                    array
                        .iter_with_secrets()
                        .map(|(item, secret)| (item, secret, depth)),
                );
            }
            Hexput::Object(object) => {
                let depth = nested(depth, max_depth)?;
                for (key, item, secret) in object.iter_with_secrets() {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((item, secret, depth));
                }
            }
            _ => return Err(Unsendable::Unrepresentable),
        }
        *budget = budget.checked_sub(bytes).ok_or(Unsendable::TooLarge)?;
    }
    Ok(())
}

/// The secret a value in a place with `secret` travels with when `leaving`: an array's or
/// object's own, otherwise the place's — and, in the result, only one the Backend supplied.
fn shown<'a>(
    value: &'a Hexput,
    secret: Option<&'a Secret>,
    leaving: Leaving,
) -> Option<&'a Secret> {
    let secret = if value.is_collection() {
        value.secret()
    } else {
        secret
    };
    secret.filter(|secret| leaving == Leaving::Call || !secret.is_generated())
}

/// Walk a Script result: refused exactly as [`result_to_wire`] refuses it — nested deeper than
/// [`MAX_RESULT_DEPTH`], holders included, or certain to encode past [`MAX_FRAME_LEN`].
///
/// # Errors
/// Which limit the result breaks.
pub fn check_result(result: &Hexput) -> Result<(), Unsendable> {
    result_to_wire(&Held::plain(result.clone())).map(drop)
}

/// A Script result as its `value` on the wire, with the holders of every secret the Backend
/// supplied — a secret this execution generated for a `Call` never appears — or why it cannot be
/// sent: its
/// Script nesting passes [`MAX_RESULT_DEPTH`], it is certain to encode past [`MAX_FRAME_LEN`] (see
/// [`measure`]), or its holders take its wire nesting past [`MAX_RESULT_DEPTH`] (all
/// [`Unsendable::TooDeep`] but the size).
///
/// # Errors
/// Which limit the result breaks.
pub fn result_to_wire(result: &Held) -> Result<Wire, Unsendable> {
    let mut budget = MAX_FRAME_LEN;
    result_to_wire_within(result, &mut budget)
}

/// [`result_to_wire`], counting the result's bytes against `budget` — what is left of one frame —
/// and reducing it by them, so the result and its modifications share a frame
/// ([`modifications_to_wire`]).
///
/// # Errors
/// As [`result_to_wire`].
pub fn result_to_wire_within(result: &Held, budget: &mut usize) -> Result<Wire, Unsendable> {
    measure_as(
        &result.value,
        result.secret.as_ref(),
        MAX_RESULT_DEPTH,
        budget,
        Leaving::Result,
        false,
    )?;
    let wire = encode(&result.value, result.secret.as_ref(), Leaving::Result);
    if wire_depth(&wire) > MAX_RESULT_DEPTH {
        return Err(Unsendable::TooDeep);
    }
    Ok(wire)
}

/// A finished Script's modifications as the result payload's `modifications` (Story 3.13,
/// decision 5): an array of `{ref, value}` maps, in order, each value the value itself with every
/// secret nested in it emitted as a holder — a generated one too, since the Backend saw it in a
/// `Call` — or why they cannot be sent: a value whose nesting, holders counted, passes
/// [`MAX_MODIFICATION_DEPTH`] ([`Unsendable::TooDeep`]), or values certain to take more than the
/// `budget` bytes left of the frame ([`Unsendable::TooLarge`]), which is reduced by what they take.
///
/// # Errors
/// Which limit the modifications break.
pub fn modifications_to_wire(
    modifications: &[Modification],
    budget: &mut usize,
) -> Result<Wire, Unsendable> {
    for modification in modifications {
        measure_as(
            &modification.value,
            None,
            MAX_MODIFICATION_DEPTH,
            budget,
            Leaving::Call,
            true,
        )?;
        *budget = budget
            .checked_sub(modification.reference.len())
            .ok_or(Unsendable::TooLarge)?;
    }
    let mut entries = Vec::with_capacity(modifications.len());
    for modification in modifications {
        let value = plain(&modification.value, Leaving::Call);
        if wire_depth(&value) > MAX_MODIFICATION_DEPTH {
            return Err(Unsendable::TooDeep);
        }
        entries.push(Wire::Map(vec![
            (Wire::from(REF_KEY), Wire::from(&*modification.reference)),
            (Wire::from(HOLDER_VALUE_KEY), value),
        ]));
    }
    Ok(Wire::Array(entries))
}

/// How many arrays, maps and extension values deep `value` nests (a scalar is `0`) — what a
/// frame's nesting limit counts. Walks without recursion.
#[must_use]
pub fn wire_depth(value: &Wire) -> usize {
    let mut deepest = 0;
    let mut pending: Vec<(&Wire, usize)> = vec![(value, 0)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            Wire::Array(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
                deepest = deepest.max(depth + 1);
            }
            Wire::Map(fields) => {
                for (key, item) in fields {
                    pending.push((key, depth + 1));
                    pending.push((item, depth + 1));
                }
                deepest = deepest.max(depth + 1);
            }
            Wire::Ext(..) => deepest = deepest.max(depth + 1),
            _ => deepest = deepest.max(depth),
        }
    }
    deepest
}

/// The exact number of bytes the Script result `value` takes on the wire as the `{value}` payload
/// of a `Result` — the map, its `value` key and the value, each MessagePack-encoded as [`to_wire`]
/// converts it and the codec writes it (every integer, string, array and map header in its most
/// compact form) — or, once the count passes `limit` or [`MAX_FRAME_LEN`], whichever is smaller,
/// some count past it: the walk stops there, so a result that shares one collection many times
/// over is never expanded in full, however large a `limit` the caller passes.
///
/// Walks without recursion, so a result of any depth is measured. Only the output size budget
/// uses it; whether the result may be sent at all is [`check_result`]'s.
#[must_use]
pub fn payload_size(value: &Hexput, limit: usize) -> usize {
    payload_size_held(value, None, limit)
}

/// [`payload_size`] for a result in a place with the secret `secret`: every holder
/// [`result_to_wire`] emits is counted exactly, and a secret the execution generated, which it
/// never emits, not at all (Story 3.11).
#[must_use]
pub fn payload_size_held(value: &Hexput, secret: Option<&Secret>, limit: usize) -> usize {
    result_payload_size(&Held::new(value.clone(), secret.cloned()), &[], limit)
}

/// The exact number of bytes a finished Script's `Result` payload takes on the wire — `{value}`,
/// or `{value, modifications}` when there are modifications (Story 3.13), each as
/// [`result_to_wire`] and [`modifications_to_wire`] convert them — or, once the count passes
/// `limit` or [`MAX_FRAME_LEN`], whichever is smaller, some count past it, exactly as
/// [`payload_size`]. What the output size budget charges; `findings` (Story 3.10) are outside it.
#[must_use]
pub fn result_payload_size(result: &Held, modifications: &[Modification], limit: usize) -> usize {
    let limit = limit.min(MAX_FRAME_LEN);
    // A map of one or two entries (1 byte), its key `value` (a 5-byte fixstr: 6 bytes), then the
    // value.
    let mut size: usize = 1 + str_size(VALUE_KEY.len());
    walk_size(
        &result.value,
        result.secret.as_ref(),
        Leaving::Result,
        false,
        limit,
        &mut size,
    );
    if !modifications.is_empty() {
        size = size
            .saturating_add(str_size(MODIFICATIONS_KEY.len()))
            .saturating_add(container_size(modifications.len()));
        for modification in modifications {
            if size > limit {
                break;
            }
            // A two-entry map, its `ref` key and the Reference ID, its `value` key, the value.
            size = size
                .saturating_add(1 + str_size(REF_KEY.len()))
                .saturating_add(str_size(modification.reference.len()))
                .saturating_add(str_size(HOLDER_VALUE_KEY.len()));
            walk_size(
                &modification.value,
                None,
                Leaving::Call,
                true,
                limit,
                &mut size,
            );
        }
    }
    size
}

/// Add the exact encoded size of `value`, in a place with `secret`, to `size` — its holders as
/// they leave for `leaving`, none around the value itself when `bare` — stopping once `size`
/// passes `limit`.
fn walk_size(
    value: &Hexput,
    secret: Option<&Secret>,
    leaving: Leaving,
    bare: bool,
    limit: usize,
    size: &mut usize,
) {
    let mut pending: Vec<(&Hexput, Option<&Secret>, bool)> = vec![(value, secret, bare)];
    while let Some((value, secret, bare)) = pending.pop() {
        if *size > limit {
            break;
        }
        let holder = if bare {
            0
        } else {
            shown(value, secret, leaving).map_or(0, holder_size)
        };
        let bytes = match value {
            Hexput::Number(number) => number_size(*number),
            Hexput::String(text) => str_size(text.len()),
            Hexput::Array(array) => {
                let before = pending.len();
                pending.extend(
                    array
                        .iter_with_secrets()
                        .map(|(item, secret)| (item, secret, false)),
                );
                container_size(pending.len() - before)
            }
            Hexput::Object(object) => {
                let mut bytes = 0usize;
                let mut entries = 0usize;
                for (key, item, secret) in object.iter_with_secrets() {
                    bytes = bytes.saturating_add(str_size(key.len()));
                    entries += 1;
                    pending.push((item, secret, false));
                }
                bytes.saturating_add(container_size(entries))
            }
            // `null`, a bool — and any kind with no wire form, which `to_wire` sends as nil.
            _ => 1,
        };
        *size = size.saturating_add(bytes).saturating_add(holder);
    }
}

/// What a holder adds around its value on the wire, exactly as [`to_wire_held`] writes it: the
/// two-entry holder map, its `__secret` and `value` keys, and the `__secret` map — `ref`, `key`
/// when there is one, then the further fields.
fn holder_size(secret: &Secret) -> usize {
    let (fields, pairs) = extra_layout(secret.extra());
    let key = secret
        .key()
        .map_or(0, |key| str_size(KEY_KEY.len()) + str_size(key.len()));
    let entries = 1 + usize::from(secret.key().is_some()) + fields;
    1 + str_size(SECRET_KEY.len())
        + container_size(entries)
        + str_size(REF_KEY.len())
        + str_size(secret.reference().len())
        + key
        + pairs
        + str_size(HOLDER_VALUE_KEY.len())
}

/// How many further fields an encoded `extra` holds, and how many bytes they take without the
/// map header around them. Anything but a map is no fields, as [`extra_fields`] reads it.
fn extra_layout(extra: &[u8]) -> (usize, usize) {
    let (fields, header) = match extra {
        [] => return (0, 0),
        [marker @ 0x80..=0x8f, ..] => (usize::from(marker & 0x0f), 1),
        [0xde, a, b, ..] => (usize::from(u16::from_be_bytes([*a, *b])), 3),
        [0xdf, a, b, c, d, ..] => (u32::from_be_bytes([*a, *b, *c, *d]) as usize, 5),
        _ => return (0, 0),
    };
    (fields, extra.len().saturating_sub(header))
}

/// The further fields of a Value Secret, decoded from the map [`to_hexput`] encoded them as.
fn extra_fields(extra: &[u8]) -> Vec<(Wire, Wire)> {
    if extra.is_empty() {
        return Vec::new();
    }
    match rmpv::decode::read_value(&mut &*extra) {
        Ok(Wire::Map(fields)) => fields,
        _ => Vec::new(),
    }
}

/// A Value Secret as the wire's `__secret` map: `ref`, `key` when there is one, then the further
/// fields in their order.
fn secret_to_wire(secret: &Secret) -> Wire {
    let mut fields = vec![(Wire::from(REF_KEY), Wire::from(secret.reference()))];
    if let Some(key) = secret.key() {
        fields.push((Wire::from(KEY_KEY), Wire::from(key)));
    }
    fields.extend(extra_fields(secret.extra()));
    Wire::Map(fields)
}

/// The payload key a Script result travels under.
const VALUE_KEY: &str = "value";

/// A MessagePack string of `len` bytes: fixstr, str8, str16 or str32 header, then the bytes.
const fn str_size(len: usize) -> usize {
    let header = if len < 32 {
        1
    } else if len <= u8::MAX as usize {
        2
    } else if len <= u16::MAX as usize {
        3
    } else {
        5
    };
    header + len
}

/// A MessagePack array or map header for `len` items: fix, 16 or 32.
const fn container_size(len: usize) -> usize {
    if len < 16 {
        1
    } else if len <= u16::MAX as usize {
        3
    } else {
        5
    }
}

/// A number as [`number_to_wire`] converts it, MessagePack-encoded.
fn number_size(number: f64) -> usize {
    match number_to_wire(number) {
        Wire::Integer(integer) => match (integer.as_u64(), integer.as_i64()) {
            (Some(unsigned), _) => match unsigned {
                0..=0x7f => 1,
                0x80..=0xff => 2,
                0x100..=0xffff => 3,
                0x1_0000..=0xffff_ffff => 5,
                _ => 9,
            },
            (None, Some(signed)) => match signed {
                -32..=-1 => 1,
                -128..=-33 => 2,
                -32_768..=-129 => 3,
                -2_147_483_648..=-32_769 => 5,
                _ => 9,
            },
            (None, None) => 9,
        },
        // A float64: its marker and eight bytes.
        _ => 9,
    }
}

/// One container level deeper, refused past `max_depth`.
const fn nested(depth: usize, max_depth: usize) -> Result<usize, Unsendable> {
    let depth = depth + 1;
    if depth > max_depth {
        return Err(Unsendable::TooDeep);
    }
    Ok(depth)
}

/// Convert a value [`measure`] accepted. Recursive, which the walk has bounded to the depth it
/// was measured against. An array or object carrying a secret, and anything inside one carrying
/// one, travels as a holder (Story 3.11); a value without secrets exactly as before.
#[must_use]
pub fn to_wire(value: &Hexput) -> Wire {
    to_wire_held(value, None)
}

/// [`to_wire`] for a value in a place with the secret `secret`: a holder whenever the value
/// travels with a secret — an array's or object's own, otherwise the place's.
///
/// Every secret travels, a generated one included: this is what a `Call` sends. A Script result
/// goes through [`result_to_wire`], which leaves generated secrets out.
#[must_use]
pub fn to_wire_held(value: &Hexput, secret: Option<&Secret>) -> Wire {
    encode(value, secret, Leaving::Call)
}

fn encode(value: &Hexput, secret: Option<&Secret>, leaving: Leaving) -> Wire {
    let plain = plain(value, leaving);
    match shown(value, secret, leaving) {
        Some(secret) => Wire::Map(vec![
            (Wire::from(SECRET_KEY), secret_to_wire(secret)),
            (Wire::from(HOLDER_VALUE_KEY), plain),
        ]),
        None => plain,
    }
}

/// `value` with no holder around it — everything inside it with theirs.
fn plain(value: &Hexput, leaving: Leaving) -> Wire {
    match value {
        Hexput::Null => Wire::Nil,
        Hexput::Bool(b) => Wire::Boolean(*b),
        Hexput::Number(number) => number_to_wire(*number),
        Hexput::String(text) => Wire::from(&**text),
        Hexput::Array(array) => Wire::Array(
            array
                .iter_with_secrets()
                .map(|(item, secret)| encode(item, secret, leaving))
                .collect(),
        ),
        Hexput::Object(object) => Wire::Map(
            object
                .iter_with_secrets()
                .map(|(key, item, secret)| (Wire::from(key), encode(item, secret, leaving)))
                .collect(),
        ),
        // `measure` refuses every kind it does not know, so this is never reached.
        _ => Wire::Nil,
    }
}

/// A finite whole number within ±2^53 as a MessagePack integer (`-0` as `0`); every other
/// number as a float64.
fn number_to_wire(number: f64) -> Wire {
    // 2^53 is exactly representable, and a whole number within ±2^53 fits an i64 exactly.
    if number.fract() == 0.0 && number.abs() <= EXACT_INTEGER as f64 {
        Wire::from(number as i64)
    } else {
        Wire::F64(number)
    }
}
