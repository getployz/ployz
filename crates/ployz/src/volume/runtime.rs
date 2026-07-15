use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
    render_api_call,
};
use crate::volume::command::{
    VolumeListCommand, VolumeListOutput, VolumeRemoveCommand, VolumeRemoveConfirmation,
};

fn confirm_remove(command: &VolumeRemoveCommand) -> Result<(), PloyzctlExecutionError> {
    let confirmation = VolumeRemoveConfirmation {
        namespace_id: command.namespace_id.clone(),
        volume_name: command.volume_name.clone(),
    };
    crate::confirmation::read_typed_confirmation(
        &confirmation.prompt(),
        &confirmation.confirmation(),
        |message| PloyzctlExecutionError::ReadVolumeRemoveConfirmation { message },
        || PloyzctlExecutionError::VolumeRemoveNotConfirmed {
            namespace_id: command.namespace_id.clone(),
            volume_name: command.volume_name.clone(),
        },
    )
}

pub(crate) async fn list(
    command: VolumeListCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    render_api_call(
        config,
        async |api| api.volume_list(&command.into_request()).await,
        |result| VolumeListOutput::from_result(result).render(),
    )
    .await
}

pub(crate) async fn remove(
    command: VolumeRemoveCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    if !command.force {
        confirm_remove(&command)?;
    }
    let api = operation_api_client(config).await?;
    let accepted = api
        .volume_remove(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            crate::operation::command::AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
}
