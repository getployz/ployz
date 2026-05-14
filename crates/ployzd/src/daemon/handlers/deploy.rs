mod apply;
mod manifest_render;
mod node;
mod responses;
mod volume_transfer;

use crate::daemon::DaemonState;
use manifest_render::{
    encode_branch_manifest_json, encode_migrate_manifest_json, export_manifest,
    render_branch_namespace_manifest, render_migrate_service_manifest,
    validate_branch_namespace_request, validate_migrate_service_request,
};
use ployz_api::{
    BranchApplyPreparedRequest, BranchEnvironmentListPayload, BranchEnvironmentPayload,
    BranchEnvironmentStatusRequest, BranchNamespaceMode, BranchNamespaceRequest,
    BranchResourceMode, BranchResourceModeOverride, DaemonPayload, DaemonResponse,
    DeployApplyPreparedRequest, DeployOptions, DeployPreparePayload, MigrateServiceMode,
    MigrateServiceRequest,
};
use ployz_config::RuntimeTarget;
use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
#[cfg(test)]
use ployz_model::DeployPhaseRecordState;
use ployz_model::{
    BranchEnvironmentFailure, BranchEnvironmentRecord, BranchEnvironmentResourceMode,
    BranchEnvironmentResourceOverride, BranchEnvironmentState, DeployId, DeployRecordState,
    PreparedDeployRecord, PreparedDeployState,
};
use ployz_orchestrator::deploy::{
    DeployApplyPreconditions, new_deploy_id, prepare, preview, validated_prepared_manifest,
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

const DEPLOY_PREPARE_TTL_SECS: u64 = 24 * 60 * 60;

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
                let manifest =
                    match render_migrate_service_manifest(&active.mesh.store, &request).await {
                        Ok(manifest) => manifest,
                        Err(error) => {
                            runtime
                            .release(
                                "failed to release NATS deploy lock after migrate render failure",
                            )
                            .await;
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
