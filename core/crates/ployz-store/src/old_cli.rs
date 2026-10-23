//! A CLI that predates native sealing sends a secret as plain text for the Store to
//! seal. Once its Organization seals on the client, that text is refused unread and
//! the CLI is told to update; every other caller still has the Store seal it.

use ployz_core::RpcError;
use serde_json::json;

use crate::Trusted;

/// Who sent a plain-text secret: the guard that refuses it from an old CLI once
/// its Organization seals on the client.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Guard {
    from_cli: bool,
}

impl Guard {
    /// A write no CLI made: Cloud's dashboard and automation, or the hidden local Store.
    pub(crate) const NOT_CLI: Self = Self { from_cli: false };

    /// Admit a plain-text secret for variable `key`, unless it came from the CLI and
    /// `sealed_on_client` finds the Organization seals there; that is read only for
    /// the CLI.
    ///
    /// # Errors
    /// Returns `unsupported` naming the variable and the update to run.
    pub(crate) fn admit(
        self,
        key: &str,
        sealed_on_client: impl FnOnce() -> Result<bool, RpcError>,
    ) -> Result<(), RpcError> {
        if self.from_cli && sealed_on_client()? {
            return Err(crate::error::unsupported(
                format!("{key}: update ployz to set a Sealed value"),
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
