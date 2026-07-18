use ployz_sdk_types::{
    AcceptedOperation, MachineBuildCachePruneError, MachineBuildCachePruneRequest,
};

use super::OperationApiHandlers;
use super::submit::owned_machine_operation;
use crate::control::sequencer::MachineBuildCachePruneSubmitCommand;

pub async fn machine_build_cache_prune(
    handlers: &OperationApiHandlers,
    request: MachineBuildCachePruneRequest,
) -> Result<AcceptedOperation, MachineBuildCachePruneError> {
    let operation_id = request.operation_id.clone();
    let machine_id = request.machine_id.clone();
    let exists = handlers
        .machine_roster
        .active_machine(&machine_id)
        .await
        .map_err(|error| MachineBuildCachePruneError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?;
    if exists.is_none() {
        return Err(MachineBuildCachePruneError::NoSuchMachine {
            operation_id,
            machine_id,
        });
    }
    let accepted = handlers
        .controllers()
        .submit_machine_build_cache_prune(MachineBuildCachePruneSubmitCommand {
            operation_id: operation_id.clone(),
            machine_id,
        })
        .await
        .map_err(|error| {
            match super::error_map::unfenced_submit_failure("machine-build-cache-prune", error) {
                super::error_map::UnfencedSubmitFailure::Unavailable { message } => {
                    MachineBuildCachePruneError::Unavailable {
                        operation_id: operation_id.clone(),
                        message,
                    }
                }
                super::error_map::UnfencedSubmitFailure::DuplicateSequenceMismatch { sequence } => {
                    MachineBuildCachePruneError::DuplicateSequenceMismatch {
                        operation_id: operation_id.clone(),
                        sequence,
                    }
                }
            }
        })?;
    let response = owned_machine_operation(
        accepted.operation_id.clone(),
        &accepted.machine_id,
        accepted.start_sequence,
    );
    handlers.machine_build_cache_prune().start(accepted).await;
    Ok(response)
}
