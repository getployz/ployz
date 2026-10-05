use std::error::Error;

/// Each `source()` below the top, raw, with a wrapper that only repeats the
/// line above it dropped.
#[must_use]
pub fn causes(top: &(dyn Error + 'static)) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut above = top.to_string();
    let mut next = top.source();
    while let Some(error) = next {
        let line = error.to_string();
        if line != above && !line.is_empty() {
            lines.push(line.clone());
        }
        above = line;
        next = error.source();
    }
    lines
}

/// An error and its causes on one line, for a place no `cause:` line can go:
/// a warning, a per-Server failure, an RPC message.
#[must_use]
pub fn inline(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    for cause in causes(error) {
        text.truncate(text.trim_end_matches('.').len());
        text.push_str(": ");
        text.push_str(&cause);
    }
    text
}
