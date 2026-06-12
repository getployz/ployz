//! Shell quoting for test harnesses that interpolate values into `sh -c`
//! lines.

/// Smallest shell-safe quoting for values interpolated into `sh -c` lines.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
