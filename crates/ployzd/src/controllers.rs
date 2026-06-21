//! Controller wiring for operation execution.

pub mod cert;

use ployz_core::backup::{BackupTarget, BackupTargetValidationError};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::OperationId;
use ployz_core::install::{MachineBootstrapUrl, MachineJoinBundle, MachineJoinTemplate};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenRedeemedAt, MachineName, RawJoinToken,
};
use ployz_core::ops::OperationStatusSnapshot;
use ployz_core::roles::InstallRolePolicy;
use ployz_nats::operations::{
    AcceptedBackupSubmission, AcceptedDeploySubmission, AcceptedMachineAddSubmission,
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    BackupOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
    MachineJoinRedemption, OperationStatusReadError, RedeemMachineJoinTokenError,
    SubmitMachineAddError, SubmitOperationError,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub use ployz_core::ops::OperationIdempotencyKey as IdempotencyKey;

const MACHINE_JOIN_TOKEN_TTL_SECONDS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySubmitCommand {
    pub operation_id: OperationId,
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
    pub target: BackupTarget,
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
    machine_bootstrap: MachineAddBootstrapConfig,
}

impl OperationControllers {
    #[must_use]
    pub fn new(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
        machine_bootstrap: MachineAddBootstrapConfig,
    ) -> Self {
        Self {
            repository: AsyncNatsOperationRepository::new(event_log, status_store),
            machine_bootstrap,
        }
    }

    #[must_use]
    pub fn for_test(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
    ) -> Self {
        Self::new(
            event_log,
            status_store,
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
            .submit_deploy(DeployOperationSubmission {
                operation_id: command.operation_id,
                target: command.target,
            })
            .await?)
    }

    pub async fn submit_machine_add(
        &self,
        command: MachineAddSubmitCommand,
    ) -> Result<AcceptedMachineAddSubmission, MachineAddSubmitCommandError> {
        Ok(self
            .repository
            .submit_machine_add(MachineAddOperationSubmission {
                operation_id: command.operation_id,
                node_id: command.node_id,
                name: command.name,
                roles: command.roles,
                join_bundle: command.join_bundle,
                join_token: command.join_token,
                raw_join_token: command.raw_join_token,
                idempotency_key: command.idempotency_key,
            })
            .await?)
    }

    pub async fn submit_backup(
        &self,
        command: BackupCreateCommand,
    ) -> Result<AcceptedBackupSubmission, BackupSubmitCommandError> {
        command
            .target
            .validate_create()
            .map_err(BackupSubmitCommandError::InvalidTarget)?;
        Ok(self
            .repository
            .submit_backup(BackupOperationSubmission {
                operation_id: command.operation_id,
                target: command.target,
            })
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
    /// need no command-side join-token policy go straight here.
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
        let now = current_unix_seconds()
            .map_err(|message| MachineAddBootstrapMaterialError::Clock { message })?;
        let raw_join_token =
            RawJoinToken::try_new(format!("join_{}_{}", operation_id.as_str(), nuid::next()))
                .map_err(|_| MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial)?;
        let fingerprint = raw_join_token
            .fingerprint()
            .map_err(|_| MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial)?;
        let expires_at =
            JoinTokenExpiresAt::try_new(now.saturating_add(MACHINE_JOIN_TOKEN_TTL_SECONDS))
                .map_err(|_| MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial)?;

        Ok(MachineAddBootstrapMaterial {
            raw_join_token,
            join_token: IssuedJoinToken::new(fingerprint, expires_at),
            bootstrap_url: self.machine_bootstrap.bootstrap_url.clone(),
            join_bundle: join_template.join_bundle.clone(),
        })
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
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusReadError> {
        self.repository
            .operation_status_snapshot(operation_id)
            .await
    }
}

/// How a submit command fails at the controller.
#[derive(Debug)]
pub enum SubmitCommandError {
    Submit(SubmitOperationError),
}

impl From<SubmitOperationError> for SubmitCommandError {
    fn from(value: SubmitOperationError) -> Self {
        Self::Submit(value)
    }
}

/// Backup-create extends the shared submit command failure with target
/// validation that must happen before an operation is recorded.
#[derive(Debug)]
pub enum BackupSubmitCommandError {
    InvalidTarget(BackupTargetValidationError),
    Submit(SubmitCommandError),
}

impl From<SubmitCommandError> for BackupSubmitCommandError {
    fn from(value: SubmitCommandError) -> Self {
        Self::Submit(value)
    }
}

impl From<SubmitOperationError> for BackupSubmitCommandError {
    fn from(value: SubmitOperationError) -> Self {
        Self::Submit(SubmitCommandError::Submit(value))
    }
}

/// Machine-add extends the shared submit command failure with join-token
/// validation.
#[derive(Debug)]
pub enum MachineAddSubmitCommandError {
    Submit(SubmitCommandError),
    JoinTokenMismatch,
    DuplicateIdempotencyKey,
}

impl From<SubmitMachineAddError> for MachineAddSubmitCommandError {
    fn from(value: SubmitMachineAddError) -> Self {
        match value {
            SubmitMachineAddError::Operation(error) => {
                Self::Submit(SubmitCommandError::Submit(error))
            }
            SubmitMachineAddError::JoinTokenMismatch => Self::JoinTokenMismatch,
            SubmitMachineAddError::DuplicateIdempotencyKey => Self::DuplicateIdempotencyKey,
        }
    }
}

fn current_unix_seconds() -> Result<u64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();

    Ok(seconds)
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
