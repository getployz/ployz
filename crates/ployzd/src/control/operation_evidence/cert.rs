use ployz_core::cert::AcmeHttp01Challenge;
use ployz_core::ids::{CertId, OperationId};
use ployz_core::ingress::ActiveCertificateMetadata;
use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertificateProvisionWarning, OperationEvent,
    OperationStatus,
};

use crate::control::intent::ingress_intent::upsert_active_certificate_metadata;

use super::{
    CertOperationPayload, CertOperationSubmission, OperationRepository, OperationStatusWrite,
    RecordCertTransitionError, RecordOperationEventError, RecordOperationEventOutcome, RecordTxn,
    SubmitOperationError, index_error, publish_progress, record_operation_event_in_txn,
};

impl OperationRepository {
    pub async fn submit_cert(
        &self,
        submission: CertOperationSubmission,
    ) -> Result<(), SubmitOperationError> {
        self.submit_operation::<CertOperationSubmission>(
            submission.operation_id,
            CertOperationPayload {
                cert_id: submission.cert_id,
            },
        )
        .await?;
        Ok(())
    }

    pub async fn unfinished_cert_operations(
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

    pub async fn record_cert_warning(
        &self,
        operation_id: &OperationId,
        cert_id: CertId,
        warning: CertificateProvisionWarning,
    ) -> Result<OperationStatusWrite, RecordCertTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::CertWarning {
                operation_id: operation_id.clone(),
                cert_id,
                warning,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn activate_cert(
        &self,
        operation_id: &OperationId,
        certificate: ActiveCertificateMetadata,
    ) -> Result<OperationStatusWrite, RecordCertTransitionError> {
        let closure_operation_id = operation_id.clone();
        let event = OperationEvent::CertCompleted {
            operation_id: operation_id.clone(),
            certificate: certificate.clone(),
        };
        let outcome = self
            .store
            .call(move |conn| {
                let transaction = conn.transaction()?;
                let outcome =
                    record_operation_event_in_txn(&transaction, &closure_operation_id, event)?;
                if matches!(&outcome, RecordTxn::Stored { .. }) {
                    upsert_active_certificate_metadata(&transaction, &certificate)?;
                    transaction.commit()?;
                }
                Ok(outcome)
            })
            .await
            .map_err(|error| RecordOperationEventError::StoreStatus(index_error(&error)))?;
        match outcome {
            RecordTxn::Missing => Err(RecordOperationEventError::MissingOperation {
                operation_id: operation_id.clone(),
            }),
            RecordTxn::InvalidNextSequence(error) => {
                Err(RecordOperationEventError::InvalidNextSequence(error))
            }
            RecordTxn::Projection(error) => Err(RecordOperationEventError::ProjectStatus(error)),
            RecordTxn::AlreadySatisfied {
                current_sequence,
                status: _,
            } => Ok(OperationStatusWrite::AlreadySatisfied { current_sequence }),
            RecordTxn::Stored {
                event,
                status,
                sequence: _,
            } => {
                publish_progress(&self.progress, *event, &status).await;
                Ok(OperationStatusWrite::Stored)
            }
        }
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
