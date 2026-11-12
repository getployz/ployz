//! A CLI that predates native sealing sends a secret as plain text for the Store to
//! seal. That text is refused unread and the CLI is told to update.

use ployz_core::RpcError;
use serde_json::json;

/// What an old CLI is told to run to update.
const UPDATE: &str = "curl -fsSL https://ployz.sh | sh";

/// The refusal an old CLI gets for plain text set as variable `key`.
pub(crate) fn told_to_update(key: &str) -> RpcError {
    crate::error::unsupported(
        format!("{key}: update ployz to set a Sealed value."),
        json!({ "variable": key, "next": UPDATE }),
    )
}
