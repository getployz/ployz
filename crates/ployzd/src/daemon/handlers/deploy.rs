mod manifest_render;
mod node;
mod responses;
mod volume_transfer;

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::{ActiveMesh, DaemonState};
use manifest_render::{
    encode_branch_manifest_json, encode_migrate_manifest_json, export_manifest,
    render_branch_namespace_manifest, render_migrate_service_manifest,
    validate_branch_namespace_request, validate_migrate_service_request,
};
use node::NatsDeployParticipantClient;
use ployz_api::{
    BranchApplyPreparedRequest, BranchEnvironmentListPayload, BranchEnvironmentPayload,
    BranchEnvironmentStatusRequest, BranchNamespaceMode, BranchNamespaceRequest,
    BranchResourceMode, BranchResourceModeOverride, DaemonPayload, DaemonResponse,
    DeployApplyPreparedRequest, DeployOptions, DeployPreparePayload, MigrateServiceMode,
    MigrateServiceRequest,
};
use ployz_cert_backends::InstantAcmeIssuerFactory;
use ployz_config::RuntimeTarget;
use ployz_nats::RpcPolicy;
use ployz_nats::{NatsDeployLock, NatsLocks, NatsStore};
use ployz_orchestrator::certificates::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_orchestrator::coordination::ReservationId;
use ployz_orchestrator::deploy::{
    DeployApplyPreconditions, apply_prepared_with_certificate_coordination,
    apply_with_deploy_id_and_preconditions, new_deploy_id, prepare, preview,
    validated_prepared_manifest,
};
use ployz_store_api::{DeployStore, StoreDriver, StoreRuntimeControl};
use ployz_types::Error as PloyzError;
use ployz_types::error::DeployError;
use ployz_types::model::{
    BranchEnvironmentFailure, BranchEnvironmentRecord, BranchEnvironmentResourceMode,
    BranchEnvironmentResourceOverride, BranchEnvironmentState, DeployId, DeployPhaseCommitPolicy,
    DeployPhaseFailure, DeployPhaseState, DeployState, PreparedDeployRecord, PreparedDeployState,
};
use ployz_types::spec::{DeployManifest, Namespace, valid_storage_segment};

#[cfg(test)]
use manifest_render::{BranchRenderError, MigrateRenderError, stable_fingerprint};
use responses::{deploy_error_code, deploy_failure_payload_for_error};
#[cfg(test)]
use volume_transfer::{DeployMoveRpcClient, run_volume_move_rpc, volume_move_result_from_transfer};

const DEPLOY_LOCK_TTL: Duration = Duration::from_secs(30 * 60);
const DEPLOY_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(10 * 60);
const DEPLOY_PARTICIPANT_RPC_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEPLOY_VOLUME_MOVE_START_RPC_TIMEOUT: Duration = Duration::from_secs(60);
const DEPLOY_VOLUME_MOVE_POLL_RPC_TIMEOUT: Duration = Duration::from_secs(60);
const DEPLOY_VOLUME_MOVE_RPC_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const DEPLOY_VOLUME_MOVE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEPLOY_VOLUME_CLONE_RPC_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const DEPLOY_VOLUME_CLONE_CLEANUP_RPC_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEPLOY_PREPARE_TTL_SECS: u64 = 24 * 60 * 60;
struct DeployApplyRuntime {
    nats_store: NatsStore,
    nats_locks: NatsLocks,
    deploy_lock: NatsDeployLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchApplyPreparedLifecycle {
    BestEffort,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchApplyPreparedRecording {
    None,
    Claimed,
}

enum BranchApplyingReplayAction {
    Replayed(BranchEnvironmentRecord),
    ResumePreparedApply,
    Busy,
}

type DeployApplyFuture<'a> = Pin<
    Box<
        dyn Future<Output = ployz_types::Result<ployz_types::model::DeployApplyResult>> + Send + 'a,
    >,
>;

impl DaemonState {
    fn overlay_network_name(&self) -> Option<String> {
        self.active
            .as_ref()
            .map(|active| format!("ployz-{}", active.config.name.0))
    }

    fn overlay_dns_server(&self) -> Option<std::net::Ipv4Addr> {
        if self.runtime_target != RuntimeTarget::Docker {
            return None;
        }
        self.active
            .as_ref()
            .and_then(|active| active.mesh.container_dns_server())
    }

    pub async fn handle_deploy_preview(
        &self,
        manifest_json: &str,
        _options: &DeployOptions,
    ) -> DaemonResponse {
        let manifest = match decode_manifest(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return *response,
        };
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let nats_store =
            match ployz_nats::NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
                Ok(store) => store.with_asset_policy(active.config.storage_replicas),
                Err(error) => return self.err("DEPLOY_PREVIEW_FAILED", error.to_string()),
            };
        if let Err(error) = nats_store.start().await {
            return self.err("DEPLOY_PREVIEW_FAILED", error.to_string());
        }
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&nats_store),
        );

        match preview(
            &active.mesh.store,
            &self.identity.machine_id,
            &manifest,
            &prober,
        )
        .await
        {
            Ok(plan) => self.ok_json_pretty(&plan, "ENCODE_PREVIEW", "encode preview"),
            Err(err) => self.deploy_error_response("DEPLOY_PREVIEW_FAILED", err),
        }
    }

    pub async fn handle_deploy_prepare(&self, manifest_json: &str) -> DaemonResponse {
        let manifest = match decode_manifest(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return *response,
        };
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let nats_store =
            match ployz_nats::NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
                Ok(store) => store.with_asset_policy(active.config.storage_replicas),
                Err(error) => return self.err("DEPLOY_PREPARE_FAILED", error.to_string()),
            };
        if let Err(error) = nats_store.start().await {
            return self.err("DEPLOY_PREPARE_FAILED", error.to_string());
        }
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&nats_store),
        );

        match prepare(
            &active.mesh.store,
            &self.identity.machine_id,
            &manifest,
            &prober,
            new_deploy_id(),
            DEPLOY_PREPARE_TTL_SECS,
        )
        .await
        {
            Ok(prepared) => self.ok_with_payload(
                "prepared deploy",
                Some(DaemonPayload::DeployPrepare(DeployPreparePayload {
                    prepared,
                })),
            ),
            Err(err) => self.deploy_error_response("DEPLOY_PREPARE_FAILED", err),
        }
    }

    pub async fn handle_deploy_apply(
        &self,
        manifest_json: &str,
        options: &DeployOptions,
    ) -> DaemonResponse {
        let preconditions = match deploy_apply_preconditions(options) {
            Ok(preconditions) => preconditions,
            Err(message) => return self.err("INVALID_DEPLOY_OPTIONS", message),
        };
        let manifest = match decode_manifest(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return *response,
        };
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let runtime = match self
            .prepare_deploy_apply_runtime(active, &manifest.namespace, "DEPLOY_APPLY_FAILED")
            .await
        {
            Ok(runtime) => runtime,
            Err(response) => return response,
        };
        self.apply_manifest_with_runtime(active, &manifest, runtime, preconditions)
            .await
    }

    pub async fn handle_deploy_apply_prepared(
        &self,
        request: &DeployApplyPreparedRequest,
    ) -> DaemonResponse {
        self.handle_deploy_apply_prepared_with_branch_lifecycle(
            request,
            BranchApplyPreparedLifecycle::BestEffort,
        )
        .await
    }

    async fn handle_deploy_apply_prepared_with_branch_lifecycle(
        &self,
        request: &DeployApplyPreparedRequest,
        lifecycle: BranchApplyPreparedLifecycle,
    ) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let prepared = match active
            .mesh
            .store
            .get_prepared_deploy(&request.prepared_deploy_id)
            .await
        {
            Ok(Some(prepared)) => prepared,
            Ok(None) => {
                return self.deploy_error_response(
                    "DEPLOY_APPLY_PREPARED_FAILED",
                    PloyzError::Deploy(DeployError::PreparedDeployMissing {
                        prepared_deploy_id: request.prepared_deploy_id.as_str().to_string(),
                    }),
                );
            }
            Err(error) => return self.deploy_error_response("DEPLOY_APPLY_PREPARED_FAILED", error),
        };
        let loaded_branch_environment = match active
            .mesh
            .store
            .get_branch_environment(&prepared.namespace)
            .await
        {
            Ok(environment) => environment,
            Err(error) => return self.deploy_error_response("DEPLOY_APPLY_PREPARED_FAILED", error),
        };

        let branch_environment = match (lifecycle, loaded_branch_environment) {
            (BranchApplyPreparedLifecycle::Required, None) => {
                return self.err(
                    "BRANCH_ENVIRONMENT_NOT_FOUND",
                    format!(
                        "prepared deploy '{}' is not current for any branch environment",
                        request.prepared_deploy_id
                    ),
                );
            }
            (BranchApplyPreparedLifecycle::Required, Some(environment))
                if environment.prepared_deploy_id.as_ref() != Some(&request.prepared_deploy_id) =>
            {
                return self.err(
                    "BRANCH_PREPARED_DEPLOY_STALE",
                    format!(
                        "prepared deploy '{}' is not current for branch environment '{}'",
                        request.prepared_deploy_id, environment.target_namespace
                    ),
                );
            }
            (BranchApplyPreparedLifecycle::Required, Some(environment)) => Some(environment),
            (BranchApplyPreparedLifecycle::BestEffort, Some(environment))
                if environment.prepared_deploy_id.as_ref() == Some(&request.prepared_deploy_id) =>
            {
                Some(environment)
            }
            (BranchApplyPreparedLifecycle::BestEffort, Some(_) | None) => None,
        };

        if lifecycle == BranchApplyPreparedLifecycle::Required
            && let Some(environment) = branch_environment.as_ref()
            && active_branch_prepared_replay(environment, &request.prepared_deploy_id)
        {
            return self.branch_environment_replay_response(environment.clone());
        }

        let branch_recording = match branch_environment.as_ref() {
            Some(environment) if environment.state == BranchEnvironmentState::Active => {
                if lifecycle == BranchApplyPreparedLifecycle::Required {
                    return self.err(
                        "BRANCH_ENVIRONMENT_ALREADY_ACTIVE",
                        format!(
                            "branch environment '{}' is already active",
                            environment.target_namespace
                        ),
                    );
                }
                BranchApplyPreparedRecording::None
            }
            Some(environment) if environment.state == BranchEnvironmentState::Applying => {
                match branch_applying_replay_action(
                    &active.mesh.store,
                    &prepared,
                    &request.prepared_deploy_id,
                )
                .await
                {
                    Ok(BranchApplyingReplayAction::Replayed(environment)) => {
                        return self.branch_environment_replay_response(environment);
                    }
                    Ok(BranchApplyingReplayAction::ResumePreparedApply) => {
                        BranchApplyPreparedRecording::Claimed
                    }
                    Ok(BranchApplyingReplayAction::Busy) => {
                        return self.err(
                            "BRANCH_ENVIRONMENT_BUSY",
                            format!(
                                "branch environment '{}' is already applying prepared deploy '{}'",
                                environment.target_namespace, request.prepared_deploy_id
                            ),
                        );
                    }
                    Err(error) => {
                        return self.err(
                            "BRANCH_ENVIRONMENT_RECORD_FAILED",
                            format!("failed to repair applying branch environment: {error}"),
                        );
                    }
                }
            }
            Some(_) => match active
                .mesh
                .store
                .mark_branch_environment_applying(
                    &prepared.namespace,
                    &request.prepared_deploy_id,
                    ployz_types::time::now_unix_secs(),
                )
                .await
            {
                Ok(_) => BranchApplyPreparedRecording::Claimed,
                Err(error) if lifecycle == BranchApplyPreparedLifecycle::BestEffort => {
                    if let Some(environment) = active_branch_prepared_replay_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    {
                        return self.branch_environment_replay_response(environment);
                    }
                    if branch_environment_applying_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                    {
                        return self.err(
                            "BRANCH_ENVIRONMENT_BUSY",
                            format!(
                                "branch environment '{}' is already applying prepared deploy '{}'",
                                prepared.namespace, request.prepared_deploy_id
                            ),
                        );
                    }
                    tracing::warn!(
                        %error,
                        target_namespace = %prepared.namespace,
                        prepared_deploy_id = %request.prepared_deploy_id,
                        "failed to claim branch apply-prepared lifecycle"
                    );
                    BranchApplyPreparedRecording::None
                }
                Err(error) => {
                    if let Some(environment) = active_branch_prepared_replay_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    {
                        return self.branch_environment_replay_response(environment);
                    }
                    if branch_environment_applying_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                    {
                        return self.err(
                            "BRANCH_ENVIRONMENT_BUSY",
                            format!(
                                "branch environment '{}' is already applying prepared deploy '{}'",
                                prepared.namespace, request.prepared_deploy_id
                            ),
                        );
                    }
                    return self.err(
                        "BRANCH_ENVIRONMENT_RECORD_FAILED",
                        format!("failed to claim branch apply-prepared lifecycle: {error}"),
                    );
                }
            },
            None => BranchApplyPreparedRecording::None,
        };

        let manifest = match validated_prepared_manifest(&prepared) {
            Ok(manifest) => manifest,
            Err(error) => {
                let response = self.deploy_error_response("DEPLOY_APPLY_PREPARED_FAILED", error);
                if branch_recording != BranchApplyPreparedRecording::None {
                    return self
                        .branch_apply_prepared_response(
                            &active.mesh.store,
                            &prepared.namespace,
                            &request.prepared_deploy_id,
                            response,
                            branch_recording,
                        )
                        .await;
                }
                return response;
            }
        };
        let runtime = match self
            .prepare_deploy_apply_runtime(
                active,
                &manifest.namespace,
                "DEPLOY_APPLY_PREPARED_FAILED",
            )
            .await
        {
            Ok(runtime) => runtime,
            Err(response) => {
                if branch_recording != BranchApplyPreparedRecording::None {
                    return self
                        .branch_apply_prepared_response(
                            &active.mesh.store,
                            &prepared.namespace,
                            &request.prepared_deploy_id,
                            response,
                            branch_recording,
                        )
                        .await;
                }
                return response;
            }
        };
        let response = self
            .apply_prepared_with_runtime(active, &request.prepared_deploy_id, runtime)
            .await;
        if branch_recording != BranchApplyPreparedRecording::None {
            return self
                .branch_apply_prepared_response(
                    &active.mesh.store,
                    &prepared.namespace,
                    &request.prepared_deploy_id,
                    response,
                    branch_recording,
                )
                .await;
        }
        response
    }

    fn branch_environment_replay_response(
        &self,
        environment: BranchEnvironmentRecord,
    ) -> DaemonResponse {
        self.ok_with_payload(
            format!(
                "branch environment '{}' is {}",
                environment.target_namespace, environment.state
            ),
            Some(DaemonPayload::BranchEnvironment(BranchEnvironmentPayload {
                environment,
            })),
        )
    }

    async fn branch_apply_prepared_response(
        &self,
        store: &StoreDriver,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        response: DaemonResponse,
        recording: BranchApplyPreparedRecording,
    ) -> DaemonResponse {
        match record_branch_apply_prepared_outcome(
            store,
            target_namespace,
            prepared_deploy_id,
            &response,
        )
        .await
        {
            Ok(_) => response,
            Err(error) if recording != BranchApplyPreparedRecording::Claimed => {
                tracing::warn!(
                    %error,
                    %target_namespace,
                    %prepared_deploy_id,
                    "failed to record branch apply-prepared outcome"
                );
                response
            }
            Err(error) => self.err(
                "BRANCH_ENVIRONMENT_RECORD_FAILED",
                format!(
                    "failed to record branch apply-prepared outcome: {error}; deploy response was {}: {}",
                    response.code, response.message
                ),
            ),
        }
    }

    async fn prepare_deploy_apply_runtime(
        &self,
        active: &ActiveMesh,
        namespace: &Namespace,
        setup_failure_code: &'static str,
    ) -> Result<DeployApplyRuntime, DaemonResponse> {
        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let nats_store =
            match ployz_nats::NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
                Ok(store) => store.with_asset_policy(active.config.storage_replicas),
                Err(error) => return Err(self.err(setup_failure_code, error.to_string())),
            };
        if let Err(error) = nats_store.start().await {
            return Err(self.err(setup_failure_code, error.to_string()));
        }
        let nats_locks = match NatsLocks::new(&nats_store).await {
            Ok(locks) => locks,
            Err(error) => return Err(self.err(setup_failure_code, error.to_string())),
        };
        let deploy_lock = match NatsDeployLock::acquire(
            nats_locks.clone(),
            namespace,
            &ReservationId::random().0,
            &self.identity.machine_id,
            DEPLOY_LOCK_TTL,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => return Err(self.err("DEPLOY_LOCK_FAILED", error.to_string())),
        };
        Ok(DeployApplyRuntime {
            nats_store,
            nats_locks,
            deploy_lock,
        })
    }

    async fn apply_manifest_with_runtime(
        &self,
        active: &ActiveMesh,
        manifest: &DeployManifest,
        runtime: DeployApplyRuntime,
        preconditions: DeployApplyPreconditions<'_>,
    ) -> DaemonResponse {
        let certificate_coordinator = Arc::new(
            crate::daemon::cert_coordination::NatsIssuanceCoordinator::new(
                runtime.nats_locks.clone(),
                self.identity.machine_id.clone(),
            ),
        );
        let account_coordinator: Arc<dyn AcmeAccountCoordinator> = certificate_coordinator.clone();
        let challenge_readiness = Arc::new(
            crate::daemon::cert_coordination::NatsChallengeReadiness::new(
                active.mesh.store.clone(),
            ),
        );
        let issuer_factory = Arc::new(InstantAcmeIssuerFactory::new(
            CertificateManagerConfig::from_env(),
        ));
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
        );
        let participant_client = NatsDeployParticipantClient::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store).with_policy(RpcPolicy {
                timeout: DEPLOY_PARTICIPANT_RPC_TIMEOUT,
            }),
        );

        let deploy_id = new_deploy_id();
        let apply = Box::pin(apply_with_deploy_id_and_preconditions(
            &active.mesh.store,
            &participant_client,
            &self.identity.machine_id,
            &manifest,
            deploy_id.clone(),
            certificate_coordinator,
            account_coordinator,
            challenge_readiness,
            issuer_factory,
            &prober,
            preconditions,
        ));
        self.run_locked_deploy_apply(
            active,
            deploy_id,
            runtime,
            apply,
            "apply",
            "DEPLOY_APPLY_FAILED",
            "ENCODE_DEPLOY",
        )
        .await
    }

    async fn apply_prepared_with_runtime(
        &self,
        active: &ActiveMesh,
        prepared_deploy_id: &DeployId,
        runtime: DeployApplyRuntime,
    ) -> DaemonResponse {
        let certificate_coordinator = Arc::new(
            crate::daemon::cert_coordination::NatsIssuanceCoordinator::new(
                runtime.nats_locks.clone(),
                self.identity.machine_id.clone(),
            ),
        );
        let account_coordinator: Arc<dyn AcmeAccountCoordinator> = certificate_coordinator.clone();
        let challenge_readiness = Arc::new(
            crate::daemon::cert_coordination::NatsChallengeReadiness::new(
                active.mesh.store.clone(),
            ),
        );
        let issuer_factory = Arc::new(InstantAcmeIssuerFactory::new(
            CertificateManagerConfig::from_env(),
        ));
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
        );
        let participant_client = NatsDeployParticipantClient::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store).with_policy(RpcPolicy {
                timeout: DEPLOY_PARTICIPANT_RPC_TIMEOUT,
            }),
        );

        let deploy_id = prepared_deploy_id.clone();
        let apply = Box::pin(apply_prepared_with_certificate_coordination(
            &active.mesh.store,
            &participant_client,
            &self.identity.machine_id,
            deploy_id.clone(),
            certificate_coordinator,
            account_coordinator,
            challenge_readiness,
            issuer_factory,
            &prober,
        ));
        self.run_locked_deploy_apply(
            active,
            deploy_id,
            runtime,
            apply,
            "apply-prepared",
            "DEPLOY_APPLY_PREPARED_FAILED",
            "ENCODE_DEPLOY_RESULT",
        )
        .await
    }

    async fn run_locked_deploy_apply(
        &self,
        active: &ActiveMesh,
        deploy_id: DeployId,
        runtime: DeployApplyRuntime,
        mut apply: DeployApplyFuture<'_>,
        operation: &'static str,
        error_code: &'static str,
        encode_code: &'static str,
    ) -> DaemonResponse {
        let mut deploy_lock_renewer = tokio::spawn(renew_deploy_lock(
            runtime.deploy_lock.clone(),
            DEPLOY_LOCK_TTL,
            DEPLOY_LOCK_RENEW_INTERVAL,
        ));
        let result = tokio::select! {
            result = &mut apply => result,
            renewal = &mut deploy_lock_renewer => {
                let message = match renewal {
                    Ok(Ok(())) => format!("deploy lock renewal task exited before {operation} completed"),
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => format!("deploy lock renewal task failed: {error}"),
                };
                match mark_deploy_failed_after_lock_loss(&active.mesh.store, &deploy_id, &message).await {
                    DeployLockLossOutcome::PastCommit => {
                        tracing::warn!(
                            %deploy_id,
                            %message,
                            %operation,
                            "deploy lock was lost after commit point; waiting for operation to finish"
                        );
                        (&mut apply).await
                    }
                    DeployLockLossOutcome::MarkedFailed | DeployLockLossOutcome::NotApplying => {
                        if let Err(error) = runtime.deploy_lock.release().await {
                            tracing::warn!(%error, "failed to release NATS deploy lock after renewal failure");
                        }
                        return self.err("DEPLOY_LOCK_FAILED", message);
                    }
                }
            }
        };
        deploy_lock_renewer.abort();
        if let Err(error) = deploy_lock_renewer.await
            && !error.is_cancelled()
        {
            tracing::warn!(%error, "deploy lock renewal task failed during shutdown");
        }
        if let Err(error) = runtime.deploy_lock.release().await {
            tracing::warn!(%error, "failed to release NATS deploy lock");
        }
        match result {
            Ok(result) => self.ok_json_pretty(&result, encode_code, "encode deploy result"),
            Err(err) => self.deploy_error_response(error_code, err),
        }
    }

    fn deploy_error_response(&self, code: &str, error: PloyzError) -> DaemonResponse {
        let code = deploy_error_code(code, &error);
        if let Some(payload) = deploy_failure_payload_for_error(&error) {
            self.err_with_payload(
                code,
                error.to_string(),
                Some(DaemonPayload::DeployFailure(payload)),
            )
        } else {
            self.err(code, error.to_string())
        }
    }

    pub async fn handle_deploy_export(&self, namespace: &str) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let namespace = Namespace::new(namespace.to_string());
        let manifest = match export_manifest(&active.mesh.store, &namespace).await {
            Ok(manifest) => manifest,
            Err(err) => return self.err("DEPLOY_EXPORT_FAILED", format!("{err}")),
        };
        self.ok_json_pretty(&manifest, "ENCODE_MANIFEST", "encode manifest")
    }

    pub async fn handle_branch_namespace(&self, request: BranchNamespaceRequest) -> DaemonResponse {
        if let Err(error) = validate_branch_namespace_request(&request) {
            return self.err(error.code(), error.to_string());
        }
        if matches!(request.mode, BranchNamespaceMode::Apply) {
            return self.err(
                "BRANCH_DIRECT_APPLY_UNSUPPORTED",
                "branch apply requires branch prepare followed by branch apply-prepared so lifecycle state is durable",
            );
        }
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        if matches!(request.mode, BranchNamespaceMode::Prepare) {
            let target_namespace = Namespace(request.target_namespace.clone());
            match active
                .mesh
                .store
                .get_branch_environment(&target_namespace)
                .await
            {
                Ok(Some(environment))
                    if matches!(environment.state, BranchEnvironmentState::Applying) =>
                {
                    return self.err(
                        "BRANCH_ENVIRONMENT_BUSY",
                        format!(
                            "branch environment '{target_namespace}' is {}",
                            environment.state
                        ),
                    );
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => {
                    return self.err("BRANCH_ENVIRONMENT_STATUS_FAILED", error.to_string());
                }
            }
        }
        let manifest = match render_branch_namespace_manifest(&active.mesh.store, &request).await {
            Ok(manifest) => manifest,
            Err(error) => return self.err(error.code(), error.to_string()),
        };
        match request.mode {
            BranchNamespaceMode::RenderManifest => {
                self.ok_json_pretty(&manifest, "ENCODE_MANIFEST", "encode branch manifest")
            }
            BranchNamespaceMode::Prepare => {
                let manifest_json = match encode_branch_manifest_json(&manifest) {
                    Ok(manifest_json) => manifest_json,
                    Err(error) => return self.err(error.code(), error.to_string()),
                };
                let response = self.handle_deploy_prepare(&manifest_json).await;
                if response.ok {
                    if let Some(DaemonPayload::DeployPrepare(payload)) = response.payload.as_ref() {
                        let record = branch_environment_record_from_prepare(&request, payload);
                        if let Err(error) =
                            active.mesh.store.upsert_branch_environment(&record).await
                        {
                            return self.err("BRANCH_ENVIRONMENT_RECORD_FAILED", error.to_string());
                        }
                    }
                }
                response
            }
            BranchNamespaceMode::Preview => {
                let manifest_json = match encode_branch_manifest_json(&manifest) {
                    Ok(manifest_json) => manifest_json,
                    Err(error) => return self.err(error.code(), error.to_string()),
                };
                self.handle_deploy_preview(&manifest_json, &DeployOptions::default())
                    .await
            }
            BranchNamespaceMode::Apply => {
                let manifest_json = match encode_branch_manifest_json(&manifest) {
                    Ok(manifest_json) => manifest_json,
                    Err(error) => return self.err(error.code(), error.to_string()),
                };
                self.handle_deploy_apply(&manifest_json, &DeployOptions::default())
                    .await
            }
        }
    }

    pub async fn handle_branch_apply_prepared(
        &self,
        request: &BranchApplyPreparedRequest,
    ) -> DaemonResponse {
        self.handle_deploy_apply_prepared_with_branch_lifecycle(
            &DeployApplyPreparedRequest {
                prepared_deploy_id: request.prepared_deploy_id.clone(),
            },
            BranchApplyPreparedLifecycle::Required,
        )
        .await
    }

    pub async fn handle_branch_environment_status(
        &self,
        request: &BranchEnvironmentStatusRequest,
    ) -> DaemonResponse {
        if !valid_storage_segment(&request.target_namespace) {
            return self.err(
                "BRANCH_ENVIRONMENT_INVALID_TARGET",
                "branch target namespace must be 1-63 chars of [a-z0-9_-], starting with a letter or digit",
            );
        }
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let target_namespace = Namespace(request.target_namespace.clone());
        let environment = match active
            .mesh
            .store
            .get_branch_environment(&target_namespace)
            .await
        {
            Ok(Some(environment)) => environment,
            Ok(None) => {
                return self.err(
                    "BRANCH_ENVIRONMENT_NOT_FOUND",
                    format!("branch environment '{target_namespace}' not found"),
                );
            }
            Err(error) => return self.err("BRANCH_ENVIRONMENT_STATUS_FAILED", error.to_string()),
        };
        self.ok_with_payload(
            format!(
                "branch environment '{}' is {}",
                environment.target_namespace, environment.state
            ),
            Some(DaemonPayload::BranchEnvironment(BranchEnvironmentPayload {
                environment,
            })),
        )
    }

    pub async fn handle_branch_environment_list(&self) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let environments = match active.mesh.store.list_branch_environments().await {
            Ok(environments) => environments,
            Err(error) => return self.err("BRANCH_ENVIRONMENT_LIST_FAILED", error.to_string()),
        };
        self.ok_with_payload(
            format!("{} branch environment(s)", environments.len()),
            Some(DaemonPayload::BranchEnvironmentList(
                BranchEnvironmentListPayload { environments },
            )),
        )
    }

    pub async fn handle_migrate_service(&self, request: MigrateServiceRequest) -> DaemonResponse {
        if let Err(error) = validate_migrate_service_request(&request) {
            return self.err(error.code(), error.to_string());
        }
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        match request.mode {
            MigrateServiceMode::RenderManifest => {
                let manifest =
                    match render_migrate_service_manifest(&active.mesh.store, &request).await {
                        Ok(manifest) => manifest,
                        Err(error) => return self.err(error.code(), error.to_string()),
                    };
                self.ok_json_pretty(&manifest, "ENCODE_MANIFEST", "encode migration manifest")
            }
            MigrateServiceMode::Preview => {
                let manifest =
                    match render_migrate_service_manifest(&active.mesh.store, &request).await {
                        Ok(manifest) => manifest,
                        Err(error) => return self.err(error.code(), error.to_string()),
                    };
                let manifest_json = match encode_migrate_manifest_json(&manifest) {
                    Ok(manifest_json) => manifest_json,
                    Err(error) => return self.err(error.code(), error.to_string()),
                };
                self.handle_deploy_preview(&manifest_json, &DeployOptions::default())
                    .await
            }
            MigrateServiceMode::Apply => {
                let namespace = Namespace::new(request.namespace.clone());
                let runtime = match self
                    .prepare_deploy_apply_runtime(active, &namespace, "DEPLOY_APPLY_FAILED")
                    .await
                {
                    Ok(runtime) => runtime,
                    Err(response) => return response,
                };
                let manifest = match render_migrate_service_manifest(&active.mesh.store, &request)
                    .await
                {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        if let Err(release_error) = runtime.deploy_lock.release().await {
                            tracing::warn!(%release_error, "failed to release NATS deploy lock after migrate render failure");
                        }
                        return self.err(error.code(), error.to_string());
                    }
                };
                self.apply_manifest_with_runtime(
                    active,
                    &manifest,
                    runtime,
                    DeployApplyPreconditions::default(),
                )
                .await
            }
        }
    }
}

fn branch_environment_record_from_prepare(
    request: &BranchNamespaceRequest,
    payload: &DeployPreparePayload,
) -> BranchEnvironmentRecord {
    BranchEnvironmentRecord {
        source_namespace: Namespace(request.source_namespace.clone()),
        target_namespace: Namespace(request.target_namespace.clone()),
        state: BranchEnvironmentState::Prepared,
        default_service_mode: branch_environment_mode(request.default_service_mode),
        default_volume_mode: branch_environment_mode(request.default_volume_mode),
        services: branch_environment_overrides(&request.services),
        volumes: branch_environment_overrides(&request.volumes),
        prepared_deploy_id: Some(payload.prepared.prepared_deploy_id.clone()),
        applied_deploy_id: None,
        manifest_hash: payload.prepared.manifest_hash.clone(),
        baseline: payload.prepared.baseline.clone(),
        service_branch_sources: payload.prepared.preview.service_branch_sources.clone(),
        volume_clones: payload.prepared.preview.volume_clones.clone(),
        image_availability: payload.prepared.preview.image_availability.clone(),
        failure: None,
        created_at: payload.prepared.created_at,
        updated_at: payload.prepared.updated_at,
    }
}

fn branch_environment_mode(mode: BranchResourceMode) -> BranchEnvironmentResourceMode {
    match mode {
        BranchResourceMode::Fresh => BranchEnvironmentResourceMode::Fresh,
        BranchResourceMode::Branch => BranchEnvironmentResourceMode::Branch,
    }
}

fn branch_environment_overrides(
    overrides: &[BranchResourceModeOverride],
) -> Vec<BranchEnvironmentResourceOverride> {
    overrides
        .iter()
        .map(|override_| BranchEnvironmentResourceOverride {
            name: override_.name.clone(),
            mode: branch_environment_mode(override_.mode),
        })
        .collect()
}

async fn record_branch_apply_prepared_outcome(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
    response: &DaemonResponse,
) -> ployz_types::Result<BranchEnvironmentRecord> {
    let updated_at = ployz_types::time::now_unix_secs();
    if let Some(environment) =
        active_branch_prepared_replay_record(store, target_namespace, prepared_deploy_id).await?
    {
        return Ok(environment);
    }
    if response.ok {
        match store
            .mark_branch_environment_active(
                target_namespace,
                prepared_deploy_id,
                prepared_deploy_id,
                updated_at,
            )
            .await
        {
            Ok(environment) => Ok(environment),
            Err(error) => {
                if let Some(environment) = active_branch_prepared_replay_record(
                    store,
                    target_namespace,
                    prepared_deploy_id,
                )
                .await?
                {
                    return Ok(environment);
                }
                Err(error)
            }
        }
    } else {
        if let Some(environment) =
            active_branch_prepared_replay_record(store, target_namespace, prepared_deploy_id)
                .await?
        {
            return Ok(environment);
        }
        let failure = BranchEnvironmentFailure {
            code: response.code.clone(),
            message: response.message.clone(),
            deploy_id: None,
        };
        match store
            .mark_branch_environment_failed(
                target_namespace,
                prepared_deploy_id,
                &failure,
                updated_at,
            )
            .await
        {
            Ok(environment) => Ok(environment),
            Err(error) => {
                if let Some(environment) = active_branch_prepared_replay_record(
                    store,
                    target_namespace,
                    prepared_deploy_id,
                )
                .await?
                {
                    return Ok(environment);
                }
                Err(error)
            }
        }
    }
}

fn active_branch_prepared_replay(
    environment: &BranchEnvironmentRecord,
    prepared_deploy_id: &DeployId,
) -> bool {
    environment.state == BranchEnvironmentState::Active
        && environment.prepared_deploy_id.as_ref() == Some(prepared_deploy_id)
        && environment.applied_deploy_id.as_ref() == Some(prepared_deploy_id)
}

async fn active_branch_prepared_replay_record(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
) -> ployz_types::Result<Option<BranchEnvironmentRecord>> {
    Ok(store
        .get_branch_environment(target_namespace)
        .await?
        .filter(|environment| active_branch_prepared_replay(environment, prepared_deploy_id)))
}

async fn branch_environment_applying_record(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
) -> ployz_types::Result<Option<BranchEnvironmentRecord>> {
    Ok(store
        .get_branch_environment(target_namespace)
        .await?
        .filter(|environment| {
            environment.state == BranchEnvironmentState::Applying
                && environment.prepared_deploy_id.as_ref() == Some(prepared_deploy_id)
        }))
}

async fn branch_applying_replay_action(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
    prepared_deploy_id: &DeployId,
) -> ployz_types::Result<BranchApplyingReplayAction> {
    if let Some(environment) =
        active_branch_prepared_replay_record(store, &prepared.namespace, prepared_deploy_id).await?
    {
        return Ok(BranchApplyingReplayAction::Replayed(environment));
    }
    if branch_environment_applying_record(store, &prepared.namespace, prepared_deploy_id)
        .await?
        .is_none()
    {
        return Ok(BranchApplyingReplayAction::Busy);
    }
    if store
        .get_deploy_commit(&prepared.namespace, prepared_deploy_id)
        .await?
        .is_some_and(|commit| {
            matches!(
                commit.deploy.state,
                DeployState::Committed | DeployState::CleanupPending
            )
        })
        || store
            .get_deploy(prepared_deploy_id)
            .await?
            .is_some_and(|record| {
                matches!(
                    record.state,
                    DeployState::Committed | DeployState::CleanupPending
                )
            })
    {
        return Ok(BranchApplyingReplayAction::ResumePreparedApply);
    }
    if prepared.state != PreparedDeployState::Applied {
        return Ok(BranchApplyingReplayAction::Busy);
    }
    match store
        .mark_branch_environment_active(
            &prepared.namespace,
            prepared_deploy_id,
            prepared_deploy_id,
            ployz_types::time::now_unix_secs(),
        )
        .await
    {
        Ok(environment) => Ok(BranchApplyingReplayAction::Replayed(environment)),
        Err(error) => {
            if let Some(environment) =
                active_branch_prepared_replay_record(store, &prepared.namespace, prepared_deploy_id)
                    .await?
            {
                return Ok(BranchApplyingReplayAction::Replayed(environment));
            }
            Err(error)
        }
    }
}

async fn renew_deploy_lock(
    deploy_lock: NatsDeployLock,
    ttl: Duration,
    interval: Duration,
) -> ployz_types::Result<()> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        deploy_lock.renew(ttl).await?;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployLockLossOutcome {
    MarkedFailed,
    PastCommit,
    NotApplying,
}

async fn mark_deploy_failed_after_lock_loss(
    store: &StoreDriver,
    deploy_id: &DeployId,
    message: &str,
) -> DeployLockLossOutcome {
    let deploy = match store.get_deploy(deploy_id).await {
        Ok(Some(mut deploy)) => {
            match deploy.state {
                ployz_types::model::DeployState::Committed
                | ployz_types::model::DeployState::CleanupPending
                | ployz_types::model::DeployState::CheckpointCommitted => {
                    return DeployLockLossOutcome::PastCommit;
                }
                ployz_types::model::DeployState::Applying => {}
                ployz_types::model::DeployState::Planning
                | ployz_types::model::DeployState::FailedAfterCheckpoint
                | ployz_types::model::DeployState::Failed => {
                    return DeployLockLossOutcome::NotApplying;
                }
            }
            if deploy_has_checkpoint_commit_point(store, &deploy.namespace, deploy_id).await {
                return DeployLockLossOutcome::PastCommit;
            }
            deploy.state = ployz_types::model::DeployState::Failed;
            deploy.finished_at = Some(ployz_types::time::now_unix_secs());
            if let Ok(mut preview) =
                serde_json::from_str::<ployz_types::model::DeployPreview>(&deploy.summary_json)
            {
                preview
                    .warnings
                    .push(format!("deploy lock lost during apply: {message}"));
                if let Ok(summary_json) = serde_json::to_string(&preview) {
                    deploy.summary_json = summary_json;
                }
            }
            deploy
        }
        Ok(None) => return DeployLockLossOutcome::NotApplying,
        Err(error) => {
            tracing::warn!(%error, %deploy_id, "failed to read deploy record after lock loss");
            return DeployLockLossOutcome::NotApplying;
        }
    };
    if let Err(error) = store.write_deploy_status(&deploy).await {
        tracing::warn!(%error, %deploy_id, "failed to mark deploy failed after lock loss");
        return DeployLockLossOutcome::NotApplying;
    }
    if let Err(error) = mark_running_deploy_phases_failed_after_lock_loss(
        store,
        &deploy.namespace,
        deploy_id,
        message,
    )
    .await
    {
        tracing::warn!(
            %error,
            %deploy_id,
            "failed to mark running deploy phases failed after lock loss"
        );
    }
    DeployLockLossOutcome::MarkedFailed
}

async fn deploy_has_checkpoint_commit_point(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
) -> bool {
    let phases = match store.list_deploy_phases(namespace, deploy_id).await {
        Ok(phases) => phases,
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy phases while classifying lock loss"
            );
            return false;
        }
    };
    let checkpoint_phase_ids = phases
        .iter()
        .filter(|phase| phase.commit_policy == DeployPhaseCommitPolicy::Checkpoint)
        .map(|phase| {
            DeployId::new(format!(
                "{}:phase:{}",
                deploy_id.as_str(),
                phase.phase_id.as_str()
            ))
        })
        .collect::<HashSet<_>>();
    if checkpoint_phase_ids.is_empty() {
        return false;
    }
    if phases.iter().any(|phase| {
        phase.commit_policy == DeployPhaseCommitPolicy::Checkpoint
            && matches!(phase.state, DeployPhaseState::Succeeded { .. })
    }) {
        return true;
    }
    match store.list_deploy_releases(namespace).await {
        Ok(releases) => {
            if releases
                .iter()
                .any(|release| checkpoint_phase_ids.contains(&release.release.updated_by_deploy_id))
            {
                return true;
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy releases while classifying lock loss"
            );
        }
    }
    match store.list_volumes(namespace).await {
        Ok(volumes) => volumes.iter().any(|volume| {
            checkpoint_phase_ids.contains(&volume.created_by_deploy_id)
                || checkpoint_phase_ids.contains(&volume.last_modified_by_deploy_id)
        }),
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy volumes while classifying lock loss"
            );
            false
        }
    }
}

async fn mark_running_deploy_phases_failed_after_lock_loss(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    message: &str,
) -> ployz_types::Result<()> {
    let phases = store.list_deploy_phases(namespace, deploy_id).await?;
    let completed_at = ployz_types::time::now_unix_secs();
    for mut phase in phases {
        if !matches!(
            phase.state,
            DeployPhaseState::Pending | DeployPhaseState::Running
        ) {
            continue;
        }
        phase.state = DeployPhaseState::Failed {
            completed_at,
            failure: DeployPhaseFailure {
                code: "DEPLOY_LOCK_LOST".into(),
                message: format!("deploy lock lost during apply: {message}"),
            },
        };
        store.upsert_deploy_phase(&phase).await?;
    }
    Ok(())
}

fn deploy_apply_preconditions(
    options: &DeployOptions,
) -> Result<DeployApplyPreconditions<'_>, &'static str> {
    Ok(DeployApplyPreconditions {
        expected_baseline: expected_baseline(options)?,
    })
}

fn expected_baseline(
    options: &DeployOptions,
) -> Result<Option<&ployz_types::model::DeployPreviewBaseline>, &'static str> {
    match options.expected_baseline.as_ref() {
        Some(baseline) if baseline.is_empty() => {
            Err("expected_baseline must be omitted or non-empty")
        }
        Some(baseline) if !baseline.is_canonical() => {
            Err("expected_baseline fingerprint must match baseline components")
        }
        Some(baseline) => Ok(Some(baseline)),
        None => Ok(None),
    }
}

fn decode_manifest(manifest_json: &str) -> Result<DeployManifest, Box<DaemonResponse>> {
    let manifest: DeployManifest = serde_json::from_str(manifest_json).map_err(|err| {
        Box::new(DaemonResponse {
            ok: false,
            code: "INVALID_MANIFEST".into(),
            message: format!("invalid deploy manifest: {err}"),
            payload: None,
        })
    })?;

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::RetainedSubnet;
    use crate::mesh_state::network::NetworkConfig;
    use ployz_api::{
        BranchResourceMode, DaemonRequest, DeployFailurePayload, VolumeZfsTransferInfo,
        VolumeZfsTransferPayload, VolumeZfsTransferState,
    };
    use ployz_nats::{NodeCommandSubject, RpcFailure, RpcFailureKind, RpcPolicy};
    use ployz_orchestrator::deploy::participant::MoveVolumeRequest;
    use ployz_orchestrator::{Mesh, WireguardDriver};
    use ployz_runtime_api::Identity;
    use ployz_store_api::{DeployCommit, DeployStore, MachineMembershipStore};
    use ployz_types::model::{
        DeployBaselineComponent, DeployBaselineDiff, DeployId, DeployPhaseCommitPolicy,
        DeployPhaseId, DeployPhaseRecord, DeployPhaseRollbackPolicy, DeployPreview,
        DeployPreviewBaseline, DeployPreviewBaselineComponents, DeployRecord, DeployState,
        MachineId, MachineLifecycle, MachineMembership, NetworkLifecycle, NetworkName, OverlayIp,
        PreparedDeployRecord, PreparedDeployState, PublicKey, ServiceRelease, ServiceReleaseRecord,
        ServiceRevisionRecord, ServiceRoutingPolicy, VolumeRecord,
    };
    use ployz_types::spec::{
        ContainerSpec, Mount, MountSource, NetworkMode, Placement, PullPolicy, Resources,
        RestartPolicy, RolloutStrategy, ServiceIntent, ServiceSpec, VolumeCloneConsistency,
        VolumeCloneDataPolicy, VolumeIntent, VolumeScope,
    };
    use std::collections::{BTreeMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_daemon_state() -> DaemonState {
        DaemonState::new_for_tests(
            &unique_temp_dir("ployz-deploy-handler"),
            Identity::generate(MachineId::new("founder"), [42; 32]),
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        )
    }

    fn install_active_mesh(state: &mut DaemonState, store: StoreDriver) {
        let identity = Identity::generate(MachineId("founder".into()), [42; 32]);
        let mut config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        config.lifecycle = NetworkLifecycle::Running;
        let mesh = Mesh::new(
            WireguardDriver::memory(),
            store,
            None,
            state.identity.machine_id.clone(),
            51820,
        );
        state.active = Some(ActiveMesh {
            retained_subnet: RetainedSubnet::from_running_config(config.subnet),
            config,
            mesh,
            nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver_bind_addr: Some("127.0.0.1:4320".parse().expect("valid address")),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        path.push(format!("{prefix}-{nanos}"));
        path
    }

    fn test_baseline(service_sources: &str) -> DeployPreviewBaseline {
        DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: service_sources.into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        })
    }

    fn test_empty_preview(namespace: &Namespace, baseline: DeployPreviewBaseline) -> DeployPreview {
        DeployPreview {
            namespace: namespace.clone(),
            manifest_hash: "manifest".into(),
            baseline: Some(baseline),
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            image_availability: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn test_branch_environment_record(
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        state: BranchEnvironmentState,
        baseline: DeployPreviewBaseline,
    ) -> BranchEnvironmentRecord {
        BranchEnvironmentRecord {
            source_namespace: Namespace("prod".into()),
            target_namespace: target_namespace.clone(),
            state,
            default_service_mode: BranchEnvironmentResourceMode::Branch,
            default_volume_mode: BranchEnvironmentResourceMode::Fresh,
            services: Vec::new(),
            volumes: Vec::new(),
            prepared_deploy_id: Some(prepared_deploy_id.clone()),
            applied_deploy_id: None,
            manifest_hash: "manifest".into(),
            baseline,
            service_branch_sources: Vec::new(),
            volume_clones: Vec::new(),
            image_availability: Vec::new(),
            failure: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn test_service() -> ServiceSpec {
        test_service_with_mounts(
            "db",
            vec![Mount {
                source: MountSource::Volume("data".into()),
                target: "/var/lib/postgresql/data".into(),
                readonly: false,
            }],
        )
    }

    fn test_service_with_mounts(name: &str, mounts: Vec<Mount>) -> ServiceSpec {
        ServiceSpec {
            name: name.into(),
            placement: Placement::replicated(1),
            template: ContainerSpec {
                image: "postgres:17".into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                mounts,
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                stop_grace_period: None,
                pid_mode: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            restart: RestartPolicy::UnlessStopped,
        }
    }

    fn test_volume_record(namespace: &Namespace, volume: &str, machine: &str) -> VolumeRecord {
        test_volume_record_with_scope(namespace, volume, machine, VolumeScope::Single)
    }

    fn test_volume_record_with_scope(
        namespace: &Namespace,
        volume: &str,
        machine: &str,
        scope: VolumeScope,
    ) -> VolumeRecord {
        VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume.into(),
            scope,
            machine_id: MachineId::new(machine),
            quota: "10G".into(),
            mode: "0750".into(),
            owner: "999:999".into(),
            attached_services: vec!["db".into()],
            created_at: 1,
            created_by_deploy_id: DeployId::new("deploy-1"),
            last_modified_at: 1,
            last_modified_by_deploy_id: DeployId::new("deploy-1"),
        }
    }

    async fn seed_committed_service(
        store: &StoreDriver,
        namespace: &Namespace,
        service: ServiceSpec,
        volumes: Vec<VolumeRecord>,
    ) {
        let revision_hash = format!("rev-{}", service.name);
        let deploy_id = DeployId::new("deploy-1");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId::new("local"),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: service.name,
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes,
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId::new("local"),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed committed service");
    }

    #[test]
    fn decode_manifest_accepts_empty_services() {
        let manifest_json = serde_json::to_string(&DeployManifest {
            namespace: Namespace::new("prod"),
            intent: None,
            volumes: Vec::new(),
            services: Vec::new(),
        })
        .expect("serialize manifest");

        let manifest = decode_manifest(&manifest_json).expect("decode manifest");

        assert_eq!(manifest.namespace, Namespace::new("prod"));
        assert!(manifest.services.is_empty());
    }

    #[test]
    fn decode_manifest_rejects_invalid_json_with_structured_error() {
        let error = decode_manifest("{not-json")
            .expect_err("invalid manifest json should return daemon error");

        assert!(!error.ok);
        assert_eq!(error.code, "INVALID_MANIFEST");
        assert!(error.message.starts_with("invalid deploy manifest:"));
        assert!(error.payload.is_none());
    }

    #[test]
    fn validate_migrate_service_request_rejects_non_segment_namespace() {
        let error = validate_migrate_service_request(&MigrateServiceRequest {
            namespace: "prod/main".into(),
            service: "db".into(),
            target_machine: "machine-b".into(),
            mode: MigrateServiceMode::Apply,
        })
        .expect_err("invalid namespace should fail");

        assert_eq!(error.code(), "MIGRATE_INVALID_REQUEST");
    }

    #[test]
    fn validate_migrate_service_request_rejects_non_segment_service() {
        let error = validate_migrate_service_request(&MigrateServiceRequest {
            namespace: "prod".into(),
            service: "db/primary".into(),
            target_machine: "machine-b".into(),
            mode: MigrateServiceMode::Apply,
        })
        .expect_err("invalid service should fail");

        assert_eq!(error.code(), "MIGRATE_INVALID_REQUEST");
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_adds_volume_move_hint() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        seed_committed_service(
            &store,
            &namespace,
            test_service(),
            vec![test_volume_record(&namespace, "data", "machine-a")],
        )
        .await;

        let manifest = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect("render migrate manifest");

        let intent = manifest.intent.expect("intent");
        let [hint] = intent.volumes.as_slice() else {
            panic!("expected one volume move hint");
        };
        assert_eq!(hint.volume, "data");
        assert_eq!(
            hint.intent,
            VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            }
        );
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_missing_service() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        seed_committed_service(
            &store,
            &namespace,
            test_service(),
            vec![test_volume_record(&namespace, "data", "machine-a")],
        )
        .await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "api".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("missing service should fail");

        assert!(matches!(error, MigrateRenderError::ServiceMissing { .. }));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_service_without_managed_volumes() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let service = test_service_with_mounts(
            "db",
            vec![Mount {
                source: MountSource::Tmpfs,
                target: "/var/lib/postgresql/data".into(),
                readonly: false,
            }],
        );
        seed_committed_service(&store, &namespace, service, Vec::new()).await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("service without managed volumes should fail");

        assert!(matches!(
            error,
            MigrateRenderError::NoManagedVolumeMounts { .. }
        ));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_bind_mounts() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let service = test_service_with_mounts(
            "db",
            vec![
                Mount {
                    source: MountSource::Volume("data".into()),
                    target: "/data".into(),
                    readonly: false,
                },
                Mount {
                    source: MountSource::Bind("/srv/db".into()),
                    target: "/host-data".into(),
                    readonly: false,
                },
            ],
        );
        seed_committed_service(
            &store,
            &namespace,
            service,
            vec![test_volume_record(&namespace, "data", "machine-a")],
        )
        .await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("bind mounts should fail migration rendering");

        assert!(matches!(
            error,
            MigrateRenderError::UnsupportedBindMount { .. }
        ));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_duplicate_managed_volume_mounts() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let service = test_service_with_mounts(
            "db",
            vec![
                Mount {
                    source: MountSource::Volume("data".into()),
                    target: "/data-a".into(),
                    readonly: false,
                },
                Mount {
                    source: MountSource::Volume("data".into()),
                    target: "/data-b".into(),
                    readonly: false,
                },
            ],
        );
        seed_committed_service(
            &store,
            &namespace,
            service,
            vec![test_volume_record(&namespace, "data", "machine-a")],
        )
        .await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("duplicate managed mount should fail");

        assert!(matches!(
            error,
            MigrateRenderError::DuplicateManagedVolumeMount { .. }
        ));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_missing_committed_volume() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        seed_committed_service(&store, &namespace, test_service(), Vec::new()).await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("missing committed volume should fail");

        assert!(matches!(
            error,
            MigrateRenderError::MissingCommittedVolume { .. }
        ));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_already_on_target() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        seed_committed_service(
            &store,
            &namespace,
            test_service(),
            vec![test_volume_record(&namespace, "data", "machine-b")],
        )
        .await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("already-on-target volume should fail");

        assert!(matches!(error, MigrateRenderError::AlreadyOnTarget { .. }));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_rejects_shared_volume() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        seed_committed_service(
            &store,
            &namespace,
            test_service(),
            vec![test_volume_record_with_scope(
                &namespace,
                "data",
                "machine-a",
                VolumeScope::Shared,
            )],
        )
        .await;

        let error = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect_err("shared volume should fail");

        assert!(matches!(
            error,
            MigrateRenderError::UnsupportedVolumeScope { .. }
        ));
    }

    #[tokio::test]
    async fn render_migrate_service_manifest_sorts_multi_volume_hints() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let service = test_service_with_mounts(
            "db",
            vec![
                Mount {
                    source: MountSource::Volume("beta".into()),
                    target: "/beta".into(),
                    readonly: false,
                },
                Mount {
                    source: MountSource::Volume("alpha".into()),
                    target: "/alpha".into(),
                    readonly: false,
                },
            ],
        );
        seed_committed_service(
            &store,
            &namespace,
            service,
            vec![
                test_volume_record(&namespace, "beta", "machine-a"),
                test_volume_record(&namespace, "alpha", "machine-a"),
            ],
        )
        .await;

        let manifest = render_migrate_service_manifest(
            &store,
            &MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        )
        .await
        .expect("render migrate manifest");

        let intent = manifest.intent.expect("intent");
        let volumes = intent
            .volumes
            .iter()
            .map(|hint| hint.volume.as_str())
            .collect::<Vec<_>>();
        assert_eq!(volumes, vec!["alpha", "beta"]);
    }

    #[tokio::test]
    async fn render_branch_namespace_manifest_branches_services_and_fresh_volumes_by_default() {
        let store = StoreDriver::memory();
        let source = Namespace::new("prod");
        seed_committed_service(
            &store,
            &source,
            test_service(),
            vec![test_volume_record(&source, "data", "machine-a")],
        )
        .await;

        let manifest = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
            },
        )
        .await
        .expect("render branch manifest");

        assert_eq!(manifest.namespace, Namespace::new("pr-39"));
        let intent = manifest.intent.expect("branch service intent");
        let [service] = intent.services.as_slice() else {
            panic!(
                "expected one service branch hint, got {:?}",
                intent.services
            );
        };
        assert_eq!(service.service, "db");
        assert_eq!(
            service.intent,
            ServiceIntent::Branch {
                source_namespace: Namespace::new("prod"),
                source_service: "db".into(),
                expected_source_revision_hash: Some("rev-db".into()),
            }
        );
        assert!(intent.volumes.is_empty());
    }

    #[tokio::test]
    async fn render_branch_namespace_manifest_clones_opted_in_volumes() {
        let store = StoreDriver::memory();
        let source = Namespace::new("prod");
        let source_volume = test_volume_record(&source, "data", "machine-a");
        let expected_source_record_fingerprint = stable_fingerprint(&source_volume);
        seed_committed_service(&store, &source, test_service(), vec![source_volume]).await;

        let manifest = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: vec![ployz_api::BranchResourceModeOverride {
                    name: "data".into(),
                    mode: BranchResourceMode::Branch,
                }],
            },
        )
        .await
        .expect("render branch manifest");

        let intent = manifest.intent.expect("branch intent");
        let [volume] = intent.volumes.as_slice() else {
            panic!("expected one volume clone hint, got {:?}", intent.volumes);
        };
        assert_eq!(volume.volume, "data");
        assert_eq!(
            volume.intent,
            VolumeIntent::Clone {
                source_namespace: Namespace::new("prod"),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
                expected_source_record_fingerprint: Some(expected_source_record_fingerprint),
            }
        );
    }

    #[tokio::test]
    async fn render_branch_namespace_manifest_service_override_fresh_removes_hint() {
        let store = StoreDriver::memory();
        let source = Namespace::new("prod");
        seed_committed_service(
            &store,
            &source,
            test_service(),
            vec![test_volume_record(&source, "data", "machine-a")],
        )
        .await;

        let manifest = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: vec![ployz_api::BranchResourceModeOverride {
                    name: "db".into(),
                    mode: BranchResourceMode::Fresh,
                }],
                volumes: Vec::new(),
            },
        )
        .await
        .expect("render branch manifest");

        assert!(manifest.intent.is_none());
    }

    #[tokio::test]
    async fn render_branch_namespace_manifest_rejects_empty_source() {
        let store = StoreDriver::memory();

        let error = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "missing".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
            },
        )
        .await
        .expect_err("empty source should fail");

        assert!(matches!(error, BranchRenderError::EmptySource { .. }));
        assert_eq!(error.code(), "BRANCH_SOURCE_EMPTY");
    }

    #[tokio::test]
    async fn handle_branch_namespace_validates_request_before_mesh() {
        let state = test_daemon_state();

        let response = state
            .handle_branch_namespace(BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "prod".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "BRANCH_INVALID_REQUEST");
    }

    #[tokio::test]
    async fn handle_branch_namespace_prepare_requires_active_mesh() {
        let state = test_daemon_state();

        let response = state
            .handle_branch_namespace(BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::Prepare,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "NO_MESH");
    }

    #[tokio::test]
    async fn handle_branch_namespace_apply_is_unsupported() {
        let state = test_daemon_state();

        let response = state
            .handle_branch_namespace(BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::Apply,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "BRANCH_DIRECT_APPLY_UNSUPPORTED");
    }

    #[tokio::test]
    async fn branch_prepare_record_preserves_compiled_source_evidence() {
        let store = StoreDriver::memory();
        let mut machine = MachineMembership::seed(
            MachineId::new("founder"),
            PublicKey([42; 32]),
            OverlayIp("fd00::42".parse().expect("valid overlay")),
            None,
            Vec::new(),
        );
        machine.lifecycle = MachineLifecycle::Active;
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed active machine");
        let source = Namespace::new("prod");
        let source_volume = test_volume_record(&source, "data", "founder");
        let expected_source_record_fingerprint = stable_fingerprint(&source_volume);
        seed_committed_service(&store, &source, test_service(), vec![source_volume]).await;
        let manifest = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::Prepare,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: vec![ployz_api::BranchResourceModeOverride {
                    name: "data".into(),
                    mode: BranchResourceMode::Branch,
                }],
            },
        )
        .await
        .expect("render branch manifest");

        let prepared = prepare(
            &store,
            &MachineId::new("founder"),
            &manifest,
            &ployz_orchestrator::deploy::NoopParticipantProbe,
            DeployId::new("prepare-branch"),
            DEPLOY_PREPARE_TTL_SECS,
        )
        .await
        .expect("prepare compiled branch manifest");

        assert_eq!(prepared.namespace, Namespace::new("pr-39"));
        let stored_manifest: DeployManifest =
            serde_json::from_str(&prepared.manifest_json).expect("stored manifest json");
        assert_eq!(stored_manifest.namespace, Namespace::new("pr-39"));
        let intent = stored_manifest.intent.expect("stored branch intent");
        let [service] = intent.services.as_slice() else {
            panic!(
                "expected one stored service branch hint, got {:?}",
                intent.services
            );
        };
        assert_eq!(
            service.intent,
            ServiceIntent::Branch {
                source_namespace: Namespace::new("prod"),
                source_service: "db".into(),
                expected_source_revision_hash: Some("rev-db".into()),
            }
        );
        let [volume] = intent.volumes.as_slice() else {
            panic!(
                "expected one stored volume clone hint, got {:?}",
                intent.volumes
            );
        };
        assert_eq!(
            volume.intent,
            VolumeIntent::Clone {
                source_namespace: Namespace::new("prod"),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
                expected_source_record_fingerprint: Some(expected_source_record_fingerprint),
            }
        );
        assert_eq!(prepared.preview.namespace, Namespace::new("pr-39"));
        assert_eq!(prepared.preview.service_branch_sources.len(), 1);
        assert_eq!(prepared.preview.volume_clones.len(), 1);
    }

    #[tokio::test]
    async fn handle_branch_namespace_prepare_allows_active_branch_environment_past_busy_guard() {
        let mut state = test_daemon_state();
        let store = StoreDriver::memory();
        let mut machine = MachineMembership::seed(
            MachineId("founder".into()),
            PublicKey([42; 32]),
            OverlayIp("fd00::42".parse().expect("valid overlay")),
            None,
            Vec::new(),
        );
        machine.lifecycle = MachineLifecycle::Active;
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed active machine");
        seed_committed_service(
            &store,
            &Namespace("prod".into()),
            test_service_with_mounts("web", Vec::new()),
            Vec::new(),
        )
        .await;
        let active_prepared_deploy_id = DeployId("prepare-old".into());
        let baseline = test_baseline("sources");
        let mut active_record = test_branch_environment_record(
            &Namespace("pr-39".into()),
            &active_prepared_deploy_id,
            BranchEnvironmentState::Prepared,
            baseline,
        );
        active_record.state = BranchEnvironmentState::Active;
        active_record.applied_deploy_id = Some(active_prepared_deploy_id.clone());
        store
            .upsert_branch_environment(&active_record)
            .await
            .expect("seed active branch environment");
        install_active_mesh(&mut state, store.clone());

        let response = state
            .handle_branch_namespace(BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::Prepare,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
            })
            .await;

        assert_ne!(response.code, "BRANCH_ENVIRONMENT_BUSY", "{response:?}");
    }

    #[tokio::test]
    async fn branch_apply_prepared_outcome_uses_expected_prepared_id_transition() {
        let store = StoreDriver::memory();
        let baseline = test_baseline("sources");
        let record = BranchEnvironmentRecord {
            source_namespace: Namespace("prod".into()),
            target_namespace: Namespace("pr-39".into()),
            state: BranchEnvironmentState::Prepared,
            default_service_mode: BranchEnvironmentResourceMode::Branch,
            default_volume_mode: BranchEnvironmentResourceMode::Fresh,
            services: Vec::new(),
            volumes: Vec::new(),
            prepared_deploy_id: Some(DeployId("prepare-new".into())),
            applied_deploy_id: None,
            manifest_hash: "manifest".into(),
            baseline,
            service_branch_sources: Vec::new(),
            volume_clones: Vec::new(),
            image_availability: Vec::new(),
            failure: None,
            created_at: 1,
            updated_at: 1,
        };
        store
            .upsert_branch_environment(&record)
            .await
            .expect("write branch environment");

        record_branch_apply_prepared_outcome(
            &store,
            &Namespace("pr-39".into()),
            &DeployId("prepare-old".into()),
            &DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "{}".into(),
                payload: None,
            },
        )
        .await
        .expect_err("stale prepared id should not transition branch environment");
        assert_eq!(
            store
                .get_branch_environment(&Namespace("pr-39".into()))
                .await
                .expect("get branch environment")
                .expect("branch environment exists")
                .state,
            BranchEnvironmentState::Prepared
        );

        store
            .mark_branch_environment_applying(
                &Namespace("pr-39".into()),
                &DeployId("prepare-new".into()),
                2,
            )
            .await
            .expect("claim branch environment apply");
        record_branch_apply_prepared_outcome(
            &store,
            &Namespace("pr-39".into()),
            &DeployId("prepare-new".into()),
            &DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "{}".into(),
                payload: None,
            },
        )
        .await
        .expect("matching prepared id should transition branch environment");
        let stored = store
            .get_branch_environment(&Namespace("pr-39".into()))
            .await
            .expect("get branch environment")
            .expect("branch environment exists");
        assert_eq!(stored.state, BranchEnvironmentState::Active);
        assert_eq!(
            stored.applied_deploy_id,
            Some(DeployId("prepare-new".into()))
        );
    }

    #[tokio::test]
    async fn branch_apply_prepared_outcome_allows_retry_from_failed_for_same_prepared_id() {
        let store = StoreDriver::memory();
        let baseline = test_baseline("sources");
        let record = BranchEnvironmentRecord {
            source_namespace: Namespace("prod".into()),
            target_namespace: Namespace("pr-39".into()),
            state: BranchEnvironmentState::Prepared,
            default_service_mode: BranchEnvironmentResourceMode::Branch,
            default_volume_mode: BranchEnvironmentResourceMode::Fresh,
            services: Vec::new(),
            volumes: Vec::new(),
            prepared_deploy_id: Some(DeployId("prepare-1".into())),
            applied_deploy_id: None,
            manifest_hash: "manifest".into(),
            baseline,
            service_branch_sources: Vec::new(),
            volume_clones: Vec::new(),
            image_availability: Vec::new(),
            failure: None,
            created_at: 1,
            updated_at: 1,
        };
        store
            .upsert_branch_environment(&record)
            .await
            .expect("write branch environment");

        store
            .mark_branch_environment_applying(
                &Namespace("pr-39".into()),
                &DeployId("prepare-1".into()),
                2,
            )
            .await
            .expect("claim branch environment apply");
        record_branch_apply_prepared_outcome(
            &store,
            &Namespace("pr-39".into()),
            &DeployId("prepare-1".into()),
            &DaemonResponse {
                ok: false,
                code: "DEPLOY_APPLY_PREPARED_FAILED".into(),
                message: "transient runtime failure".into(),
                payload: None,
            },
        )
        .await
        .expect("failure should mark branch environment failed");
        assert_eq!(
            store
                .get_branch_environment(&Namespace("pr-39".into()))
                .await
                .expect("get branch environment")
                .expect("branch environment exists")
                .state,
            BranchEnvironmentState::Failed
        );

        store
            .mark_branch_environment_applying(
                &Namespace("pr-39".into()),
                &DeployId("prepare-1".into()),
                21,
            )
            .await
            .expect("reclaim failed branch environment apply");
        record_branch_apply_prepared_outcome(
            &store,
            &Namespace("pr-39".into()),
            &DeployId("prepare-1".into()),
            &DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "{}".into(),
                payload: None,
            },
        )
        .await
        .expect("same prepared id retry should activate branch environment");
        let stored = store
            .get_branch_environment(&Namespace("pr-39".into()))
            .await
            .expect("get branch environment")
            .expect("branch environment exists");
        assert_eq!(stored.state, BranchEnvironmentState::Active);
        assert_eq!(stored.applied_deploy_id, Some(DeployId("prepare-1".into())));
    }

    #[tokio::test]
    async fn branch_apply_prepared_outcome_replays_active_matching_prepared_id() {
        let store = StoreDriver::memory();
        let baseline = test_baseline("sources");
        let record = BranchEnvironmentRecord {
            source_namespace: Namespace("prod".into()),
            target_namespace: Namespace("pr-39".into()),
            state: BranchEnvironmentState::Active,
            default_service_mode: BranchEnvironmentResourceMode::Branch,
            default_volume_mode: BranchEnvironmentResourceMode::Fresh,
            services: Vec::new(),
            volumes: Vec::new(),
            prepared_deploy_id: Some(DeployId("prepare-1".into())),
            applied_deploy_id: Some(DeployId("prepare-1".into())),
            manifest_hash: "manifest".into(),
            baseline,
            service_branch_sources: Vec::new(),
            volume_clones: Vec::new(),
            image_availability: Vec::new(),
            failure: None,
            created_at: 1,
            updated_at: 10,
        };
        store
            .upsert_branch_environment(&record)
            .await
            .expect("write active branch environment");

        let stored = record_branch_apply_prepared_outcome(
            &store,
            &Namespace("pr-39".into()),
            &DeployId("prepare-1".into()),
            &DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "{}".into(),
                payload: None,
            },
        )
        .await
        .expect("matching active prepared id should replay");

        assert_eq!(stored, record);
        assert_eq!(
            store
                .get_branch_environment(&Namespace("pr-39".into()))
                .await
                .expect("get branch environment")
                .expect("branch environment exists"),
            record
        );
    }

    #[tokio::test]
    async fn handle_branch_apply_prepared_replays_active_environment_before_runtime_setup() {
        let mut state = test_daemon_state();
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-1".into());
        let baseline = test_baseline("sources");
        let preview = DeployPreview {
            namespace: namespace.clone(),
            manifest_hash: "manifest".into(),
            baseline: Some(baseline.clone()),
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            image_availability: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            warnings: Vec::new(),
        };
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{not-json".into(),
                preview,
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Applied,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write prepared deploy");
        store
            .upsert_branch_environment(&BranchEnvironmentRecord {
                source_namespace: Namespace("prod".into()),
                target_namespace: namespace.clone(),
                state: BranchEnvironmentState::Active,
                default_service_mode: BranchEnvironmentResourceMode::Branch,
                default_volume_mode: BranchEnvironmentResourceMode::Fresh,
                services: Vec::new(),
                volumes: Vec::new(),
                prepared_deploy_id: Some(prepared_deploy_id.clone()),
                applied_deploy_id: Some(prepared_deploy_id.clone()),
                manifest_hash: "manifest".into(),
                baseline,
                service_branch_sources: Vec::new(),
                volume_clones: Vec::new(),
                image_availability: Vec::new(),
                failure: None,
                created_at: 1,
                updated_at: 10,
            })
            .await
            .expect("write active branch environment");
        install_active_mesh(&mut state, store);

        let response = state
            .handle_branch_apply_prepared(&BranchApplyPreparedRequest {
                prepared_deploy_id: prepared_deploy_id.clone(),
            })
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::BranchEnvironment(payload)) = response.payload else {
            panic!("expected branch environment replay payload: {response:?}");
        };
        assert_eq!(payload.environment.target_namespace, namespace);
        assert_eq!(payload.environment.state, BranchEnvironmentState::Active);
        assert_eq!(
            payload.environment.applied_deploy_id,
            Some(prepared_deploy_id)
        );
    }

    #[tokio::test]
    async fn handle_branch_apply_prepared_marks_applying_before_manifest_validation_failure() {
        let mut state = test_daemon_state();
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-1".into());
        let baseline = test_baseline("sources");
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{not-json".into(),
                preview: test_empty_preview(&namespace, baseline.clone()),
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Prepared,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write prepared deploy");
        store
            .upsert_branch_environment(&test_branch_environment_record(
                &namespace,
                &prepared_deploy_id,
                BranchEnvironmentState::Prepared,
                baseline,
            ))
            .await
            .expect("write prepared branch environment");
        install_active_mesh(&mut state, store.clone());

        let response = state
            .handle_branch_apply_prepared(&BranchApplyPreparedRequest {
                prepared_deploy_id: prepared_deploy_id.clone(),
            })
            .await;

        assert!(!response.ok, "{response:?}");
        assert_eq!(response.code, "PREPARED_DEPLOY_INVALID");
        let stored = store
            .get_branch_environment(&namespace)
            .await
            .expect("get branch environment")
            .expect("branch environment exists");
        assert_eq!(stored.state, BranchEnvironmentState::Failed);
        assert_eq!(stored.prepared_deploy_id, Some(prepared_deploy_id));
        assert_eq!(
            stored.failure.as_ref().map(|failure| failure.code.as_str()),
            Some("PREPARED_DEPLOY_INVALID")
        );
    }

    #[tokio::test]
    async fn deploy_apply_prepared_best_effort_ignores_stale_branch_environment() {
        let mut state = test_daemon_state();
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-new".into());
        let stale_prepared_deploy_id = DeployId("prepare-old".into());
        let baseline = test_baseline("sources");
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{not-json".into(),
                preview: test_empty_preview(&namespace, baseline.clone()),
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Prepared,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write prepared deploy");
        let stale_record = test_branch_environment_record(
            &namespace,
            &stale_prepared_deploy_id,
            BranchEnvironmentState::Prepared,
            baseline,
        );
        store
            .upsert_branch_environment(&stale_record)
            .await
            .expect("write stale branch environment");
        install_active_mesh(&mut state, store.clone());

        let response = state
            .handle_deploy_apply_prepared(&DeployApplyPreparedRequest { prepared_deploy_id })
            .await;

        assert!(!response.ok, "{response:?}");
        assert_eq!(response.code, "PREPARED_DEPLOY_INVALID");
        assert_eq!(
            store
                .get_branch_environment(&namespace)
                .await
                .expect("get branch environment"),
            Some(stale_record)
        );
    }

    #[tokio::test]
    async fn branch_apply_prepared_rejects_existing_applying_before_runtime_work() {
        let mut state = test_daemon_state();
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-1".into());
        let baseline = test_baseline("sources");
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{not-json".into(),
                preview: test_empty_preview(&namespace, baseline.clone()),
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Prepared,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write prepared deploy");
        store
            .upsert_branch_environment(&test_branch_environment_record(
                &namespace,
                &prepared_deploy_id,
                BranchEnvironmentState::Prepared,
                baseline,
            ))
            .await
            .expect("write branch environment");
        store
            .mark_branch_environment_applying(&namespace, &prepared_deploy_id, 20)
            .await
            .expect("mark branch environment applying");
        install_active_mesh(&mut state, store.clone());

        let response = state
            .handle_branch_apply_prepared(&BranchApplyPreparedRequest {
                prepared_deploy_id: prepared_deploy_id.clone(),
            })
            .await;

        assert!(!response.ok, "{response:?}");
        assert_eq!(response.code, "BRANCH_ENVIRONMENT_BUSY");
        let stored = store
            .get_branch_environment(&namespace)
            .await
            .expect("get branch environment")
            .expect("branch environment exists");
        assert_eq!(stored.state, BranchEnvironmentState::Applying);
        assert_eq!(stored.prepared_deploy_id, Some(prepared_deploy_id));
    }

    #[tokio::test]
    async fn branch_apply_prepared_repairs_applying_environment_after_durable_apply() {
        let mut state = test_daemon_state();
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-1".into());
        let baseline = test_baseline("sources");
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{not-json".into(),
                preview: test_empty_preview(&namespace, baseline.clone()),
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Applied,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write applied prepared deploy");
        store
            .upsert_branch_environment(&test_branch_environment_record(
                &namespace,
                &prepared_deploy_id,
                BranchEnvironmentState::Prepared,
                baseline,
            ))
            .await
            .expect("write branch environment");
        store
            .mark_branch_environment_applying(&namespace, &prepared_deploy_id, 20)
            .await
            .expect("mark branch environment applying");
        install_active_mesh(&mut state, store.clone());

        let response = state
            .handle_branch_apply_prepared(&BranchApplyPreparedRequest {
                prepared_deploy_id: prepared_deploy_id.clone(),
            })
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::BranchEnvironment(payload)) = response.payload else {
            panic!("expected branch environment replay payload: {response:?}");
        };
        assert_eq!(payload.environment.state, BranchEnvironmentState::Active);
        assert_eq!(
            payload.environment.applied_deploy_id,
            Some(prepared_deploy_id.clone())
        );
        let stored = store
            .get_branch_environment(&namespace)
            .await
            .expect("get branch environment")
            .expect("branch environment exists");
        assert_eq!(stored.state, BranchEnvironmentState::Active);
        assert_eq!(stored.applied_deploy_id, Some(prepared_deploy_id));
    }

    #[tokio::test]
    async fn branch_applying_durable_commit_resumes_prepared_apply_repair_path() {
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-1".into());
        let baseline = test_baseline("sources");
        let preview = test_empty_preview(&namespace, baseline.clone());
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{}".into(),
                preview: preview.clone(),
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Prepared,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write prepared deploy");
        store
            .upsert_branch_environment(&test_branch_environment_record(
                &namespace,
                &prepared_deploy_id,
                BranchEnvironmentState::Prepared,
                baseline,
            ))
            .await
            .expect("write branch environment");
        store
            .mark_branch_environment_applying(&namespace, &prepared_deploy_id, 20)
            .await
            .expect("mark branch environment applying");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: Vec::new(),
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id: prepared_deploy_id.clone(),
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId("founder".into()),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(2),
                    finished_at: None,
                    summary_json: serde_json::to_string(&preview).expect("preview json"),
                },
            })
            .await
            .expect("write durable commit");
        let prepared = store
            .get_prepared_deploy(&prepared_deploy_id)
            .await
            .expect("get prepared deploy")
            .expect("prepared deploy exists");

        assert!(matches!(
            branch_applying_replay_action(&store, &prepared, &prepared_deploy_id)
                .await
                .expect("classify replay action"),
            BranchApplyingReplayAction::ResumePreparedApply
        ));
    }

    #[tokio::test]
    async fn branch_applying_replay_action_prefers_active_replay_after_race() {
        let store = StoreDriver::memory();
        let namespace = Namespace("pr-39".into());
        let prepared_deploy_id = DeployId("prepare-1".into());
        let baseline = test_baseline("sources");
        store
            .write_prepared_deploy(&PreparedDeployRecord {
                prepared_deploy_id: prepared_deploy_id.clone(),
                namespace: namespace.clone(),
                manifest_hash: "manifest".into(),
                manifest_json: "{}".into(),
                preview: test_empty_preview(&namespace, baseline.clone()),
                baseline: baseline.clone(),
                coordinator_machine_id: MachineId("founder".into()),
                state: PreparedDeployState::Prepared,
                created_at: 1,
                expires_at: 100,
                updated_at: 10,
            })
            .await
            .expect("write prepared deploy");
        store
            .upsert_branch_environment(&test_branch_environment_record(
                &namespace,
                &prepared_deploy_id,
                BranchEnvironmentState::Prepared,
                baseline,
            ))
            .await
            .expect("write branch environment");
        store
            .mark_branch_environment_applying(&namespace, &prepared_deploy_id, 20)
            .await
            .expect("mark branch environment applying");
        store
            .mark_branch_environment_active(
                &namespace,
                &prepared_deploy_id,
                &prepared_deploy_id,
                30,
            )
            .await
            .expect("mark branch environment active");
        let prepared = store
            .get_prepared_deploy(&prepared_deploy_id)
            .await
            .expect("get prepared deploy")
            .expect("prepared deploy exists");

        let BranchApplyingReplayAction::Replayed(environment) =
            branch_applying_replay_action(&store, &prepared, &prepared_deploy_id)
                .await
                .expect("classify replay action")
        else {
            panic!("expected active replay after applying-to-active race");
        };
        assert_eq!(environment.state, BranchEnvironmentState::Active);
        assert_eq!(environment.applied_deploy_id, Some(prepared_deploy_id));
    }

    #[tokio::test]
    async fn render_branch_namespace_manifest_rejects_unknown_and_duplicate_overrides() {
        let store = StoreDriver::memory();
        let source = Namespace::new("prod");
        seed_committed_service(
            &store,
            &source,
            test_service(),
            vec![test_volume_record(&source, "data", "machine-a")],
        )
        .await;

        let unknown = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: vec![ployz_api::BranchResourceModeOverride {
                    name: "missing".into(),
                    mode: BranchResourceMode::Fresh,
                }],
                volumes: Vec::new(),
            },
        )
        .await
        .expect_err("unknown override should fail");
        assert!(matches!(unknown, BranchRenderError::UnknownService { .. }));
        assert_eq!(unknown.code(), "BRANCH_UNKNOWN_SERVICE");

        let unknown_volume = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: vec![ployz_api::BranchResourceModeOverride {
                    name: "missing".into(),
                    mode: BranchResourceMode::Branch,
                }],
            },
        )
        .await
        .expect_err("unknown volume override should fail");
        assert!(matches!(
            unknown_volume,
            BranchRenderError::UnknownVolume { .. }
        ));
        assert_eq!(unknown_volume.code(), "BRANCH_UNKNOWN_VOLUME");

        let duplicate_service = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: vec![
                    ployz_api::BranchResourceModeOverride {
                        name: "db".into(),
                        mode: BranchResourceMode::Branch,
                    },
                    ployz_api::BranchResourceModeOverride {
                        name: "db".into(),
                        mode: BranchResourceMode::Fresh,
                    },
                ],
                volumes: Vec::new(),
            },
        )
        .await
        .expect_err("duplicate service override should fail");
        assert!(matches!(
            duplicate_service,
            BranchRenderError::DuplicateService { .. }
        ));
        assert_eq!(duplicate_service.code(), "BRANCH_DUPLICATE_SERVICE");

        let duplicate = render_branch_namespace_manifest(
            &store,
            &BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: Vec::new(),
                volumes: vec![
                    ployz_api::BranchResourceModeOverride {
                        name: "data".into(),
                        mode: BranchResourceMode::Branch,
                    },
                    ployz_api::BranchResourceModeOverride {
                        name: "data".into(),
                        mode: BranchResourceMode::Fresh,
                    },
                ],
            },
        )
        .await
        .expect_err("duplicate override should fail");
        assert!(matches!(
            duplicate,
            BranchRenderError::DuplicateVolume { .. }
        ));
        assert_eq!(duplicate.code(), "BRANCH_DUPLICATE_VOLUME");
    }

    #[test]
    fn volume_move_result_maps_succeeded_transfer() {
        let result = volume_move_result_from_transfer(transfer_payload(
            "succeeded",
            Some(42),
            Some(4096),
            None,
        ))
        .expect("transfer result")
        .expect("succeeded transfer should complete");

        assert_eq!(result.snapshot, "ployz-move-manifest-data");
        assert_eq!(result.snapshot_guid, 42);
        assert_eq!(result.bytes_transferred, 4096);
    }

    #[test]
    fn volume_move_result_keeps_running_transfer_pending() {
        let result =
            volume_move_result_from_transfer(transfer_payload("running", None, None, None))
                .expect("running transfer is not an error");

        assert!(result.is_none());
    }

    #[test]
    fn volume_move_result_rejects_terminal_failed_transfer() {
        let error = volume_move_result_from_transfer(transfer_payload(
            "failed",
            None,
            None,
            Some("receiver disconnected".into()),
        ))
        .expect_err("failed transfer should fail deploy move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: "failed".into(),
                message: "receiver disconnected".into(),
            })
        );
    }

    #[test]
    fn volume_move_result_requires_success_evidence() {
        let missing_guid = serde_json::json!({
            "id": "transfer-1",
            "namespace": "prod",
            "volume": "data",
            "source_machine": "machine-a",
            "target_machine": "machine-b",
            "snapshot_name": "ployz-move-manifest-data",
            "started_at": 1,
            "updated_at": 2,
            "state": {
                "status": "succeeded",
                "stage": "finished",
                "bytes_transferred": 4096
            }
        });
        serde_json::from_value::<VolumeZfsTransferInfo>(missing_guid)
            .expect_err("success state requires snapshot guid");

        let missing_bytes = serde_json::json!({
            "id": "transfer-1",
            "namespace": "prod",
            "volume": "data",
            "source_machine": "machine-a",
            "target_machine": "machine-b",
            "snapshot_name": "ployz-move-manifest-data",
            "started_at": 1,
            "updated_at": 2,
            "state": {
                "status": "succeeded",
                "stage": "finished",
                "snapshot_guid": 42
            }
        });
        serde_json::from_value::<VolumeZfsTransferInfo>(missing_bytes)
            .expect_err("success state requires bytes");
    }

    #[tokio::test]
    async fn volume_move_rpc_waits_for_terminal_transfer_success() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(transfer_response("running", None, None, None)),
            Ok(transfer_response("succeeded", Some(42), Some(4096), None)),
        ]);

        let result = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect("move result");

        assert_eq!(result.snapshot_guid, 42);
        assert_eq!(result.bytes_transferred, 4096);
        let requests = client.requests();
        let [send, first_poll, second_poll] = requests.as_slice() else {
            panic!("expected send plus two polls, got {requests:?}");
        };
        assert!(send.0.contains("volume.zfs.send"), "{send:?}");
        assert!(
            first_poll.0.contains("volume.zfs.transfer_get"),
            "{first_poll:?}"
        );
        assert!(
            second_poll.0.contains("volume.zfs.transfer_get"),
            "{second_poll:?}"
        );
        match &send.1 {
            DaemonRequest::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                from_snapshot,
            } => {
                assert_eq!(namespace, "prod");
                assert_eq!(volume, "data");
                assert_eq!(snapshot, "ployz-move-manifest-data");
                assert_eq!(target_machine, "machine-b");
                assert_eq!(from_snapshot, &None);
            }
            other => panic!("expected VolumeZfsSend, got {other:?}"),
        }
        assert_eq!(
            client
                .policies()
                .first()
                .expect("move start timeout policy should be applied")
                .timeout,
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_classifies_transport_failure_as_volume_move_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![Err(RpcFailure::new(
            RpcFailureKind::NoResponders,
            "no responder",
        ))]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("transport error should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_send",
                code: "NATS_RPC_NO_RESPONDERS".into(),
                message: "no responder".into(),
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_rejects_source_machine_mismatch_before_rpc() {
        let client = FakeMoveRpcClient::with_responses(Vec::new());

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-other"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("source mismatch should fail before RPC");

        assert!(matches!(
            error,
            PloyzError::Operation {
                operation: "deploy_node_move_volume",
                ..
            }
        ));
        assert!(client.requests().is_empty());
    }

    #[tokio::test]
    async fn volume_move_rpc_surfaces_start_response_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![Ok(DaemonResponse {
            ok: false,
            code: "START_FAILED".into(),
            message: "cannot snapshot".into(),
            payload: None,
        })]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("failed start response should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_send",
                code: "START_FAILED".into(),
                message: "cannot snapshot".into(),
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_requires_start_payload() {
        let client = FakeMoveRpcClient::with_responses(vec![Ok(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "ok".into(),
            payload: None,
        })]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("missing start payload should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer",
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_retries_transient_transfer_poll_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Err(RpcFailure::new(
                RpcFailureKind::NoResponders,
                "no responder",
            )),
            Ok(transfer_response("succeeded", Some(42), Some(4096), None)),
        ]);

        let result = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect("move result");

        assert_eq!(result.snapshot_guid, 42);
        assert_eq!(client.requests().len(), 3);
    }

    #[tokio::test]
    async fn volume_move_rpc_surfaces_terminal_poll_response_failure() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(DaemonResponse {
                ok: false,
                code: "TRANSFER_FAILED".into(),
                message: "receiver disconnected".into(),
                payload: None,
            }),
        ]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("failed poll response should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer_get",
                code: "TRANSFER_FAILED".into(),
                message: "receiver disconnected".into(),
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_requires_poll_payload() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "ok".into(),
                payload: None,
            }),
        ]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_millis(1),
        )
        .await
        .expect_err("missing poll payload should fail move");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer",
            })
        );
    }

    #[tokio::test]
    async fn volume_move_rpc_times_out_waiting_for_running_transfer() {
        let client = FakeMoveRpcClient::with_responses(vec![
            Ok(transfer_response("running", None, None, None)),
            Ok(transfer_response("running", None, None, None)),
        ]);

        let error = run_volume_move_rpc(
            &client,
            &MachineId::new("machine-a"),
            &Namespace::new("prod"),
            &DeployId::new("deploy-1"),
            MoveVolumeRequest {
                volume: "data".into(),
                from_machine: MachineId::new("machine-a"),
                to_machine: MachineId::new("machine-b"),
                snapshot: "ployz-move-manifest-data".into(),
            },
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_millis(1),
        )
        .await
        .expect_err("running transfer should time out");

        assert!(matches!(
            error,
            PloyzError::Operation {
                operation: "volume_zfs_transfer",
                ..
            }
        ));
    }

    #[test]
    fn deploy_failure_payload_preserves_baseline_details() {
        let expected = test_baseline("old");
        let actual = test_baseline("new");
        let payload = deploy_failure_payload_for_error(&PloyzError::Deploy(
            DeployError::DeployBaselineChanged {
                diff: DeployBaselineDiff::new(expected.clone(), actual.clone()),
            },
        ))
        .expect("baseline failure payload");

        let DeployFailurePayload::DeployBaselineChanged {
            expected_baseline,
            actual_baseline,
            baseline_changed_components,
        } = payload
        else {
            panic!("expected baseline changed payload, got {payload:?}");
        };
        assert_eq!(expected_baseline, expected);
        assert_eq!(actual_baseline, actual);
        assert_eq!(
            baseline_changed_components,
            vec![DeployBaselineComponent::ServiceSources]
        );
    }

    #[test]
    fn deploy_error_response_wraps_baseline_payload() {
        let expected = test_baseline("old");
        let actual = test_baseline("new");
        let response = test_daemon_state().deploy_error_response(
            "DEPLOY_APPLY_FAILED",
            PloyzError::Deploy(DeployError::DeployBaselineChanged {
                diff: DeployBaselineDiff::new(expected.clone(), actual.clone()),
            }),
        );

        assert!(!response.ok);
        assert_eq!(response.code, "DEPLOY_BASELINE_CHANGED");
        assert!(response.message.contains("deploy baseline changed"));
        let Some(DaemonPayload::DeployFailure(payload)) = response.payload else {
            panic!("expected deploy failure payload");
        };
        let DeployFailurePayload::DeployBaselineChanged {
            expected_baseline,
            actual_baseline,
            baseline_changed_components,
        } = payload
        else {
            panic!("expected baseline changed payload, got {payload:?}");
        };
        assert_eq!(expected_baseline, expected);
        assert_eq!(actual_baseline, actual);
        assert_eq!(
            baseline_changed_components,
            vec![DeployBaselineComponent::ServiceSources]
        );
    }

    #[test]
    fn deploy_error_response_wraps_prepared_deploy_payloads() {
        let missing = test_daemon_state().deploy_error_response(
            "DEPLOY_APPLY_PREPARED_FAILED",
            PloyzError::Deploy(DeployError::PreparedDeployMissing {
                prepared_deploy_id: "prepare-missing".into(),
            }),
        );
        assert_eq!(missing.code, "PREPARED_DEPLOY_MISSING");
        let Some(DaemonPayload::DeployFailure(payload)) = missing.payload else {
            panic!("expected missing payload");
        };
        let DeployFailurePayload::PreparedDeployMissing { prepared_deploy_id } = payload else {
            panic!("expected missing payload, got {payload:?}");
        };
        assert_eq!(prepared_deploy_id, DeployId::new("prepare-missing"));

        let not_applicable = test_daemon_state().deploy_error_response(
            "DEPLOY_APPLY_PREPARED_FAILED",
            PloyzError::Deploy(DeployError::PreparedDeployNotApplicable {
                prepared_deploy_id: "prepare-applied".into(),
                state: PreparedDeployState::Applied,
            }),
        );
        assert_eq!(not_applicable.code, "PREPARED_DEPLOY_NOT_APPLICABLE");
        let Some(DaemonPayload::DeployFailure(payload)) = not_applicable.payload else {
            panic!("expected not-applicable payload");
        };
        let DeployFailurePayload::PreparedDeployNotApplicable {
            prepared_deploy_id,
            prepared_deploy_state,
        } = payload
        else {
            panic!("expected not-applicable payload, got {payload:?}");
        };
        assert_eq!(prepared_deploy_id, DeployId::new("prepare-applied"));
        assert_eq!(prepared_deploy_state, PreparedDeployState::Applied);

        let expired = test_daemon_state().deploy_error_response(
            "DEPLOY_APPLY_PREPARED_FAILED",
            PloyzError::Deploy(DeployError::PreparedDeployExpired {
                prepared_deploy_id: "prepare-expired".into(),
                expires_at: 42,
            }),
        );
        assert_eq!(expired.code, "PREPARED_DEPLOY_EXPIRED");
        let Some(DaemonPayload::DeployFailure(payload)) = expired.payload else {
            panic!("expected expired payload");
        };
        let DeployFailurePayload::PreparedDeployExpired {
            prepared_deploy_id,
            prepared_deploy_state,
            prepared_deploy_expires_at,
        } = payload
        else {
            panic!("expected expired payload, got {payload:?}");
        };
        assert_eq!(prepared_deploy_id, DeployId::new("prepare-expired"));
        assert_eq!(prepared_deploy_state, PreparedDeployState::Expired);
        assert_eq!(prepared_deploy_expires_at, 42);

        let invalid = test_daemon_state().deploy_error_response(
            "DEPLOY_APPLY_PREPARED_FAILED",
            PloyzError::Deploy(DeployError::PreparedDeployInvalid {
                prepared_deploy_id: "prepare-invalid".into(),
                message: "bad manifest".into(),
            }),
        );
        assert_eq!(invalid.code, "PREPARED_DEPLOY_INVALID");
        let Some(DaemonPayload::DeployFailure(payload)) = invalid.payload else {
            panic!("expected invalid payload");
        };
        let DeployFailurePayload::PreparedDeployInvalid { prepared_deploy_id } = payload else {
            panic!("expected invalid payload, got {payload:?}");
        };
        assert_eq!(prepared_deploy_id, DeployId::new("prepare-invalid"));
    }

    #[test]
    fn deploy_error_response_wraps_image_availability_payload() {
        let response = test_daemon_state().deploy_error_response(
            "DEPLOY_APPLY_FAILED",
            PloyzError::Deploy(DeployError::DeployImageAvailabilityMissing {
                service: "web".into(),
                slot_id: "slot-0001".into(),
                machine_id: "machine-a".into(),
                image: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            }),
        );

        assert!(!response.ok);
        assert_eq!(response.code, "DEPLOY_IMAGE_AVAILABILITY_MISSING");
        let Some(DaemonPayload::DeployFailure(payload)) = response.payload else {
            panic!("expected deploy failure payload");
        };
        let DeployFailurePayload::DeployImageAvailabilityMissing {
            service,
            slot_id,
            machine_id,
            digest,
            ..
        } = payload
        else {
            panic!("expected image availability missing payload, got {payload:?}");
        };
        assert_eq!(service, "web");
        assert_eq!(slot_id, "slot-0001");
        assert_eq!(machine_id, "machine-a");
        assert_eq!(digest, "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn deploy_error_response_uses_stable_image_preflight_codes() {
        let state = test_daemon_state();

        let digest_required = state.deploy_error_response(
            "DEPLOY_PREVIEW_FAILED",
            PloyzError::Deploy(DeployError::DeployImageDigestRequired {
                service: "web".into(),
                image: "web:latest".into(),
            }),
        );
        assert_eq!(digest_required.code, "DEPLOY_IMAGE_DIGEST_REQUIRED");

        let not_present = state.deploy_error_response(
            "DEPLOY_PREVIEW_FAILED",
            PloyzError::Deploy(DeployError::DeployImageAvailabilityNotPresent {
                service: "web".into(),
                slot_id: "slot-0001".into(),
                machine_id: "machine-a".into(),
                image: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                state: "absent".into(),
            }),
        );
        assert_eq!(not_present.code, "DEPLOY_IMAGE_AVAILABILITY_NOT_PRESENT");
        let Some(DaemonPayload::DeployFailure(payload)) = not_present.payload else {
            panic!("expected deploy failure payload");
        };
        let DeployFailurePayload::DeployImageAvailabilityNotPresent { state, .. } = payload else {
            panic!("expected image availability not-present payload, got {payload:?}");
        };
        assert_eq!(state, "absent");
    }

    #[test]
    fn expected_baseline_rejects_empty_baseline() {
        let no_baseline = DeployOptions::default();
        assert_eq!(expected_baseline(&no_baseline).expect("no baseline"), None);

        let empty_baseline = DeployOptions {
            expected_baseline: Some(DeployPreviewBaseline {
                fingerprint: String::new(),
                components: test_baseline("sources").components,
            }),
            ..DeployOptions::default()
        };
        assert!(expected_baseline(&empty_baseline).is_err());

        let malformed_baseline = DeployOptions {
            expected_baseline: Some(DeployPreviewBaseline {
                fingerprint: "bogus".into(),
                components: test_baseline("sources").components,
            }),
            ..DeployOptions::default()
        };
        assert!(expected_baseline(&malformed_baseline).is_err());

        let expected = test_baseline("sources");
        let baseline = DeployOptions {
            expected_baseline: Some(expected.clone()),
            ..DeployOptions::default()
        };
        assert_eq!(
            expected_baseline(&baseline).expect("baseline"),
            Some(&expected)
        );
    }

    #[test]
    fn deploy_apply_preconditions_preserve_non_empty_baseline() {
        let expected = test_baseline("sources");
        let options = DeployOptions {
            expected_baseline: Some(expected.clone()),
            ..DeployOptions::default()
        };

        let preconditions = deploy_apply_preconditions(&options).expect("preconditions");

        assert_eq!(preconditions.expected_baseline, Some(&expected));
    }

    #[tokio::test]
    async fn deploy_apply_rejects_empty_baseline_option_before_mesh_setup() {
        let response = test_daemon_state()
            .handle_deploy_apply(
                "{}",
                &DeployOptions {
                    expected_baseline: Some(DeployPreviewBaseline {
                        fingerprint: String::new(),
                        components: test_baseline("sources").components,
                    }),
                    ..DeployOptions::default()
                },
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "INVALID_DEPLOY_OPTIONS");
        assert!(response.message.contains("expected_baseline"));
        assert!(response.payload.is_none());
    }

    #[tokio::test]
    async fn deploy_prepare_and_apply_prepared_require_active_mesh() {
        let state = test_daemon_state();
        let manifest_json = serde_json::to_string(&DeployManifest {
            namespace: Namespace::new("prod"),
            intent: None,
            volumes: Vec::new(),
            services: Vec::new(),
        })
        .expect("serialize manifest");
        let prepare = state.handle_deploy_prepare(&manifest_json).await;
        let apply = state
            .handle_deploy_apply_prepared(&DeployApplyPreparedRequest {
                prepared_deploy_id: DeployId::new("prepare-1"),
            })
            .await;
        let prepare_via_shared = state
            .handle_shared(DaemonRequest::DeployPrepare {
                manifest_json: manifest_json.clone(),
            })
            .await;
        let apply_via_shared = state
            .handle_shared(DaemonRequest::DeployApplyPrepared {
                request: DeployApplyPreparedRequest {
                    prepared_deploy_id: DeployId::new("prepare-2"),
                },
            })
            .await;

        assert!(!prepare.ok);
        assert_eq!(prepare.code, "NO_MESH");
        assert!(!apply.ok);
        assert_eq!(apply.code, "NO_MESH");
        assert!(!prepare_via_shared.ok);
        assert_eq!(prepare_via_shared.code, "NO_MESH");
        assert!(!apply_via_shared.ok);
        assert_eq!(apply_via_shared.code, "NO_MESH");
    }

    #[tokio::test]
    async fn lock_loss_marks_applying_deploy_failed_with_warning() {
        let store = StoreDriver::memory();
        let deploy_id = DeployId::new("deploy-lock-loss");
        let preview = DeployPreview {
            namespace: Namespace::new("prod"),
            manifest_hash: "manifest".into(),
            baseline: None,
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            image_availability: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            warnings: Vec::new(),
        };
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: Namespace::new("prod"),
                coordinator_machine_id: MachineId::new("local"),
                manifest_hash: "manifest".into(),
                state: DeployState::Applying,
                started_at: 1,
                committed_at: None,
                finished_at: None,
                summary_json: serde_json::to_string(&preview).expect("preview json"),
            })
            .await
            .expect("seed applying deploy");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;
        assert_eq!(outcome, DeployLockLossOutcome::MarkedFailed);

        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::Failed);
        assert!(record.finished_at.is_some());
        let preview: DeployPreview =
            serde_json::from_str(&record.summary_json).expect("preview json");
        assert!(
            preview
                .warnings
                .iter()
                .any(|warning| warning.contains("renew failed"))
        );
    }

    #[tokio::test]
    async fn lock_loss_marks_running_deploy_phases_failed() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let deploy_id = DeployId::new("deploy-lock-loss-phase");
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId::new("local"),
                manifest_hash: "manifest".into(),
                state: DeployState::Applying,
                started_at: 1,
                committed_at: None,
                finished_at: None,
                summary_json: serde_json::to_string(&DeployPreview {
                    namespace: namespace.clone(),
                    manifest_hash: "manifest".into(),
                    baseline: None,
                    participants: Vec::new(),
                    phases: Vec::new(),
                    services: Vec::new(),
                    service_sources: Vec::new(),
                    service_source_fingerprint: String::new(),
                    service_branch_sources: Vec::new(),
                    image_availability: Vec::new(),
                    volume_moves: Vec::new(),
                    volume_clones: Vec::new(),
                    volume_clone_preflights: Vec::new(),
                    warnings: Vec::new(),
                })
                .expect("preview json"),
            })
            .await
            .expect("seed applying deploy");
        store
            .upsert_deploy_phase(&DeployPhaseRecord {
                namespace: namespace.clone(),
                deploy_id: deploy_id.clone(),
                phase_id: DeployPhaseId::new("deploy"),
                commit_deploy_id: None,
                name: "Deploy".into(),
                order: 0,
                after: Vec::new(),
                participants: Vec::new(),
                work: Vec::new(),
                state: DeployPhaseState::Running,
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: ployz_types::model::DeployPhaseAdvancePolicy::Immediate,
                started_at: 1,
            })
            .await
            .expect("seed running phase");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::MarkedFailed);
        let phases = store
            .list_deploy_phases(&namespace, &deploy_id)
            .await
            .expect("list phases");
        let [phase] = phases.as_slice() else {
            panic!("expected one phase, got {phases:?}");
        };
        let DeployPhaseState::Failed { failure, .. } = &phase.state else {
            panic!("expected failed phase, got {:?}", phase.state);
        };
        assert_eq!(failure.code, "DEPLOY_LOCK_LOST");
        assert!(failure.message.contains("renew failed"));
    }

    #[tokio::test]
    async fn lock_loss_after_commit_does_not_mark_deploy_failed() {
        let store = StoreDriver::memory();
        let deploy_id = DeployId::new("deploy-lock-loss-committed");
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: Namespace::new("prod"),
                coordinator_machine_id: MachineId::new("local"),
                manifest_hash: "manifest".into(),
                state: DeployState::Committed,
                started_at: 1,
                committed_at: Some(2),
                finished_at: Some(2),
                summary_json: "{}".into(),
            })
            .await
            .expect("seed committed deploy");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::PastCommit);
        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::Committed);
    }

    #[tokio::test]
    async fn lock_loss_after_checkpoint_status_waits_for_apply() {
        let store = StoreDriver::memory();
        let deploy_id = DeployId::new("deploy-lock-loss-checkpoint-status");
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: Namespace::new("prod"),
                coordinator_machine_id: MachineId::new("local"),
                manifest_hash: "manifest".into(),
                state: DeployState::CheckpointCommitted,
                started_at: 1,
                committed_at: Some(2),
                finished_at: None,
                summary_json: "{}".into(),
            })
            .await
            .expect("seed checkpoint committed deploy");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::PastCommit);
        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::CheckpointCommitted);
    }

    #[tokio::test]
    async fn lock_loss_after_durable_checkpoint_commit_evidence_waits_for_apply() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let deploy_id = DeployId::new("deploy-lock-loss-checkpoint-evidence");
        let phase_id = DeployPhaseId::new("db");
        let phase_commit_id = DeployId::new(format!(
            "{}:phase:{}",
            deploy_id.as_str(),
            phase_id.as_str()
        ));
        let service = test_service();
        let revision_hash = "rev-db".to_string();
        store
            .write_deploy_status(&DeployRecord {
                deploy_id: deploy_id.clone(),
                namespace: namespace.clone(),
                coordinator_machine_id: MachineId::new("local"),
                manifest_hash: "manifest".into(),
                state: DeployState::Applying,
                started_at: 1,
                committed_at: None,
                finished_at: None,
                summary_json: serde_json::to_string(&DeployPreview {
                    namespace: namespace.clone(),
                    manifest_hash: "manifest".into(),
                    baseline: None,
                    participants: Vec::new(),
                    phases: Vec::new(),
                    services: Vec::new(),
                    service_sources: Vec::new(),
                    service_source_fingerprint: String::new(),
                    service_branch_sources: Vec::new(),
                    image_availability: Vec::new(),
                    volume_moves: Vec::new(),
                    volume_clones: Vec::new(),
                    volume_clone_preflights: Vec::new(),
                    warnings: Vec::new(),
                })
                .expect("preview json"),
            })
            .await
            .expect("seed applying deploy");
        store
            .upsert_deploy_phase(&DeployPhaseRecord {
                namespace: namespace.clone(),
                deploy_id: deploy_id.clone(),
                phase_id: phase_id.clone(),
                name: "db".into(),
                order: 0,
                after: Vec::new(),
                participants: Vec::new(),
                work: Vec::new(),
                state: DeployPhaseState::Running,
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: ployz_types::model::DeployPhaseAdvancePolicy::Immediate,
                started_at: 1,
                commit_deploy_id: None,
            })
            .await
            .expect("seed running checkpoint phase");
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId::new("local"),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: phase_commit_id.clone(),
                        updated_at: 2,
                    },
                }],
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id: phase_commit_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId::new("local"),
                    manifest_hash: "manifest".into(),
                    state: DeployState::CheckpointCommitted,
                    started_at: 1,
                    committed_at: Some(2),
                    finished_at: None,
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed durable checkpoint commit evidence");

        let outcome = mark_deploy_failed_after_lock_loss(&store, &deploy_id, "renew failed").await;

        assert_eq!(outcome, DeployLockLossOutcome::PastCommit);
        let record = store
            .get_deploy(&deploy_id)
            .await
            .expect("get deploy")
            .expect("deploy");
        assert_eq!(record.state, DeployState::Applying);
        let phase = store
            .get_deploy_phase(&namespace, &deploy_id, &phase_id)
            .await
            .expect("get phase")
            .expect("phase");
        assert!(matches!(phase.state, DeployPhaseState::Running));
    }

    #[tokio::test]
    async fn export_manifest_includes_stored_volume_declarations() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let service = test_service();
        let revision_hash = "rev-db".to_string();
        let deploy_id = DeployId::new("deploy-1");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId::new("local"),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: service.name.clone(),
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes: vec![VolumeRecord {
                    namespace: namespace.clone(),
                    volume_name: "data".into(),
                    scope: VolumeScope::Single,
                    machine_id: MachineId::new("machine-a"),
                    quota: "10G".into(),
                    mode: "0750".into(),
                    owner: "999:999".into(),
                    attached_services: vec!["db".into()],
                    created_at: 1,
                    created_by_deploy_id: deploy_id.clone(),
                    last_modified_at: 1,
                    last_modified_by_deploy_id: deploy_id.clone(),
                }],
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId::new("local"),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed release and volume");

        let manifest = export_manifest(&store, &namespace)
            .await
            .expect("export manifest");

        let [volume] = manifest.volumes.as_slice() else {
            panic!("expected one volume declaration");
        };
        assert_eq!(volume.name, "data");
        assert_eq!(volume.scope, VolumeScope::Single);
        assert_eq!(volume.quota, "10G");
        assert_eq!(volume.mode, "0750");
        assert_eq!(volume.owner, "999:999");
        manifest.validate().expect("export should validate");
    }

    #[tokio::test]
    async fn export_manifest_surfaces_release_referencing_missing_revision() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let deploy_id = DeployId::new("deploy-1");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: "api".into(),
                    release: ServiceRelease {
                        primary_revision_hash: "missing-rev".into(),
                        referenced_revision_hashes: vec!["missing-rev".into()],
                        routing: ServiceRoutingPolicy::Direct {
                            revision_hash: "missing-rev".into(),
                        },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId::new("local"),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed corrupt release");

        let error = export_manifest(&store, &namespace)
            .await
            .expect_err("missing revision should fail export");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::StoredReleaseMissingRevision {
                service: "api".into(),
                revision_hash: "missing-rev".into()
            })
        );
    }

    #[tokio::test]
    async fn export_manifest_surfaces_stored_spec_service_mismatch() {
        let store = StoreDriver::memory();
        let namespace = Namespace::new("prod");
        let mut service = test_service();
        service.name = "wrong-service".into();
        let revision_hash = "rev-api".to_string();
        let deploy_id = DeployId::new("deploy-1");

        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: vec![ServiceRevisionRecord {
                    namespace: namespace.clone(),
                    service: "api".into(),
                    revision_hash: revision_hash.clone(),
                    spec_json: serde_json::to_string(&service).expect("serialize service"),
                    created_by: MachineId::new("local"),
                    created_at: 1,
                }],
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: vec![ServiceReleaseRecord {
                    namespace: namespace.clone(),
                    service: "api".into(),
                    release: ServiceRelease {
                        primary_revision_hash: revision_hash.clone(),
                        referenced_revision_hashes: vec![revision_hash.clone()],
                        routing: ServiceRoutingPolicy::Direct { revision_hash },
                        slots: Vec::new(),
                        updated_by_deploy_id: deploy_id.clone(),
                        updated_at: 1,
                    },
                }],
                volumes: Vec::new(),
                deploy: DeployRecord {
                    deploy_id,
                    namespace: namespace.clone(),
                    coordinator_machine_id: MachineId::new("local"),
                    manifest_hash: "manifest".into(),
                    state: DeployState::Committed,
                    started_at: 1,
                    committed_at: Some(1),
                    finished_at: Some(1),
                    summary_json: "{}".into(),
                },
            })
            .await
            .expect("seed release");

        let error = export_manifest(&store, &namespace)
            .await
            .expect_err("mismatched stored spec should fail export");

        assert_eq!(
            error,
            PloyzError::Deploy(DeployError::StoredSpecServiceMismatch {
                stored_service: "wrong-service".into(),
                release_service: "api".into()
            })
        );
    }

    #[derive(Clone, Default)]
    struct FakeMoveRpcClient {
        responses: Arc<Mutex<VecDeque<std::result::Result<DaemonResponse, RpcFailure>>>>,
        requests: Arc<Mutex<Vec<(String, DaemonRequest)>>>,
        policies: Arc<Mutex<Vec<RpcPolicy>>>,
    }

    impl FakeMoveRpcClient {
        fn with_responses(responses: Vec<std::result::Result<DaemonResponse, RpcFailure>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<(String, DaemonRequest)> {
            self.requests.lock().expect("requests").clone()
        }

        fn policies(&self) -> Vec<RpcPolicy> {
            self.policies.lock().expect("policies").clone()
        }
    }

    #[async_trait::async_trait]
    impl DeployMoveRpcClient for FakeMoveRpcClient {
        fn with_rpc_policy(&self, policy: RpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn request(
            &self,
            subject: NodeCommandSubject,
            request: &DaemonRequest,
        ) -> std::result::Result<DaemonResponse, RpcFailure> {
            self.requests
                .lock()
                .expect("requests")
                .push((format!("{subject:?}"), request.clone()));
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("fake response queued")
        }
    }

    fn transfer_response(
        status: &str,
        snapshot_guid: Option<u64>,
        bytes_transferred: Option<u64>,
        last_error: Option<String>,
    ) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "ok".into(),
            payload: Some(DaemonPayload::VolumeZfsTransfer(transfer_payload(
                status,
                snapshot_guid,
                bytes_transferred,
                last_error,
            ))),
        }
    }

    fn transfer_payload(
        status: &str,
        snapshot_guid: Option<u64>,
        bytes_transferred: Option<u64>,
        last_error: Option<String>,
    ) -> VolumeZfsTransferPayload {
        VolumeZfsTransferPayload {
            transfer: VolumeZfsTransferInfo {
                id: "transfer-1".into(),
                namespace: "prod".into(),
                volume: "data".into(),
                source_machine: MachineId::new("machine-a"),
                target_machine: MachineId::new("machine-b"),
                snapshot_name: "ployz-move-manifest-data".into(),
                from_snapshot_name: None,
                started_at: 1,
                updated_at: 2,
                state: match status {
                    "succeeded" => VolumeZfsTransferState::Succeeded {
                        stage: "finished".into(),
                        snapshot_guid: snapshot_guid.expect("test success snapshot guid"),
                        from_snapshot_guid: None,
                        bytes_transferred: bytes_transferred.expect("test success bytes"),
                    },
                    "failed" => VolumeZfsTransferState::Failed {
                        stage: "finished".into(),
                        last_error: last_error.unwrap_or_else(|| "transfer failed".into()),
                        snapshot_guid,
                        from_snapshot_guid: None,
                        bytes_transferred,
                    },
                    "interrupted" => VolumeZfsTransferState::Interrupted {
                        stage: "finished".into(),
                        last_error,
                        snapshot_guid,
                        from_snapshot_guid: None,
                        bytes_transferred,
                    },
                    "running" => VolumeZfsTransferState::Running {
                        stage: "finished".into(),
                        snapshot_guid,
                        from_snapshot_guid: None,
                        bytes_transferred,
                        last_error,
                    },
                    other => panic!("unknown test transfer status {other}"),
                },
            },
        }
    }
}
