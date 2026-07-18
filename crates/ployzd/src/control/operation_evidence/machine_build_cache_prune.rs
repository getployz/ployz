use super::{
    AcceptedMachineBuildCachePruneSubmission, MachineBuildCachePruneOperationSubmission,
    MachineBuildCachePrunePayload, OperationRepository, OperationStatusWrite,
    RecordOperationEventError, RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::MachineBuildCachePruneTransition;

impl OperationRepository {
    pub async fn submit_machine_build_cache_prune(
        &self,
        submission: MachineBuildCachePruneOperationSubmission,
    ) -> Result<AcceptedMachineBuildCachePruneSubmission, SubmitOperationError> {
        let payload = MachineBuildCachePrunePayload {
            machine_id: submission.machine_id,
        };
        let submitted = self
            .submit_operation::<MachineBuildCachePruneOperationSubmission>(
                submission.operation_id,
                payload,
            )
            .await?;
        Ok(AcceptedMachineBuildCachePruneSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_machine_build_cache_prune_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineBuildCachePruneTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
