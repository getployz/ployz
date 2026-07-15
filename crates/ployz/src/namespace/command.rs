use clap::Args;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_sdk_types::NamespaceRemoveRequest;

use crate::commands::{PloyzctlCliError, invalid_value};
use crate::execution_support::generate_client_namespace_remove_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub force: bool,
    pub detach: bool,
}

impl NamespaceRemoveCommand {
    #[must_use]
    pub fn into_request(self) -> NamespaceRemoveRequest {
        NamespaceRemoveRequest {
            operation_id: self.operation_id,
            namespace_id: self.namespace_id,
        }
    }
}

pub(crate) fn namespace_remove_command(
    parsed: NamespaceRemoveCli,
) -> Result<NamespaceRemoveCommand, PloyzctlCliError> {
    let namespace_id = NamespaceId::try_new(parsed.namespace)
        .map_err(|error| invalid_value("<namespace>", error))?;
    let operation_id = generate_client_namespace_remove_id(&namespace_id)
        .map_err(|error| invalid_value("<namespace>", error))?
        .operation_id;
    Ok(NamespaceRemoveCommand {
        operation_id,
        namespace_id,
        force: parsed.force,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct NamespaceRemoveCli {
    namespace: String,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveConfirmation {
    pub namespace_id: NamespaceId,
    pub volume_backed_services: Vec<String>,
}

impl NamespaceRemoveConfirmation {
    #[must_use]
    pub fn prompt(&self) -> String {
        let services = if self.volume_backed_services.is_empty() {
            "none recorded".to_owned()
        } else {
            self.volume_backed_services.join(", ")
        };
        format!(
            "This removes service containers and Route Bindings for namespace {}.\nVolume data is preserved. Volume-backed services: {}.\nType {} to continue: ",
            self.namespace_id.as_str(),
            services,
            self.namespace_id.as_str()
        )
    }
}
