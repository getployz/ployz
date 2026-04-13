use std::collections::HashMap;
use std::time::Duration;

use crate::coordination::fanout::{accepted_targets, fanout_abort, fanout_prepare, FanOutTarget};
use crate::daemon::DaemonState;
use ployz_api::{
    CoordinationAbortRequest, CoordinationLockKey, CoordinationOperation,
    CoordinationPrepareRequest, DaemonPayload, DaemonResponse, DeployApplyPayload,
    DeployExportPayload, DeployOptions, DeployPreviewPayload,
};
use ployz_config::RuntimeTarget;
use ployz_orchestrator::deploy::{apply, export_manifest, preview};
use ployz_runtime_backends::deploy::DefaultDeploySessionFactory;
use ployz_types::spec::{DeployManifest, Namespace};
use ployz_types::time::now_unix_secs;

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

        // Run a preview to determine which machines will participate, so we
        // can fan-out the namespace lock to only those machines.
        let initial_preview = match preview(
            deploy_read.as_ref(),
            machine_store.as_ref(),
            &self.identity.machine_id,
            &manifest,
        )
        .await
        {
            Ok(p) => p,
            Err(err) => return self.err("DEPLOY_PREVIEW_FAILED", format!("{err}")),
        };

        // Build fan-out targets from the planned participants (excluding self).
        let machines = match machine_store.list_machines().await {
            Ok(m) => m,
            Err(err) => {
                return self.err(
                    "DEPLOY_APPLY_FAILED",
                    format!("failed to list machines for namespace lock: {err}"),
                )
            }
        };
        let self_id = &self.identity.machine_id;
        let machine_map: HashMap<_, _> = machines
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let peers: Vec<FanOutTarget> = initial_preview
            .participants
            .iter()
            .filter(|id| *id != self_id)
            .filter_map(|id| {
                machine_map.get(id).map(|m| FanOutTarget {
                    machine_id: m.id.clone(),
                    overlay_ip: m.overlay_ip,
                })
            })
            .collect();

        let rpc_port = self.coordination_rpc_port;
        let owner_id = self.identity.machine_id.0.clone();
        let nonce = format!("deploy:{}:{}", owner_id, now_unix_secs());
        let namespace_str = manifest.namespace.0.clone();
        let lock_op = CoordinationOperation::LockAcquire {
            key: CoordinationLockKey::DeployNamespace {
                namespace: namespace_str.clone(),
            },
        };

        // Local prepare.
        let local_prepare = self
            .handle_coordination_prepare(CoordinationPrepareRequest {
                owner_id: owner_id.clone(),
                nonce: nonce.clone(),
                lease_ttl_secs: 120,
                operation: lock_op.clone(),
            })
            .await;
        if !local_prepare.ok {
            return self.err("DEPLOY_LOCKED", local_prepare.message);
        }

        // Fan-out prepare to all planned participants.
        let fanout_result = fanout_prepare(
            &peers,
            rpc_port,
            CoordinationPrepareRequest {
                owner_id: owner_id.clone(),
                nonce: nonce.clone(),
                lease_ttl_secs: 120,
                operation: lock_op.clone(),
            },
            Duration::from_secs(10),
        )
        .await;

        if !fanout_result.all_online_accepted {
            let abort_req = CoordinationAbortRequest {
                owner_id: owner_id.clone(),
                nonce: nonce.clone(),
                operation: lock_op.clone(),
            };
            self.handle_coordination_abort(abort_req.clone()).await;
            fanout_abort(&accepted_targets(&fanout_result.accepted), rpc_port, abort_req).await;
            return self.err(
                "DEPLOY_LOCKED",
                format!(
                    "deploy namespace '{}' is locked on a participant",
                    namespace_str
                ),
            );
        }

        let factory = DefaultDeploySessionFactory::for_local_machine(
            active.store.deploy_read(),
            active.store.deploy_write(),
            self.namespace_locks.clone(),
            self.identity.machine_id.clone(),
            self.overlay_network_name(),
            self.overlay_dns_server(),
            self.remote_control_port,
        );

        let apply_result = apply(
            deploy_read.as_ref(),
            deploy_write.as_ref(),
            deploy_commit.as_ref(),
            machine_store.as_ref(),
            &factory,
            &self.identity.machine_id,
            &manifest,
        )
        .await;

        // Release the namespace lock on all peers regardless of outcome.
        // Using abort (not commit) so the lock key does not enter committed_by_key:
        // a committed LockAcquire key is permanent and would block all future deploys
        // to the same namespace. Abort releases the prepared entry immediately, and
        // the 120-second TTL acts as a fallback if we crash before aborting.
        let abort_req = CoordinationAbortRequest {
            owner_id,
            nonce,
            operation: lock_op,
        };
        self.handle_coordination_abort(abort_req.clone()).await;
        fanout_abort(
            &accepted_targets(&fanout_result.accepted),
            rpc_port,
            abort_req,
        )
        .await;

        match apply_result {
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
