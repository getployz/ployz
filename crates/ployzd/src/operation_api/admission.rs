//! Controller wiring for operation execution.

use crate::operations::log::{
    AcceptedDeploySubmission, AcceptedMachineAddSubmission, DeployOperationSubmission,
    MachineAddOperationSubmission, MachineJoinRedemption, MachineLifecycleOperationSubmission,
    MachineUpdateOperationSubmission, OperationRepository, OperationStatusStoreError,
    RedeemMachineJoinTokenError, SubmitMachineAddError, SubmitOperationError,
};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::install::{
    InstallArtifactVersion, MachineBootstrapUrl, MachineJoinBundle, MachineJoinSecretDelivery,
    MachineJoinTemplate,
};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenRedeemedAt, MachineName, RawJoinToken,
};
use ployz_core::ops::{OperationStatus, OperationStatusSnapshot};
use ployz_core::roles::InstallRolePolicy;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

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
    pub machine_id: ployz_core::ids::MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpdateSubmitCommand {
    pub operation_id: OperationId,
    pub machine_id: ployz_core::ids::MachineId,
    pub target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineLifecycleSubmitCommand {
    pub operation_id: OperationId,
    pub machine_id: ployz_core::ids::MachineId,
    pub target: ployz_core::state::MachineLifecycle,
}

/// Bootstrap material available at submit time.
///
/// The per-machine secret is still minted afterwards as bounded operation
/// work. `join_secret_delivery` is the low-privilege Join credential used by
/// the target keeper to redeem and report this one join token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddBootstrapMaterial {
    pub raw_join_token: RawJoinToken,
    pub join_token: IssuedJoinToken,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_bundle: MachineJoinBundle,
    pub join_secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineAddBootstrapMaterialError {
    #[error("clock: {message}")]
    Clock { message: String },
    #[error("machine-add bootstrap material contains invalid join token material")]
    InvalidJoinTokenMaterial,
    #[error("machine-add bootstrap material missing join template")]
    MissingJoinTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddBootstrapConfig {
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_material: Option<Box<MachineAddBootstrapJoinMaterial>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddBootstrapJoinMaterial {
    pub join_template: MachineJoinTemplate,
    pub join_secret_delivery: MachineJoinSecretDelivery,
}

impl MachineAddBootstrapConfig {
    #[must_use]
    pub fn new(bootstrap_url: MachineBootstrapUrl) -> Self {
        Self {
            bootstrap_url,
            join_material: None,
        }
    }

    #[must_use]
    pub fn with_join_material(
        mut self,
        join_template: MachineJoinTemplate,
        join_secret_delivery: MachineJoinSecretDelivery,
    ) -> Self {
        self.join_material = Some(Box::new(MachineAddBootstrapJoinMaterial {
            join_template,
            join_secret_delivery,
        }));
        self
    }
}

#[derive(Debug, Clone)]
pub struct OperationControllers {
    repository: OperationRepository,
    /// The namespace fence is an in-process map, not a durable cross-process
    /// lock, and that is intentional: the core is the single sequencer, so a
    /// process-local mutex is the whole cluster's mutual exclusion. It is
    /// deliberately not persisted — a restart interrupts any in-flight deploy,
    /// and the resubmit converges from observed reality rather than resuming a
    /// half-held lock. Do not reintroduce a durable lock here.
    namespace_locks: Arc<Mutex<BTreeMap<NamespaceId, OperationId>>>,
    machine_bootstrap: MachineAddBootstrapConfig,
}

impl OperationControllers {
    #[must_use]
    pub fn new(
        repository: OperationRepository,
        machine_bootstrap: MachineAddBootstrapConfig,
    ) -> Self {
        Self {
            repository,
            namespace_locks: Arc::default(),
            machine_bootstrap,
        }
    }

    pub async fn submit_deploy(
        &self,
        command: DeploySubmitCommand,
    ) -> Result<AcceptedDeploySubmission, SubmitCommandError> {
        let operation_id = command.operation_id;
        let idempotency_key = command.idempotency_key;
        let target = command.target;
        let claimed = self
            .repository
            .claim_deploy(DeployOperationSubmission {
                operation_id,
                idempotency_key,
                target,
            })
            .await?;
        let operation_id = claimed.operation_id.clone();
        let target = claimed.target.clone();
        let namespace_id = target.namespace_id.clone();
        let claim = self.claim_namespace(&namespace_id, &operation_id).await;
        if let NamespaceClaim::Busy { owner } = claim {
            return Err(SubmitCommandError::NamespaceBusy {
                namespace_id,
                owner,
            });
        }

        let submitted = self.repository.submit_deploy(claimed).await;

        match submitted {
            Ok(accepted) => {
                if !accepted.should_start_execution && matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &operation_id).await;
                }
                Ok(accepted)
            }
            Err(error) => {
                if matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &operation_id).await;
                }
                Err(SubmitCommandError::Submit(error))
            }
        }
    }

    pub async fn submit_machine_add(
        &self,
        command: MachineAddSubmitCommand,
    ) -> Result<AcceptedMachineAddSubmission, MachineAddSubmitCommandError> {
        Ok(self
            .repository
            .submit_machine_add(MachineAddOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
                name: command.name,
                roles: command.roles,
                join_bundle: command.join_bundle,
                join_token: command.join_token,
                raw_join_token: command.raw_join_token,
                idempotency_key: command.idempotency_key,
            })
            .await?)
    }

    pub async fn submit_machine_update(
        &self,
        command: MachineUpdateSubmitCommand,
    ) -> Result<crate::operations::log::AcceptedMachineUpdateSubmission, SubmitCommandError> {
        Ok(self
            .repository
            .submit_machine_update(MachineUpdateOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
                target_version: command.target_version,
            })
            .await?)
    }

    pub async fn submit_machine_lifecycle(
        &self,
        command: MachineLifecycleSubmitCommand,
    ) -> Result<crate::operations::log::AcceptedMachineLifecycleSubmission, SubmitCommandError> {
        Ok(self
            .repository
            .submit_machine_lifecycle(MachineLifecycleOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
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
    pub const fn repository(&self) -> &OperationRepository {
        &self.repository
    }

    pub fn issue_machine_add_bootstrap_material(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
        let Some(join_material) = self.machine_bootstrap.join_material.as_ref() else {
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
            join_bundle: join_material.join_template.join_bundle.clone(),
            join_secret_delivery: join_material.join_secret_delivery.clone(),
        })
    }

    #[must_use]
    pub fn machine_bootstrap_url(&self) -> &MachineBootstrapUrl {
        &self.machine_bootstrap.bootstrap_url
    }

    #[must_use]
    pub fn has_machine_join_template(&self) -> bool {
        self.machine_bootstrap.join_material.is_some()
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusStoreError> {
        self.repository
            .operation_status_snapshot(operation_id)
            .await
    }

    pub async fn operation_statuses(
        &self,
    ) -> Result<Vec<OperationStatus>, OperationStatusStoreError> {
        self.repository.operation_statuses().await
    }

    async fn claim_namespace(
        &self,
        namespace_id: &NamespaceId,
        operation_id: &OperationId,
    ) -> NamespaceClaim {
        let mut locks = self.namespace_locks.lock().await;
        match locks.get(namespace_id) {
            Some(owner) if owner == operation_id => NamespaceClaim::AlreadyOwned,
            Some(owner) => NamespaceClaim::Busy {
                owner: owner.clone(),
            },
            None => {
                locks.insert(namespace_id.clone(), operation_id.clone());
                NamespaceClaim::Acquired
            }
        }
    }

    pub async fn release_namespace(&self, namespace_id: &NamespaceId, operation_id: &OperationId) {
        let mut locks = self.namespace_locks.lock().await;
        if locks.get(namespace_id) == Some(operation_id) {
            locks.remove(namespace_id);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NamespaceClaim {
    Acquired,
    AlreadyOwned,
    Busy { owner: OperationId },
}

/// How a submit command fails at the controller.
#[derive(Debug)]
pub enum SubmitCommandError {
    Clock {
        message: String,
    },
    NamespaceBusy {
        namespace_id: NamespaceId,
        owner: OperationId,
    },
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
