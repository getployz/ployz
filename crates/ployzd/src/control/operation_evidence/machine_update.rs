use super::{
    AcceptedMachineUpdateSubmission, MachineUpdateOperationSubmission, MachineUpdatePayload,
    OperationRepository, OperationStatusWrite, RecordOperationEventError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::MachineUpdateTransition;

impl OperationRepository {
    pub async fn submit_machine_update(
        &self,
        submission: MachineUpdateOperationSubmission,
    ) -> Result<AcceptedMachineUpdateSubmission, SubmitOperationError> {
        let payload = MachineUpdatePayload {
            machine_id: submission.machine_id,
            target_version: submission.target_version,
        };
        let submitted = self
            .submit_operation::<MachineUpdateOperationSubmission>(submission.operation_id, payload)
            .await?;
        Ok(AcceptedMachineUpdateSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            target_version: submitted.payload.target_version,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_machine_update_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineUpdateTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
