//! Decoding the tunable execution limits (Story 3.7) from the wire: one decoder for a Session's
//! `Init.config`, a `ConfigUpdate`'s `config` (Story 3.8) and an `ExecutionStart`'s `overrides`,
//! so they can never disagree on a key, a type or a range.
//!
//! The shape is the [`Setting`] table's dotted paths as nested maps:
//!
//! ```text
//! { budget: { cpu_time_ms, memory_bytes, allocations, rpc_calls, output_size_bytes, side_effects },
//!   argument_depth, authorization_timeout_ms,
//!   features: { loops, conditionals, callbacks, object_literals, array_literals, rpc_calls },
//!   check: "off" | "warn" | "error" }
//! ```
//!
//! Every key is optional, and `{}` sets nothing. Each value is a MessagePack integer within its
//! setting's range; a float — even a whole one — a negative or out-of-range integer, or any other
//! type is refused, as is an unknown, repeated or non-string key. Every refusal names the full
//! path, and a value refused for its range names the range.
//!
//! The `features` map (Story 3.9) holds the language feature toggles: each key optional, each
//! value a MessagePack boolean. A key outside the closed set is refused as an unknown toggle, and
//! any value but a boolean — `0`, `"no"`, nil — as not a boolean.
//!
//! The root `check` key (Story 3.10) holds the static check mode as a string: `"off"`, `"warn"`
//! or `"error"`. Any other string is refused, echoed bounded; any other type is named by its type.

use core::fmt;

use rmpv::Value;

pub use hexput_shared::budget::{OutOfRange, Setting, Settings};
pub use hexput_shared::policy::{CheckMode, Feature, Features};

use crate::MAX_FRAME_LEN;

// The setting table's output ceiling is the maximum frame: a result past it can never be sent.
const _: () = assert!(Setting::OutputSizeBytes.max() == MAX_FRAME_LEN as u64);

/// The most characters of a Backend's key a refusal echoes back, so a refusal always fits one
/// frame however large the key.
const ECHO_LIMIT: usize = 64;

/// Decode a settings map found at `prefix` — `config` for an `Init`'s or a `ConfigUpdate`'s
/// Config, `overrides` for an `ExecutionStart`'s — into the [`Settings`] it sets.
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

/// The key under which the feature toggles sit, at the root of a settings map.
const FEATURES: &str = "features";

/// The key under which the static check mode sits, at the root of a settings map.
const CHECK: &str = "check";

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
        if relative.is_empty() && key == FEATURES {
            if seen.contains(&key) {
                return Err(format!("`{}` repeats the key `{key}`", at()));
            }
            seen.push(key);
            decode_features(
                value,
                &Path {
                    prefix,
                    relative: FEATURES,
                },
                settings,
            )?;
            continue;
        }
        if relative.is_empty() && key == CHECK {
            if seen.contains(&key) {
                return Err(format!("`{}` repeats the key `{key}`", at()));
            }
            seen.push(key);
            decode_check(
                value,
                &Path {
                    prefix,
                    relative: CHECK,
                },
                settings,
            )?;
            continue;
        }
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

/// Decode the feature toggles map found at `path` into `settings`.
fn decode_features(value: &Value, path: &Path<'_>, settings: &mut Settings) -> Result<(), String> {
    let Value::Map(fields) = value else {
        return Err(format!("`{path}` is not a map"));
    };
    let mut seen: Vec<Feature> = Vec::new();
    for (key, value) in fields {
        let Some(key) = key.as_str() else {
            return Err(format!("`{path}` has a key that is not a string"));
        };
        let Some(feature) = Feature::from_name(key) else {
            let toggles: Vec<&str> = Feature::ALL.iter().map(|f| f.as_str()).collect();
            return Err(format!(
                "`{path}.{}` is not a known feature toggle (the toggles are {})",
                bounded(key),
                toggles.join(", ")
            ));
        };
        if seen.contains(&feature) {
            return Err(format!("`{path}` repeats the key `{key}`"));
        }
        seen.push(feature);
        match value {
            Value::Boolean(enabled) => settings.set_feature(feature, *enabled),
            other => {
                return Err(format!(
                    "`{path}.{key}` must be a boolean; found {}",
                    describe(other)
                ));
            }
        }
    }
    Ok(())
}

/// Decode the static check mode found at `path` into `settings`.
fn decode_check(value: &Value, path: &Path<'_>, settings: &mut Settings) -> Result<(), String> {
    if let Some(mode) = value.as_str().and_then(CheckMode::from_name) {
        settings.set_check(mode);
        return Ok(());
    }
    let found = match value {
        Value::String(text) => match text.as_str() {
            Some(text) => format!("\"{}\"", bounded(text)),
            None => "a string that is not valid UTF-8".to_owned(),
        },
        other => describe(other),
    };
    let modes: Vec<String> = CheckMode::ALL
        .iter()
        .map(|m| format!("\"{}\"", m.as_str()))
        .collect();
    Err(format!(
        "`{path}` must be one of {}; found {found}",
        modes.join(", ")
    ))
}

/// How a refusal names a value it found.
fn describe(value: &Value) -> String {
    match value {
        Value::Integer(integer) => integer.as_u64().map_or_else(
            || integer.as_i64().unwrap_or_default().to_string(),
            |unsigned| unsigned.to_string(),
        ),
        Value::F32(_) | Value::F64(_) => "a float".to_owned(),
        Value::Nil => "nil".to_owned(),
        Value::Boolean(_) => "a boolean".to_owned(),
        Value::String(_) => "a string".to_owned(),
        Value::Binary(_) => "binary data".to_owned(),
        Value::Array(_) => "an array".to_owned(),
        Value::Map(_) => "a map".to_owned(),
        Value::Ext(..) => "an extension".to_owned(),
    }
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
            None => describe(value),
        },
        other => describe(other),
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
