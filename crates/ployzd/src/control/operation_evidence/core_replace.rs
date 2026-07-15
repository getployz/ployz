use super::{
    AcceptedCoreReplaceSubmission, CoreReplaceOperationSubmission, CoreReplacePayload,
    OperationRepository, OperationStatusWrite, RecordOperationEventError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::CoreReplaceTransition;

impl OperationRepository {
    pub async fn submit_core_replace(
        &self,
        submission: CoreReplaceOperationSubmission,
    ) -> Result<AcceptedCoreReplaceSubmission, SubmitOperationError> {
        let payload = CoreReplacePayload {
            machine_id: submission.machine_id,
            successor_nats_url: submission.successor_nats_url,
        };
        let submitted = self
            .submit_operation::<CoreReplaceOperationSubmission>(submission.operation_id, payload)
            .await?;
        Ok(AcceptedCoreReplaceSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            successor_nats_url: submitted.payload.successor_nats_url,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_core_replace_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: CoreReplaceTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
