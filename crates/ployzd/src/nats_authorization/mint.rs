use std::path::{Path, PathBuf};
use std::sync::Arc;

use ployz_core::ids::{NodeId, OperationId};
use ployz_core::install::{MachineJoinNatsCredentials, MachineJoinSecretDelivery};
use ployz_core::machine::{MachineAddFailure, MachineCredentialProvisioningStep};
use ployz_core::nats_config::{NatsAuthorizedUser, NatsUserPublicKey, NatsUserSeed};
use ployz_core::ops::{FailureMessage, OperationIdempotencyKey};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::operations::StoredMachineAddSecretDelivery;

use crate::controllers::OperationControllers;

use super::tasks::MintTaskRegistry;
use super::writer::{NatsAuthorizationError, NatsAuthorizationHandle, RenderMode};

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

    fn node_connect_config(&self, node_id: &NodeId, seed: NatsUserSeed) -> NatsConnectConfig {
        NatsConnectConfig {
            url: self.url.clone(),
            auth: NatsClientAuth::NkeySeed(seed),
            trust: self.trust.clone(),
            principal: NatsPrincipal::Node {
                node_id: node_id.clone(),
            },
        }
    }
}

/// One machine-add mint job.
#[derive(Debug, Clone)]
pub struct MintRequest {
    pub operation_id: OperationId,
    pub node_id: NodeId,
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

/// Bounded operation work that mints per-machine credentials after a
/// machine-add submission is accepted.
#[derive(Clone)]
pub struct MachineCredentialMintRuntime {
    controllers: OperationControllers,
    core_state: AsyncNatsCoreStateStore,
    authorization: NatsAuthorizationHandle,
    verify: MintVerifyEndpoint,
    node_seed_file: PathBuf,
    tasks: MintTaskRegistry,
    mint_serial: Arc<tokio::sync::Mutex<()>>,
}

impl MachineCredentialMintRuntime {
    #[must_use]
    pub fn new(
        controllers: OperationControllers,
        core_state: AsyncNatsCoreStateStore,
        authorization: NatsAuthorizationHandle,
        verify: MintVerifyEndpoint,
        node_seed_file: PathBuf,
        tasks: MintTaskRegistry,
    ) -> Self {
        Self {
            controllers,
            core_state,
            authorization,
            verify,
            node_seed_file,
            tasks,
            mint_serial: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    #[must_use]
    pub fn node_seed_file(&self) -> &Path {
        &self.node_seed_file
    }

    pub fn start(&self, request: MintRequest) {
        let runtime = self.clone();
        self.tasks.spawn(async move {
            let _outcome = runtime.run(request).await;
        });
    }

    pub async fn run(&self, request: MintRequest) -> MintOutcome {
        let _serial = self.mint_serial.lock().await;

        match self
            .controllers
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

        let pair = nkeys::KeyPair::new_user();
        let minted = match minted_material(&pair) {
            Ok(minted) => minted,
            Err(message) => return self.fail_unusable(&request, message).await,
        };
        if let Err(error) = self
            .core_state
            .replace_nats_authorized_user(&NatsAuthorizedUser {
                principal: NatsPrincipal::Node {
                    node_id: request.node_id.clone(),
                },
                nkey_public: minted.public,
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
            .node_connect_config(&request.node_id, minted.seed.clone());
        match self
            .authorization
            .render(RenderMode::PreserveUsers, Some(verify_config))
            .await
        {
            Ok(_) => {}
            Err(
                error @ (NatsAuthorizationError::ReadAuthority { .. }
                | NatsAuthorizationError::ReadFile { .. }
                | NatsAuthorizationError::ParseFile { .. }
                | NatsAuthorizationError::RefusedShrink { .. }
                | NatsAuthorizationError::WriteFile { .. }
                | NatsAuthorizationError::WriterClosed),
            ) => {
                return self.fail_render(&request, error.to_string()).await;
            }
            Err(NatsAuthorizationError::Reload { evidence }) => {
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
                            message: failure_message(&format!(
                                "{} -> {}",
                                evidence.command, evidence.output
                            )),
                        },
                    )
                    .await;
            }
            Err(NatsAuthorizationError::VerifyConnect { message }) => {
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

        let credentials = match MachineJoinNatsCredentials::try_new(minted.seed.secret()) {
            Ok(credentials) => credentials,
            Err(error) => {
                return self
                    .fail_unusable(&request, format!("minted seed is not storable: {error}"))
                    .await;
            }
        };
        if let Err(error) = self
            .controllers
            .store_machine_add_secret_delivery(
                &request.idempotency_key,
                &StoredMachineAddSecretDelivery {
                    operation_id: request.operation_id.clone(),
                    secret_delivery: MachineJoinSecretDelivery {
                        nats_credentials: credentials,
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
        match self
            .controllers
            .record_machine_add_credential_step(&request.operation_id, &request.node_id, step)
            .await
        {
            Ok(_) => None,
            Err(error) => Some(MintOutcome::RecordingFailed {
                message: format!(
                    "failed to record credential step {}: {error:?}",
                    step.as_subject_token()
                ),
            }),
        }
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
            .record_machine_add_mint_failure(
                &request.operation_id,
                &request.node_id,
                failure.clone(),
            )
            .await
        {
            Ok(_) => MintOutcome::Failed(failure),
            Err(error) => MintOutcome::RecordingFailed {
                message: format!("failed to record mint failure: {error:?}"),
            },
        }
    }
}

struct MintedMaterial {
    public: NatsUserPublicKey,
    seed: NatsUserSeed,
}

fn minted_material(pair: &nkeys::KeyPair) -> Result<MintedMaterial, String> {
    let seed = pair
        .seed()
        .map_err(|error| format!("generated keypair has no seed: {error}"))?;
    let seed = NatsUserSeed::try_new(seed)
        .map_err(|error| format!("generated seed is invalid: {error}"))?;
    let public = NatsUserPublicKey::try_new(pair.public_key())
        .map_err(|error| format!("generated public key is invalid: {error}"))?;
    Ok(MintedMaterial { public, seed })
}

fn failure_message(message: &str) -> FailureMessage {
    match FailureMessage::try_new(message) {
        Ok(message) => message,
        Err(_) => FailureMessage::try_new("credential minting failed without evidence")
            .expect("static failure message is non-empty"),
    }
}
