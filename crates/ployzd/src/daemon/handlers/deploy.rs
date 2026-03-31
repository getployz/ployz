use crate::daemon::DaemonState;
use ployz_api::{
    DaemonPayload, DaemonResponse, DeployApplyPayload, DeployExportPayload, DeployOptions,
    DeployPreviewPayload,
};
use ployz_config::RuntimeTarget;
use ployz_orchestrator::deploy::{apply, export_manifest, preview};
use ployz_runtime_backends::deploy::DefaultDeploySessionFactory;
use ployz_types::spec::{DeployManifest, Namespace};

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
        let active = match &self.active {
            Some(active) => active,
            None => return self.err("NO_MESH", "no mesh is running"),
        };
        let deploy_read = active.store.deploy_read();
        let machine_store = active.store.machine();

        match preview(
            deploy_read.as_ref(),
            machine_store.as_ref(),
            &self.identity.machine_id,
            &manifest,
        )
        .await
        {
            Ok(plan) => match serde_json::to_string_pretty(&plan) {
                Ok(json) => self.ok_with_payload(
                    json,
                    Some(DaemonPayload::DeployPreview(DeployPreviewPayload {
                        preview: plan,
                    })),
                ),
                Err(err) => self.err("ENCODE_PREVIEW", format!("encode preview: {err}")),
            },
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
        let active = match &self.active {
            Some(active) => active,
            None => return self.err("NO_MESH", "no mesh is running"),
        };
        let deploy_read = active.store.deploy_read();
        let deploy_write = active.store.deploy_write();
        let deploy_commit = active.store.deploy_commit();
        let machine_store = active.store.machine();

        let factory = DefaultDeploySessionFactory::for_local_machine(
            active.store.deploy_read(),
            active.store.deploy_write(),
            self.namespace_locks.clone(),
            self.identity.machine_id.clone(),
            self.overlay_network_name(),
            self.overlay_dns_server(),
            self.remote_control_port,
        );

        match apply(
            deploy_read.as_ref(),
            deploy_write.as_ref(),
            deploy_commit.as_ref(),
            machine_store.as_ref(),
            &factory,
            &self.identity.machine_id,
            &manifest,
        )
        .await
        {
            Ok(result) => match serde_json::to_string_pretty(&result) {
                Ok(json) => self.ok_with_payload(
                    json,
                    Some(DaemonPayload::DeployApply(DeployApplyPayload { result })),
                ),
                Err(err) => self.err("ENCODE_DEPLOY", format!("encode deploy result: {err}")),
            },
            Err(err) => self.err("DEPLOY_APPLY_FAILED", format!("{err}")),
        }
    }

    pub async fn handle_deploy_export(&self, namespace: &str) -> DaemonResponse {
        let active = match &self.active {
            Some(active) => active,
            None => return self.err("NO_MESH", "no mesh is running"),
        };
        let namespace = Namespace(namespace.to_string());
        let deploy_read = active.store.deploy_read();
        let manifest = match export_manifest(deploy_read.as_ref(), &namespace).await {
            Ok(manifest) => manifest,
            Err(err) => return self.err("DEPLOY_EXPORT_FAILED", format!("{err}")),
        };
        match serde_json::to_string_pretty(&manifest) {
            Ok(json) => self.ok_with_payload(
                json,
                Some(DaemonPayload::DeployExport(DeployExportPayload {
                    manifest,
                })),
            ),
            Err(err) => self.err("ENCODE_MANIFEST", format!("encode manifest: {err}")),
        }
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
