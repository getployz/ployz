//! Controller wiring for operation execution.

use ployz_core::ids::{OperationId, OperationOwnerId, ServiceId};
use ployz_core::ops::{
    DeployEvidence, DeployTransition, EventSequence, OperationEventReplayPage,
    OperationEventReplayRequest, OperationLeaseDurationSeconds, OperationLeaseExpiresAt,
    OperationOwnerLease, OperationStatus, OperationStatusSnapshot,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, OperationLeaseClaim, OperationStatusReadError,
    OperationStatusStoreError, OperationStatusWrite, RecordDeployEvidenceError,
    RecordDeployTransitionError, ReplayOperationEventsError, StoredOperationEvent,
    SubmitDeployError,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub use ployz_core::ops::OperationIdempotencyKey as IdempotencyKey;

pub const DEFAULT_OPERATION_LEASE_SECONDS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone)]
pub struct OperationControllers {
    repository: AsyncNatsOperationRepository,
    owner_id: OperationOwnerId,
    lease_seconds: OperationLeaseDurationSeconds,
}

impl OperationControllers {
    #[must_use]
    pub fn with_owner(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
        owner_id: OperationOwnerId,
    ) -> Self {
        Self {
            repository: AsyncNatsOperationRepository::new(event_log, status_store),
            owner_id,
            lease_seconds: default_lease_seconds(),
        }
    }

    #[must_use]
    pub fn for_test(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
    ) -> Self {
        Self::with_owner(event_log, status_store, test_owner_id())
    }

    pub async fn submit_deploy(
        &self,
        command: DeploySubmitCommand,
    ) -> Result<AcceptedDeployOperation, SubmitDeployError> {
        let submitted = self
            .repository
            .submit_deploy(
                DeployOperationSubmission {
                    operation_id: command.operation_id,
                    service_id: command.service_id,
                    idempotency_key: command.idempotency_key,
                },
                self.lease_claim()?,
            )
            .await?;

        Ok(AcceptedDeployOperation {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            lease: submitted.lease,
        })
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
    ) -> Result<Option<OperationStatus>, OperationStatusReadError> {
        self.repository.operation_status(operation_id).await
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusStoreError> {
        self.repository
            .operation_status_snapshot(
                operation_id,
                self.current_lease_time()
                    .map_err(|error| OperationStatusStoreError::Clock {
                        message: error.message,
                    })?,
            )
            .await
    }

    pub async fn replay_operation_events(
        &self,
        request: OperationEventReplayRequest,
    ) -> Result<OperationEventReplayPage, ReplayOperationEventsError> {
        self.repository.replay_operation_events(request).await
    }
}

impl OperationControllers {
    fn lease_claim(&self) -> Result<OperationLeaseClaim, SubmitDeployError> {
        let now = self
            .current_lease_time()
            .map_err(|error| SubmitDeployError::Clock {
                message: error.message,
            })?;
        let expires_at = OperationLeaseExpiresAt::try_new(
            now.unix_seconds().saturating_add(self.lease_seconds.get()),
        )
        .map_err(|error| SubmitDeployError::Clock {
            message: error.to_string(),
        })?;

        OperationLeaseClaim::try_new(self.owner_id.clone(), now, expires_at).map_err(|error| {
            SubmitDeployError::Clock {
                message: error.to_string(),
            }
        })
    }

    fn current_lease_time(&self) -> Result<OperationLeaseExpiresAt, OperationLeaseClockError> {
        current_lease_time()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeployOperation {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub lease: OperationOwnerLease,
}

fn test_owner_id() -> OperationOwnerId {
    match OperationOwnerId::try_new("control") {
        Ok(owner_id) => owner_id,
        Err(error) => panic!("test operation owner id is invalid: {error}"),
    }
}

fn default_lease_seconds() -> OperationLeaseDurationSeconds {
    match OperationLeaseDurationSeconds::try_new(DEFAULT_OPERATION_LEASE_SECONDS) {
        Ok(duration) => duration,
        Err(error) => panic!("default operation lease duration is invalid: {error}"),
    }
}

fn current_lease_time() -> Result<OperationLeaseExpiresAt, OperationLeaseClockError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| OperationLeaseClockError {
            message: error.to_string(),
        })?
        .as_secs();

    OperationLeaseExpiresAt::try_new(seconds).map_err(|error| OperationLeaseClockError {
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationLeaseClockError {
    message: String,
}
