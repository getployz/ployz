//! Controller wiring for operation execution.

use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{DeployEvidence, DeployTransition, EventSequence, OperationStatus};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, OperationStatusStoreError, OperationStatusWrite,
    RecordDeployEvidenceError, RecordDeployTransitionError, StoredDeploySubmission,
    StoredOperationEvent, SubmitDeployError,
};

pub use ployz_core::ops::OperationIdempotencyKey as IdempotencyKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone)]
pub struct OperationControllers {
    repository: AsyncNatsOperationRepository,
}

impl OperationControllers {
    #[must_use]
    pub fn new(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
    ) -> Self {
        Self {
            repository: AsyncNatsOperationRepository::new(event_log, status_store),
        }
    }

    pub async fn submit_deploy(
        &self,
        command: DeploySubmitCommand,
    ) -> Result<AcceptedDeployOperation, SubmitDeployError> {
        self.repository
            .submit_deploy(DeployOperationSubmission {
                operation_id: command.operation_id,
                service_id: command.service_id,
                idempotency_key: command.idempotency_key,
            })
            .await
            .map(Into::into)
    }

    pub async fn record_deploy_transition(
        &self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        self.repository
            .record_deploy_transition(operation_id, transition)
            .await
    }

    pub async fn record_deploy_evidence(
        &self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<StoredOperationEvent, RecordDeployEvidenceError> {
        self.repository
            .record_deploy_evidence(operation_id, evidence)
            .await
    }

    pub async fn operation_status(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusStoreError> {
        self.repository.operation_status(operation_id).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeployOperation {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
}

impl From<StoredDeploySubmission> for AcceptedDeployOperation {
    fn from(value: StoredDeploySubmission) -> Self {
        Self {
            operation_id: value.operation_id,
            start_sequence: value.start_sequence,
        }
    }
}
