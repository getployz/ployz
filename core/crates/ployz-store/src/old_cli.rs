//! What a CLI from before a change sends that this Store no longer reads. Each is refused
//! unread, and the CLI is told to update.

use ployz_core::RpcError;
use serde_json::{Value, json};

use crate::followup::FollowUp;

/// The refusal an old CLI gets for plain text set as variable `key`, which a CLI that
/// predates native sealing sends for the Store to seal.
pub(crate) fn told_to_update(key: &str) -> RpcError {
    crate::error::unsupported(
        format!("{key}: update ployz to set a Sealed value."),
        FollowUp::UpdateClient { beta: false }.within(json!({ "variable": key })),
    )
}

/// Refuse `command`, as sent before it decodes, when it is a Deploy or redeploy carrying
/// a deploy approval. Only a CLI from before signed versions sends one, and no Server
/// reads it now, so that CLI's signed Services would never run.
///
/// # Errors
/// `unsupported`, naming the update, for such a command.
pub fn refuse_deploy_approval(command: &Value) -> Result<(), RpcError> {
    let field = |name| command.get(name).and_then(Value::as_str);
    let admits =
        field("command") == Some("admit") && matches!(field("admit"), Some("deploy" | "retry"));
    if admits && command.get("approval").is_some() {
        return Err(crate::error::unsupported(
            "This version of ployz signs deploys in a form servers no longer accept, so nothing \
             was deployed. Update ployz, then deploy again.",
            FollowUp::UpdateClient { beta: true }.details(),
        ));
    }
    Ok(())
}
