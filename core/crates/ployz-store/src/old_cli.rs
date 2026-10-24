//! A CLI that predates native sealing sends a secret as plain text for the Store to
//! seal. That text is refused unread and the CLI is told to update.

use ployz_core::RpcError;
use serde_json::json;

use crate::Trusted;

/// Who sent a plain-text secret: the guard that names the update to an old CLI.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Guard {
    from_cli: bool,
}

impl Guard {
    /// The refusal a plain-text secret for variable `key` gets: `otherwise`, or
    /// `unsupported` naming the update when it came from the CLI.
    pub(crate) fn refuse(self, key: &str, otherwise: RpcError) -> RpcError {
        if self.from_cli {
            return crate::error::unsupported(
                format!("{key}: update ployz to set a Sealed value."),
                json!({ "variable": key, "next": "curl -fsSL https://ployz.sh | sh" }),
            );
        }
        otherwise
    }
}

impl From<&Trusted> for Guard {
    fn from(trusted: &Trusted) -> Self {
        Self {
            from_cli: trusted.from_cli,
        }
    }
}
