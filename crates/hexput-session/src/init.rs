//! Decoding an `Init` payload into what a Session is created from.
//!
//! By hand over the untyped [`Value`], like the envelope itself: serde's messages do not
//! reliably name a missing key, and a Backend is owed the exact key or index that was wrong.

use core::fmt;
use std::collections::HashMap;

use hexput_port::Value;

/// The per-backend Config a Backend hands over at init (FR-1). Never a file, and never read,
/// written or reloaded alongside System Config (AD-5).
///
/// No keys are defined until Epic 3, so a Config is empty and an `Init` whose `config` names
/// any key is refused: a Backend must never believe a policy is in force that nothing
/// enforces. Not `Clone`: the Session registry holds the single live copy.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {}

/// A function the Backend exposes to its Scripts, named at init (FR-1, FR-6), with its
/// Capability grant.
///
/// On the wire a registration is `{name, blanket}`, `blanket` an optional boolean. A blanket
/// grant (Story 3.2) makes the function callable by every Script of the Session with no per-call
/// round trip. Without one the function is not callable at all until Story 3.3 adds the per-call
/// handler: the Daemon fails closed. Whether a call may go ahead is decided in `hexput-enforce`,
/// never here (AD-3) — this is only the Session's record of what the Backend said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredFunction {
    name: String,
    blanket: bool,
}

impl RegisteredFunction {
    /// The name a Script calls it by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the Backend granted it blanket at registration.
    #[must_use]
    pub fn blanket(&self) -> bool {
        self.blanket
    }
}

/// A decoded, valid `Init` payload: the Backend's Config and its Registered Functions.
#[derive(Debug, PartialEq, Eq)]
pub struct InitRequest {
    pub(crate) config: Config,
    pub(crate) registrations: Vec<RegisteredFunction>,
}

/// Why an `Init` payload was refused, naming the offending key or index. No Session is ever
/// created from a refused payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitError {
    message: String,
}

impl InitError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// What exactly was wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl core::error::Error for InitError {}

const CONFIG: &str = "config";
const REGISTRATIONS: &str = "registrations";
const NAME: &str = "name";
const BLANKET: &str = "blanket";

impl InitRequest {
    /// Decode an `Init` payload: a map with exactly the keys `config` and `registrations`.
    ///
    /// A key that is absent or nil is *missing*, and both missing keys are reported together.
    /// An empty `registrations` array is valid — a Backend exposing nothing — because empty is
    /// not missing.
    ///
    /// # Errors
    ///
    /// An [`InitError`] naming the first offending key or index.
    pub fn from_value(payload: &Value) -> Result<Self, InitError> {
        let fields: &[(Value, Value)] = match payload {
            Value::Nil => &[],
            Value::Map(fields) => fields,
            _ => {
                return Err(InitError::new(format!(
                    "the `Init` payload must be a map with `{CONFIG}` and `{REGISTRATIONS}`"
                )));
            }
        };

        let mut config = None;
        let mut registrations = None;
        for (key, value) in fields {
            let slot = match key.as_str() {
                Some(CONFIG) => &mut config,
                Some(REGISTRATIONS) => &mut registrations,
                _ => {
                    return Err(InitError::new(format!(
                        "the `Init` payload has an unknown key {}",
                        describe_key(key)
                    )));
                }
            };
            if slot.is_some() {
                return Err(InitError::new(format!(
                    "the `Init` payload repeats the key {}",
                    describe_key(key)
                )));
            }
            *slot = Some(value);
        }

        let (config, registrations) = match (present(config), present(registrations)) {
            (Some(config), Some(registrations)) => (config, registrations),
            (None, Some(_)) => return Err(missing(&[CONFIG])),
            (Some(_), None) => return Err(missing(&[REGISTRATIONS])),
            (None, None) => return Err(missing(&[CONFIG, REGISTRATIONS])),
        };

        Ok(Self {
            config: decode_config(config)?,
            registrations: decode_registrations(registrations)?,
        })
    }

    /// The Registered Functions, in the order the Backend listed them.
    #[must_use]
    pub fn registrations(&self) -> &[RegisteredFunction] {
        &self.registrations
    }
}

/// A key that is absent or nil is missing.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !v.is_nil())
}

fn missing(keys: &[&str]) -> InitError {
    let names: Vec<String> = keys.iter().map(|k| format!("`{k}`")).collect();
    InitError::new(format!(
        "the `Init` payload is missing {}",
        names.join(" and ")
    ))
}

/// The most characters of Backend input a refusal echoes back, so a refusal always fits one
/// frame however large the offending key or name.
const ECHO_LIMIT: usize = 64;

/// The most offending `config` keys a refusal lists before summarizing the rest.
const LISTED_KEYS: usize = 3;

/// `text` cut to [`ECHO_LIMIT`] characters, with an ellipsis when anything was cut.
fn bounded(text: &str) -> String {
    match text.char_indices().nth(ECHO_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// A map key as a Backend would recognise it: a string in backticks, anything else as the
/// MessagePack value it was — either bounded to [`ECHO_LIMIT`] characters.
fn describe_key(key: &Value) -> String {
    key.as_str().map_or_else(
        || {
            // Render at most one character past the limit, however large the value.
            let mut shown = String::new();
            let _ = fmt::write(
                &mut Truncating {
                    out: &mut shown,
                    room: ECHO_LIMIT + 1,
                },
                format_args!("{key}"),
            );
            format!("{} (not a string)", bounded(&shown))
        },
        |k| format!("`{}`", bounded(k)),
    )
}

/// A `fmt::Write` that keeps the first `room` characters and then refuses the rest, so
/// formatting a huge value stops early instead of building it in full.
struct Truncating<'a> {
    out: &'a mut String,
    room: usize,
}

impl fmt::Write for Truncating<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            if self.room == 0 {
                return Err(fmt::Error);
            }
            self.out.push(c);
            self.room -= 1;
        }
        Ok(())
    }
}

fn decode_config(value: &Value) -> Result<Config, InitError> {
    let Value::Map(fields) = value else {
        return Err(InitError::new(format!("`{CONFIG}` is not a map")));
    };
    if fields.is_empty() {
        return Ok(Config {});
    }
    let keys: Vec<String> = fields
        .iter()
        .take(LISTED_KEYS)
        .map(|(key, _)| describe_key(key))
        .collect();
    let rest = fields.len().saturating_sub(LISTED_KEYS);
    let more = if rest == 0 {
        String::new()
    } else {
        format!(" and {rest} more")
    };
    Err(InitError::new(format!(
        "`{CONFIG}` accepts no keys yet; found {}{more}",
        keys.join(", ")
    )))
}

fn decode_registrations(value: &Value) -> Result<Vec<RegisteredFunction>, InitError> {
    let Value::Array(entries) = value else {
        return Err(InitError::new(format!("`{REGISTRATIONS}` is not an array")));
    };
    let mut registrations: Vec<RegisteredFunction> = Vec::with_capacity(entries.len());
    // Name -> first index, so a duplicate is found without a quadratic scan of a large list.
    let mut seen: HashMap<&str, usize> = HashMap::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        // Built only on failure: a long valid list costs no formatting.
        let at = || format!("`{REGISTRATIONS}[{index}]`");
        let Value::Map(fields) = entry else {
            return Err(InitError::new(format!("{} is not a map", at())));
        };
        let mut name = None;
        let mut blanket = None;
        for (key, value) in fields {
            let (slot, spelled) = match key.as_str() {
                Some(NAME) => (&mut name, NAME),
                Some(BLANKET) => (&mut blanket, BLANKET),
                _ => {
                    return Err(InitError::new(format!(
                        "{} has an unknown key {}",
                        at(),
                        describe_key(key)
                    )));
                }
            };
            if slot.is_some() {
                return Err(InitError::new(format!(
                    "{} repeats the key `{spelled}`",
                    at()
                )));
            }
            *slot = Some(value);
        }
        // Absent means no blanket grant; anything present must be a boolean, so a Backend never
        // believes it granted (or withheld) something the Daemon read otherwise.
        let blanket = match blanket {
            None => false,
            Some(Value::Boolean(blanket)) => *blanket,
            Some(_) => {
                return Err(InitError::new(format!(
                    "`{REGISTRATIONS}[{index}].{BLANKET}` is not a boolean"
                )));
            }
        };
        let field = || format!("`{REGISTRATIONS}[{index}].{NAME}`");
        let Some(name) = name else {
            return Err(InitError::new(format!("{} is missing", field())));
        };
        let Some(name) = name.as_str() else {
            return Err(InitError::new(format!("{} is not a string", field())));
        };
        if name.is_empty() {
            return Err(InitError::new(format!("{} is empty", field())));
        }
        if let Some(first) = seen.insert(name, index) {
            return Err(InitError::new(format!(
                "`{}` is registered twice, at `{REGISTRATIONS}[{first}]` and \
                 `{REGISTRATIONS}[{index}]`",
                bounded(name)
            )));
        }
        registrations.push(RegisteredFunction {
            name: name.to_owned(),
            blanket,
        });
    }
    Ok(registrations)
}
