mod apply;
mod branch;
mod manifest_render;
mod migrate;
mod nats;
mod node;
mod planning;
mod responses;
mod volume_transfer;

use crate::daemon::DaemonState;
use ployz_api::{DaemonResponse, DeployOptions};
use ployz_config::RuntimeTarget;
#[cfg(test)]
use ployz_model::DeployPhaseRecordState;
use ployz_orchestrator::deploy::{DeployApplyPreconditions, export_manifest};
use ployz_spec::{DeployManifest, Namespace};
#[cfg(test)]
use ployz_store_memory::StoreDriverMemoryExt as _;

#[cfg(test)]
use manifest_render::{BranchRenderError, MigrateRenderError, stable_fingerprint};
#[cfg(test)]
pub(super) use planning::DEPLOY_PREPARE_TTL_SECS;
#[cfg(test)]
use volume_transfer::run_volume_move_rpc;

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
