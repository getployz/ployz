use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use crate::coordination::fanout::{
    FanOutTarget, NodeStatusResult, accepted_targets, fanout_abort, fanout_node_status,
    fanout_prepare,
};
use crate::daemon::DaemonState;
use async_trait::async_trait;
use ployz_api::{
    CoordinationAbortRequest, CoordinationLockKey, CoordinationOperation,
    CoordinationPrepareRequest, DaemonPayload, DaemonResponse, DeployApplyPayload,
    DeployExportPayload, DeployOptions, DeployPreviewPayload,
};
use ployz_config::RuntimeTarget;
use ployz_orchestrator::deploy::{apply, export_manifest, preview};
use ployz_runtime_backends::deploy::DefaultDeploySessionFactory;
use ployz_store_api::{MachineStore, MachineSubscription};
use ployz_types::Result as TypesResult;
use ployz_types::model::{MachineId, Participation};
use ployz_types::spec::{DeployManifest, Namespace};
use ployz_types::time::now_unix_secs;
use std::sync::Arc;

const NODE_STATUS_DEADLINE: Duration = Duration::from_millis(750);

struct DeployNodeSelection {
    eligible: BTreeSet<MachineId>,
    unreachable: Vec<MachineId>,
    drained: Vec<MachineId>,
    invalid_identity: Vec<MachineId>,
}

struct EligibleMachineStore {
    inner: Arc<dyn MachineStore>,
    eligible: BTreeSet<MachineId>,
}

#[async_trait]
impl MachineStore for EligibleMachineStore {
    async fn list_machines(&self) -> TypesResult<Vec<ployz_types::model::MachineRecord>> {
        let machines = self.inner.list_machines().await?;
        Ok(machines
            .into_iter()
            .filter(|machine| self.eligible.contains(&machine.id))
            .collect())
    }

    async fn upsert_self_machine(
        &self,
        record: &ployz_types::model::MachineRecord,
    ) -> TypesResult<()> {
        self.inner.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> TypesResult<()> {
        self.inner.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> TypesResult<MachineSubscription> {
        self.inner.subscribe_machines().await
    }
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
            Ok(machines) => machines,
            Err(err) => {
                return self.err(
                    "DEPLOY_PREVIEW_FAILED",
                    format!("failed to list machines for node eligibility: {err}"),
                );
            }
        };
        let selection = self
            .select_deploy_nodes(active, &machines)
            .await
            .map_err(|err| {
                self.err(
                    "DEPLOY_PREVIEW_FAILED",
                    format!("failed to collect node status for deploy preview: {err}"),
                )
            });
        let selection = match selection {
            Ok(selection) => selection,
            Err(response) => return response,
        };
        let filtered_machine_store = EligibleMachineStore {
            inner: Arc::clone(&machine_store),
            eligible: selection.eligible.clone(),
        };

        match preview(
            deploy_read.as_ref(),
            &filtered_machine_store,
            &self.identity.machine_id,
            &manifest,
        )
        .await
        {
            Ok(mut plan) => {
                plan.warnings.extend(selection.preview_warnings());
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
            Ok(machines) => machines,
            Err(err) => {
                return self.err(
                    "DEPLOY_APPLY_FAILED",
                    format!("failed to list machines for node eligibility: {err}"),
                );
            }
        };
        let selection = match self.select_deploy_nodes(active, &machines).await {
            Ok(selection) => selection,
            Err(err) => {
                return self.err(
                    "DEPLOY_APPLY_FAILED",
                    format!("failed to collect node status for deploy apply: {err}"),
                );
            }
        };
        let cluster_size = machines.len();
        let quorum_size = cluster_size / 2 + 1;
        if selection.eligible.len() < quorum_size {
            return self.err(
                "DEPLOY_QUORUM_LOST",
                format!(
                    "apply requires quorum ({quorum_size}/{cluster_size}); eligible={} unreachable=[{}] drained=[{}] invalid_identity=[{}]",
                    selection.eligible.len(),
                    format_machine_ids(&selection.unreachable),
                    format_machine_ids(&selection.drained),
                    format_machine_ids(&selection.invalid_identity),
                ),
            );
        }
        let filtered_machine_store = EligibleMachineStore {
            inner: Arc::clone(&machine_store),
            eligible: selection.eligible.clone(),
        };

        // Run a preview to determine which machines will participate, so we
        // can fan-out the namespace lock to only those machines.
        let initial_preview = match preview(
            deploy_read.as_ref(),
            &filtered_machine_store,
            &self.identity.machine_id,
            &manifest,
        )
        .await
        {
            Ok(p) => p,
            Err(err) => return self.err("DEPLOY_PREVIEW_FAILED", format!("{err}")),
        };

        // Build fan-out targets from the planned participants (excluding self).
        let self_id = &self.identity.machine_id;
        let machine_map: HashMap<_, _> = machines.into_iter().map(|m| (m.id.clone(), m)).collect();
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
            fanout_abort(
                &accepted_targets(&fanout_result.accepted),
                rpc_port,
                abort_req,
            )
            .await;
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
            &filtered_machine_store,
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

    async fn select_deploy_nodes(
        &self,
        active: &crate::daemon::ActiveMesh,
        machines: &[ployz_types::model::MachineRecord],
    ) -> Result<DeployNodeSelection, String> {
        let self_machine_id = self.identity.machine_id.clone();
        let self_ready = active.mesh.ready_status().await.ready;
        let self_draining = active
            .mesh
            .authoritative_self_record()
            .await
            .is_some_and(|record| record.participation == Participation::Draining);

        let mut eligible = BTreeSet::new();
        let mut unreachable = Vec::new();
        let mut drained = Vec::new();
        let mut invalid_identity = Vec::new();
        if self_ready && !self_draining {
            eligible.insert(self_machine_id.clone());
        } else if self_draining {
            drained.push(self_machine_id.clone());
        }

        let targets: Vec<FanOutTarget> = machines
            .iter()
            .filter(|machine| machine.id != self_machine_id)
            .map(|machine| FanOutTarget {
                machine_id: machine.id.clone(),
                overlay_ip: machine.overlay_ip,
            })
            .collect();
        let fanout = fanout_node_status(&targets, self.coordination_rpc_port, NODE_STATUS_DEADLINE)
            .await
            .into_iter()
            .collect::<HashMap<MachineId, NodeStatusResult>>();

        for machine in machines {
            if machine.id == self_machine_id {
                continue;
            }
            let Some(status) = fanout.get(&machine.id) else {
                unreachable.push(machine.id.clone());
                continue;
            };
            match status {
                NodeStatusResult::Ok(payload) => {
                    if payload.draining {
                        drained.push(machine.id.clone());
                    } else if payload.ready {
                        eligible.insert(machine.id.clone());
                    }
                }
                NodeStatusResult::Offline => unreachable.push(machine.id.clone()),
                NodeStatusResult::InvalidIdentity { .. } => {
                    invalid_identity.push(machine.id.clone());
                }
            }
        }

        Ok(DeployNodeSelection {
            eligible,
            unreachable,
            drained,
            invalid_identity,
        })
    }
}

impl DeployNodeSelection {
    fn preview_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.unreachable.is_empty() {
            warnings.push(format!(
                "unreachable peers were excluded from planning: {}",
                format_machine_ids(&self.unreachable)
            ));
        }
        if !self.drained.is_empty() {
            warnings.push(format!(
                "draining peers were excluded from planning: {}",
                format_machine_ids(&self.drained)
            ));
        }
        if !self.invalid_identity.is_empty() {
            warnings.push(format!(
                "invalid identity peers were excluded from planning: {}",
                format_machine_ids(&self.invalid_identity)
            ));
        }
        warnings
    }
}

fn format_machine_ids(machine_ids: &[MachineId]) -> String {
    let mut ids: Vec<&str> = machine_ids
        .iter()
        .map(|machine_id| machine_id.0.as_str())
        .collect();
    ids.sort_unstable();
    ids.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{DeployNodeSelection, format_machine_ids};
    use ployz_types::model::MachineId;
    use std::collections::BTreeSet;

    #[test]
    fn format_machine_ids_sorts_output() {
        let ids = vec![
            MachineId("peer-b".into()),
            MachineId("peer-a".into()),
            MachineId("peer-c".into()),
        ];

        let rendered = format_machine_ids(&ids);
        assert_eq!(rendered, "peer-a, peer-b, peer-c");
    }

    #[test]
    fn preview_warnings_include_all_excluded_sets() {
        let selection = DeployNodeSelection {
            eligible: BTreeSet::new(),
            unreachable: vec![MachineId("peer-2".into())],
            drained: vec![MachineId("peer-3".into())],
            invalid_identity: vec![MachineId("peer-4".into())],
        };

        let warnings = selection.preview_warnings();
        assert_eq!(warnings.len(), 3);
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("unreachable peers were excluded from planning: peer-2"))
        );
        assert!(
            warnings
                .iter()
                .any(|line| line.contains("draining peers were excluded from planning: peer-3"))
        );
        assert!(warnings.iter().any(|line| {
            line.contains("invalid identity peers were excluded from planning: peer-4")
        }));
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
