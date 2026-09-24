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

use core::fmt::Write as _;
use std::collections::HashSet;
use std::sync::Arc;

use hexput_interpreter::{Array, Object, Value as Hexput};
use hexput_rpc::{MAX_FRAME_LEN, MAX_NESTING_DEPTH, Value as Wire};

/// The deepest container nesting a Script result may have. The Daemon's decoder accepts
/// [`MAX_NESTING_DEPTH`] levels per frame and the envelope map and the `{value}` payload map take
/// two of them, so a result nested any deeper could not be read back by a peer applying the same
/// limit.
pub const MAX_RESULT_DEPTH: usize = MAX_NESTING_DEPTH - 2;

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

/// Convert a wire value — a starting variable's, or a host call's reply — into a Hexput value.
///
/// Recursive, and bounded: a payload the Daemon decoded is at most [`MAX_NESTING_DEPTH`] levels
/// deep, and a value handed in directly deeper than that is refused rather than followed.
///
/// # Errors
///
/// A message naming the path to the first value with no lossless Hexput representation.
pub fn to_hexput<'a>(value: &'a Wire, path: &mut Path<'a>) -> Result<Hexput, String> {
    if path.segments.len() > MAX_NESTING_DEPTH {
        return Err(format!(
            "{} nests deeper than {MAX_NESTING_DEPTH} levels",
            path.render()
        ));
    }
    let refuse = |path: &Path<'_>, what: &str| Err(format!("{} {what}", path.render()));
    match value {
        Wire::Nil => Ok(Hexput::Null),
        Wire::Boolean(b) => Ok(Hexput::Bool(*b)),
        Wire::Integer(integer) => {
            let exact = match (integer.as_i64(), integer.as_u64()) {
                // Within ±2^53, so either conversion is exact.
                (Some(signed), _) if signed.unsigned_abs() <= EXACT_INTEGER => Some(signed as f64),
                (_, Some(unsigned)) if unsigned <= EXACT_INTEGER => Some(unsigned as f64),
                _ => None,
            };
            match exact {
                Some(number) => Ok(Hexput::Number(number)),
                None => refuse(
                    path,
                    &format!(
                        "is the integer {integer}, outside ±2^53, which a Hexput number cannot \
                         hold exactly"
                    ),
                ),
            }
        }
        Wire::F32(number) => finite(f64::from(*number), path),
        Wire::F64(number) => finite(*number, path),
        Wire::String(text) => match text.as_str() {
            Some(text) => Ok(Hexput::String(Arc::from(text))),
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
                let value = to_hexput(item, path)?;
                path.segments.pop();
                values.push(value);
            }
            Ok(Hexput::Array(Array::from_values(values)))
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
                let value = to_hexput(item, path)?;
                path.segments.pop();
                entries.push((key, value));
            }
            Ok(Hexput::Object(Object::from_entries(entries)))
        }
    }
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
    let mut pending: Vec<(&Hexput, usize)> = vec![(value, 0)];
    while let Some((value, depth)) = pending.pop() {
        let mut bytes: usize = 1;
        match value {
            Hexput::Null | Hexput::Bool(_) | Hexput::Number(_) => {}
            Hexput::String(text) => bytes = bytes.saturating_add(text.len()),
            Hexput::Array(array) => {
                let depth = nested(depth, max_depth)?;
                pending.extend(array.iter().map(|item| (item, depth)));
            }
            Hexput::Object(object) => {
                let depth = nested(depth, max_depth)?;
                for (key, item) in object.iter() {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((item, depth));
                }
            }
            _ => return Err(Unsendable::Unrepresentable),
        }
        *budget = budget.checked_sub(bytes).ok_or(Unsendable::TooLarge)?;
    }
    Ok(())
}

/// Walk a Script result: refused when it nests deeper than [`MAX_RESULT_DEPTH`], or is certain to
/// encode past [`MAX_FRAME_LEN`] (see [`measure`]).
///
/// # Errors
/// Which limit the result breaks.
pub fn check_result(result: &Hexput) -> Result<(), Unsendable> {
    let mut budget = MAX_FRAME_LEN;
    measure(result, MAX_RESULT_DEPTH, &mut budget)
}

/// The exact number of bytes the Script result `value` takes on the wire as the `{value}` payload
/// of a `Result` — the map, its `value` key and the value, each MessagePack-encoded as [`to_wire`]
/// converts it and the codec writes it (every integer, string, array and map header in its most
/// compact form) — or, once the count passes `limit`, some count past `limit`: the walk stops
/// there, so a result that shares one collection many times over is never expanded in full.
///
/// Walks without recursion, so a result of any depth is measured. Only the output size budget
/// uses it; whether the result may be sent at all is [`check_result`]'s.
#[must_use]
pub fn payload_size(value: &Hexput, limit: usize) -> usize {
    // A one-entry map (1 byte), its key `value` (a 5-byte fixstr: 6 bytes), then the value.
    let mut size: usize = 1 + str_size(VALUE_KEY.len());
    let mut pending: Vec<&Hexput> = vec![value];
    while let Some(value) = pending.pop() {
        if size > limit {
            break;
        }
        let bytes = match value {
            Hexput::Number(number) => number_size(*number),
            Hexput::String(text) => str_size(text.len()),
            Hexput::Array(array) => {
                let before = pending.len();
                pending.extend(array.iter());
                container_size(pending.len() - before)
            }
            Hexput::Object(object) => {
                let mut bytes = 0usize;
                let mut entries = 0usize;
                for (key, item) in object.iter() {
                    bytes = bytes.saturating_add(str_size(key.len()));
                    entries += 1;
                    pending.push(item);
                }
                bytes.saturating_add(container_size(entries))
            }
            // `null`, a bool — and any kind with no wire form, which `to_wire` sends as nil.
            _ => 1,
        };
        size = size.saturating_add(bytes);
    }
    size
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
/// was measured against.
#[must_use]
pub fn to_wire(value: &Hexput) -> Wire {
    match value {
        Hexput::Null => Wire::Nil,
        Hexput::Bool(b) => Wire::Boolean(*b),
        Hexput::Number(number) => number_to_wire(*number),
        Hexput::String(text) => Wire::from(&**text),
        Hexput::Array(array) => Wire::Array(array.iter().map(to_wire).collect()),
        Hexput::Object(object) => Wire::Map(
            object
                .iter()
                .map(|(key, item)| (Wire::from(key), to_wire(item)))
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
