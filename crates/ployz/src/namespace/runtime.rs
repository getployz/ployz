use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{PloyzctlExecutionOutput, api_error, operation_api_client};
use crate::namespace::command::{NamespaceRemoveCommand, NamespaceRemoveConfirmation};
use ployz_sdk_types::VolumeListRequest;

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

fn removal_evidence_request(force: bool) -> Option<VolumeListRequest> {
    (!force).then_some(VolumeListRequest {})
}

fn confirm_remove(
    command: &NamespaceRemoveCommand,
    confirmation: &NamespaceRemoveConfirmation,
) -> Result<(), PloyzctlExecutionError> {
    let prompt = confirmation.prompt();
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
    let api = operation_api_client(config).await?;
    if let Some(request) = removal_evidence_request(command.force) {
        let result = api.volume_list(&request).await.map_err(api_error)?;
        let confirmation =
            NamespaceRemoveConfirmation::from_volume_list(command.namespace_id.clone(), result);
        confirm_remove(&command, &confirmation)?;
    }
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

#[cfg(test)]
mod tests {
    use super::removal_evidence_request;

    #[test]
    fn force_bypasses_namespace_volume_list_confirmation_gather() {
        assert_eq!(removal_evidence_request(true), None);
        assert_eq!(
            removal_evidence_request(false),
            Some(ployz_sdk_types::VolumeListRequest {})
        );
    }
}
