use crate::roles::machine::protocol::{
    MachineContainerRunDomainError, MachineContainerRunRpcResponse,
};
use crate::roles::machine::runner::MachineContainerRunnerError;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::ops::{FailureMessage, OperatorHint};
use ployz_nats::service_runtime::{NatsServiceError, NatsServiceResponse};

pub(crate) fn machine_success(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_ok(&response)
}

pub(crate) fn machine_domain_error(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_domain_error(&response)
}

pub(crate) fn runner_error(error: MachineContainerRunnerError) -> NatsServiceResponse {
    match error {
        MachineContainerRunnerError::ListExisting { message } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "container list failed: {message}"
            )))
        }
        MachineContainerRunnerError::EnsureEndpointNetwork { message } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "endpoint network ensure failed: {message}"
            )))
        }
        MachineContainerRunnerError::EndpointNetworkSubnetMismatch { expected, observed } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "endpoint network subnet is {observed:?}, expected {expected:?}"
            )))
        }
        MachineContainerRunnerError::Create { message } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container create failed: {message}")),
        ),
        MachineContainerRunnerError::ImagePull { message } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("image pull failed: {message}")),
        ),
        MachineContainerRunnerError::Start { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container start failed: {message}")),
        ),
        MachineContainerRunnerError::Wait { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container wait failed: {message}")),
        ),
        MachineContainerRunnerError::Stop { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container stop failed: {message}")),
        ),
        MachineContainerRunnerError::Restart { message, .. } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "container restart failed: {message}"
            )))
        }
        MachineContainerRunnerError::Remove { message, .. } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "container remove failed: {message}"
            )))
        }
        MachineContainerRunnerError::RemoveVolume { message, .. } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "volume remove failed: {message}"
            )))
        }
    }
}

/// Map a start failure to the endpoint's domain error, parameterized by which
/// start-failed variant (created vs existing) the caller is reporting.
pub(crate) fn container_start_error(
    machine_id: MachineId,
    container_id: ContainerId,
    error: MachineContainerRunnerError,
    start_failed: impl FnOnce(
        ContainerId,
        FailureMessage,
        OperatorHint,
    ) -> MachineContainerRunDomainError,
) -> NatsServiceResponse {
    match error {
        MachineContainerRunnerError::Start { message, .. } => {
            machine_domain_error(MachineContainerRunRpcResponse::DomainError {
                machine_id,
                error: start_failed(
                    container_id.clone(),
                    failure_message(format!("container start failed: {message}")),
                    inspect_hint(&container_id),
                ),
            })
        }
        error @ (MachineContainerRunnerError::ListExisting { .. }
        | MachineContainerRunnerError::EnsureEndpointNetwork { .. }
        | MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. }
        | MachineContainerRunnerError::Create { .. }
        | MachineContainerRunnerError::ImagePull { .. }
        | MachineContainerRunnerError::Wait { .. }
        | MachineContainerRunnerError::Stop { .. }
        | MachineContainerRunnerError::Restart { .. }
        | MachineContainerRunnerError::Remove { .. }
        | MachineContainerRunnerError::RemoveVolume { .. }) => runner_error(error),
    }
}

pub(crate) fn failure_message(value: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(value).expect("generated failure message is non-empty")
}

pub(crate) fn inspect_hint(container_id: &ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
}

pub(crate) fn log_hint(container_id: &ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployzctl logs {}", container_id.as_str()))
        .expect("generated log hint is non-empty")
}
