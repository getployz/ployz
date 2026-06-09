//! Controller wiring for operation execution.

pub mod cert;

use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{OperationId, OperationOwnerId};
use ployz_core::install::{MachineBootstrapUrl, MachineJoinBundle, MachineJoinSecretDelivery};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenRedeemedAt, MachineAddFailure, MachineName,
    RawJoinToken,
};
use ployz_core::ops::{
    BackupTransition, DeployEvidence, DeployTransition, EventSequence, OperationEventReplayPage,
    OperationEventReplayRequest, OperationLeaseExpiresAt, OperationOwnerLease, OperationStatus,
    OperationStatusSnapshot,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    BackupOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
    MachineJoinRedemption, OperationLeaseClaim, OperationStatusReadError,
    OperationStatusStoreError, OperationStatusWrite, RecordBackupEventError,
    RecordDeployEvidenceError, RecordDeployTransitionError, RecordMachineJoinReportError,
    RecordedMachineJoinReport, RedeemMachineJoinTokenError, ReplayOperationEventsError,
    StoredOperationEvent, SubmitBackupError, SubmitDeployError, SubmitMachineAddError,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::DEFAULT_MACHINE_BOOTSTRAP_URL;
use crate::operation_lease::OperationLeasePolicy;

pub use ployz_core::ops::OperationIdempotencyKey as IdempotencyKey;

const MACHINE_JOIN_TOKEN_TTL_SECONDS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub target: DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddSubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub node_id: ployz_core::ids::NodeId,
    pub name: MachineName,
    pub gateway: FirstNodeGateway,
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCreateCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddBootstrapMaterial {
    pub raw_join_token: RawJoinToken,
    pub join_token: IssuedJoinToken,
    pub bootstrap_url: MachineBootstrapUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineAddBootstrapMaterialError {
    Clock { message: String },
    InvalidJoinTokenMaterial,
}

#[derive(Debug, Clone)]
pub struct OperationControllers {
    repository: AsyncNatsOperationRepository,
    owner_id: OperationOwnerId,
    lease_policy: OperationLeasePolicy,
    machine_bootstrap_url: MachineBootstrapUrl,
}

impl OperationControllers {
    #[must_use]
    pub fn with_owner(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
        owner_id: OperationOwnerId,
        machine_bootstrap_url: MachineBootstrapUrl,
    ) -> Self {
        Self::with_owner_and_lease_policy(
            event_log,
            status_store,
            owner_id,
            machine_bootstrap_url,
            OperationLeasePolicy::default_policy(),
        )
    }

    #[must_use]
    pub fn with_owner_and_lease_policy(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
        owner_id: OperationOwnerId,
        machine_bootstrap_url: MachineBootstrapUrl,
        lease_policy: OperationLeasePolicy,
    ) -> Self {
        Self {
            repository: AsyncNatsOperationRepository::new(event_log, status_store),
            owner_id,
            lease_policy,
            machine_bootstrap_url,
        }
    }

    #[must_use]
    pub fn for_test(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
    ) -> Self {
        Self::with_owner(
            event_log,
            status_store,
            test_owner_id(),
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default machine bootstrap URL is valid"),
        )
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
                    target: command.target,
                    idempotency_key: command.idempotency_key,
                },
                self.lease_claim()?,
            )
            .await?;

        Ok(AcceptedDeployOperation {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target: submitted.target,
            lease: submitted.lease,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn submit_machine_add(
        &self,
        command: MachineAddSubmitCommand,
    ) -> Result<AcceptedMachineAddOperation, SubmitMachineAddError> {
        let submitted = self
            .repository
            .submit_machine_add(
                MachineAddOperationSubmission {
                    operation_id: command.operation_id,
                    node_id: command.node_id,
                    name: command.name,
                    gateway: command.gateway,
                    join_bundle: command.join_bundle,
                    secret_delivery: command.secret_delivery,
                    join_token: command.join_token,
                    raw_join_token: command.raw_join_token,
                    idempotency_key: command.idempotency_key,
                },
                self.machine_add_lease_claim()?,
            )
            .await?;

        Ok(AcceptedMachineAddOperation {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            node_id: submitted.node_id,
            name: submitted.name,
            gateway: submitted.gateway,
            join_bundle: submitted.join_bundle,
            join_token: submitted.join_token,
            raw_join_token: submitted.raw_join_token,
            lease: submitted.lease,
        })
    }

    pub async fn submit_backup(
        &self,
        command: BackupCreateCommand,
    ) -> Result<AcceptedBackupOperation, SubmitBackupError> {
        let submitted = self
            .repository
            .submit_backup(
                BackupOperationSubmission {
                    operation_id: command.operation_id,
                    idempotency_key: command.idempotency_key,
                },
                self.backup_lease_claim()?,
            )
            .await?;

        Ok(AcceptedBackupOperation {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            lease: submitted.lease,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn redeem_machine_join_token(
        &self,
        token: &RawJoinToken,
    ) -> Result<MachineJoinRedemption, RedeemMachineJoinTokenError> {
        self.repository
            .redeem_machine_join_token(token, current_join_time()?)
            .await
    }

    pub async fn record_machine_join_completed(
        &self,
        token: &RawJoinToken,
    ) -> Result<RecordedMachineJoinReport, RecordMachineJoinReportError> {
        self.repository.record_machine_join_completed(token).await
    }

    pub async fn record_machine_join_failed(
        &self,
        token: &RawJoinToken,
        failure: MachineAddFailure,
    ) -> Result<RecordedMachineJoinReport, RecordMachineJoinReportError> {
        self.repository
            .record_machine_join_failed(token, failure)
            .await
    }

    pub fn issue_machine_add_bootstrap_material(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
        let now =
            self.current_lease_time()
                .map_err(|error| MachineAddBootstrapMaterialError::Clock {
                    message: error.message,
                })?;
        let raw_join_token =
            RawJoinToken::try_new(format!("join_{}_{}", operation_id.as_str(), nuid::next()))
                .map_err(|_| MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial)?;
        let fingerprint = raw_join_token
            .fingerprint()
            .map_err(|_| MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial)?;
        let expires_at = JoinTokenExpiresAt::try_new(
            now.unix_seconds()
                .saturating_add(MACHINE_JOIN_TOKEN_TTL_SECONDS),
        )
        .map_err(|_| MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial)?;

        Ok(MachineAddBootstrapMaterial {
            raw_join_token,
            join_token: IssuedJoinToken::new(fingerprint, expires_at),
            bootstrap_url: self.machine_bootstrap_url.clone(),
        })
    }

    #[must_use]
    pub fn owner_id(&self) -> &OperationOwnerId {
        &self.owner_id
    }

    #[must_use]
    pub const fn lease_policy(&self) -> OperationLeasePolicy {
        self.lease_policy
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

    pub async fn record_backup_transition(
        &self,
        operation_id: &OperationId,
        transition: BackupTransition,
    ) -> Result<OperationStatusWrite, RecordBackupEventError> {
        self.repository
            .record_backup_transition(operation_id, transition)
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

    pub async fn claim_owner_lease(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationOwnerLease, OperationStatusStoreError> {
        self.repository
            .claim_owner_lease(
                operation_id,
                self.build_lease_claim()
                    .map_err(|message| OperationStatusStoreError::Clock { message })?,
            )
            .await
    }

    pub async fn renew_owner_lease(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationOwnerLease>, OperationStatusStoreError> {
        self.repository
            .renew_owner_lease(
                operation_id,
                self.build_lease_claim()
                    .map_err(|message| OperationStatusStoreError::Clock { message })?,
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
        self.build_lease_claim()
            .map_err(|message| SubmitDeployError::Clock { message })
    }

    fn machine_add_lease_claim(&self) -> Result<OperationLeaseClaim, SubmitMachineAddError> {
        self.build_lease_claim()
            .map_err(|message| SubmitMachineAddError::Clock { message })
    }

    fn backup_lease_claim(&self) -> Result<OperationLeaseClaim, SubmitBackupError> {
        self.build_lease_claim()
            .map_err(|message| SubmitBackupError::Clock { message })
    }

    fn build_lease_claim(&self) -> Result<OperationLeaseClaim, String> {
        let now = self.current_lease_time().map_err(|error| error.message)?;
        let expires_at = OperationLeaseExpiresAt::try_new(
            now.unix_seconds()
                .saturating_add(self.lease_policy.duration_seconds().get()),
        )
        .map_err(|error| error.to_string())?;

        OperationLeaseClaim::try_new(self.owner_id.clone(), now, expires_at)
            .map_err(|error| error.to_string())
    }

    fn current_lease_time(&self) -> Result<OperationLeaseExpiresAt, OperationLeaseClockError> {
        current_lease_time()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeployOperation {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target: DeployRequest,
    pub lease: OperationOwnerLease,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineAddOperation {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub node_id: ployz_core::ids::NodeId,
    pub name: MachineName,
    pub gateway: FirstNodeGateway,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
    pub lease: OperationOwnerLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedBackupOperation {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub lease: OperationOwnerLease,
    pub should_start_execution: bool,
}

fn test_owner_id() -> OperationOwnerId {
    match OperationOwnerId::try_new("control") {
        Ok(owner_id) => owner_id,
        Err(error) => panic!("test operation owner id is invalid: {error}"),
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

fn current_join_time() -> Result<JoinTokenRedeemedAt, RedeemMachineJoinTokenError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RedeemMachineJoinTokenError::Clock {
            message: error.to_string(),
        })?
        .as_secs();

    JoinTokenRedeemedAt::try_new(seconds).map_err(|error| RedeemMachineJoinTokenError::Clock {
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationLeaseClockError {
    message: String,
}
