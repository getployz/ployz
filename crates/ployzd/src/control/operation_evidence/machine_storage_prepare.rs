use super::{
    AcceptedMachineStoragePrepareSubmission, MachineStoragePrepareOperationSubmission,
    MachineStoragePreparePayload, OperationRepository, OperationStatusWrite,
    RecordOperationEventError, RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::MachineStoragePrepareTransition;

impl OperationRepository {
    pub async fn submit_machine_storage_prepare(
        &self,
        submission: MachineStoragePrepareOperationSubmission,
    ) -> Result<AcceptedMachineStoragePrepareSubmission, SubmitOperationError> {
        let payload = MachineStoragePreparePayload {
            machine_id: submission.machine_id,
            requested_pool: submission.requested_pool,
        };
        let submitted = self
            .submit_operation::<MachineStoragePrepareOperationSubmission>(
                submission.operation_id,
                payload,
            )
            .await?;
        Ok(AcceptedMachineStoragePrepareSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            requested_pool: submitted.payload.requested_pool,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_machine_storage_prepare_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineStoragePrepareTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
