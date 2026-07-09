use super::{
    AcceptedManagedLeaseSubmission, ManagedLeaseOperationSubmission, ManagedLeasePayload,
    OperationRepository, OperationStatusWrite, RecordManagedLeaseTransitionError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{ManagedLeaseOperationState, OperationStatus};
use ployz_core::ops::{ManagedLeaseSubject, ManagedLeaseTransition};

impl OperationRepository {
    pub async fn accepted_managed_lease_operations(
        &self,
    ) -> Result<Vec<OperationStatus>, super::OperationStatusStoreError> {
        self.store
            .call(|conn| {
                let statuses = crate::core_store::query_json_list(
                    conn,
                    "SELECT status_json FROM operations ORDER BY operation_id",
                )?;
                Ok(statuses
                    .into_iter()
                    .filter(|status| {
                        matches!(
                            status,
                            OperationStatus::ManagedLease {
                                state: ManagedLeaseOperationState::Accepted,
                                ..
                            }
                        )
                    })
                    .collect())
            })
            .await
            .map_err(|error| super::index_error(&error))
    }

    pub async fn submit_managed_lease(
        &self,
        submission: ManagedLeaseOperationSubmission,
    ) -> Result<AcceptedManagedLeaseSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<ManagedLeaseOperationSubmission>(
                submission.operation_id,
                ManagedLeasePayload {
                    subject: submission.subject,
                },
            )
            .await?;
        Ok(AcceptedManagedLeaseSubmission {
            operation_id: submitted.operation_id,
        })
    }

    pub async fn record_managed_lease_transition(
        &self,
        operation_id: &OperationId,
        subject: &ManagedLeaseSubject,
        transition: ManagedLeaseTransition,
    ) -> Result<OperationStatusWrite, RecordManagedLeaseTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id, subject))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
