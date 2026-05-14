mod apply;
mod branch;
mod manifest_render;
mod migrate;
mod node;
mod responses;
mod volume_transfer;

use crate::daemon::DaemonState;
use manifest_render::export_manifest;
use ployz_api::{DaemonPayload, DaemonResponse, DeployOptions, DeployPreparePayload};
use ployz_config::RuntimeTarget;
use ployz_error::Error as PloyzError;
#[cfg(test)]
use ployz_model::DeployPhaseRecordState;
use ployz_orchestrator::deploy::{DeployApplyPreconditions, new_deploy_id, prepare, preview};
use ployz_spec::{DeployManifest, Namespace};
use ployz_store_api::StoreRuntimeControl;
#[cfg(test)]
use ployz_store_memory::StoreDriverMemoryExt as _;

#[cfg(test)]
use manifest_render::{BranchRenderError, MigrateRenderError, stable_fingerprint};
use responses::{deploy_error_code, deploy_failure_payload_for_error};
#[cfg(test)]
use volume_transfer::{run_volume_move_rpc, volume_move_result_from_transfer};

const DEPLOY_PREPARE_TTL_SECS: u64 = 24 * 60 * 60;

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
