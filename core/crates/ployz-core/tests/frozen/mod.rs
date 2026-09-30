//! Shared by the frozen-format tests in ployz-core and ployzd.
//!
//! `fixtures/v0.2.0/` holds what a 0.2.0 daemon stored and spoke (DESIGN.md,
//! "Stable promise"). These files are frozen: never edit or delete them. A new
//! release that freezes more adds its own `fixtures/vX.Y.Z/` directory.

use serde_json::Value;

/// Fails unless `reencoded` still carries every field of `frozen` with the same
/// value. New fields are allowed; a dropped, renamed or changed one is not.
pub fn assert_retains(frozen: &Value, reencoded: &Value, at: &str) {
    match (frozen, reencoded) {
        (Value::Object(frozen), Value::Object(reencoded)) => {
            for (key, value) in frozen {
                let Some(now) = reencoded.get(key) else {
                    panic!("{at}.{key}: frozen field is no longer written");
                };
                assert_retains(value, now, &format!("{at}.{key}"));
            }
        }
        (Value::Array(frozen), Value::Array(reencoded)) => {
            assert_eq!(frozen.len(), reencoded.len(), "{at}: array length changed");
            for (index, (value, now)) in frozen.iter().zip(reencoded).enumerate() {
                assert_retains(value, now, &format!("{at}[{index}]"));
            }
        }
        _ => assert_eq!(frozen, reencoded, "{at}: frozen value changed"),
    }
}
