//! Central test crate for the workspace. Deliberately empty: every test lives under `tests/`,
//! one file per crate under test, and each crate under test is a dev-dependency.
//!
//! Two consequences worth knowing before adding a test here.
//!
//! **These are integration tests, not unit tests.** A separate crate sees only the public API of
//! the crate it tests, so anything reaching into a private function has to stay in a
//! `#[cfg(test)] mod tests` inside its own crate. That is the exception, not the rule — prefer
//! testing the public surface here, since that is what the rest of the workspace depends on.
//!
//! **Crates under test are dev-dependencies.** `scripts/check-crate-graph.py` enforces the
//! Architecture Spine's dependency rules over normal dependencies only, because those rules are
//! about what production code can reach. Keeping the edges here in `[dev-dependencies]` lets this
//! crate test `hexput-enforce` (AD-3) or `hexput-transport` (AD-1) without claiming a production
//! path to them.
//!
//! **It does carry shared test helpers.** Anything more than one `tests/<crate>.rs` needs lives
//! here rather than being copy-pasted per file, so a rule encoded in one of them cannot drift
//! out of step with the others.

/// The 1-based line and scalar column of a byte offset, by the LANGUAGE-REFERENCE §2 rules the
/// lexer uses: a leading BOM is not a column, and `\n`, `\r\n` and a lone `\r` each end one line.
///
/// The producer sweeps re-derive a diagnostic's position with this and compare it against the
/// recorded span, so a span cannot be right in bytes but wrong in the coordinates rendering
/// marks. It is deliberately a second, independent implementation of the rule.
///
/// # Panics
///
/// If `offset` is not a character boundary of `source`.
#[must_use]
pub fn line_and_column(source: &str, offset: usize) -> (usize, usize) {
    let bom = if source.starts_with('\u{feff}') { 3 } else { 0 };
    let mut chars = source[bom..offset].chars().peekable();
    let (mut line, mut column) = (1, 1);
    while let Some(c) = chars.next() {
        if c == '\n' || (c == '\r' && chars.peek() != Some(&'\n')) {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
