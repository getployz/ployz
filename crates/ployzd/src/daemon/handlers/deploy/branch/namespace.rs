use crate::daemon::DaemonState;
use ployz_api::{
    BranchNamespaceMode, BranchNamespaceRequest, BranchResourceMode, BranchResourceModeOverride,
    DaemonPayload, DaemonResponse, DeployOptions, DeployPreparePayload,
};
use ployz_model::{
    BranchEnvironmentRecord, BranchEnvironmentResourceMode, BranchEnvironmentResourceOverride,
    BranchEnvironmentState,
};
use ployz_spec::Namespace;
use ployz_store_api::DeployStore;

use super::super::manifest_render::{
    encode_branch_manifest_json, render_branch_namespace_manifest,
    validate_branch_namespace_request,
};

impl DaemonState {
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
