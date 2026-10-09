//! The severity a reader shows for one line of container output.
//!
//! The Log Store keeps raw bytes. Readers call [`log_level`] when they print,
//! so a rule change ships with a CLI or Cloud release, never a Machine upgrade.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

/// Reads, in order, a JSON `level`, `lvl`, or `severity` key, a logfmt
/// `level=`, then a leading `ERROR`, `WARN`, `INFO`, or `DEBUG` word or an
/// `[error]`-style tag, all case-insensitive. Anything else is `info`,
/// whichever stream the line came from.
#[must_use]
pub fn log_level(line: &[u8]) -> LogLevel {
    let line = line.trim_ascii();
    json_level(line)
        .or_else(|| logfmt_level(line))
        .or_else(|| leading_level(line))
        .unwrap_or(LogLevel::Info)
}

fn json_level(line: &[u8]) -> Option<LogLevel> {
    if line.first() != Some(&b'{') {
        return None;
    }
    let serde_json::Value::Object(fields) = serde_json::from_slice(line).ok()? else {
        return None;
    };
    ["level", "lvl", "severity"].iter().find_map(|key| {
        fields
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .and_then(|(_, value)| value.as_str())
            .and_then(|value| named(value.as_bytes()))
    })
}

fn logfmt_level(line: &[u8]) -> Option<LogLevel> {
    line.split(u8::is_ascii_whitespace).find_map(|pair| {
        let (key, value) = split_once(pair, b'=')?;
        if !key.eq_ignore_ascii_case(b"level") {
            return None;
        }
        named(
            value
                .strip_prefix(b"\"")
                .unwrap_or(value)
                .strip_suffix(b"\"")
                .unwrap_or(value),
        )
    })
}

fn leading_level(line: &[u8]) -> Option<LogLevel> {
    if let Some(tagged) = line.strip_prefix(b"[") {
        let (tag, _) = split_once(tagged, b']')?;
        return named(tag);
    }
    let word = line
        .split(|byte| !byte.is_ascii_alphabetic())
        .next()
        .filter(|word| {
            line.get(word.len())
                .is_none_or(|next| !next.is_ascii_alphanumeric())
        })?;
    named(word)
}

fn named(value: &[u8]) -> Option<LogLevel> {
    let value = value.to_ascii_lowercase();
    match value.as_slice() {
        b"error" | b"err" | b"fatal" | b"critical" | b"crit" | b"panic" | b"alert" | b"emerg"
        | b"emergency" => Some(LogLevel::Error),
        b"warn" | b"warning" => Some(LogLevel::Warn),
        b"info" | b"notice" | b"information" => Some(LogLevel::Info),
        b"debug" | b"trace" | b"dbug" => Some(LogLevel::Debug),
        _ => None,
    }
}

fn split_once(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let mut parts = bytes.splitn(2, |byte| *byte == separator);
    Some((parts.next()?, parts.next()?))
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, log_level};

    #[test]
    fn each_rule_reads_its_level() {
        for (line, level) in [
            (r#"{"level":"error","msg":"x"}"#, LogLevel::Error),
            (r#"{"lvl":"WARN"}"#, LogLevel::Warn),
            (r#"{"severity":"debug"}"#, LogLevel::Debug),
            (r#"{"Level":"warning"}"#, LogLevel::Warn),
            ("ts=1 level=error msg=boom", LogLevel::Error),
            (r#"msg="x" level="debug""#, LogLevel::Debug),
            ("ERROR something broke", LogLevel::Error),
            ("warn: disk at 90%", LogLevel::Warn),
            ("Debug starting", LogLevel::Debug),
            ("[error] tagged", LogLevel::Error),
            ("[WARN] tagged", LogLevel::Warn),
            ("  INFO indented", LogLevel::Info),
        ] {
            assert_eq!(log_level(line.as_bytes()), level, "{line}");
        }
    }

    #[test]
    fn earlier_rules_win() {
        for (line, level) in [
            (r#"{"level":"debug","severity":"error"}"#, LogLevel::Debug),
            (r#"{"lvl":"warn","severity":"error"}"#, LogLevel::Warn),
            (r#"{"level":"warn","msg":"level=error"}"#, LogLevel::Warn),
            ("ERROR level=debug", LogLevel::Debug),
            ("[error] level=warn", LogLevel::Warn),
        ] {
            assert_eq!(log_level(line.as_bytes()), level, "{line}");
        }
    }

    #[test]
    fn anything_else_is_info() {
        for line in [
            "",
            "plain text on stdout",
            "plain text on stderr",
            "Errors are counted later",
            "errored out",
            "[req-12] handled",
            r#"{"level":7}"#,
            r#"{"level":"loud"}"#,
            "{not json level=",
            "level=loud",
            "\u{fffd}\u{fffd} binary",
        ] {
            assert_eq!(log_level(line.as_bytes()), LogLevel::Info, "{line}");
        }
    }

    #[test]
    fn non_utf8_bytes_are_info() {
        assert_eq!(log_level(&[0xff, 0xfe, b'x']), LogLevel::Info);
    }
}
