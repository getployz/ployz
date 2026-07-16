use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    PloyzctlExecutionOutput, api_error, operation_api_client, render_api_call,
};
use crate::volume::command::{
    VolumeCreateCommand, VolumeListCommand, VolumeListOutput, VolumeRemoveCommand,
    VolumeRemoveConfirmation,
};
use ployz_sdk_types::{VolumeListRequest, VolumeListResult};

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
    #[error(
        "volume rm target {}/{} was not present in the fresh volume list",
        namespace_id.as_str(),
        volume_name.as_str()
    )]
    RemoveTargetNotFound {
        namespace_id: ployz_core::ids::NamespaceId,
        volume_name: ployz_core::deploy::VolumeName,
    },
}

impl From<VolumeExecutionError> for PloyzctlExecutionError {
    fn from(error: VolumeExecutionError) -> Self {
        Self::Volume(error)
    }
}

fn remove_confirmation(
    command: &VolumeRemoveCommand,
    result: VolumeListResult,
) -> Result<VolumeRemoveConfirmation, VolumeExecutionError> {
    let Some(volume) = result.volumes.into_iter().find(|volume| {
        volume.namespace_id == command.namespace_id && volume.volume_name == command.volume_name
    }) else {
        return Err(VolumeExecutionError::RemoveTargetNotFound {
            namespace_id: command.namespace_id.clone(),
            volume_name: command.volume_name.clone(),
        });
    };
    Ok(VolumeRemoveConfirmation { volume })
}

fn confirm_remove(
    command: &VolumeRemoveCommand,
    confirmation: &VolumeRemoveConfirmation,
) -> Result<(), PloyzctlExecutionError> {
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
    let api = operation_api_client(config).await?;
    if !command.force {
        let result = api
            .volume_list(&VolumeListRequest {})
            .await
            .map_err(api_error)?;
        let confirmation = remove_confirmation(&command, result)?;
        confirm_remove(&command, &confirmation)?;
    }
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

#[cfg(test)]
mod tests {
    use super::{VolumeExecutionError, remove_confirmation};
    use crate::volume::command::VolumeRemoveCommand;
    use ployz_core::deploy::VolumeName;
    use ployz_core::ids::{NamespaceId, OperationId};
    use ployz_sdk_types::VolumeListResult;

    fn command(force: bool) -> VolumeRemoveCommand {
        VolumeRemoveCommand {
            operation_id: OperationId::try_new("op_volume_remove").expect("valid operation id"),
            namespace_id: NamespaceId::try_new("prod").expect("valid namespace"),
            volume_name: VolumeName::try_new("data").expect("valid volume"),
            force,
            detach: false,
        }
    }

    #[test]
    fn unforced_remove_requires_an_exact_fresh_snapshot() {
        let error = remove_confirmation(
            &command(false),
            VolumeListResult {
                volumes: Vec::new(),
            },
        )
        .expect_err("missing exact target must fail");

        assert_eq!(
            error,
            VolumeExecutionError::RemoveTargetNotFound {
                namespace_id: NamespaceId::try_new("prod").expect("valid namespace"),
                volume_name: VolumeName::try_new("data").expect("valid volume"),
            }
        );
    }
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
