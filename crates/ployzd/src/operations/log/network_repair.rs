use super::{
    AcceptedNetworkRepairSubmission, NetworkRepairOperationSubmission, NetworkRepairPayload,
    OperationRepository, OperationStatusWrite, RecordNetworkRepairEvidenceError,
    RecordNetworkRepairTransitionError, RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{EventSequence, NetworkRepairEvidence, NetworkRepairTransition};

impl OperationRepository {
    pub async fn submit_network_repair(
        &self,
        submission: NetworkRepairOperationSubmission,
    ) -> Result<AcceptedNetworkRepairSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<NetworkRepairOperationSubmission>(
                submission.operation_id,
                NetworkRepairPayload {
                    target_machine_id: submission.target_machine_id,
                },
            )
            .await?;
        Ok(AcceptedNetworkRepairSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target_machine_id: submitted.payload.target_machine_id,
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

    pub async fn record_network_repair_evidence(
        &self,
        operation_id: &OperationId,
        evidence: NetworkRepairEvidence,
    ) -> Result<EventSequence, RecordNetworkRepairEvidenceError> {
        self.record_operation_event(operation_id, evidence.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::sequence)
    }
}
