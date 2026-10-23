//! How static check findings travel (Stories 3.10, 4.1, 6.3): the one rule every submission path
//! — Direct Execution, `CodeRegister` and `PluginRegister` — shapes, bounds and fits them by, so
//! the paths can never disagree (AD-8).
//!
//! * [`MAX_FINDINGS`] bounds how many one reply carries, in source order.
//! * [`shape`] turns a pass's diagnostics into the bodies a reply carries, or — under check mode
//!   `error`, for a source with an error-severity finding — into the [`Rejection`].
//! * [`fitted`] and [`with_findings`] drop an error body's findings when they would not fit a
//!   frame; [`push_findings`] appends them to a successful reply only when they fit.
//!
//! Which mode is in force, and logging a rejection, stay with the caller.

use hexput_shared::diagnostics::{Diagnostic, Severity};
use rmpv::Value;

use crate::error::{ErrorBody, findings_value};
use crate::frame::MAX_FRAME_LEN;

/// The most static check findings one reply carries, in source order. A source with more has the
/// rest dropped — except that a rejection's first error is always kept, in place of the last one,
/// so the rejection's own body is always among its `findings`.
pub const MAX_FINDINGS: usize = 100;

/// Room left in a frame for everything around a reply's payload: the envelope map, its `id`,
/// `type` and `payload` keys, the largest id and the longest type name, with margin.
pub const ENVELOPE_ROOM: usize = 64;

/// The `findings` key of a reply.
const FINDINGS: &str = "findings";

/// A source the static check rejected: the reply's body (not yet fitted to a frame), and how many
/// findings the pass made before [`MAX_FINDINGS`] applied — for the caller's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The first error-severity finding's body, with every kept finding under `findings`.
    pub body: Box<ErrorBody>,
    /// How many findings the pass made.
    pub found: usize,
}

/// The findings `all` — one pass's diagnostics, in source order — as a reply carries them: at
/// most [`MAX_FINDINGS`] bodies, in source order.
///
/// # Errors
///
/// When `reject` (check mode `error`) and some finding is an error: the [`Rejection`], whose body
/// is the first error's with every kept finding — warnings too — under `findings`. When that
/// error lies past the cap it replaces the last kept finding, so source order holds and the
/// rejection is among its own findings.
pub fn shape(all: &[Diagnostic], reject: bool) -> Result<Vec<ErrorBody>, Rejection> {
    let mut bodies: Vec<ErrorBody> = all.iter().take(MAX_FINDINGS).map(ErrorBody::from).collect();
    if reject
        && let Some(at) = all
            .iter()
            .position(|finding| finding.severity == Severity::Error)
    {
        let first = ErrorBody::from(&all[at]);
        if at >= MAX_FINDINGS {
            bodies.pop();
            bodies.push(first.clone());
        }
        let mut body = first;
        body.findings = bodies;
        return Err(Rejection {
            body: Box::new(body),
            found: all.len(),
        });
    }
    Ok(bodies)
}

/// `body` carrying `findings`, fitted to a frame.
#[must_use]
pub fn with_findings(mut body: ErrorBody, findings: Vec<ErrorBody>) -> Box<ErrorBody> {
    body.findings = findings;
    fitted(Box::new(body))
}

/// `body` as it can be sent: without its `findings` when with them it would not fit a frame.
#[must_use]
pub fn fitted(mut body: Box<ErrorBody>) -> Box<ErrorBody> {
    if !body.findings.is_empty() && encoded_len(&body.to_value()) > MAX_FRAME_LEN - ENVELOPE_ROOM {
        body.findings.clear();
    }
    body
}

/// Append `findings` to a successful reply's `fields` under `findings`, unless the reply — `size`
/// bytes without them — would then not fit a frame. The one fitting rule for every successful
/// reply that carries findings (Direct Execution's `{value}`, a registration's `{handle}` or
/// `{name}`). Nothing is appended when there are none.
pub fn push_findings(fields: &mut Vec<(Value, Value)>, size: usize, findings: &[ErrorBody]) {
    if findings.is_empty() {
        return;
    }
    let listed = findings_value(findings);
    // The `findings` key and value, beside the payload without them.
    let size = size
        .saturating_add(FINDINGS.len() + 1)
        .saturating_add(encoded_len(&listed));
    if size <= MAX_FRAME_LEN - ENVELOPE_ROOM {
        fields.push((Value::from(FINDINGS), listed));
    }
}

/// The MessagePack length of `value`.
#[must_use]
pub fn encoded_len(value: &Value) -> usize {
    let mut bytes = Vec::new();
    // Writing to a `Vec` cannot fail.
    let _ = rmpv::encode::write_value(&mut bytes, value);
    bytes.len()
}
