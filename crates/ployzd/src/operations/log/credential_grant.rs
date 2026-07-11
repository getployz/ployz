use super::{
    AcceptedCredentialGrantSubmission, CredentialGrantOperationSubmission, OperationRepository,
    OperationStatusWrite, RecordOperationEventError, RecordOperationEventOutcome,
    SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::CredentialGrantTransition;

impl OperationRepository {
    pub async fn submit_credential_grant(
        &self,
        submission: CredentialGrantOperationSubmission,
    ) -> Result<AcceptedCredentialGrantSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<CredentialGrantOperationSubmission>(
                submission.operation_id,
                submission.action,
            )
            .await?;
        Ok(AcceptedCredentialGrantSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            action: submitted.payload,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_credential_grant_transition(
        &self,
        operation_id: &OperationId,
        transition: CredentialGrantTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
