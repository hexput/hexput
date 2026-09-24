//! Decoding the tunable execution limits (Story 3.7) from the wire: one decoder for a Session's
//! `Init.config` and an `ExecutionStart`'s `overrides`, so the two can never disagree on a key, a
//! type or a range.
//!
//! The shape is the [`Setting`] table's dotted paths as nested maps:
//!
//! ```text
//! { budget: { cpu_time_ms, memory_bytes, allocations, rpc_calls, output_size_bytes, side_effects },
//!   argument_depth, authorization_timeout_ms }
//! ```
//!
//! Every key is optional, and `{}` sets nothing. Each value is a MessagePack integer within its
//! setting's range; a float — even a whole one — a negative or out-of-range integer, or any other
//! type is refused, as is an unknown, repeated or non-string key. Every refusal names the full
//! path, and a value refused for its range names the range.

use core::fmt;

use rmpv::Value;

pub use hexput_shared::budget::{OutOfRange, Setting, Settings};

use crate::MAX_FRAME_LEN;

// The setting table's output ceiling is the maximum frame: a result past it can never be sent.
const _: () = assert!(Setting::OutputSizeBytes.max() == MAX_FRAME_LEN as u64);

/// The most characters of a Backend's key a refusal echoes back, so a refusal always fits one
/// frame however large the key.
const ECHO_LIMIT: usize = 64;

/// Decode a settings map found at `prefix` — `config` for an `Init`'s Config, `overrides` for an
/// `ExecutionStart`'s — into the [`Settings`] it sets.
///
/// # Errors
///
/// A message naming the offending path in backticks under `prefix`, and for a value its range:
/// `` `config.budget.rpc_calls` must be an integer from 0 to 100000; found 200000 ``.
pub fn decode_settings(value: &Value, prefix: &str) -> Result<Settings, String> {
    let mut settings = Settings::new();
    decode_map(value, prefix, "", &mut settings)?;
    Ok(settings)
}

/// Decode the map at `relative` (a dotted path within the settings, `""` for the root) into
/// `settings`.
fn decode_map(
    value: &Value,
    prefix: &str,
    relative: &str,
    settings: &mut Settings,
) -> Result<(), String> {
    let at = || Path { prefix, relative };
    let Value::Map(fields) = value else {
        return Err(format!("`{}` is not a map", at()));
    };
    let mut seen: Vec<&str> = Vec::new();
    for (key, value) in fields {
        let Some(key) = key.as_str() else {
            return Err(format!("`{}` has a key that is not a string", at()));
        };
        let path = if relative.is_empty() {
            key.to_owned()
        } else {
            format!("{relative}.{key}")
        };
        let setting = Setting::ALL.iter().copied().find(|s| s.path() == path);
        let group = || {
            Setting::ALL.iter().any(|s| {
                s.path()
                    .strip_prefix(path.as_str())
                    .is_some_and(|r| r.starts_with('.'))
            })
        };
        // A dotted key is never a setting, even one whose spelling matches a nested path.
        if key.contains('.') || (setting.is_none() && !group()) {
            return Err(format!(
                "`{}` is not a known setting",
                Path {
                    prefix,
                    relative: &bounded(&path),
                }
            ));
        }
        // Only a known key is compared, so the scan is bounded by the table's size.
        if seen.contains(&key) {
            return Err(format!("`{}` repeats the key `{key}`", at()));
        }
        seen.push(key);
        match setting {
            Some(setting) => decode_value(
                value,
                setting,
                &Path {
                    prefix,
                    relative: &path,
                },
                settings,
            )?,
            None => decode_map(value, prefix, &path, settings)?,
        }
    }
    Ok(())
}

/// Decode one setting's value, found at `path`, into `settings`.
fn decode_value(
    value: &Value,
    setting: Setting,
    path: &Path<'_>,
    settings: &mut Settings,
) -> Result<(), String> {
    let found = match value {
        Value::Integer(integer) => match integer.as_u64() {
            Some(unsigned) => match settings.set(setting, unsigned) {
                Ok(()) => return Ok(()),
                Err(out_of_range) => out_of_range.found().to_string(),
            },
            // Only a negative integer does not fit a `u64`.
            None => integer.as_i64().unwrap_or_default().to_string(),
        },
        Value::F32(_) | Value::F64(_) => "a float".to_owned(),
        Value::Nil => "nil".to_owned(),
        Value::Boolean(_) => "a boolean".to_owned(),
        Value::String(_) => "a string".to_owned(),
        Value::Binary(_) => "binary data".to_owned(),
        Value::Array(_) => "an array".to_owned(),
        Value::Map(_) => "a map".to_owned(),
        Value::Ext(..) => "an extension".to_owned(),
    };
    Err(format!(
        "`{path}` must be an integer from {} to {}; found {found}",
        setting.min(),
        setting.max()
    ))
}

/// A dotted path under its prefix, displayed as `prefix.relative` (or `prefix` alone at the
/// root).
struct Path<'a> {
    prefix: &'a str,
    relative: &'a str,
}

impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.relative.is_empty() {
            f.write_str(self.prefix)
        } else {
            write!(f, "{}.{}", self.prefix, self.relative)
        }
    }
}

/// `text` cut to [`ECHO_LIMIT`] characters, with an ellipsis when anything was cut.
fn bounded(text: &str) -> String {
    match text.char_indices().nth(ECHO_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
