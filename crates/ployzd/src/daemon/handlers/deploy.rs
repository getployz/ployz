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
use ployz_cert_acme::InstantAcmeIssuerFactory;
use ployz_cert_acme_api::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_config::RuntimeTarget;
use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
#[cfg(test)]
use ployz_model::DeployPhaseRecordState;
use ployz_model::{
    BranchEnvironmentFailure, BranchEnvironmentRecord, BranchEnvironmentResourceMode,
    BranchEnvironmentResourceOverride, BranchEnvironmentState, DeployId, DeployPhaseCommitPolicy,
    DeployPhaseFailure, DeployPhaseState, DeployRecordState, PreparedDeployRecord,
    PreparedDeployState,
};
use ployz_nats::{NatsDeployLock, NatsLocks, NatsStore};
use ployz_orchestrator::coordination::ReservationId;
use ployz_orchestrator::deploy::{
    DeployApplyPreconditions, apply_prepared_with_certificate_coordination,
    apply_with_deploy_id_and_preconditions, new_deploy_id, prepare, preview,
    validated_prepared_manifest,
};
use ployz_spec::{DeployManifest, Namespace, valid_storage_segment};
use ployz_store_api::{DeployStore, StoreDriver, StoreRuntimeControl};
#[cfg(test)]
use ployz_store_memory::StoreDriverMemoryExt as _;

#[cfg(test)]
use manifest_render::{BranchRenderError, MigrateRenderError, stable_fingerprint};
use responses::{deploy_error_code, deploy_failure_payload_for_error};
#[cfg(test)]
use volume_transfer::{run_volume_move_rpc, volume_move_result_from_transfer};

const DEPLOY_LOCK_TTL: Duration = Duration::from_secs(30 * 60);
const DEPLOY_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(10 * 60);
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

type DeployApplyFuture<'a> =
    Pin<Box<dyn Future<Output = ployz_error::Result<ployz_model::DeployApplyResult>> + Send + 'a>>;

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
                    ployz_time::now_unix_secs(),
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
                    response.code(), response.message()
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
        preconditions: DeployApplyPreconditions,
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
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
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
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("DEPLOY_EXPORT_FAILED", error),
        };
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
            let target_namespace = Namespace::new(request.target_namespace.clone());
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
                if response.is_ok() {
                    if let Some(DaemonPayload::DeployPrepare(payload)) = response.payload() {
                        let record = branch_environment_record_from_prepare(&request, &payload);
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
        let target_namespace = Namespace::new(request.target_namespace.clone());
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
        source_namespace: Namespace::new(request.source_namespace.clone()),
        target_namespace: Namespace::new(request.target_namespace.clone()),
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
) -> ployz_error::Result<BranchEnvironmentRecord> {
    let updated_at = ployz_time::now_unix_secs();
    if let Some(environment) =
        active_branch_prepared_replay_record(store, target_namespace, prepared_deploy_id).await?
    {
        return Ok(environment);
    }
    if response.is_ok() {
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
            code: response.code().to_string(),
            message: response.message().to_string(),
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
) -> ployz_error::Result<Option<BranchEnvironmentRecord>> {
    Ok(store
        .get_branch_environment(target_namespace)
        .await?
        .filter(|environment| active_branch_prepared_replay(environment, prepared_deploy_id)))
}

async fn branch_environment_applying_record(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
) -> ployz_error::Result<Option<BranchEnvironmentRecord>> {
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
) -> ployz_error::Result<BranchApplyingReplayAction> {
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
                DeployRecordState::Committed { .. } | DeployRecordState::CleanupPending { .. }
            )
        })
        || store
            .get_deploy(prepared_deploy_id)
            .await?
            .is_some_and(|record| {
                matches!(
                    record.state,
                    DeployRecordState::Committed { .. } | DeployRecordState::CleanupPending { .. }
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
            ployz_time::now_unix_secs(),
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
) -> ployz_error::Result<()> {
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
            match deploy.state() {
                ployz_model::DeployState::Committed
                | ployz_model::DeployState::CleanupPending
                | ployz_model::DeployState::CheckpointCommitted => {
                    return DeployLockLossOutcome::PastCommit;
                }
                ployz_model::DeployState::Applying => {}
                ployz_model::DeployState::Planning
                | ployz_model::DeployState::FailedAfterCheckpoint
                | ployz_model::DeployState::Failed => {
                    return DeployLockLossOutcome::NotApplying;
                }
            }
            if deploy_has_checkpoint_commit_point(store, &deploy.namespace, deploy_id).await {
                return DeployLockLossOutcome::PastCommit;
            }
            let finished_at = ployz_time::now_unix_secs();
            let mut summary_json = deploy.summary_json().to_string();
            if let Ok(mut preview) =
                serde_json::from_str::<ployz_model::DeployPreview>(deploy.summary_json())
            {
                preview
                    .warnings
                    .push(format!("deploy lock lost during apply: {message}"));
                if let Ok(next_summary_json) = serde_json::to_string(&preview) {
                    summary_json = next_summary_json;
                }
            }
            deploy.mark_failed(finished_at, summary_json);
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
        .filter(|phase| phase.commit_policy() == DeployPhaseCommitPolicy::Checkpoint)
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
        phase.commit_policy() == DeployPhaseCommitPolicy::Checkpoint
            && matches!(phase.lifecycle_state(), DeployPhaseState::Succeeded { .. })
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
) -> ployz_error::Result<()> {
    let phases = store.list_deploy_phases(namespace, deploy_id).await?;
    let completed_at = ployz_time::now_unix_secs();
    for mut phase in phases {
        if !matches!(
            phase.lifecycle_state(),
            DeployPhaseState::Pending | DeployPhaseState::Running
        ) {
            continue;
        }
        phase.mark_failed(
            completed_at,
            DeployPhaseFailure {
                code: "DEPLOY_LOCK_LOST".into(),
                message: format!("deploy lock lost during apply: {message}"),
            },
        );
        store.upsert_deploy_phase(&phase).await?;
    }
    Ok(())
}

fn deploy_apply_preconditions(
    options: &DeployOptions,
) -> Result<DeployApplyPreconditions, &'static str> {
    Ok(DeployApplyPreconditions {
        expected_baseline: expected_baseline(options)?,
    })
}

fn expected_baseline(
    options: &DeployOptions,
) -> Result<Option<ployz_model::DeployPreviewBaseline>, &'static str> {
    match options.expected_baseline.as_ref() {
        Some(baseline) if baseline.is_empty() => {
            Err("expected_baseline must be omitted or non-empty")
        }
        Some(baseline) if !baseline.is_canonical() => {
            Err("expected_baseline fingerprint must match baseline components")
        }
        Some(baseline) => Ok(Some(baseline.clone())),
        None => Ok(None),
    }
}

fn decode_manifest(manifest_json: &str) -> Result<DeployManifest, Box<DaemonResponse>> {
    let manifest: DeployManifest = serde_json::from_str(manifest_json).map_err(|err| {
        Box::new(DaemonResponse::error(
            "INVALID_MANIFEST",
            format!("invalid deploy manifest: {err}"),
            None,
        ))
    })?;

    Ok(manifest)
}

#[cfg(test)]
mod tests;
