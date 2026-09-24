//! The §4.3 conversion rules. Each returns `None` where the language raises a `type` error; the
//! caller owns the diagnostic because only it knows the operator and span.

use std::borrow::Cow;

use crate::heap::RtValue;

/// To-number (§4.3): `null` → 0, bools → 1/0, numeric strings parse, collections and functions
/// fail.
pub(crate) fn to_number(value: &RtValue) -> Option<f64> {
    match value {
        RtValue::Null => Some(0.0),
        RtValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        RtValue::Number(n) => Some(*n),
        RtValue::String(s) => parse_number(s),
        RtValue::Array(_) | RtValue::Object(_) | RtValue::Function(_) => None,
    }
}

/// To-string (§4.3): collections and functions have no string form.
pub(crate) fn to_string(value: &RtValue) -> Option<Cow<'_, str>> {
    match value {
        RtValue::Null => Some(Cow::Borrowed("null")),
        RtValue::Bool(b) => Some(Cow::Borrowed(if *b { "true" } else { "false" })),
        RtValue::Number(n) => Some(Cow::Owned(number_to_string(*n))),
        RtValue::String(s) => Some(Cow::Borrowed(s.as_str())),
        RtValue::Array(_) | RtValue::Object(_) | RtValue::Function(_) => None,
    }
}

/// A string is a number when, after trimming surrounding whitespace, it is one optional `+` or
/// `-` followed by a number literal as §3 lexes it (digits, optional `.digits`, optional
/// complete exponent) with a finite value. `""`, `"NaN"`, `"Infinity"`, `"0x10"`, `".5"` fail.
pub(crate) fn parse_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    let (negative, unsigned) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    if !is_number_literal(unsigned.as_bytes()) {
        return None;
    }
    let magnitude: f64 = unsigned.parse().ok()?;
    if !magnitude.is_finite() {
        return None;
    }
    Some(if negative { -magnitude } else { magnitude })
}

fn is_number_literal(bytes: &[u8]) -> bool {
    let mut at = 0;
    let digits = |at: &mut usize| {
        let start = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        *at > start
    };
    if !digits(&mut at) {
        return false;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        if !digits(&mut at) {
            return false;
        }
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        if !digits(&mut at) {
            return false;
        }
    }
    at == bytes.len()
}

/// JavaScript-style number formatting: shortest round-trip digits, plain notation for
/// magnitudes in `[1e-6, 1e21)`, exponent notation (`1e+21`, `1.5e-7`) outside it, and `-0`
/// prints as `0`.
pub(crate) fn number_to_string(n: f64) -> String {
    if n == 0.0 {
        return "0".to_owned();
    }
    // `{:e}` yields the shortest round-tripping digits: `1.2345e3`, `1e21`, `5e-7`.
    let scientific = format!("{:e}", n.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return n.to_string();
    };
    let Ok(exponent) = exponent.parse::<i64>() else {
        return n.to_string();
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    // The value is 0.DIGITS × 10^point.
    let point = exponent + 1;
    let mut out = String::new();
    if n < 0.0 {
        out.push('-');
    }
    if k <= point && point <= 21 {
        out.push_str(&digits);
        out.extend(core::iter::repeat_n(
            '0',
            usize::try_from(point - k).unwrap_or(0),
        ));
    } else if 0 < point && point <= 21 {
        let split = usize::try_from(point).unwrap_or(0);
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
    } else if -6 < point && point <= 0 {
        out.push_str("0.");
        out.extend(core::iter::repeat_n(
            '0',
            usize::try_from(-point).unwrap_or(0),
        ));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if point - 1 < 0 { '-' } else { '+' });
        out.push_str(&(point - 1).abs().to_string());
    }
    out
}
