use crate::daemon::DaemonState;
use ployz_api::{DaemonPayload, DaemonResponse, DeployOptions, DeployPreparePayload};
use ployz_orchestrator::deploy::{new_deploy_id, prepare, preview};

use super::decode_manifest;

pub(crate) const DEPLOY_PREPARE_TTL_SECS: u64 = 24 * 60 * 60;

impl DaemonState {
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
        let prober = match self
            .deploy_participant_probe(active, "DEPLOY_PREVIEW_FAILED")
            .await
        {
            Ok(prober) => prober,
            Err(response) => return response,
        };

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
        let prober = match self
            .deploy_participant_probe(active, "DEPLOY_PREPARE_FAILED")
            .await
        {
            Ok(prober) => prober,
            Err(response) => return response,
        };

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
}
