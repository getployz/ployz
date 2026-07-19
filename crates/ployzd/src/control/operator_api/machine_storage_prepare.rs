//! Operator admission for machine-local storage preparation.

use ployz_sdk_types::{
    AcceptedOperation, MachineStoragePrepareError, MachineStoragePrepareRequest,
};

use super::OperationApiHandlers;
use super::submit::owned_machine_operation;
use crate::control::sequencer::MachineStoragePrepareSubmitCommand;

impl From<MachineStoragePrepareRequest> for MachineStoragePrepareSubmitCommand {
    fn from(value: MachineStoragePrepareRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            machine_id: value.machine_id,
            requested_pool: value.pool,
        }
    }
}

pub async fn machine_storage_prepare(
    handlers: &OperationApiHandlers,
    request: MachineStoragePrepareRequest,
) -> Result<AcceptedOperation, MachineStoragePrepareError> {
    let operation_id = request.operation_id.clone();
    let target_machine = handlers
        .machine_roster
        .active_machine(&request.machine_id)
        .await
        .map_err(|error| MachineStoragePrepareError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?;
    if target_machine.is_none() {
        return Err(MachineStoragePrepareError::NoSuchMachine {
            operation_id,
            machine_id: request.machine_id,
        });
    }

    let operation_id = request.operation_id.clone();
    let accepted =
        handlers
            .controllers()
            .submit_machine_storage_prepare(request.into())
            .await
            .map_err(|error| {
                let error = match error {
                crate::control::sequencer::SubmitCommandError::MachineSubstrateBusy {
                    machine_id,
                    owner,
                } => {
                    return MachineStoragePrepareError::MachineSubstrateBusy {
                        operation_id: operation_id.clone(),
                        machine_id,
                        owner_operation_id: owner,
                    };
                }
                error @ crate::control::sequencer::SubmitCommandError::Clock { .. }
                | error @ crate::control::sequencer::SubmitCommandError::NamespaceBusy { .. }
                | error @ crate::control::sequencer::SubmitCommandError::IngressBusy { .. }
                | error @ crate::control::sequencer::SubmitCommandError::ReservationNotFound {
                    ..
                }
                | error @ crate::control::sequencer::SubmitCommandError::ReservationExpired {
                    ..
                }
                | error @ crate::control::sequencer::SubmitCommandError::ReservedSystemNamespace {
                    ..
                }
                | error @ crate::control::sequencer::SubmitCommandError::SystemNamespaceRequired {
                    ..
                }
                | error @ crate::control::sequencer::SubmitCommandError::Submit(_) => error,
            };
                match super::error_map::unfenced_submit_failure("machine-storage-prepare", error) {
                    super::error_map::UnfencedSubmitFailure::Unavailable { message } => {
                        MachineStoragePrepareError::Unavailable {
                            operation_id: operation_id.clone(),
                            message,
                        }
                    }
                    super::error_map::UnfencedSubmitFailure::DuplicateSequenceMismatch {
                        sequence,
                    } => MachineStoragePrepareError::DuplicateSequenceMismatch {
                        operation_id: operation_id.clone(),
                        sequence,
                    },
                }
            })?;
    let operation = owned_machine_operation(
        accepted.operation_id.clone(),
        &accepted.machine_id,
        accepted.start_sequence,
    );
    handlers.machine_storage_prepare().start(accepted).await;
    Ok(operation)
}
