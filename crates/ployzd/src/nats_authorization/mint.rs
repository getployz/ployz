use std::fmt;
use std::path::{Path, PathBuf};

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::MachineJoinSecretDelivery;
use ployz_core::machine::{
    MachineAddFailure, MachineAddOperationState, MachineCredentialProvisioningStep,
};
use ployz_core::nats_config::{MintedNatsUser, NatsAuthorizedUser, NatsUserSeed};
use ployz_core::ops::{FailureMessage, OperationIdempotencyKey, OperationStatus};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::operations::{
    OperationStatusReadError, OperationStatusStoreError, StoredMachineAddMintClaim,
    StoredMachineAddSecretDelivery,
};

use crate::controllers::OperationControllers;

use super::writer::{NatsAuthorizationHandle, RenderFailure};
use crate::tasks::TaskRegistry;

/// Where the mint worker test-connects with a freshly minted seed.
#[derive(Debug, Clone)]
pub struct MintVerifyEndpoint {
    pub url: NatsClientUrl,
    pub trust: NatsTlsTrust,
}

impl MintVerifyEndpoint {
    #[must_use]
    pub fn from_connect(config: &NatsConnectConfig) -> Self {
        Self {
            url: config.url.clone(),
            trust: config.trust.clone(),
        }
    }

    fn machine_connect_config(
        &self,
        machine_id: &MachineId,
        seed: NatsUserSeed,
    ) -> NatsConnectConfig {
        NatsConnectConfig {
            url: self.url.clone(),
            auth: NatsClientAuth::NkeySeed(seed),
            trust: self.trust.clone(),
            principal: NatsPrincipal::Machine {
                machine_id: machine_id.clone(),
            },
        }
    }
}

/// One machine-add mint job.
#[derive(Debug, Clone)]
pub struct MintRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub idempotency_key: OperationIdempotencyKey,
}

/// What one mint run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintOutcome {
    MaterialReady,
    AlreadyMinted,
    Failed(MachineAddFailure),
    RecordingFailed { message: String },
}

/// Why the control-start mint reconciliation pass could not finish.
#[derive(Debug)]
pub enum MintResumeError {
    ListSubmissions(OperationStatusStoreError),
    ReadSecretDelivery(OperationStatusStoreError),
    ReadStatus(OperationStatusReadError),
}

impl fmt::Display for MintResumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListSubmissions(error) => {
                write!(
                    formatter,
                    "failed to list machine-add submissions: {error:?}"
                )
            }
            Self::ReadSecretDelivery(error) => {
                write!(
                    formatter,
                    "failed to read minted material record: {error:?}"
                )
            }
            Self::ReadStatus(error) => {
                write!(formatter, "failed to read machine-add status: {error:?}")
            }
        }
    }
}

impl std::error::Error for MintResumeError {}

/// Bounded operation work that mints per-machine credentials after a
/// machine-add submission is accepted.
#[derive(Clone)]
pub struct MachineCredentialMintRuntime {
    controllers: OperationControllers,
    core_state: AsyncNatsCoreStateStore,
    authorization: NatsAuthorizationHandle,
    verify: MintVerifyEndpoint,
    machine_seed_file: PathBuf,
    tasks: TaskRegistry,
}

impl MachineCredentialMintRuntime {
    #[must_use]
    pub fn new(
        controllers: OperationControllers,
        core_state: AsyncNatsCoreStateStore,
        authorization: NatsAuthorizationHandle,
        verify: MintVerifyEndpoint,
        machine_seed_file: PathBuf,
        tasks: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            core_state,
            authorization,
            verify,
            machine_seed_file,
            tasks,
        }
    }

    #[must_use]
    pub fn machine_seed_file(&self) -> &Path {
        &self.machine_seed_file
    }

    pub fn start(&self, request: MintRequest) {
        let runtime = self.clone();
        self.tasks.spawn(async move {
            let _outcome = runtime.run(request).await;
        });
    }

    /// One bounded startup pass owned by control start: a control crash
    /// between machine-add acceptance and material-ready leaves the mint
    /// without a worker. Every accepted, still-pending machine-add that
    /// lacks a minted-material record gets its mint resumed as owned
    /// operation work; the per-key mint claim makes the resumed run
    /// converge on any partially minted material.
    pub async fn resume_unfinished_mints(&self) -> Result<Vec<MintRequest>, MintResumeError> {
        let submissions = self
            .controllers
            .repository()
            .records()
            .machine_add_submissions()
            .await
            .map_err(MintResumeError::ListSubmissions)?;
        let mut resumed = Vec::new();
        for submission in submissions {
            let delivered = self
                .controllers
                .repository()
                .records()
                .machine_add_secret_delivery(&submission.idempotency_key)
                .await
                .map_err(MintResumeError::ReadSecretDelivery)?;
            if delivered.is_some() {
                continue;
            }
            let Some(status) = self
                .controllers
                .repository()
                .records()
                .get(&submission.operation_id)
                .await
                .map_err(MintResumeError::ReadStatus)?
            else {
                continue;
            };
            let OperationStatus::MachineAdd { state, .. } = status else {
                continue;
            };
            match state {
                MachineAddOperationState::Pending { .. } => {}
                MachineAddOperationState::Joining { .. }
                | MachineAddOperationState::Completed
                | MachineAddOperationState::Failed { .. }
                | MachineAddOperationState::Cancelled { .. } => continue,
            }
            let request = MintRequest {
                operation_id: submission.operation_id,
                machine_id: submission.machine_id,
                idempotency_key: submission.idempotency_key,
            };
            self.start(request.clone());
            resumed.push(request);
        }
        Ok(resumed)
    }

    pub async fn run(&self, request: MintRequest) -> MintOutcome {
        match self
            .controllers
            .repository()
            .records()
            .machine_add_secret_delivery(&request.idempotency_key)
            .await
        {
            Ok(Some(_)) => return MintOutcome::AlreadyMinted,
            Ok(None) => {}
            Err(error) => {
                return MintOutcome::RecordingFailed {
                    message: format!("failed to read minted material record: {error:?}"),
                };
            }
        }

        // ADR-0015: a create-only claim on the idempotency key fences
        // duplicate mints without a cluster-wide lock. The first run claims
        // its freshly minted seed; concurrent or resumed runs adopt the
        // claimed material and converge on the same delivery record.
        let candidate = match mint_claim_candidate(&request.operation_id) {
            Ok(candidate) => candidate,
            Err(message) => return self.fail_unusable(&request, message).await,
        };
        let claim = match self
            .controllers
            .repository()
            .records()
            .put_machine_add_mint_claim_if_absent(&request.idempotency_key, &candidate)
            .await
        {
            Ok(claim) => claim,
            Err(error) => {
                return self
                    .fail_render(
                        &request,
                        format!("failed to claim minted material: {error:?}"),
                    )
                    .await;
            }
        };
        if let Err(error) = self
            .core_state
            .replace_nats_authorized_user(&NatsAuthorizedUser {
                principal: NatsPrincipal::Machine {
                    machine_id: request.machine_id.clone(),
                },
                nkey_public: claim.nkey_public.clone(),
            })
            .await
        {
            return self
                .fail_render(
                    &request,
                    format!("failed to store principal record: {error}"),
                )
                .await;
        }
        if let Some(outcome) = self
            .record_step(&request, MachineCredentialProvisioningStep::Minted)
            .await
        {
            return outcome;
        }

        let verify_config = self
            .verify
            .machine_connect_config(&request.machine_id, claim.nkey_seed.clone());
        match self.authorization.render(Some(verify_config)).await {
            Ok(_) => {}
            Err(RenderFailure::Prepare { failure }) => {
                return self.fail_render(&request, failure.to_string()).await;
            }
            Err(RenderFailure::Reload { failure }) => {
                if let Some(outcome) = self
                    .record_step(&request, MachineCredentialProvisioningStep::Rendered)
                    .await
                {
                    return outcome;
                }
                return self
                    .fail(
                        &request,
                        MachineAddFailure::NatsReloadFailed {
                            message: failure_message(&failure.to_string()),
                        },
                    )
                    .await;
            }
            Err(RenderFailure::Verify { message }) => {
                for step in [
                    MachineCredentialProvisioningStep::Rendered,
                    MachineCredentialProvisioningStep::Reloaded,
                ] {
                    if let Some(outcome) = self.record_step(&request, step).await {
                        return outcome;
                    }
                }
                return self.fail_unusable(&request, message).await;
            }
        }
        for step in [
            MachineCredentialProvisioningStep::Rendered,
            MachineCredentialProvisioningStep::Reloaded,
            MachineCredentialProvisioningStep::Verified,
        ] {
            if let Some(outcome) = self.record_step(&request, step).await {
                return outcome;
            }
        }

        if let Err(error) = self
            .controllers
            .repository()
            .records()
            .put_machine_add_secret_delivery_if_absent(
                &request.idempotency_key,
                &StoredMachineAddSecretDelivery {
                    operation_id: request.operation_id.clone(),
                    secret_delivery: MachineJoinSecretDelivery {
                        nats_credentials: claim.nkey_seed.clone(),
                    },
                },
            )
            .await
        {
            return self
                .fail_unusable(
                    &request,
                    format!("failed to store minted material: {error:?}"),
                )
                .await;
        }
        if let Some(outcome) = self
            .record_step(&request, MachineCredentialProvisioningStep::MaterialReady)
            .await
        {
            return outcome;
        }

        MintOutcome::MaterialReady
    }

    async fn record_step(
        &self,
        request: &MintRequest,
        step: MachineCredentialProvisioningStep,
    ) -> Option<MintOutcome> {
        let error = match self
            .controllers
            .repository()
            .record_machine_add_credential_provisioned(
                &request.operation_id,
                &request.machine_id,
                step,
            )
            .await
        {
            Ok(_) => return None,
            Err(error) => error,
        };
        // The step itself ran; only its evidence write failed. Attempt the
        // typed terminal failure so the operation is not stranded
        // non-terminal with a keeper retrying blind.
        Some(
            self.fail(
                request,
                MachineAddFailure::CredentialEvidenceWriteFailed {
                    message: failure_message(&format!(
                        "failed to record credential step {}: {error:?}",
                        step.as_subject_token()
                    )),
                },
            )
            .await,
        )
    }

    async fn fail_render(&self, request: &MintRequest, message: String) -> MintOutcome {
        self.fail(
            request,
            MachineAddFailure::AuthorizationRenderFailed {
                message: failure_message(&message),
            },
        )
        .await
    }

    async fn fail_unusable(&self, request: &MintRequest, message: String) -> MintOutcome {
        self.fail(
            request,
            MachineAddFailure::MintedCredentialUnusable {
                message: failure_message(&message),
            },
        )
        .await
    }

    async fn fail(&self, request: &MintRequest, failure: MachineAddFailure) -> MintOutcome {
        match self
            .controllers
            .repository()
            .record_machine_add_failed(&request.operation_id, &request.machine_id, failure.clone())
            .await
        {
            Ok(_) => MintOutcome::Failed(failure),
            Err(error) => MintOutcome::RecordingFailed {
                message: format!("failed to record mint failure: {error:?}"),
            },
        }
    }
}

fn mint_claim_candidate(operation_id: &OperationId) -> Result<StoredMachineAddMintClaim, String> {
    let minted =
        MintedNatsUser::generate().map_err(|error| format!("failed to mint NKey user: {error}"))?;
    Ok(StoredMachineAddMintClaim {
        operation_id: operation_id.clone(),
        nkey_public: minted.public,
        nkey_seed: minted.seed,
    })
}

fn failure_message(message: &str) -> FailureMessage {
    match FailureMessage::try_new(message) {
        Ok(message) => message,
        Err(_) => FailureMessage::try_new("credential minting failed without evidence")
            .expect("static failure message is non-empty"),
    }
}
