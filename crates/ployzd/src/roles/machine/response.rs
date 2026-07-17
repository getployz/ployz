use crate::roles::machine::runner::MachineContainerListError;
use ployz_core::ids::ContainerId;
use ployz_core::operation::{FailureMessage, OperatorHint};
use ployz_nats::service_runtime::{NatsServiceError, NatsServiceResponse};

pub(crate) fn machine_success(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_ok(&response)
}

pub(crate) fn machine_domain_error(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_domain_error(&response)
}

pub(crate) fn container_list_error(error: MachineContainerListError) -> NatsServiceResponse {
    match error {
        MachineContainerListError::ListExisting { message } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "container list failed: {message}"
            )))
        }
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
