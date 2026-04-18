use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use crate::coordination::fanout::{FanOutTarget, accepted_targets, fanout_abort, fanout_prepare};
use crate::daemon::DaemonState;
use crate::peers::fanout::{NodeStatusFanoutItem, NodeStatusResult, fanout_node_status};
use ployz_api::{
    CoordinationAbortRequest, CoordinationLockKey, CoordinationOperation,
    CoordinationPrepareRequest, DaemonPayload, DaemonResponse, DeployApplyPayload,
    DeployExportPayload, DeployOptions, DeployPreviewPayload,
};
use ployz_config::RuntimeTarget;
use ployz_orchestrator::deploy::{apply, export_manifest, preview};
use ployz_runtime_backends::deploy::DefaultDeploySessionFactory;
use ployz_types::model::{MachineId, MachineRecord};
use ployz_types::spec::{DeployManifest, Namespace};
use ployz_types::time::now_unix_secs;

const NODE_STATUS_FANOUT_DEADLINE: Duration = Duration::from_secs(5);

struct LiveSurvey {
    live_machines: BTreeSet<MachineId>,
    warnings: Vec<String>,
    eligible_peer_count: usize,
    live_peer_count: usize,
}

fn survey_live_machines(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    items: Vec<NodeStatusFanoutItem>,
) -> LiveSurvey {
    let eligible_peer_count = machines
        .iter()
        .filter(|m| m.id != *local_machine_id)
        .filter(|m| !m.drain)
        .count();

    let mut live_machines = BTreeSet::new();
    live_machines.insert(local_machine_id.clone());

    let mut warnings = Vec::new();
    let mut live_peer_count = 0usize;
    for item in items {
        match item.result {
            NodeStatusResult::Ok(payload) => {
                if payload.draining {
                    warnings.push(format!(
                        "peer '{}' is draining; skipping",
                        item.expected
                    ));
                } else if payload.ready {
                    live_machines.insert(item.expected.clone());
                    live_peer_count += 1;
                } else {
                    warnings.push(format!(
                        "peer '{}' is reachable but not ready (phase {})",
                        item.expected, payload.phase
                    ));
                }
            }
            NodeStatusResult::Offline => {
                warnings.push(format!("peer '{}' did not respond to NodeStatus", item.expected));
            }
            NodeStatusResult::InvalidIdentity { reported } => {
                warnings.push(format!(
                    "peer '{}' reported identity '{}'; skipping",
                    item.expected, reported
                ));
            }
        }
    }

    LiveSurvey {
        live_machines,
        warnings,
        eligible_peer_count,
        live_peer_count,
    }
}

fn eligible_peer_targets(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
) -> Vec<FanOutTarget> {
    machines
        .iter()
        .filter(|m| m.id != *local_machine_id)
        .filter(|m| !m.drain)
        .map(|m| FanOutTarget {
            machine_id: m.id.clone(),
            overlay_ip: m.overlay_ip,
        })
        .collect()
}

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

        let machines = match machine_store.list_machines().await {
            Ok(m) => m,
            Err(err) => {
                return self.err(
                    "DEPLOY_PREVIEW_FAILED",
                    format!("failed to list machines: {err}"),
                );
            }
        };
        let self_id = &self.identity.machine_id;
        let peer_targets = eligible_peer_targets(&machines, self_id);
        let items = fanout_node_status(
            &peer_targets,
            self.coordination_rpc_port,
            NODE_STATUS_FANOUT_DEADLINE,
        )
        .await;
        let survey = survey_live_machines(&machines, self_id, items);

        match preview(
            deploy_read.as_ref(),
            machine_store.as_ref(),
            self_id,
            &manifest,
            &survey.live_machines,
        )
        .await
        {
            Ok(mut plan) => {
                plan.warnings.extend(survey.warnings);
                match serde_json::to_string_pretty(&plan) {
                    Ok(json) => self.ok_with_payload(
                        json,
                        Some(DaemonPayload::DeployPreview(DeployPreviewPayload {
                            preview: plan,
                        })),
                    ),
                    Err(err) => self.err("ENCODE_PREVIEW", format!("encode preview: {err}")),
                }
            }
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

        let machines = match machine_store.list_machines().await {
            Ok(m) => m,
            Err(err) => {
                return self.err(
                    "DEPLOY_APPLY_FAILED",
                    format!("failed to list machines: {err}"),
                );
            }
        };
        let self_id = &self.identity.machine_id;
        let peer_targets = eligible_peer_targets(&machines, self_id);
        let items = fanout_node_status(
            &peer_targets,
            self.coordination_rpc_port,
            NODE_STATUS_FANOUT_DEADLINE,
        )
        .await;
        let survey = survey_live_machines(&machines, self_id, items);

        // Apply enforces strict quorum: enough enabled peers must respond to
        // NodeStatus, plus self. Preview returned warnings for the same peers;
        // apply converts that signal into a hard failure before mutating state.
        let cluster_size = survey.eligible_peer_count + 1;
        let required = cluster_size / 2 + 1;
        let live_total = survey.live_peer_count + 1;
        if live_total < required {
            let msg = format!(
                "quorum lost: {live_total} of {cluster_size} enabled machines reachable (need {required})"
            );
            return self.err("QUORUM_LOST", msg);
        }

        let initial_preview = match preview(
            deploy_read.as_ref(),
            machine_store.as_ref(),
            self_id,
            &manifest,
            &survey.live_machines,
        )
        .await
        {
            Ok(p) => p,
            Err(err) => return self.err("DEPLOY_PREVIEW_FAILED", format!("{err}")),
        };

        // Build fan-out targets from the planned participants (excluding self).
        let machine_map: HashMap<_, _> = machines
            .iter()
            .map(|m| (m.id.clone(), m.clone()))
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
        // Deploy locks require all participants to accept (not quorum-based),
        // so we pass cluster_size = 1 to make quorum trivially met.
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
            1, // deploy uses all_online_accepted, not quorum
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
            self_id,
            &manifest,
            &survey.live_machines,
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
