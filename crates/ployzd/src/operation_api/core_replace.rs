use crate::operation_api::error_map::corrupt;
use crate::operations::log::RecordOperationEventError;
use ployz_core::ops::{CoreReplaceTransition, OperationStatus};
use ployz_sdk_types::{
    CoreReplaceReportError, CoreReplaceReportOutcome, CoreReplaceReportRequest, CoreReplaceReported,
};

use super::OperationApiHandlers;

pub async fn core_replace_report(
    handlers: &OperationApiHandlers,
    request: CoreReplaceReportRequest,
) -> Result<CoreReplaceReported, CoreReplaceReportError> {
    let operation_id = request.operation_id.clone();
    let machine_id = request.machine_id.clone();
    let transition = match request.outcome.clone() {
        CoreReplaceReportOutcome::Completed => CoreReplaceTransition::Completed,
        CoreReplaceReportOutcome::Failed { failure } => CoreReplaceTransition::Failed { failure },
    };
    handlers
        .controllers()
        .repository()
        .record_core_replace_transition(&operation_id, &machine_id, transition)
        .await
        .map_err(|error| match error {
            RecordOperationEventError::MissingOperation { .. } => {
                CoreReplaceReportError::UnknownOperation {
                    operation_id: operation_id.clone(),
                }
            }
            error @ (RecordOperationEventError::StoreStatus(_)
            | RecordOperationEventError::ProjectStatus(_)) => CoreReplaceReportError::Unavailable {
                operation_id: operation_id.clone(),
                message: error.to_string(),
            },
        })?;

    let status = handlers
        .controllers()
        .repository()
        .get(&operation_id)
        .await
        .map_err(|error| CoreReplaceReportError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?
        .ok_or(CoreReplaceReportError::Unavailable {
            operation_id: operation_id.clone(),
            message: corrupt("missing core-replace operation after report"),
        })?;
    let OperationStatus::CoreReplace {
        last_event_sequence,
        ..
    } = status
    else {
        return Err(CoreReplaceReportError::Unavailable {
            operation_id,
            message: corrupt("reported operation is not core-replace"),
        });
    };

    Ok(CoreReplaceReported {
        operation_id,
        machine_id,
        last_event_sequence,
        outcome: request.outcome,
    })
}
