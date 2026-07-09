use super::{
    AcceptedManagedLeaseSubmission, ManagedLeaseOperationSubmission, ManagedLeasePayload,
    OperationRepository, OperationStatusWrite, RecordManagedLeaseTransitionError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::cert::ManagedLeaseName;
use ployz_core::ids::OperationId;
use ployz_core::ops::ManagedLeaseTransition;
use ployz_core::ops::{ManagedLeaseOperationState, OperationStatus};

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
                    lease_name: submission.lease_name,
                },
            )
            .await?;
        Ok(AcceptedManagedLeaseSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            lease_name: submitted.payload.lease_name,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_managed_lease_transition(
        &self,
        operation_id: &OperationId,
        lease_name: &ManagedLeaseName,
        transition: ManagedLeaseTransition,
    ) -> Result<OperationStatusWrite, RecordManagedLeaseTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id, lease_name))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
