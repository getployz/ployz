//! The wall clock the observe loops stamp rows with, and the RFC 3339 text
//! Docker states times in.

use std::time::SystemTime;

pub(super) fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

pub(super) fn rfc3339_nanos(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()?
        .timestamp_nanos_opt()
}
