use super::{
    AcceptedNetworkRepairSubmission, NetworkRepairOperationSubmission, NetworkRepairPayload,
    OperationRepository, OperationStatusWrite, RecordNetworkRepairTransitionError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::NetworkRepairTransition;

impl OperationRepository {
    pub async fn submit_network_repair(
        &self,
        submission: NetworkRepairOperationSubmission,
    ) -> Result<AcceptedNetworkRepairSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<NetworkRepairOperationSubmission>(
                submission.operation_id,
                NetworkRepairPayload,
            )
            .await?;
        Ok(AcceptedNetworkRepairSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_network_repair_transition(
        &self,
        operation_id: &OperationId,
        transition: NetworkRepairTransition,
    ) -> Result<OperationStatusWrite, RecordNetworkRepairTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
