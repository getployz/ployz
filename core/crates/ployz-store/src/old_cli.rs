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
    /// Admit a plain-text secret for variable `key` to the refusal every plain-text
    /// secret gets, unless it came from the CLI, which is told to update instead.
    ///
    /// # Errors
    /// Returns `unsupported` naming the variable and the update to run.
    pub(crate) fn admit(self, key: &str) -> Result<(), RpcError> {
        if self.from_cli {
            return Err(crate::error::unsupported(
                format!("{key}: update ployz to set a Sealed value."),
                json!({ "variable": key, "next": "curl -fsSL https://ployz.sh | sh" }),
            ));
        }
        Ok(())
    }
}

impl From<&Trusted> for Guard {
    fn from(trusted: &Trusted) -> Self {
        Self {
            from_cli: trusted.from_cli,
        }
    }
}
