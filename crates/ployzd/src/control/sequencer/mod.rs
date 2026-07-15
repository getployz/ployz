//! Operation admission, idempotency, resource fences, transitions, and evidence.

mod ingress;

use ingress::{IngressClaim, deploy_requires_ingress};
pub use ingress::{IngressConfigureSubmitCommand, IngressConfigureSubmitError};

use crate::control::operation_evidence::{
    AcceptedDeploySubmission, AcceptedMachineAddSubmission, CoreReplaceOperationSubmission,
    CredentialGrantOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
    MachineJoinIdentity, MachineJoinRedemption, MachineLifecycleOperationSubmission,
    MachineUpdateOperationSubmission, NamespaceRemoveOperationSubmission,
    NetworkRepairOperationSubmission, OperationRepository, OperationStatusStoreError,
    RedeemMachineJoinTokenError, ServiceRestartOperationSubmission, SubmitMachineAddError,
    SubmitOperationError, VolumeRemoveOperationSubmission,
};
use ployz_core::deploy::{
    DEFAULT_DEPLOY_RESERVATION_TTL_SECONDS, DeployRequest, DeployReservationExpiresAt,
    DeployReservationId, RegistryCredential, VolumeName,
};
use ployz_core::ids::{NamespaceId, OperationId, ServiceId};
use ployz_core::install::{
    HostPortAssurance, InstallArtifactVersion, MachineBootstrapUrl, MachineJoinBundle,
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate,
};
use ployz_core::intent::recovery::PendingMachineJoinRecovery;
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenRedeemedAt, MachineName, RawJoinToken,
    redeem_pending_join_token,
};
use ployz_core::operation::CredentialGrantAction;
use ployz_core::operation::{OperationStatus, OperationStatusSnapshot};
use ployz_core::roles::InstallRolePolicy;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

pub use ployz_core::operation::OperationIdempotencyKey as IdempotencyKey;

const MACHINE_JOIN_TOKEN_TTL_SECONDS: u64 = 600;

type DeployReservations =
    BTreeMap<NamespaceId, BTreeMap<DeployReservationId, DeployReservationExpiresAt>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub reservation_id: DeployReservationId,
    pub target: DeployRequest,
    pub registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeployExecution {
    pub submission: AcceptedDeploySubmission,
    pub registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
    pub ingress_fence_owner: Option<OperationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssuedDeployReservation {
    pub reservation_id: DeployReservationId,
    pub expires_at: DeployReservationExpiresAt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddSubmitCommand {
    pub operation_id: OperationId,
    pub idempotency_key: IdempotencyKey,
    pub machine_id: ployz_core::ids::MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub host_port_assurance: HostPortAssurance,
    pub endpoint_subnet: MachineAddEndpointSubnet,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineAddEndpointSubnet {
    Allocate,
    Preserve(ployz_core::network::MachineEndpointSubnet),
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
    pub target: ployz_core::machine::MachineLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreReplaceSubmitCommand {
    pub operation_id: OperationId,
    pub machine_id: ployz_core::ids::MachineId,
    pub successor_nats_url: MachineJoinRuntimeNatsUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRestartSubmitCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveSubmitCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRepairSubmitCommand {
    pub operation_id: OperationId,
    pub target_machine_id: Option<ployz_core::ids::MachineId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRemoveSubmitCommand {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialGrantSubmitCommand {
    pub operation_id: OperationId,
    pub action: CredentialGrantAction,
}

/// Bootstrap material available at submit time.
///
/// The per-machine secret is still minted afterwards as bounded operation
/// work. `join_secret_delivery` is the low-privilege Join credential used by
/// the target Host Runner to redeem and report this one join token.
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
    deploy_reservations: Arc<Mutex<DeployReservations>>,
    mesh_lock: Arc<Mutex<()>>,
    ingress_lock: Arc<Mutex<Option<OperationId>>>,
    machine_bootstrap: MachineAddBootstrapConfig,
    pending_join_recovery: Arc<Mutex<Vec<PendingMachineJoinRecovery>>>,
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
            deploy_reservations: Arc::default(),
            mesh_lock: Arc::default(),
            ingress_lock: Arc::default(),
            machine_bootstrap,
            pending_join_recovery: Arc::default(),
        }
    }

    #[must_use]
    pub fn new_with_pending_join_recovery(
        repository: OperationRepository,
        machine_bootstrap: MachineAddBootstrapConfig,
        pending_join_recovery: Vec<PendingMachineJoinRecovery>,
    ) -> Self {
        Self {
            repository,
            namespace_locks: Arc::default(),
            deploy_reservations: Arc::default(),
            mesh_lock: Arc::default(),
            ingress_lock: Arc::default(),
            machine_bootstrap,
            pending_join_recovery: Arc::new(Mutex::new(pending_join_recovery)),
        }
    }

    #[must_use]
    pub(crate) fn mesh_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.mesh_lock)
    }

    pub async fn submit_deploy(
        &self,
        command: DeploySubmitCommand,
    ) -> Result<AcceptedDeployExecution, SubmitCommandError> {
        let ingress_fence_owner =
            deploy_requires_ingress(&command.target).then(|| command.operation_id.clone());
        let ingress_claim = if let Some(owner) = &ingress_fence_owner {
            Some(self.claim_ingress(owner).await)
        } else {
            None
        };
        let acquired_ingress = matches!(ingress_claim, Some(IngressClaim::Acquired));
        if let Some(IngressClaim::Busy { owner }) = &ingress_claim {
            return Err(SubmitCommandError::IngressBusy {
                namespace_id: command.target.namespace_id.clone(),
                owner: owner.clone(),
            });
        }
        let operation_id = command.operation_id.clone();
        let result = self.submit_deploy_inner(command, ingress_fence_owner).await;
        if acquired_ingress {
            match &result {
                Err(_) => self.release_ingress(&operation_id).await,
                Ok(accepted) if !accepted.submission.should_start_execution => {
                    let Some(owner) = &accepted.ingress_fence_owner else {
                        unreachable!("automatic deploy owns the ingress fence")
                    };
                    self.release_ingress(owner).await;
                }
                Ok(_) => {}
            }
        }
        result
    }

    async fn submit_deploy_inner(
        &self,
        command: DeploySubmitCommand,
        ingress_fence_owner: Option<OperationId>,
    ) -> Result<AcceptedDeployExecution, SubmitCommandError> {
        let operation_id = command.operation_id;
        let idempotency_key = command.idempotency_key;
        let reservation_id = command.reservation_id;
        let target = command.target;
        let registry_credentials = command.registry_credentials;
        let claimed = self
            .repository
            .claim_deploy(DeployOperationSubmission {
                operation_id,
                idempotency_key,
                reservation_id,
                target,
            })
            .await?;
        let operation_id = claimed.operation_id.clone();
        if self
            .repository
            .get(&operation_id)
            .await
            .map_err(SubmitOperationError::StoreStatus)?
            .is_none()
        {
            self.validate_deploy_reservation(&claimed.target.namespace_id, claimed.reservation_id)
                .await?;
        }
        let target = claimed.target.clone();
        let namespace_id = target.namespace_id.clone();
        let claim = self.claim_namespace(&namespace_id, &operation_id).await;
        if let NamespaceClaim::Busy { owner } = claim {
            self.repository
                .check_deploy_reservation_fence(
                    &namespace_id,
                    claimed.reservation_id,
                    &operation_id,
                )
                .await?;
            return Err(SubmitCommandError::NamespaceBusy {
                namespace_id,
                owner,
            });
        }

        let submitted = self.repository.submit_claimed_deploy(claimed).await;

        match submitted {
            Ok(accepted) => {
                if !accepted.should_start_execution && matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &operation_id).await;
                }
                Ok(AcceptedDeployExecution {
                    submission: accepted,
                    registry_credentials,
                    ingress_fence_owner,
                })
            }
            Err(error) => {
                if matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &operation_id).await;
                }
                Err(SubmitCommandError::Submit(error))
            }
        }
    }

    pub async fn submit_credential_grant(
        &self,
        command: CredentialGrantSubmitCommand,
    ) -> Result<
        crate::control::operation_evidence::AcceptedCredentialGrantSubmission,
        SubmitCommandError,
    > {
        self.repository
            .submit_credential_grant(CredentialGrantOperationSubmission {
                operation_id: command.operation_id,
                action: command.action,
            })
            .await
            .map_err(SubmitCommandError::Submit)
    }

    pub async fn reserve_deploy(
        &self,
        namespace_id: &NamespaceId,
    ) -> Result<IssuedDeployReservation, DeployReservationIssueError> {
        let now = current_unix_seconds().map_err(DeployReservationIssueError::Clock)?;
        let reservation_id = self
            .repository
            .issue_deploy_reservation(namespace_id)
            .await
            .map_err(DeployReservationIssueError::Store)?;
        let expires_at = now
            .checked_add(DEFAULT_DEPLOY_RESERVATION_TTL_SECONDS)
            .and_then(|value| DeployReservationExpiresAt::try_new(value).ok())
            .ok_or_else(|| DeployReservationIssueError::Clock("timestamp overflow".to_owned()))?;
        let mut reservations = self.deploy_reservations.lock().await;
        let namespace_reservations = reservations.entry(namespace_id.clone()).or_default();
        namespace_reservations.retain(|_, expiry| expiry.unix_seconds() > now);
        namespace_reservations.insert(reservation_id, expires_at);
        Ok(IssuedDeployReservation {
            reservation_id,
            expires_at,
        })
    }

    async fn validate_deploy_reservation(
        &self,
        namespace_id: &NamespaceId,
        reservation_id: DeployReservationId,
    ) -> Result<(), SubmitCommandError> {
        let now =
            current_unix_seconds().map_err(|message| SubmitCommandError::Clock { message })?;
        let mut reservations = self.deploy_reservations.lock().await;
        validate_deploy_reservation_at(&mut reservations, namespace_id, reservation_id, now)
    }

    pub async fn submit_machine_add(
        &self,
        command: MachineAddSubmitCommand,
        endpoint_supernet: &ployz_core::network::MachineEndpointSupernet,
        machine_roster: &crate::control::intent::machine_roster::MachineRosterStore,
    ) -> Result<AcceptedMachineAddSubmission, MachineAddSubmitCommandError> {
        let _mesh = self.mesh_lock.lock().await;
        if let Some(existing) = self
            .repository
            .machine_add_submission(&command.idempotency_key)
            .await
            .map_err(MachineAddSubmitCommandError::Store)?
        {
            return Ok(self
                .repository
                .submit_machine_add(MachineAddOperationSubmission {
                    operation_id: command.operation_id,
                    identity: MachineJoinIdentity {
                        machine_id: command.machine_id,
                        name: command.name,
                        roles: command.roles,
                        host_port_assurance: existing.identity.host_port_assurance,
                        endpoint_subnet: existing.identity.endpoint_subnet,
                        join_bundle: command.join_bundle,
                        join_token: command.join_token,
                        raw_join_token: command.raw_join_token,
                    },
                    idempotency_key: command.idempotency_key,
                })
                .await?);
        }
        let endpoint_subnet = match command.endpoint_subnet {
            MachineAddEndpointSubnet::Preserve(subnet) => subnet,
            MachineAddEndpointSubnet::Allocate => {
                let active = machine_roster.active_machines().await.map_err(|error| {
                    MachineAddSubmitCommandError::Admission {
                        message: error.to_string(),
                    }
                })?;
                let mut assigned = active
                    .into_iter()
                    .map(|machine| machine.endpoint_subnet)
                    .collect::<Vec<_>>();
                assigned.extend(
                    self.repository
                        .live_machine_add_endpoint_subnets()
                        .await
                        .map_err(MachineAddSubmitCommandError::Store)?,
                );
                let derived = ployz_core::network::MachineEndpointSubnet::try_new(
                    ployz_core::network::default_endpoint_subnet(&command.machine_id),
                )
                .map_err(|error| MachineAddSubmitCommandError::Admission {
                    message: error.to_string(),
                })?;
                if assigned.contains(&derived) {
                    endpoint_supernet.allocate_next(assigned).map_err(|error| {
                        MachineAddSubmitCommandError::Admission {
                            message: error.to_string(),
                        }
                    })?
                } else {
                    derived
                }
            }
        };
        Ok(self
            .repository
            .submit_machine_add(MachineAddOperationSubmission {
                operation_id: command.operation_id,
                identity: MachineJoinIdentity {
                    machine_id: command.machine_id,
                    name: command.name,
                    roles: command.roles,
                    host_port_assurance: command.host_port_assurance,
                    endpoint_subnet,
                    join_bundle: command.join_bundle,
                    join_token: command.join_token,
                    raw_join_token: command.raw_join_token,
                },
                idempotency_key: command.idempotency_key,
            })
            .await?)
    }

    pub async fn submit_machine_update(
        &self,
        command: MachineUpdateSubmitCommand,
    ) -> Result<
        crate::control::operation_evidence::AcceptedMachineUpdateSubmission,
        SubmitCommandError,
    > {
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
    ) -> Result<
        crate::control::operation_evidence::AcceptedMachineLifecycleSubmission,
        SubmitCommandError,
    > {
        Ok(self
            .repository
            .submit_machine_lifecycle(MachineLifecycleOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
                target: command.target,
            })
            .await?)
    }

    pub async fn submit_core_replace(
        &self,
        command: CoreReplaceSubmitCommand,
    ) -> Result<crate::control::operation_evidence::AcceptedCoreReplaceSubmission, SubmitCommandError>
    {
        Ok(self
            .repository
            .submit_core_replace(CoreReplaceOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
                successor_nats_url: command.successor_nats_url,
            })
            .await?)
    }

    pub async fn submit_network_repair(
        &self,
        command: NetworkRepairSubmitCommand,
    ) -> Result<
        crate::control::operation_evidence::AcceptedNetworkRepairSubmission,
        SubmitCommandError,
    > {
        Ok(self
            .repository
            .submit_network_repair(NetworkRepairOperationSubmission {
                operation_id: command.operation_id,
                target_machine_id: command.target_machine_id,
            })
            .await?)
    }

    pub async fn submit_service_restart(
        &self,
        command: ServiceRestartSubmitCommand,
    ) -> Result<
        crate::control::operation_evidence::AcceptedServiceRestartSubmission,
        SubmitCommandError,
    > {
        let namespace_id = command.namespace_id.clone();
        let operation_id = command.operation_id.clone();
        let claim = self.claim_namespace(&namespace_id, &operation_id).await;
        if let NamespaceClaim::Busy { owner } = claim {
            return Err(SubmitCommandError::NamespaceBusy {
                namespace_id,
                owner,
            });
        }

        let submitted = self
            .repository
            .submit_service_restart(ServiceRestartOperationSubmission {
                operation_id,
                namespace_id: command.namespace_id,
                service_id: command.service_id,
            })
            .await;
        match submitted {
            Ok(accepted) => {
                if !accepted.should_start_execution && matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&accepted.namespace_id, &accepted.operation_id)
                        .await;
                }
                Ok(accepted)
            }
            Err(error) => {
                if matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &command.operation_id)
                        .await;
                }
                Err(SubmitCommandError::Submit(error))
            }
        }
    }

    pub async fn submit_namespace_remove(
        &self,
        command: NamespaceRemoveSubmitCommand,
    ) -> Result<
        crate::control::operation_evidence::AcceptedNamespaceRemoveSubmission,
        SubmitCommandError,
    > {
        let namespace_id = command.namespace_id.clone();
        let operation_id = command.operation_id.clone();
        let claim = self.claim_namespace(&namespace_id, &operation_id).await;
        if let NamespaceClaim::Busy { owner } = claim {
            return Err(SubmitCommandError::NamespaceBusy {
                namespace_id,
                owner,
            });
        }

        let submitted = self
            .repository
            .submit_namespace_remove(NamespaceRemoveOperationSubmission {
                operation_id,
                namespace_id: command.namespace_id,
            })
            .await;
        match submitted {
            Ok(accepted) => {
                if !accepted.should_start_execution && matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&accepted.namespace_id, &accepted.operation_id)
                        .await;
                }
                Ok(accepted)
            }
            Err(error) => {
                if matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &command.operation_id)
                        .await;
                }
                Err(SubmitCommandError::Submit(error))
            }
        }
    }

    pub async fn submit_volume_remove(
        &self,
        command: VolumeRemoveSubmitCommand,
    ) -> Result<
        crate::control::operation_evidence::AcceptedVolumeRemoveSubmission,
        SubmitCommandError,
    > {
        let namespace_id = command.namespace_id.clone();
        let operation_id = command.operation_id.clone();
        let claim = self.claim_namespace(&namespace_id, &operation_id).await;
        if let NamespaceClaim::Busy { owner } = claim {
            return Err(SubmitCommandError::NamespaceBusy {
                namespace_id,
                owner,
            });
        }

        let submitted = self
            .repository
            .submit_volume_remove(VolumeRemoveOperationSubmission {
                operation_id,
                namespace_id: command.namespace_id,
                volume_name: command.volume_name,
            })
            .await;
        match submitted {
            Ok(accepted) => {
                if !accepted.should_start_execution && matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&accepted.namespace_id, &accepted.operation_id)
                        .await;
                }
                Ok(accepted)
            }
            Err(error) => {
                if matches!(claim, NamespaceClaim::Acquired) {
                    self.release_namespace(&namespace_id, &command.operation_id)
                        .await;
                }
                Err(SubmitCommandError::Submit(error))
            }
        }
    }

    pub async fn redeem_machine_join_token(
        &self,
        token: &RawJoinToken,
    ) -> Result<MachineJoinRedemption, RedeemMachineJoinTokenError> {
        self.repository
            .redeem_machine_join_token(token, current_join_time()?)
            .await
    }

    pub async fn recover_machine_join_submission(
        &self,
        token: &RawJoinToken,
    ) -> Result<Option<MachineAddSubmitCommand>, RedeemMachineJoinTokenError> {
        let fingerprint = token
            .fingerprint()
            .map_err(|_| RedeemMachineJoinTokenError::InvalidJoinToken)?;
        let now = current_join_time()?;
        let pending = self.pending_join_recovery.lock().await;
        for recovery in pending.iter() {
            if redeem_pending_join_token(&recovery.join_token, &fingerprint, now).is_ok() {
                let Some(join_material) = self.machine_bootstrap.join_material.as_ref() else {
                    return Ok(None);
                };
                let suffix = fingerprint.as_str().chars().take(16).collect::<String>();
                let operation_id = OperationId::try_new(format!(
                    "op_recover_join_{}_{}",
                    recovery.machine_id.as_str(),
                    suffix
                ))
                .map_err(|_| RedeemMachineJoinTokenError::InvalidJoinToken)?;
                let idempotency_key = IdempotencyKey::try_new(format!(
                    "idem_recover_join_{}_{}",
                    recovery.machine_id.as_str(),
                    suffix
                ))
                .map_err(|_| RedeemMachineJoinTokenError::InvalidJoinToken)?;
                return Ok(Some(MachineAddSubmitCommand {
                    operation_id,
                    idempotency_key,
                    machine_id: recovery.machine_id.clone(),
                    name: recovery.name.clone(),
                    roles: recovery.roles,
                    host_port_assurance: recovery.host_port_assurance,
                    endpoint_subnet: MachineAddEndpointSubnet::Preserve(
                        recovery.endpoint_subnet.clone(),
                    ),
                    join_bundle: join_material.join_template.join_bundle.clone(),
                    join_token: recovery.join_token.clone(),
                    raw_join_token: token.clone(),
                }));
            }
        }
        Ok(None)
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

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusStoreError> {
        self.repository
            .operation_status_snapshot(operation_id)
            .await
    }

    pub async fn operation_statuses_newest_first(
        &self,
        active_only: bool,
    ) -> Result<Vec<OperationStatus>, OperationStatusStoreError> {
        self.repository
            .operation_statuses_newest_first(active_only)
            .await
    }

    pub async fn operation_statuses_before(
        &self,
        before: &OperationId,
        active_only: bool,
    ) -> Result<Option<Vec<OperationStatus>>, OperationStatusStoreError> {
        self.repository
            .operation_statuses_before(before, active_only)
            .await
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
    IngressBusy {
        namespace_id: NamespaceId,
        owner: OperationId,
    },
    ReservationNotFound {
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
    },
    ReservationExpired {
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        expired_at: DeployReservationExpiresAt,
    },
    Submit(SubmitOperationError),
}

#[derive(Debug, thiserror::Error)]
pub enum DeployReservationIssueError {
    #[error("clock: {0}")]
    Clock(String),
    #[error("{0}")]
    Store(OperationStatusStoreError),
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
    Store(OperationStatusStoreError),
    Admission { message: String },
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

fn validate_deploy_reservation_at(
    reservations: &mut DeployReservations,
    namespace_id: &NamespaceId,
    reservation_id: DeployReservationId,
    now: u64,
) -> Result<(), SubmitCommandError> {
    let Some(namespace_reservations) = reservations.get_mut(namespace_id) else {
        return Err(SubmitCommandError::ReservationNotFound {
            namespace_id: namespace_id.clone(),
            reservation_id,
        });
    };
    let Some(expires_at) = namespace_reservations.get(&reservation_id).copied() else {
        return Err(SubmitCommandError::ReservationNotFound {
            namespace_id: namespace_id.clone(),
            reservation_id,
        });
    };
    if expires_at.unix_seconds() <= now {
        namespace_reservations.remove(&reservation_id);
        return Err(SubmitCommandError::ReservationExpired {
            namespace_id: namespace_id.clone(),
            reservation_id,
            expired_at: expires_at,
        });
    }
    Ok(())
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

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
