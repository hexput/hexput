//! Hexput values to and from the wire's MessagePack values.
//!
//! Inbound, a starting variable's value becomes a Hexput value **losslessly or not at all**:
//! every MessagePack shape with no exact Hexput counterpart is refused with the path to it, never
//! rounded, truncated or dropped. Outbound, a Script result is walked iteratively first, so a
//! result the Daemon could not send — nested past what its own decoder accepts, or certain to
//! exceed a frame — is refused before the recursive conversion and the recursive encoder ever see
//! it.

use core::fmt::Write as _;
use std::collections::HashSet;
use std::sync::Arc;

use hexput_interpreter::{Array, Object, Value as Hexput};
use hexput_port::{MAX_FRAME_LEN, MAX_NESTING_DEPTH, ProtocolCode, ProtocolError, Value as Wire};

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

/// Where a value sits inside the `ExecutionStart` payload, rendered only on failure.
pub(crate) struct Path<'a> {
    segments: Vec<Segment<'a>>,
}

impl<'a> Path<'a> {
    /// The path of one starting variable, `variables.<name>`.
    pub(crate) fn variable(name: &'a str) -> Self {
        Self {
            segments: vec![Segment::Key(name)],
        }
    }

    /// `variables.user.tags[2]`, with non-identifier keys quoted and every part bounded.
    fn render(&self) -> String {
        let mut out = String::from("`variables");
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
pub(crate) fn bounded(text: &str) -> String {
    bounded_to(text, SEGMENT_LIMIT)
}

fn bounded_to(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Convert one starting variable's wire value into a Hexput value.
///
/// Recursive, and bounded: a payload the Daemon decoded is at most [`MAX_NESTING_DEPTH`] levels
/// deep, and a value handed in directly deeper than that is refused rather than followed.
///
/// # Errors
///
/// The message for `protocol.invalid_payload`, naming the path to the first value with no
/// lossless Hexput representation.
pub(crate) fn to_hexput<'a>(value: &'a Wire, path: &mut Path<'a>) -> Result<Hexput, String> {
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

/// Why a Script result cannot be sent.
pub(crate) enum Unsendable {
    /// Refused with this protocol error.
    Protocol(ProtocolError),
    /// The result holds a value kind this crate does not know how to put on the wire.
    Unrepresentable,
}

/// Walk a Script result without recursion and refuse it if it is nested deeper than
/// [`MAX_RESULT_DEPTH`], or certain to encode past [`MAX_FRAME_LEN`].
///
/// The size check is a lower bound — every value encodes to at least one byte, and a string or
/// key to at least its own bytes — so it never refuses a result that would have fit. It also
/// bounds the walk itself: a result that shares one collection many times over (`[x, x]` nested
/// repeatedly) is small in the execution but exponential on the wire, and the walk stops as soon
/// as the bound is passed instead of expanding it.
pub(crate) fn check_result(result: &Hexput) -> Result<(), Unsendable> {
    let too_large = || {
        Unsendable::Protocol(ProtocolError::new(
            ProtocolCode::ResponseTooLarge,
            format!(
                "the Script's result encodes to more than the maximum frame of {MAX_FRAME_LEN} \
                 bytes"
            ),
        ))
    };
    let mut bytes: usize = 0;
    let mut pending: Vec<(&Hexput, usize)> = vec![(result, 0)];
    while let Some((value, depth)) = pending.pop() {
        bytes = bytes.saturating_add(1);
        match value {
            Hexput::Null | Hexput::Bool(_) | Hexput::Number(_) => {}
            Hexput::String(text) => bytes = bytes.saturating_add(text.len()),
            Hexput::Array(array) => {
                let depth = nested(depth)?;
                pending.extend(array.iter().map(|item| (item, depth)));
            }
            Hexput::Object(object) => {
                let depth = nested(depth)?;
                for (key, item) in object.iter() {
                    bytes = bytes.saturating_add(key.len());
                    pending.push((item, depth));
                }
            }
            _ => return Err(Unsendable::Unrepresentable),
        }
        if bytes > MAX_FRAME_LEN {
            return Err(too_large());
        }
    }
    Ok(())
}

/// One container level deeper, refused past [`MAX_RESULT_DEPTH`].
fn nested(depth: usize) -> Result<usize, Unsendable> {
    let depth = depth + 1;
    if depth > MAX_RESULT_DEPTH {
        return Err(Unsendable::Protocol(ProtocolError::new(
            ProtocolCode::ResultTooDeep,
            format!(
                "the Script's result nests more than {MAX_RESULT_DEPTH} arrays or objects deep, \
                 past what a frame may carry"
            ),
        )));
    }
    Ok(depth)
}

/// Convert a result [`check_result`] accepted. Recursive, which the check has bounded to
/// [`MAX_RESULT_DEPTH`] levels.
pub(crate) fn to_wire(value: &Hexput) -> Wire {
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
        // `check_result` refuses every kind it does not know, so this is never reached.
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
