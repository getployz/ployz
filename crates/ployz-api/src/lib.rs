use ipnet::Ipv4Net;
use ployz_types::model::{
    InstanceStatusRecord, MachineId, MachineLifecycle, MachineMembership, NetworkId,
    NetworkLifecycle, PublicKey, RoutingEvent, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_files: Vec<String>,
    #[serde(default)]
    pub prune: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineAddOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_identity_private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<MachineInstallOptions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallRuntimeTarget {
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallServiceMode {
    User,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallSource {
    Release,
    Git,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInstallOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_target: Option<InstallRuntimeTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_mode: Option<InstallServiceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InstallSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebugTickTask {
    PeerSync,
    Endpoints,
    Heartbeat,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// Liveness probe used by deploy reachability checks. Returns immediately
    /// with no side effects so callers can confirm the peer daemon is alive
    /// at decision time.
    Ping,
    Status,
    Doctor,
    DebugTick {
        task: DebugTickTask,
        repeat: u32,
    },
    MeshList,
    MeshStatus {
        network: String,
    },
    MeshJoin {
        token: String,
    },
    MeshReady {
        json: bool,
    },
    MeshCreate {
        network: String,
    },
    MeshInit {
        network: String,
    },
    MeshStart {
        network: String,
    },
    MeshStop {
        force: bool,
    },
    MeshDestroy {
        network: String,
    },
    MeshPeerPrepareDestroy {
        operation_id: String,
        network_id: NetworkId,
        coordinator_id: MachineId,
        expected_machine_ids: Vec<MachineId>,
    },
    MeshPeerCancelDestroy {
        operation_id: String,
    },
    MeshPeerExecuteDestroy {
        operation_id: String,
        network_id: NetworkId,
    },
    MachineList,
    MachineInit {
        target: String,
        network: String,
        install: MachineInstallOptions,
    },
    MachineAdd {
        targets: Vec<String>,
        options: MachineAddOptions,
    },
    MachineUpdate {
        ids: Vec<String>,
        version: String,
    },
    MachineActivate {
        target: String,
    },
    MachineDrain {
        target: String,
    },
    MachineStandby {
        target: String,
        force: bool,
    },
    MachineRemove {
        id: String,
        force: bool,
    },
    MachineRtt,
    MeshPeerRttSnapshot,
    MeshPeerPrepareUpdate {
        operation_id: String,
        version: String,
    },
    MeshPeerExecuteUpdate {
        operation_id: String,
        version: String,
    },
    MeshPeerRemoveMachine {
        operation_id: String,
        network_id: NetworkId,
        machine_id: MachineId,
    },
    MachineOperationList,
    MachineOperationGet {
        id: String,
    },
    MachineInviteCreate {
        ttl_secs: u64,
    },
    MachineInviteRevoke {
        invite_id: String,
    },
    MachineInviteList,
    MachineInviteImport {
        token: String,
    },
    MeshBootstrap {
        request: MeshBootstrapRequest,
    },
    MachineTransitionSelf {
        goal: MachineTransitionGoal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assigned_subnet: Option<Ipv4Net>,
        force: bool,
    },
    AcmeChallengeReady {
        hostname: String,
        token: String,
    },
    MeshSelfRecord,
    DeployPreview {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployApply {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployExport {
        namespace: String,
    },
    DeployNodeInspectNamespace {
        namespace: String,
        deploy_id: String,
    },
    DeployNodeStartCandidate {
        namespace: String,
        deploy_id: String,
        service: String,
        slot_id: String,
        instance_id: String,
        spec_json: String,
        volumes_json: String,
    },
    DeployNodeDrainInstance {
        namespace: String,
        deploy_id: String,
        instance_id: String,
    },
    DeployNodeRemoveInstance {
        namespace: String,
        deploy_id: String,
        instance_id: String,
    },
    RuntimeSubscribe,
    VolumeZfsInspect {
        namespace: String,
        volume: String,
        machine: Option<String>,
    },
    VolumeZfsSnapshot {
        namespace: String,
        volume: String,
        snapshot: String,
    },
    VolumeZfsSend {
        namespace: String,
        volume: String,
        snapshot: String,
        target_machine: String,
        from_snapshot: Option<String>,
    },
    VolumeZfsPeerSnapshot {
        namespace: String,
        volume: String,
        snapshot: String,
    },
    VolumeZfsPeerSnapshotGuid {
        namespace: String,
        volume: String,
        snapshot: String,
    },
    VolumeZfsPeerStartSend {
        namespace: String,
        volume: String,
        snapshot: String,
        target_machine: String,
        expected_guid: u64,
        from_snapshot: Option<String>,
        from_snapshot_guid: Option<u64>,
    },
    VolumeZfsTransferGet {
        id: String,
    },
    VolumeZfsTransferList,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DaemonPayload {
    Doctor(DoctorPayload),
    Status(StatusPayload),
    MachineList(MachineListPayload),
    MachineRtt(MachineRttPayload),
    MachineAdd(MachineAddPayload),
    MachineUpdate(MachineUpdatePayload),
    MachineRemove(MachineRemovePayload),
    MeshList(MeshListPayload),
    MeshStatus(MeshStatusPayload),
    MeshReady(MeshReadyPayload),
    MeshSelfRecord(MeshSelfRecordPayload),
    MachineInviteList(MachineInviteListPayload),
    MachineOperationList(MachineOperationListPayload),
    MachineOperation(MachineOperationPayload),
    DeployNamespaceSnapshot(DeployNamespaceSnapshotPayload),
    DeployCandidateStarted(DeployCandidateStartedPayload),
    VolumeZfsInspect(VolumeZfsInspectPayload),
    VolumeZfsSnapshot(VolumeZfsSnapshotPayload),
    VolumeZfsPeerSend(VolumeZfsPeerSendPayload),
    VolumeZfsTransfer(VolumeZfsTransferPayload),
    VolumeZfsTransferList(VolumeZfsTransferListPayload),
    RuntimeState(RuntimeStatePayload),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTable {
    Machine,
    Revision,
    Release,
    Instance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RuntimeRecord {
    Machine(MachineMembership),
    Revision(ServiceRevisionRecord),
    Release(ServiceReleaseRecord),
    Instance(InstanceStatusRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeWatchFrame {
    Snapshot {
        state: RoutingState,
    },
    Upsert {
        table: RuntimeTable,
        key: String,
        record: RuntimeRecord,
    },
    Remove {
        table: RuntimeTable,
        key: String,
        record: RuntimeRecord,
    },
    Error {
        code: String,
        message: String,
    },
    Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeStatePayload {
    pub state: RoutingState,
}

#[must_use]
pub fn machine_runtime_key(record: &MachineMembership) -> String {
    record.id.0.clone()
}

#[must_use]
pub fn revision_runtime_key(record: &ServiceRevisionRecord) -> String {
    format!(
        "{}:{}:{}",
        record.namespace, record.service, record.revision_hash
    )
}

#[must_use]
pub fn release_runtime_key(record: &ServiceReleaseRecord) -> String {
    format!("{}:{}", record.namespace, record.service)
}

#[must_use]
pub fn instance_runtime_key(record: &InstanceStatusRecord) -> String {
    record.instance_id.0.clone()
}

pub fn sort_routing_state(state: &mut RoutingState) {
    state.machines.sort_by_key(machine_runtime_key);
    state.revisions.sort_by_key(revision_runtime_key);
    state.releases.sort_by_key(release_runtime_key);
    state.instances.sort_by_key(instance_runtime_key);
}

#[must_use]
pub fn runtime_frame_from_event(event: RoutingEvent) -> RuntimeWatchFrame {
    match event {
        RoutingEvent::MachineAdded(record) | RoutingEvent::MachineUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: machine_runtime_key(&record),
                table: RuntimeTable::Machine,
                record: RuntimeRecord::Machine(record),
            }
        }
        RoutingEvent::MachineRemoved(record) => RuntimeWatchFrame::Remove {
            key: machine_runtime_key(&record),
            table: RuntimeTable::Machine,
            record: RuntimeRecord::Machine(record),
        },
        RoutingEvent::RevisionAdded(record) | RoutingEvent::RevisionUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: revision_runtime_key(&record),
                table: RuntimeTable::Revision,
                record: RuntimeRecord::Revision(record),
            }
        }
        RoutingEvent::RevisionRemoved(record) => RuntimeWatchFrame::Remove {
            key: revision_runtime_key(&record),
            table: RuntimeTable::Revision,
            record: RuntimeRecord::Revision(record),
        },
        RoutingEvent::ReleaseAdded(record) | RoutingEvent::ReleaseUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: release_runtime_key(&record),
                table: RuntimeTable::Release,
                record: RuntimeRecord::Release(record),
            }
        }
        RoutingEvent::ReleaseRemoved(record) => RuntimeWatchFrame::Remove {
            key: release_runtime_key(&record),
            table: RuntimeTable::Release,
            record: RuntimeRecord::Release(record),
        },
        RoutingEvent::InstanceAdded(record) | RoutingEvent::InstanceUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: instance_runtime_key(&record),
                table: RuntimeTable::Instance,
                record: RuntimeRecord::Instance(record),
            }
        }
        RoutingEvent::InstanceRemoved(record) => RuntimeWatchFrame::Remove {
            key: instance_runtime_key(&record),
            table: RuntimeTable::Instance,
            record: RuntimeRecord::Instance(record),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineListPayload {
    pub rows: Vec<MachineListRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineListRow {
    pub id: String,
    pub lifecycle: String,
    pub region: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_zone: Option<String>,
    pub overlay_ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRttPayload {
    pub rows: Vec<MachineRttRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRttRow {
    pub machine: String,
    pub peer: String,
    pub median_ms: f64,
    pub stddev_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub machine_id: String,
    pub public_key: PublicKey,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_lifecycle: Option<NetworkLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_machine_lifecycle: Option<MachineLifecycle>,
    pub mesh_phase: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_sync: Vec<EdgeSyncStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nats_assets: Vec<NatsAssetStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_plane: Vec<ControlPlaneStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSyncStatus {
    pub service: String,
    pub stream: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since_unix_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatsAssetStatus {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPlaneStatus {
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since_unix_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_failures: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorPayload {
    pub overall: DoctorOverall,
    pub local: DoctorLocal,
    pub peers: Vec<DoctorPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorOverall {
    pub lifecycle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorLocal {
    pub machine_id: String,
    pub network: String,
    pub network_lifecycle: String,
    pub machine_lifecycle: String,
    pub machine_role: String,
    pub config_subnet: Option<String>,
    pub record_subnet: Option<String>,
    pub runtime_running: bool,
    pub published_endpoints: Vec<String>,
    pub detected_endpoints: Vec<String>,
    pub endpoint_watch_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorPeer {
    pub machine_id: String,
    pub role: String,
    pub machine_role: String,
    pub blocking: bool,
    pub store_lifecycle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    pub wg_state: String,
    pub probe_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_median_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_stddev_ms: Option<f64>,
    pub cause_code: String,
    pub cause_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineAddPayload {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub awaiting_self_publication: Vec<MachineAwaitingSelfPublication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_preflight: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_join: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_self_record: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_ready: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_enable: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineAwaitingSelfPublication {
    pub target: String,
    pub joiner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRemovePayload {
    pub id: String,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineUpdatePayload {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated: Vec<MachineUpdateRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<MachineUpdateRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineUpdateRow {
    pub id: String,
    pub version: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshReadyPayload {
    pub ready: bool,
    pub phase: String,
    pub store_healthy: bool,
    pub sync_connected: bool,
    #[serde(default)]
    pub workload_subnet_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshListPayload {
    pub networks: Vec<MeshListEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshListEntry {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStatusPayload {
    pub network: String,
    pub overlay_ip: String,
    pub lifecycle: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineTransitionGoal {
    Activate,
    Drain,
    Standby,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSelfRecordPayload {
    pub record: MachineMembership,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInviteListPayload {
    pub invites: Vec<MachineInviteInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInviteInfo {
    pub invite_id: String,
    pub expires_at: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshBootstrapRequest {
    pub network_id: NetworkId,
    pub network_name: String,
    pub cluster_cidr: String,
    pub assigned_subnet: Ipv4Net,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_control_target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_peers: Vec<MachineMembership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineOperationListPayload {
    pub operations: Vec<MachineOperationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineOperationPayload {
    pub operation: MachineOperationInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineOperationInfo {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    pub status: String,
    pub stage: String,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_subnet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployNamespaceSnapshotPayload {
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployCandidateStartedPayload {
    pub status: InstanceStatusRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsInspectPayload {
    pub namespace: String,
    pub volume: String,
    pub machine_id: MachineId,
    pub dataset: String,
    pub mountpoint: String,
    pub quota: String,
    pub used_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<VolumeZfsSnapshotInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsSnapshotPayload {
    pub namespace: String,
    pub volume: String,
    pub machine_id: MachineId,
    pub dataset: String,
    pub snapshot: String,
    pub guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsSnapshotInfo {
    pub name: String,
    pub guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsPeerSendPayload {
    pub bytes_transferred: u64,
    pub snapshot_guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsTransferPayload {
    pub transfer: VolumeZfsTransferInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsTransferListPayload {
    pub transfers: Vec<VolumeZfsTransferInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsTransferInfo {
    pub id: String,
    pub namespace: String,
    pub volume: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub status: String,
    pub stage: String,
    pub snapshot_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_guid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_guid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_transferred: Option<u64>,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<DaemonPayload>,
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeRecord, RuntimeTable, RuntimeWatchFrame, instance_runtime_key, machine_runtime_key,
        release_runtime_key, revision_runtime_key, runtime_frame_from_event, sort_routing_state,
    };
    use ployz_types::model::{
        DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
        MachineLifecycle, MachineMembership, MachineTopology, OverlayIp, PublicKey, RoutingEvent,
        RoutingState, ServiceRelease, ServiceReleaseRecord, ServiceReleaseSlot,
        ServiceRevisionRecord, ServiceRoutingPolicy, SlotId,
    };
    use ployz_types::spec::Namespace;
    use std::collections::BTreeMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn sort_routing_state_orders_all_tables_by_runtime_key() {
        let mut state = RoutingState {
            machines: vec![machine_record("machine-b"), machine_record("machine-a")],
            revisions: vec![
                revision_record("prod", "web", "bbbb"),
                revision_record("prod", "api", "aaaa"),
            ],
            releases: vec![release_record("prod", "web"), release_record("dev", "api")],
            instances: vec![
                instance_record("instance-b", "prod", "web"),
                instance_record("instance-a", "prod", "api"),
            ],
        };

        sort_routing_state(&mut state);

        assert_eq!(
            state
                .machines
                .iter()
                .map(machine_runtime_key)
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
        assert_eq!(
            state
                .revisions
                .iter()
                .map(revision_runtime_key)
                .collect::<Vec<_>>(),
            ["prod:api:aaaa", "prod:web:bbbb"]
        );
        assert_eq!(
            state
                .releases
                .iter()
                .map(release_runtime_key)
                .collect::<Vec<_>>(),
            ["dev:api", "prod:web"]
        );
        assert_eq!(
            state
                .instances
                .iter()
                .map(instance_runtime_key)
                .collect::<Vec<_>>(),
            ["instance-a", "instance-b"]
        );
    }

    #[test]
    fn instance_events_map_to_idempotent_watch_frames() {
        let old = instance_record("instance-1", "prod", "api");
        let mut new = old.clone();
        new.ready = true;

        let added = runtime_frame_from_event(RoutingEvent::InstanceAdded(old.clone()));
        let updated = runtime_frame_from_event(RoutingEvent::InstanceUpdated {
            old: old.clone(),
            new: new.clone(),
        });
        let removed = runtime_frame_from_event(RoutingEvent::InstanceRemoved(new.clone()));

        assert_eq!(
            added,
            RuntimeWatchFrame::Upsert {
                table: RuntimeTable::Instance,
                key: String::from("instance-1"),
                record: RuntimeRecord::Instance(old.clone()),
            }
        );
        assert_eq!(
            updated,
            RuntimeWatchFrame::Upsert {
                table: RuntimeTable::Instance,
                key: String::from("instance-1"),
                record: RuntimeRecord::Instance(new.clone()),
            }
        );
        assert_eq!(
            removed,
            RuntimeWatchFrame::Remove {
                table: RuntimeTable::Instance,
                key: String::from("instance-1"),
                record: RuntimeRecord::Instance(new),
            }
        );
    }

    #[test]
    fn runtime_frame_keys_are_deterministic() {
        assert_eq!(
            machine_runtime_key(&machine_record("machine-1")),
            "machine-1"
        );
        assert_eq!(
            revision_runtime_key(&revision_record("prod", "api", "abcd")),
            "prod:api:abcd"
        );
        assert_eq!(
            release_runtime_key(&release_record("prod", "api")),
            "prod:api"
        );
        assert_eq!(
            instance_runtime_key(&instance_record("instance-1", "prod", "api")),
            "instance-1"
        );
    }

    #[test]
    fn runtime_watch_frame_serialization_roundtrips() {
        let frame = RuntimeWatchFrame::Upsert {
            table: RuntimeTable::Instance,
            key: String::from("instance-1"),
            record: RuntimeRecord::Instance(instance_record("instance-1", "prod", "api")),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime watch frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "upsert",
                "table": "instance",
                "key": "instance-1",
                "record": {
                    "instance_id": "instance-1",
                    "namespace": "prod",
                    "service": "api",
                    "slot_id": "slot-1",
                    "machine_id": "machine-1",
                    "revision_hash": "rev-1",
                    "deploy_id": "deploy-1",
                    "docker_container_id": "container-1",
                    "overlay_ip": "10.0.0.2",
                    "backend_ports": {
                        "http": 8080
                    },
                    "phase": "Ready",
                    "ready": false,
                    "drain_state": "None",
                    "error": null,
                    "started_at": 10,
                    "updated_at": 20
                }
            })
        );

        let decoded: RuntimeWatchFrame =
            serde_json::from_value(json).expect("deserialize runtime watch frame");
        assert_eq!(decoded, frame);
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: vec![String::from("127.0.0.1:51820")],
            lifecycle: MachineLifecycle::Active,
            role: ployz_types::model::MachineRole::StorageCandidate,
            created_at: 1,
            updated_at: 2,
            labels: BTreeMap::new(),
        }
    }

    fn revision_record(
        namespace: &str,
        service: &str,
        revision_hash: &str,
    ) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace(namespace.into()),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: String::from("{}"),
            created_by: MachineId(String::from("machine-1")),
            created_at: 1,
        }
    }

    fn release_record(namespace: &str, service: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: Namespace(namespace.into()),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: String::from("rev-1"),
                referenced_revision_hashes: vec![String::from("rev-1")],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: String::from("rev-1"),
                },
                slots: vec![ServiceReleaseSlot {
                    slot_id: SlotId(String::from("slot-1")),
                    machine_id: MachineId(String::from("machine-1")),
                    active_instance_id: InstanceId(String::from("instance-1")),
                    revision_hash: String::from("rev-1"),
                }],
                updated_by_deploy_id: DeployId(String::from("deploy-1")),
                updated_at: 1,
            },
        }
    }

    fn instance_record(id: &str, namespace: &str, service: &str) -> InstanceStatusRecord {
        let mut backend_ports = BTreeMap::new();
        backend_ports.insert(String::from("http"), 8080);
        InstanceStatusRecord {
            instance_id: InstanceId(id.into()),
            namespace: Namespace(namespace.into()),
            service: service.into(),
            slot_id: SlotId(String::from("slot-1")),
            machine_id: MachineId(String::from("machine-1")),
            revision_hash: String::from("rev-1"),
            deploy_id: DeployId(String::from("deploy-1")),
            docker_container_id: String::from("container-1"),
            overlay_ip: Some(Ipv4Addr::new(10, 0, 0, 2)),
            backend_ports,
            phase: InstancePhase::Ready,
            ready: false,
            drain_state: DrainState::None,
            error: None,
            started_at: 10,
            updated_at: 20,
        }
    }
}
