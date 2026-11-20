//! What a CLI from before a change sends that this Store no longer reads. Each is refused
//! unread, and the CLI is told to update.

use ployz_core::{MachineVersion, RpcError};
use serde_json::{Value, json};

use crate::Command;
use crate::followup::FollowUp;

/// The refusal an old CLI gets for plain text set as variable `key`, which a CLI that
/// predates native sealing sends for the Store to seal.
pub(crate) fn told_to_update(key: &str) -> RpcError {
    crate::error::unsupported(
        format!("{key}: update ployz to set a Sealed value."),
        FollowUp::UpdateClient { beta: false }.within(json!({ "variable": key })),
    )
}

impl Command {
    /// Read a command a client sent. A Deploy or redeploy that carries a deploy approval
    /// is refused before it decodes: only a CLI from before signed versions sends one,
    /// and this Store keeps no approval for its runners to hand on, so that CLI's signed
    /// Services would never run.
    ///
    /// # Errors
    /// `unsupported`, naming the update, for such a command; `invalid_argument`, saying
    /// where it went wrong, for one that doesn't decode.
    pub fn sent(command: Value) -> Result<Self, RpcError> {
        let field = |name| command.get(name).and_then(Value::as_str);
        let admits =
            field("command") == Some("admit") && matches!(field("admit"), Some("deploy" | "retry"));
        if admits && command.get("approval").is_some() {
            return Err(crate::error::unsupported(
                "This ployz signs deploys in an older form this Config Store no longer takes, \
                 so nothing was deployed. Update ployz, then deploy again.",
                FollowUp::UpdateClient {
                    beta: this_is_a_beta(),
                }
                .details(),
            ));
        }
        Self::decode(command).map_err(|error| {
            RpcError::invalid_argument(format!("Expected a Config Store command: {error}."))
        })
    }
}

/// Whether this Store is a beta, whose CLI only the beta channel installs.
fn this_is_a_beta() -> bool {
    MachineVersion::parse(env!("CARGO_PKG_VERSION")).is_ok_and(|version| version.is_prerelease())
}
