use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{PloyzctlExecutionOutput, api_error, operation_api_client};
use crate::namespace::command::{NamespaceRemoveCommand, NamespaceRemoveConfirmation};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NamespaceExecutionError {
    #[error("namespace rm {} was not confirmed", namespace_id.as_str())]
    RemoveNotConfirmed {
        namespace_id: ployz_core::ids::NamespaceId,
    },
    #[error("failed to read namespace rm confirmation: {message}")]
    ReadRemoveConfirmation { message: String },
}

impl From<NamespaceExecutionError> for PloyzctlExecutionError {
    fn from(error: NamespaceExecutionError) -> Self {
        Self::Namespace(error)
    }
}

fn confirm_remove(command: &NamespaceRemoveCommand) -> Result<(), PloyzctlExecutionError> {
    let prompt = NamespaceRemoveConfirmation {
        namespace_id: command.namespace_id.clone(),
        volume_backed_services: Vec::new(),
    }
    .prompt();
    crate::confirmation::read_typed_confirmation(
        &prompt,
        command.namespace_id.as_str(),
        |message| NamespaceExecutionError::ReadRemoveConfirmation { message }.into(),
        || {
            NamespaceExecutionError::RemoveNotConfirmed {
                namespace_id: command.namespace_id.clone(),
            }
            .into()
        },
    )
}

pub(crate) async fn remove(
    command: NamespaceRemoveCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    if !command.force {
        confirm_remove(&command)?;
    }
    let api = operation_api_client(config).await?;
    let accepted = api
        .namespace_remove(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            crate::operation::command::AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
}
