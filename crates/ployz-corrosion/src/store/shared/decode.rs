use corro_api_types::SqliteValue;
use ployz_sdk::error::{Error, Result};

pub(crate) fn integer(val: &SqliteValue, field: &'static str) -> Result<i64> {
    if let Some(&v) = val.as_integer() {
        return Ok(v);
    }
    if let Some(s) = val.as_text() {
        if s.is_empty() {
            return Ok(0);
        }
        return s.parse::<i64>().map_err(|e| {
            Error::operation("decode", format!("invalid integer for '{field}': {e}"))
        });
    }
    Err(Error::operation(
        "decode",
        format!("expected integer for '{field}', got {:?}", val),
    ))
}

pub(crate) fn text(val: &SqliteValue, field: &'static str) -> Result<String> {
    val.as_text()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::operation("decode", format!("expected text for '{field}'")))
}

pub(crate) fn blob(val: &SqliteValue, field: &'static str) -> Result<Vec<u8>> {
    val.as_blob()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::operation("decode", format!("expected blob for '{field}'")))
}
