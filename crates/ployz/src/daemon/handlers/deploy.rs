use crate::daemon::DaemonState;
use crate::deploy::{LocalDeployRuntime, apply, preview};
use crate::spec::{DeployManifest, Namespace, ServiceSpec};
use crate::transport::{DaemonResponse, DeployManifestFormat, DeployManifestInput, DeployOptions};

impl DaemonState {
    fn overlay_network_name(&self) -> Option<String> {
        self.active
            .as_ref()
            .map(|active| format!("ployz-{}", active.config.name.0))
    }

    pub async fn handle_deploy_preview(
        &self,
        namespace: &str,
        manifest_json: &str,
        _options: &DeployOptions,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let manifest = match parse_manifest_input(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return response,
        };
        let active = match &self.active {
            Some(active) => active,
            None => return self.err("NO_MESH", "no mesh is running"),
        };

        let deploy_manifest = match decode_manifest(&namespace, manifest) {
            Ok(manifest) => manifest,
            Err(response) => return response,
        };

        match preview(
            &active.mesh.store,
            &self.identity.machine_id,
            &namespace,
            &deploy_manifest,
        )
        .await
        {
            Ok(plan) => match serde_json::to_string_pretty(&plan) {
                Ok(json) => self.ok(json),
                Err(err) => self.err("ENCODE_PREVIEW", format!("encode preview: {err}")),
            },
            Err(err) => self.err("DEPLOY_PREVIEW_FAILED", format!("{err}")),
        }
    }

    pub async fn handle_deploy_apply(
        &self,
        namespace: &str,
        manifest_json: &str,
        _options: &DeployOptions,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let manifest = match parse_manifest_input(manifest_json) {
            Ok(manifest) => manifest,
            Err(response) => return response,
        };
        let active = match &self.active {
            Some(active) => active,
            None => return self.err("NO_MESH", "no mesh is running"),
        };

        let deploy_manifest = match decode_manifest(&namespace, manifest) {
            Ok(manifest) => manifest,
            Err(response) => return response,
        };

        let runtime = match LocalDeployRuntime::new(self.overlay_network_name()) {
            Ok(runtime) => runtime,
            Err(err) => return self.err("DOCKER_ERROR", format!("{err}")),
        };

        match apply(
            &active.mesh.store,
            &runtime,
            &self.namespace_locks,
            &self.identity.machine_id,
            self.remote_control_port,
            &namespace,
            &deploy_manifest,
        )
        .await
        {
            Ok(result) => match serde_json::to_string_pretty(&result) {
                Ok(json) => self.ok(json),
                Err(err) => self.err("ENCODE_DEPLOY", format!("encode deploy result: {err}")),
            },
            Err(err) => self.err("DEPLOY_APPLY_FAILED", format!("{err}")),
        }
    }
}

fn parse_manifest_input(manifest_json: &str) -> Result<DeployManifestInput, DaemonResponse> {
    serde_json::from_str(manifest_json).map_err(|err| DaemonResponse {
        ok: false,
        code: "INVALID_MANIFEST".into(),
        message: format!("invalid deploy manifest envelope: {err}"),
    })
}

fn decode_manifest(
    namespace: &Namespace,
    manifest: DeployManifestInput,
) -> Result<DeployManifest, DaemonResponse> {
    if manifest.body.trim().is_empty() {
        return Err(DaemonResponse {
            ok: false,
            code: "INVALID_MANIFEST".into(),
            message: "deploy manifest body was empty".into(),
        });
    }

    match manifest.format {
        DeployManifestFormat::Service => {
            let spec: ServiceSpec =
                serde_json::from_str(&manifest.body).map_err(|err| DaemonResponse {
                    ok: false,
                    code: "INVALID_SPEC".into(),
                    message: format!("invalid service manifest: {err}"),
                })?;
            if spec.namespace != *namespace {
                return Err(DaemonResponse {
                    ok: false,
                    code: "INVALID_ARGUMENT".into(),
                    message: format!(
                        "manifest namespace '{}' did not match requested namespace '{}'",
                        spec.namespace, namespace
                    ),
                });
            }
            Ok(DeployManifest {
                services: vec![spec],
            })
        }
        DeployManifestFormat::Auto | DeployManifestFormat::Compose => Err(DaemonResponse {
            ok: false,
            code: "NOT_IMPLEMENTED".into(),
            message: format!(
                "{} manifest planning is not implemented yet",
                manifest.format
            ),
        }),
    }
}
