use ployz_core::cert::{AcmeHttp01Challenge, ActiveCertState};
use ployz_core::ids::{CertId, OperationId};
use ployz_core::ops::{CertOperationFailure, CertOperationState, OperationEvent, OperationStatus};

use super::{
    AcceptedCertSubmission, CertOperationPayload, CertOperationSubmission, OperationRepository,
    OperationStatusWrite, RecordCertTransitionError, RecordOperationEventOutcome,
    SubmitOperationError,
};

impl OperationRepository {
    pub async fn submit_cert(
        &self,
        submission: CertOperationSubmission,
    ) -> Result<AcceptedCertSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<CertOperationSubmission>(
                submission.operation_id,
                CertOperationPayload {
                    cert_id: submission.cert_id,
                },
            )
            .await?;
        Ok(AcceptedCertSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            cert_id: submitted.payload.cert_id,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn unfinished_cert_operations(
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
                            OperationStatus::Cert {
                                state: CertOperationState::Accepted
                                    | CertOperationState::Running { .. },
                                ..
                            }
                        )
                    })
                    .collect())
            })
            .await
            .map_err(|error| super::index_error(&error))
    }

    pub async fn record_cert_challenge(
        &self,
        operation_id: &OperationId,
        cert_id: CertId,
        challenge: AcmeHttp01Challenge,
    ) -> Result<OperationStatusWrite, RecordCertTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::CertChallengePublished {
                operation_id: operation_id.clone(),
                cert_id,
                challenge,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_cert_validation_started(
        &self,
        operation_id: &OperationId,
        cert_id: CertId,
    ) -> Result<OperationStatusWrite, RecordCertTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::CertValidationStarted {
                operation_id: operation_id.clone(),
                cert_id,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_cert_completed(
        &self,
        operation_id: &OperationId,
        active_cert: ActiveCertState,
    ) -> Result<OperationStatusWrite, RecordCertTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::CertCompleted {
                operation_id: operation_id.clone(),
                active_cert,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_cert_failed(
        &self,
        operation_id: &OperationId,
        failure: CertOperationFailure,
    ) -> Result<OperationStatusWrite, RecordCertTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::CertFailed {
                operation_id: operation_id.clone(),
                failure,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }
}
