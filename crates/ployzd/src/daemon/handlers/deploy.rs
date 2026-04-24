use std::collections::BTreeMap;
use std::sync::Arc;

use crate::daemon::DaemonState;
use ployz_api::{DaemonResponse, DeployOptions};
use ployz_config::RuntimeTarget;
use ployz_orchestrator::deploy::{apply, preview};
use ployz_runtime_backends::deploy::remote::DeployAgent;
use ployz_runtime_backends::deploy::session::DefaultDeploySessionFactory;
use ployz_store_api::DeployStore;
use ployz_store_api::StoreDriver;
use ployz_types::Error as PloyzError;
use ployz_types::spec::{DeployManifest, Namespace, ServiceSpec};

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

        match preview(&active.mesh.store, &self.identity.machine_id, &manifest).await {
            Ok(plan) => self.ok_json_pretty(&plan, "ENCODE_PREVIEW", "encode preview"),
            Err(err) => self.err("DEPLOY_PREVIEW_FAILED", format!("{err}")),
        }
    }

    pub async fn handle_deploy_apply(
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

        let agent = Arc::new(DeployAgent::new(
            active.mesh.store.clone(),
            self.namespace_locks.clone(),
            self.identity.machine_id.clone(),
            self.overlay_network_name(),
            self.overlay_dns_server(),
        ));
        let factory = DefaultDeploySessionFactory::new(
            agent,
            self.identity.machine_id.clone(),
            self.remote_control_port,
        );

        match apply(
            &active.mesh.store,
            &factory,
            &self.identity.machine_id,
            &manifest,
        )
        .await
        {
            Ok(result) => self.ok_json_pretty(&result, "ENCODE_DEPLOY", "encode deploy result"),
            Err(err) => self.err("DEPLOY_APPLY_FAILED", format!("{err}")),
        }
    }

    pub async fn handle_deploy_export(&self, namespace: &str) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let namespace = Namespace(namespace.to_string());
        let manifest = match export_manifest(&active.mesh.store, &namespace).await {
            Ok(manifest) => manifest,
            Err(err) => return self.err("DEPLOY_EXPORT_FAILED", format!("{err}")),
        };
        self.ok_json_pretty(&manifest, "ENCODE_MANIFEST", "encode manifest")
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

    if manifest.services.is_empty() {
        return Err(Box::new(DaemonResponse {
            ok: false,
            code: "INVALID_MANIFEST".into(),
            message: "deploy manifest must contain at least one service".into(),
            payload: None,
        }));
    }

    Ok(manifest)
}

async fn export_manifest(
    store: &StoreDriver,
    namespace: &Namespace,
) -> ployz_types::Result<DeployManifest> {
    let releases = store.list_service_releases(namespace).await?;
    let revisions = store.list_service_revisions(namespace).await?;
    let revisions_by_key: BTreeMap<(String, String), String> = revisions
        .into_iter()
        .map(|revision| {
            (
                (revision.service.clone(), revision.revision_hash.clone()),
                revision.spec_json,
            )
        })
        .collect();

    let mut services = Vec::with_capacity(releases.len());
    for release in releases {
        let key = (
            release.service.clone(),
            release.release.primary_revision_hash.clone(),
        );
        let Some(spec_json) = revisions_by_key.get(&key) else {
            return Err(PloyzError::operation(
                "deploy_export",
                format!(
                    "current release for service '{}' referenced missing revision '{}'",
                    release.service, release.release.primary_revision_hash
                ),
            ));
        };
        let spec: ServiceSpec = serde_json::from_str(spec_json).map_err(|err| {
            PloyzError::operation(
                "deploy_export",
                format!(
                    "invalid stored spec for service '{}': {err}",
                    release.service
                ),
            )
        })?;
        if spec.name != release.service {
            return Err(PloyzError::operation(
                "deploy_export",
                format!(
                    "stored spec service '{}' did not match release service '{}'",
                    spec.name, release.service
                ),
            ));
        }
        services.push(spec);
    }
    services.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(DeployManifest {
        namespace: namespace.clone(),
        services,
    })
}
