use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    PloyzctlExecutionOutput, api_error, operation_api_client, render_api_call,
};
use crate::volume::command::{
    VolumeCreateCommand, VolumeListCommand, VolumeListOutput, VolumeRemoveCommand,
    VolumeRemoveConfirmation,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumeExecutionError {
    #[error(
        "volume rm {}/{} was not confirmed",
        namespace_id.as_str(),
        volume_name.as_str()
    )]
    RemoveNotConfirmed {
        namespace_id: ployz_core::ids::NamespaceId,
        volume_name: ployz_core::deploy::VolumeName,
    },
    #[error("failed to read volume rm confirmation: {message}")]
    ReadRemoveConfirmation { message: String },
}

impl From<VolumeExecutionError> for PloyzctlExecutionError {
    fn from(error: VolumeExecutionError) -> Self {
        Self::Volume(error)
    }
}

fn confirm_remove(command: &VolumeRemoveCommand) -> Result<(), PloyzctlExecutionError> {
    let confirmation = VolumeRemoveConfirmation {
        namespace_id: command.namespace_id.clone(),
        volume_name: command.volume_name.clone(),
    };
    crate::confirmation::read_typed_confirmation(
        &confirmation.prompt(),
        &confirmation.confirmation(),
        |message| VolumeExecutionError::ReadRemoveConfirmation { message }.into(),
        || {
            VolumeExecutionError::RemoveNotConfirmed {
                namespace_id: command.namespace_id.clone(),
                volume_name: command.volume_name.clone(),
            }
            .into()
        },
    )
}

pub(crate) async fn list(
    command: VolumeListCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    Ok(render_api_call(
        config,
        async |api| api.volume_list(&command.into_request()).await,
        |result| VolumeListOutput::from_result(result).render(),
    )
    .await?)
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

pub(crate) async fn create(
    command: VolumeCreateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let accepted = api
        .volume_create(&command.into_request())
        .await
        .map_err(api_error)?;
    crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
}
