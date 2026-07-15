use super::{
    AcceptedMachineLifecycleSubmission, MachineLifecycleOperationSubmission,
    MachineLifecyclePayload, OperationRepository, OperationStatusWrite, RecordOperationEventError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::MachineLifecycleTransition;

impl OperationRepository {
    pub async fn submit_machine_lifecycle(
        &self,
        submission: MachineLifecycleOperationSubmission,
    ) -> Result<AcceptedMachineLifecycleSubmission, SubmitOperationError> {
        let payload = MachineLifecyclePayload {
            machine_id: submission.machine_id,
            target: submission.target,
        };
        let submitted = self
            .submit_operation::<MachineLifecycleOperationSubmission>(
                submission.operation_id,
                payload,
            )
            .await?;
        Ok(AcceptedMachineLifecycleSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            target: submitted.payload.target,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_machine_lifecycle_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineLifecycleTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
