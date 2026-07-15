use ployz_core::ids::OperationId;
use ployz_core::operation::IngressRefreshTransition;

use super::{
    AcceptedIngressRefreshSubmission, IngressRefreshOperationSubmission, IngressRefreshPayload,
    OperationRepository, OperationStatusWrite, RecordIngressRefreshTransitionError,
    RecordOperationEventOutcome, SubmitOperationError,
};

impl OperationRepository {
    pub async fn submit_ingress_refresh(
        &self,
        submission: IngressRefreshOperationSubmission,
    ) -> Result<AcceptedIngressRefreshSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<IngressRefreshOperationSubmission>(
                submission.operation_id,
                IngressRefreshPayload,
            )
            .await?;
        Ok(AcceptedIngressRefreshSubmission {
            operation_id: submitted.operation_id,
        })
    }

    pub async fn record_ingress_refresh_transition(
        &self,
        operation_id: &OperationId,
        transition: IngressRefreshTransition,
    ) -> Result<OperationStatusWrite, RecordIngressRefreshTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
