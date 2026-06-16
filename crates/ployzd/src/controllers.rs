//! Controller wiring for operation execution.

pub mod cert;

use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{OperationId, OperationOwnerId};
use ployz_core::install::{MachineBootstrapUrl, MachineJoinBundle, MachineJoinTemplate};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenRedeemedAt, MachineName, RawJoinToken,
};
use ployz_core::ops::{OperationLeaseExpiresAt, OperationOwnerLease, OperationStatusSnapshot};
use ployz_core::roles::InstallRolePolicy;
use ployz_nats::operations::{
    AcceptedBackupSubmission, AcceptedDeploySubmission, AcceptedMachineAddSubmission,
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    BackupOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
    MachineJoinRedemption, OperationLeaseClaim, OperationStatusStoreError,
    RedeemMachineJoinTokenError, SubmitMachineAddError, SubmitOperationError,
};
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCreateCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
}

/// Non-secret material available at submit time: the install line needs
/// only the join token, bootstrap URL, and join bundle. The per-machine
/// secret is minted afterwards as bounded operation work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddBootstrapMaterial {
    pub raw_join_token: RawJoinToken,
    pub join_token: IssuedJoinToken,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_bundle: MachineJoinBundle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineAddBootstrapMaterialError {
    Clock { message: String },
    InvalidJoinTokenMaterial,
    MissingJoinTemplate,
    StatusRead { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddBootstrapConfig {
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_template: Option<Box<MachineJoinTemplate>>,
}

impl MachineAddBootstrapConfig {
    #[must_use]
    pub fn new(bootstrap_url: MachineBootstrapUrl) -> Self {
        Self {
            bootstrap_url,
            join_template: None,
        }
    }

    #[must_use]
    pub fn with_join_template(mut self, join_template: MachineJoinTemplate) -> Self {
        self.join_template = Some(Box::new(join_template));
        self
    }
}

#[derive(Debug, Clone)]
pub struct OperationControllers {
    repository: AsyncNatsOperationRepository,
    owner_id: OperationOwnerId,
    lease_policy: OperationLeasePolicy,
    machine_bootstrap: MachineAddBootstrapConfig,
}

impl OperationControllers {
    #[must_use]
    pub fn with_owner(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
        owner_id: OperationOwnerId,
        machine_bootstrap: MachineAddBootstrapConfig,
    ) -> Self {
        Self::with_owner_and_lease_policy(
            event_log,
            status_store,
            owner_id,
            machine_bootstrap,
            OperationLeasePolicy::default_policy(),
        )
    }

    #[must_use]
    pub fn with_owner_and_lease_policy(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
        owner_id: OperationOwnerId,
        machine_bootstrap: MachineAddBootstrapConfig,
        lease_policy: OperationLeasePolicy,
    ) -> Self {
        Self {
            repository: AsyncNatsOperationRepository::new(event_log, status_store),
            owner_id,
            lease_policy,
            machine_bootstrap,
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
            MachineAddBootstrapConfig::new(
                MachineBootstrapUrl::try_new(crate::config::DEFAULT_MACHINE_BOOTSTRAP_URL)
                    .expect("default machine bootstrap URL is valid"),
            ),
        )
    }

    pub async fn submit_deploy(
        &self,
        command: DeploySubmitCommand,
    ) -> Result<AcceptedDeploySubmission, SubmitCommandError> {
        Ok(self
            .repository
            .submit_deploy(
                DeployOperationSubmission {
                    operation_id: command.operation_id,
                    target: command.target,
                    idempotency_key: command.idempotency_key,
                },
                self.lease_claim()?,
            )
            .await?)
    }

    pub async fn submit_machine_add(
        &self,
        command: MachineAddSubmitCommand,
    ) -> Result<AcceptedMachineAddSubmission, MachineAddSubmitCommandError> {
        Ok(self
            .repository
            .submit_machine_add(
                MachineAddOperationSubmission {
                    operation_id: command.operation_id,
                    node_id: command.node_id,
                    name: command.name,
                    roles: command.roles,
                    join_bundle: command.join_bundle,
                    join_token: command.join_token,
                    raw_join_token: command.raw_join_token,
                    idempotency_key: command.idempotency_key,
                },
                self.lease_claim()
                    .map_err(MachineAddSubmitCommandError::Submit)?,
            )
            .await?)
    }

    pub async fn submitted_machine_add_bootstrap_material(
        &self,
        idempotency_key: &IdempotencyKey,
    ) -> Result<Option<SubmittedMachineAddBootstrapMaterial>, MachineAddBootstrapMaterialError>
    {
        let Some(submission) = self
            .repository
            .records()
            .machine_add_submission(idempotency_key)
            .await
            .map_err(machine_add_bootstrap_status_read_error)?
        else {
            return Ok(None);
        };
        Ok(Some(SubmittedMachineAddBootstrapMaterial {
            join_bundle: submission.join_bundle,
            join_token: submission.join_token,
            raw_join_token: submission.raw_join_token,
        }))
    }

    pub async fn submit_backup(
        &self,
        command: BackupCreateCommand,
    ) -> Result<AcceptedBackupSubmission, SubmitCommandError> {
        Ok(self
            .repository
            .submit_backup(
                BackupOperationSubmission {
                    operation_id: command.operation_id,
                    idempotency_key: command.idempotency_key,
                },
                self.lease_claim()?,
            )
            .await?)
    }

    pub async fn redeem_machine_join_token(
        &self,
        token: &RawJoinToken,
    ) -> Result<MachineJoinRedemption, RedeemMachineJoinTokenError> {
        self.repository
            .redeem_machine_join_token(token, current_join_time()?)
            .await
    }

    /// The repository this controller submits into. Record/read paths that
    /// need no lease-clock or join-token policy go straight here.
    #[must_use]
    pub const fn repository(&self) -> &AsyncNatsOperationRepository {
        &self.repository
    }

    pub fn issue_machine_add_bootstrap_material(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
        let Some(join_template) = self.machine_bootstrap.join_template.clone() else {
            return Err(MachineAddBootstrapMaterialError::MissingJoinTemplate);
        };
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
            bootstrap_url: self.machine_bootstrap.bootstrap_url.clone(),
            join_bundle: join_template.join_bundle.clone(),
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

    #[must_use]
    pub fn machine_bootstrap_url(&self) -> &MachineBootstrapUrl {
        &self.machine_bootstrap.bootstrap_url
    }

    #[must_use]
    pub fn has_machine_join_template(&self) -> bool {
        self.machine_bootstrap.join_template.is_some()
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
        let claim = self
            .build_lease_claim()
            .map_err(|message| OperationStatusStoreError::Clock { message })?;
        self.repository
            .records()
            .claim_owner_lease(
                operation_id,
                claim.owner_id(),
                claim.now(),
                claim.expires_at(),
            )
            .await
    }

    pub async fn renew_owner_lease(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationOwnerLease>, OperationStatusStoreError> {
        let claim = self
            .build_lease_claim()
            .map_err(|message| OperationStatusStoreError::Clock { message })?;
        self.repository
            .records()
            .renew_owner_lease(
                operation_id,
                claim.owner_id(),
                claim.now(),
                claim.expires_at(),
            )
            .await
    }
}

fn machine_add_bootstrap_status_read_error(
    error: OperationStatusStoreError,
) -> MachineAddBootstrapMaterialError {
    MachineAddBootstrapMaterialError::StatusRead {
        message: format!("{error:?}"),
    }
}

impl OperationControllers {
    fn lease_claim(&self) -> Result<OperationLeaseClaim, SubmitCommandError> {
        self.build_lease_claim()
            .map_err(|message| SubmitCommandError::Clock { message })
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

/// How a submit command fails at the controller: the repository submit
/// failure plus the lease-clock read this process performs before
/// submitting (`ployz-nats` never constructs a clock failure).
#[derive(Debug)]
pub enum SubmitCommandError {
    Clock { message: String },
    Submit(SubmitOperationError),
}

impl From<SubmitOperationError> for SubmitCommandError {
    fn from(value: SubmitOperationError) -> Self {
        Self::Submit(value)
    }
}

/// Machine-add extends the shared submit command failure with join-token
/// validation.
#[derive(Debug)]
pub enum MachineAddSubmitCommandError {
    Submit(SubmitCommandError),
    JoinTokenMismatch,
}

impl From<SubmitMachineAddError> for MachineAddSubmitCommandError {
    fn from(value: SubmitMachineAddError) -> Self {
        match value {
            SubmitMachineAddError::Operation(error) => {
                Self::Submit(SubmitCommandError::Submit(error))
            }
            SubmitMachineAddError::JoinTokenMismatch => Self::JoinTokenMismatch,
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedMachineAddBootstrapMaterial {
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
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
