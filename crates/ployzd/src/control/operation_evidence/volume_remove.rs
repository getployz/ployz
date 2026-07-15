use super::{
    AcceptedVolumeRemoveSubmission, OperationRepository, OperationStatusWrite,
    RecordOperationEventOutcome, RecordVolumeRemoveTransitionError, SubmitOperationError,
    VolumeRemoveOperationSubmission, VolumeRemovePayload,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::VolumeRemoveTransition;

impl OperationRepository {
    pub async fn submit_volume_remove(
        &self,
        submission: VolumeRemoveOperationSubmission,
    ) -> Result<AcceptedVolumeRemoveSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<VolumeRemoveOperationSubmission>(
                submission.operation_id,
                VolumeRemovePayload {
                    namespace_id: submission.namespace_id,
                    volume_name: submission.volume_name,
                },
            )
            .await?;
        Ok(AcceptedVolumeRemoveSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            namespace_id: submitted.payload.namespace_id,
            volume_name: submitted.payload.volume_name,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_volume_remove_transition(
        &self,
        operation_id: &OperationId,
        transition: VolumeRemoveTransition,
    ) -> Result<OperationStatusWrite, RecordVolumeRemoveTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
