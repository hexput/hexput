//! Decoding a `Reconnect` payload into the credential it presents (Story 5.6).
//!
//! By hand, like `Init`: a Backend is owed the exact key that was wrong. Every refusal is decided
//! from the payload alone, before any registry lookup, so its outcome never depends on whether a
//! Session exists — and no refusal ever echoes a presented Client ID or secret.

use core::str::FromStr;

use hexput_port::Value;
use hexput_shared::ids::ClientId;

use crate::credential::ReconnectSecret;
use crate::init::{InitError, describe_key, missing, present};

const MESSAGE: &str = "Reconnect";
const CLIENT_ID: &str = "client_id";
const RECONNECT_SECRET: &str = "reconnect_secret";

/// The longest unknown key a refusal echoes. Far below a Client ID (32 characters) or a secret
/// (64), so a credential sent as a key — a wrongly serialized struct — is never echoed back or
/// logged, while a stray `config` or `registrations` is still named.
const UNKNOWN_KEY_ECHO_LIMIT: usize = 16;

/// An unknown key as the refusal names it: a short string key as [`describe_key`] does, anything
/// else without its content.
fn describe_unknown_key(key: &Value) -> String {
    match key.as_str() {
        Some(text) if text.chars().count() <= UNKNOWN_KEY_ECHO_LIMIT => describe_key(key),
        Some(_) => "(a string too long to show)".to_owned(),
        None => "(not a string)".to_owned(),
    }
}

/// A decoded, well-formed `Reconnect` payload: the Client ID of the Session to resume and the
/// reconnect secret presented for it. Well-formed is all it is — whether the two match a live
/// Session is decided once, by [`Sessions::reconnect`](crate::Sessions::reconnect).
///
/// Its `Debug` shows the Client ID and redacts the secret.
#[derive(Debug)]
pub struct ReconnectRequest {
    client_id: ClientId,
    secret: ReconnectSecret,
}

impl ReconnectRequest {
    /// Decode a `Reconnect` payload: a map with exactly the keys `client_id` (32 lowercase hex
    /// characters) and `reconnect_secret` (64 lowercase hex characters).
    ///
    /// A key that is absent or nil is *missing*, and both missing keys are reported together.
    /// Any other key — `config` and `registrations` included — is refused, never ignored: a
    /// reconnect resumes the Session's stored Config and registrations, and a Backend must not
    /// believe a Config it carried was applied.
    ///
    /// # Errors
    ///
    /// An [`InitError`] naming what was wrong: a payload that is not a map, an unknown or repeated
    /// key, a missing key, or a value that is not a string of the right form. The message never
    /// contains a presented value, and names an unknown key only when it is a short string.
    pub fn from_value(payload: &Value) -> Result<Self, InitError> {
        let fields: &[(Value, Value)] = match payload {
            Value::Nil => &[],
            Value::Map(fields) => fields,
            _ => {
                return Err(InitError::new(format!(
                    "the `{MESSAGE}` payload must be a map with `{CLIENT_ID}` and \
                     `{RECONNECT_SECRET}`"
                )));
            }
        };

        let mut client_id = None;
        let mut secret = None;
        for (key, value) in fields {
            let slot = match key.as_str() {
                Some(CLIENT_ID) => &mut client_id,
                Some(RECONNECT_SECRET) => &mut secret,
                _ => {
                    return Err(InitError::new(format!(
                        "the `{MESSAGE}` payload has an unknown key {}: a reconnect resumes the \
                         Session's stored Config and registrations and carries only \
                         `{CLIENT_ID}` and `{RECONNECT_SECRET}` (send a `ConfigUpdate` after \
                         reconnecting to change the Config)",
                        describe_unknown_key(key)
                    )));
                }
            };
            if slot.is_some() {
                return Err(InitError::new(format!(
                    "the `{MESSAGE}` payload repeats the key {}",
                    describe_key(key)
                )));
            }
            *slot = Some(value);
        }

        let (client_id, secret) = match (present(client_id), present(secret)) {
            (Some(client_id), Some(secret)) => (client_id, secret),
            (None, Some(_)) => return Err(missing(MESSAGE, &[CLIENT_ID])),
            (Some(_), None) => return Err(missing(MESSAGE, &[RECONNECT_SECRET])),
            (None, None) => return Err(missing(MESSAGE, &[CLIENT_ID, RECONNECT_SECRET])),
        };

        let client_id = client_id
            .as_str()
            .and_then(|text| ClientId::from_str(text).ok())
            .ok_or_else(|| {
                InitError::new(format!(
                    "`{CLIENT_ID}` must be a Client ID: a string of 32 lowercase hexadecimal \
                     characters"
                ))
            })?;
        let secret = secret
            .as_str()
            .and_then(ReconnectSecret::from_hex)
            .ok_or_else(|| {
                InitError::new(format!(
                    "`{RECONNECT_SECRET}` must be a reconnect secret: a string of 64 lowercase \
                     hexadecimal characters"
                ))
            })?;
        Ok(Self { client_id, secret })
    }

    /// The Client ID of the Session to resume.
    #[must_use]
    pub fn client_id(&self) -> ClientId {
        self.client_id
    }

    /// The presented reconnect secret.
    #[must_use]
    pub fn secret(&self) -> &ReconnectSecret {
        &self.secret
    }
}
