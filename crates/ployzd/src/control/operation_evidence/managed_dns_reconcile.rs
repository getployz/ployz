use super::{
    AcceptedManagedDnsReconcileSubmission, ManagedDnsReconcileOperationSubmission,
    ManagedDnsReconcilePayload, OperationRepository, OperationStatusWrite,
    RecordManagedDnsReconcileTransitionError, RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    ManagedDnsReconcileOperationState, ManagedDnsReconcileSubject, ManagedDnsReconcileTransition,
    OperationStatus,
};

impl OperationRepository {
    pub async fn accepted_managed_dns_reconcile_operations(
        &self,
    ) -> Result<Vec<OperationStatus>, super::OperationStatusStoreError> {
        self.store
            .call(|conn| {
                let statuses = crate::control::store::query_json_list(
                    conn,
                    "SELECT status_json FROM operations ORDER BY operation_id",
                )?;
                Ok(statuses
                    .into_iter()
                    .filter(|status| {
                        matches!(
                            status,
                            OperationStatus::ManagedDnsReconcile {
                                state: ManagedDnsReconcileOperationState::Accepted,
                                ..
                            }
                        )
                    })
                    .collect())
            })
            .await
            .map_err(|error| super::index_error(&error))
    }

    pub async fn submit_managed_dns_reconcile(
        &self,
        submission: ManagedDnsReconcileOperationSubmission,
    ) -> Result<AcceptedManagedDnsReconcileSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<ManagedDnsReconcileOperationSubmission>(
                submission.operation_id,
                ManagedDnsReconcilePayload {
                    subject: submission.subject,
                },
            )
            .await?;
        Ok(AcceptedManagedDnsReconcileSubmission {
            operation_id: submitted.operation_id,
        })
    }

    pub async fn record_managed_dns_reconcile_transition(
        &self,
        operation_id: &OperationId,
        subject: &ManagedDnsReconcileSubject,
        transition: ManagedDnsReconcileTransition,
    ) -> Result<OperationStatusWrite, RecordManagedDnsReconcileTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id, subject))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}
