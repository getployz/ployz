use std::path::{Path, PathBuf};

use crate::control::operation_evidence::{
    OperationStatusStoreError, StoredMachineAddMintClaim, StoredMachineAddSecretDelivery,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::MachineJoinSecretDelivery;
use ployz_core::machine::{
    MachineAddFailure, MachineAddInterruptionEvidence, MachineAddInterruptionStage,
    MachineCredentialProvisioningStep,
};
use ployz_core::nats_config::{
    MintedNatsUser, NatsAuthorizationGrant, NatsInternalAuthority, NatsUserSeed,
};
use ployz_core::operation::{
    FailureMessage, MachineAddOperationState, OperationIdempotencyKey, OperationInterruptionCause,
    OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust};

use crate::control::sequencer::OperationControllers;

use super::writer::{NatsAuthorizationHandle, RenderFailure};
use crate::tasks::TaskSpawner;

const MINT_STARTUP_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
#[derive(Debug, thiserror::Error)]
pub enum MintResumeError {
    #[error("failed to list machine-add submissions: {0:?}")]
    ListSubmissions(OperationStatusStoreError),
    #[error("failed to read minted material record: {0:?}")]
    ReadSecretDelivery(OperationStatusStoreError),
    #[error("failed to read machine-add status: {0:?}")]
    ReadStatus(OperationStatusStoreError),
    #[error("failed to read machine-add provisioning stage: {0:?}")]
    ReadProvisioningStage(OperationStatusStoreError),
    #[error("failed to record interrupted machine-add mint {}: {message}", .operation_id.as_str())]
    RecordInterruption {
        operation_id: OperationId,
        message: String,
    },
    #[error("resumed machine-add mint {} did not record terminal evidence: {message}", .operation_id.as_str())]
    RecoveryRecordingFailed {
        operation_id: OperationId,
        message: String,
    },
}

/// Bounded operation work that mints per-machine credentials after a
/// machine-add submission is accepted.
#[derive(Clone)]
pub struct MachineCredentialMint {
    controllers: OperationControllers,
    authorization: NatsAuthorizationHandle,
    verify: MintVerifyEndpoint,
    machine_seed_file: PathBuf,
    tasks: TaskSpawner,
}

impl MachineCredentialMint {
    #[must_use]
    pub fn new(
        controllers: OperationControllers,
        authorization: NatsAuthorizationHandle,
        verify: MintVerifyEndpoint,
        machine_seed_file: PathBuf,
        tasks: TaskSpawner,
    ) -> Self {
        Self {
            controllers,
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

    pub async fn start(&self, request: MintRequest) {
        if self.admit(request.clone()).is_err() {
            let outcome = self
                .fail(
                    &request,
                    MachineAddFailure::ControlTaskInterrupted {
                        evidence: MachineAddInterruptionEvidence::new(
                            OperationInterruptionCause::CoreShutdown,
                            MachineAddInterruptionStage::Accepted,
                        ),
                    },
                )
                .await;
            if let MintOutcome::RecordingFailed { message } = outcome {
                eprintln!(
                    "machine-add operation {} task admission failed and terminal evidence could not be recorded: {message}",
                    request.operation_id.as_str()
                );
            }
        }
    }

    fn admit(&self, request: MintRequest) -> Result<(), crate::tasks::TaskAdmissionError> {
        let runtime = self.clone();
        self.tasks.spawn(|| async move {
            let _outcome = runtime.run(request).await;
        })
    }

    /// One bounded startup pass owned by control start: a control crash
    /// between machine-add acceptance and material-ready leaves the mint
    /// without a worker. Every accepted, still-pending machine-add that
    /// lacks a minted-material record gets its mint completed before startup
    /// proceeds; the per-key mint claim makes the resumed run converge on any
    /// partially minted material.
    pub async fn resume_unfinished_mints(&self) -> Result<Vec<MintRequest>, MintResumeError> {
        let submissions = self
            .controllers
            .repository()
            .machine_add_submissions()
            .await
            .map_err(MintResumeError::ListSubmissions)?;
        let mut resumed = Vec::new();
        for submission in submissions {
            let delivered = self
                .controllers
                .repository()
                .machine_add_secret_delivery(&submission.idempotency_key)
                .await
                .map_err(MintResumeError::ReadSecretDelivery)?;
            if delivered.is_some() {
                continue;
            }
            let Some(status) = self
                .controllers
                .repository()
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
                machine_id: submission.identity.machine_id,
                idempotency_key: submission.idempotency_key,
            };
            let recovery =
                tokio::time::timeout(MINT_STARTUP_RECOVERY_TIMEOUT, self.run(request.clone()))
                    .await;
            let outcome = match recovery {
                Ok(outcome) => outcome,
                Err(_) => {
                    let last_durable_stage = self
                        .last_durable_stage(&request.operation_id)
                        .await
                        .map_err(MintResumeError::ReadProvisioningStage)?;
                    self.fail(
                        &request,
                        MachineAddFailure::ControlTaskInterrupted {
                            evidence: MachineAddInterruptionEvidence::new(
                                OperationInterruptionCause::PriorCoreProcessLoss,
                                last_durable_stage,
                            ),
                        },
                    )
                    .await
                }
            };
            if let MintOutcome::RecordingFailed { message } = outcome {
                return Err(MintResumeError::RecoveryRecordingFailed {
                    operation_id: request.operation_id,
                    message,
                });
            }
            resumed.push(request);
        }
        Ok(resumed)
    }

    pub async fn fail_unfinished_mints(&self) -> Result<usize, MintResumeError> {
        let submissions = self
            .controllers
            .repository()
            .machine_add_submissions()
            .await
            .map_err(MintResumeError::ListSubmissions)?;
        let mut failed = 0;
        for submission in submissions {
            let delivered = self
                .controllers
                .repository()
                .machine_add_secret_delivery(&submission.idempotency_key)
                .await
                .map_err(MintResumeError::ReadSecretDelivery)?;
            if delivered.is_some() {
                continue;
            }
            let Some(status) = self
                .controllers
                .repository()
                .get(&submission.operation_id)
                .await
                .map_err(MintResumeError::ReadStatus)?
            else {
                continue;
            };
            let OperationStatus::MachineAdd {
                state: MachineAddOperationState::Pending { .. },
                ..
            } = status
            else {
                continue;
            };
            let last_durable_stage = self
                .last_durable_stage(&submission.operation_id)
                .await
                .map_err(MintResumeError::ReadProvisioningStage)?;
            self.controllers
                .repository()
                .record_machine_add_failed(
                    &submission.operation_id,
                    &submission.identity.machine_id,
                    MachineAddFailure::ControlTaskInterrupted {
                        evidence: MachineAddInterruptionEvidence::new(
                            OperationInterruptionCause::CoreShutdown,
                            last_durable_stage,
                        ),
                    },
                )
                .await
                .map_err(|error| MintResumeError::RecordInterruption {
                    operation_id: submission.operation_id.clone(),
                    message: error.to_string(),
                })?;
            failed += 1;
        }
        Ok(failed)
    }

    async fn last_durable_stage(
        &self,
        operation_id: &OperationId,
    ) -> Result<MachineAddInterruptionStage, OperationStatusStoreError> {
        Ok(self
            .controllers
            .repository()
            .last_machine_add_provisioning_step(operation_id)
            .await?
            .map_or(MachineAddInterruptionStage::Accepted, |step| {
                MachineAddInterruptionStage::CredentialProvisioning { step }
            }))
    }

    pub async fn run(&self, request: MintRequest) -> MintOutcome {
        match self
            .controllers
            .repository()
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
        if let Some(outcome) = self
            .record_step(&request, MachineCredentialProvisioningStep::Minted)
            .await
        {
            return outcome;
        }

        let verify_config = self
            .verify
            .machine_connect_config(&request.machine_id, claim.nkey_seed.clone());
        let user = NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Machine {
                machine_id: request.machine_id.clone(),
            },
            public_key: claim.nkey_public.clone(),
        };
        match self
            .authorization
            .authorize_and_render(user, Some(verify_config))
            .await
        {
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
        // non-terminal with a Host Runner retrying blind.
        Some(
            self.fail(
                request,
                MachineAddFailure::CredentialEvidenceWriteFailed {
                    message: failure_message(&format!(
                        "failed to record credential step {}: {error:?}",
                        ployz_nats::operation_event_subject::machine_credential_provisioning_step_token(step)
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
