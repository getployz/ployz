use super::{
    AcceptedIngressConfigureSubmission, IngressConfigureOperationSubmission, OperationRepository,
    OperationStatusWrite, RecordIngressConfigureTransitionError, RecordOperationEventOutcome,
    SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::IngressConfigureTransition;

impl OperationRepository {
    pub async fn submit_ingress_configure(
        &self,
        submission: IngressConfigureOperationSubmission,
    ) -> Result<AcceptedIngressConfigureSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<IngressConfigureOperationSubmission>(
                submission.operation_id,
                submission.configuration,
            )
            .await?;
        Ok(AcceptedIngressConfigureSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            configuration: submitted.payload,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_ingress_configure_transition(
        &self,
        operation_id: &OperationId,
        transition: IngressConfigureTransition,
    ) -> Result<OperationStatusWrite, RecordIngressConfigureTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
