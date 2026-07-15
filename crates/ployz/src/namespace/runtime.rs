use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
};
use crate::namespace::command::{NamespaceRemoveCommand, NamespaceRemoveConfirmation};

fn confirm_remove(command: &NamespaceRemoveCommand) -> Result<(), PloyzctlExecutionError> {
    let prompt = NamespaceRemoveConfirmation {
        namespace_id: command.namespace_id.clone(),
        volume_backed_services: Vec::new(),
    }
    .prompt();
    crate::confirmation::read_typed_confirmation(
        &prompt,
        command.namespace_id.as_str(),
        |message| PloyzctlExecutionError::ReadNamespaceRemoveConfirmation { message },
        || PloyzctlExecutionError::NamespaceRemoveNotConfirmed {
            namespace_id: command.namespace_id.clone(),
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
