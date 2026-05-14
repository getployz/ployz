use crate::daemon::DaemonState;
use ployz_api::{DaemonResponse, DeployOptions, MigrateServiceMode, MigrateServiceRequest};
use ployz_orchestrator::deploy::DeployApplyPreconditions;
use ployz_spec::Namespace;

use super::manifest_render::{
    encode_migrate_manifest_json, render_migrate_service_manifest, validate_migrate_service_request,
};

impl DaemonState {
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
                let manifest_result =
                    render_migrate_service_manifest(&active.mesh.store, &request).await;
                let manifest = match manifest_result {
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
