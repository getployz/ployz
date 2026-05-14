use crate::daemon::DaemonState;
use ployz_api::{
    BranchApplyPreparedRequest, BranchEnvironmentListPayload, BranchEnvironmentPayload,
    BranchEnvironmentStatusRequest, BranchNamespaceMode, BranchNamespaceRequest,
    BranchResourceMode, BranchResourceModeOverride, DaemonPayload, DaemonResponse,
    DeployApplyPreparedRequest, DeployOptions, DeployPreparePayload,
};
use ployz_error::{DeployError, Error as PloyzError};
use ployz_model::{
    BranchEnvironmentFailure, BranchEnvironmentRecord, BranchEnvironmentResourceMode,
    BranchEnvironmentResourceOverride, BranchEnvironmentState, DeployId, DeployRecordState,
    PreparedDeployRecord, PreparedDeployState,
};
use ployz_orchestrator::deploy::validated_prepared_manifest;
use ployz_spec::{Namespace, valid_storage_segment};
use ployz_store_api::{DeployStore, StoreDriver};

use super::manifest_render::{
    encode_branch_manifest_json, render_branch_namespace_manifest,
    validate_branch_namespace_request,
};

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

pub(super) enum BranchApplyingReplayAction {
    Replayed(Box<BranchEnvironmentRecord>),
    ResumePreparedApply,
    Busy,
}

impl DaemonState {
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
                        return self.branch_environment_replay_response(*environment);
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
                if response.is_ok()
                    && let Some(DaemonPayload::DeployPrepare(payload)) = response.payload()
                {
                    let record = branch_environment_record_from_prepare(&request, &payload);
                    if let Err(error) = active.mesh.store.upsert_branch_environment(&record).await {
                        return self.err("BRANCH_ENVIRONMENT_RECORD_FAILED", error.to_string());
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

pub(super) async fn record_branch_apply_prepared_outcome(
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

pub(super) async fn branch_applying_replay_action(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
    prepared_deploy_id: &DeployId,
) -> ployz_error::Result<BranchApplyingReplayAction> {
    if let Some(environment) =
        active_branch_prepared_replay_record(store, &prepared.namespace, prepared_deploy_id).await?
    {
        return Ok(BranchApplyingReplayAction::Replayed(Box::new(environment)));
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
        Ok(environment) => Ok(BranchApplyingReplayAction::Replayed(Box::new(environment))),
        Err(error) => {
            if let Some(environment) =
                active_branch_prepared_replay_record(store, &prepared.namespace, prepared_deploy_id)
                    .await?
            {
                return Ok(BranchApplyingReplayAction::Replayed(Box::new(environment)));
            }
            Err(error)
        }
    }
}
