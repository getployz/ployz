use corro_api_types::SqliteValue;
use ployz_sdk::error::{Error, Result};

pub(crate) fn text(val: &SqliteValue, field: &'static str) -> Result<String> {
    val.as_text()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::operation("decode", format!("expected text for '{field}'")))
}
