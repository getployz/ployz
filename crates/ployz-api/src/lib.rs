use ipnet::Ipv4Net;
use ployz_types::model::{
    AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord, InstanceStatusRecord,
    MachineId, MachineLifecycle, MachineMembership, NetworkId, NetworkLifecycle, PublicKey,
    RoutingEvent, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
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
    AcmeHttp01Status {
        hostname: String,
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
    AcmeHttp01Status(AcmeHttp01StatusPayload),
    DeployNamespaceSnapshot(DeployNamespaceSnapshotPayload),
    DeployCandidateStarted(DeployCandidateStartedPayload),
    VolumeZfsInspect(VolumeZfsInspectPayload),
    VolumeZfsSnapshot(VolumeZfsSnapshotPayload),
    VolumeZfsPeerSend(VolumeZfsPeerSendPayload),
    VolumeZfsTransfer(VolumeZfsTransferPayload),
    VolumeZfsTransferList(VolumeZfsTransferListPayload),
    RuntimeState(RuntimeStatePayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeHttp01StatusPayload {
    pub hostname: String,
    pub certificate: Option<CertificateRecord>,
    pub challenges: Vec<AcmeHttp01ChallengeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeHttp01ChallengeStatus {
    pub challenge: AcmeChallengeRecord,
    pub readiness: Vec<AcmeChallengeReadinessRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCollection {
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
        collection: RuntimeCollection,
        key: String,
        record: RuntimeRecord,
    },
    Remove {
        collection: RuntimeCollection,
        key: String,
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
                collection: RuntimeCollection::Machine,
                record: RuntimeRecord::Machine(record),
            }
        }
        RoutingEvent::MachineRemoved { id } => RuntimeWatchFrame::Remove {
            key: id.0,
            collection: RuntimeCollection::Machine,
        },
        RoutingEvent::RevisionAdded(record) | RoutingEvent::RevisionUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: revision_runtime_key(&record),
                collection: RuntimeCollection::Revision,
                record: RuntimeRecord::Revision(record),
            }
        }
        RoutingEvent::RevisionRemoved {
            namespace,
            service,
            revision_hash,
        } => RuntimeWatchFrame::Remove {
            key: format!("{namespace}:{service}:{revision_hash}"),
            collection: RuntimeCollection::Revision,
        },
        RoutingEvent::ReleaseAdded(record) | RoutingEvent::ReleaseUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: release_runtime_key(&record),
                collection: RuntimeCollection::Release,
                record: RuntimeRecord::Release(record),
            }
        }
        RoutingEvent::ReleaseRemoved { namespace, service } => RuntimeWatchFrame::Remove {
            key: format!("{namespace}:{service}"),
            collection: RuntimeCollection::Release,
        },
        RoutingEvent::InstanceAdded(record) | RoutingEvent::InstanceUpdated { new: record, .. } => {
            RuntimeWatchFrame::Upsert {
                key: instance_runtime_key(&record),
                collection: RuntimeCollection::Instance,
                record: RuntimeRecord::Instance(record),
            }
        }
        RoutingEvent::InstanceRemoved { instance_id } => RuntimeWatchFrame::Remove {
            key: instance_id.0,
            collection: RuntimeCollection::Instance,
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
    #[serde(flatten)]
    pub state: EdgeSyncHealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EdgeSyncHealthState {
    Healthy {
        failures_total: u64,
    },
    Stale {
        stale_since_unix_secs: u64,
        failures_total: u64,
    },
    Unknown {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failures_total: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatsAssetStatus {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(flatten)]
    pub state: NatsAssetHealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NatsAssetHealthState {
    Healthy(NatsAssetReplicaStatus),
    Stale(NatsAssetReplicaStatus),
    Unknown { error: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatsAssetReplicaStatus {
    pub replicas: usize,
    pub current_replicas: usize,
    pub offline_replicas: usize,
    pub max_lag: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPlaneStatus {
    pub component: String,
    #[serde(flatten)]
    pub state: ControlPlaneHealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ControlPlaneHealthState {
    Healthy,
    Stale {
        stale_since_unix_secs: u64,
        consecutive_failures: u64,
        error: String,
    },
    Unknown {
        error: String,
    },
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
    pub storage: bool,
    pub storage_participation: String,
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
    pub storage: bool,
    pub storage_participation: String,
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
        ControlPlaneHealthState, ControlPlaneStatus, DaemonPayload, DaemonResponse,
        EdgeSyncHealthState, EdgeSyncStatus, MachineOperationInfo, MachineOperationPayload,
        MachineUpdatePayload, MachineUpdateRow, RuntimeCollection, RuntimeRecord,
        RuntimeWatchFrame, StatusPayload, instance_runtime_key, machine_runtime_key,
        release_runtime_key, revision_runtime_key, runtime_frame_from_event, sort_routing_state,
    };
    use ployz_types::model::{
        DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
        MachineLifecycle, MachineMembership, MachineTopology, NetworkLifecycle, OverlayIp,
        PublicKey, RoutingEvent, RoutingState, ServiceRelease, ServiceReleaseRecord,
        ServiceReleaseSlot, ServiceRevisionRecord, ServiceRoutingPolicy, SlotId,
        StorageParticipation,
    };
    use ployz_types::spec::Namespace;
    use std::collections::BTreeMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn sort_routing_state_orders_all_collections_by_runtime_key() {
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
        let removed = runtime_frame_from_event(RoutingEvent::InstanceRemoved {
            instance_id: new.instance_id.clone(),
        });

        assert_eq!(
            added,
            RuntimeWatchFrame::Upsert {
                collection: RuntimeCollection::Instance,
                key: String::from("instance-1"),
                record: RuntimeRecord::Instance(old.clone()),
            }
        );
        assert_eq!(
            updated,
            RuntimeWatchFrame::Upsert {
                collection: RuntimeCollection::Instance,
                key: String::from("instance-1"),
                record: RuntimeRecord::Instance(new.clone()),
            }
        );
        assert_eq!(
            removed,
            RuntimeWatchFrame::Remove {
                collection: RuntimeCollection::Instance,
                key: String::from("instance-1"),
            }
        );
    }

    #[test]
    fn routing_events_for_all_collections_map_to_runtime_watch_frames() {
        let old_machine = machine_record("machine-1");
        let mut new_machine = old_machine.clone();
        new_machine.lifecycle = MachineLifecycle::Draining;
        let old_revision = revision_record("prod", "api", "rev-1");
        let mut new_revision = old_revision.clone();
        new_revision.spec_json = r#"{"image":"api:2"}"#.into();
        let old_release = release_record("prod", "api");
        let mut new_release = old_release.clone();
        new_release.release.primary_revision_hash = "rev-2".into();

        let cases = [
            (
                runtime_frame_from_event(RoutingEvent::MachineUpdated {
                    old: old_machine.clone(),
                    new: new_machine.clone(),
                }),
                RuntimeWatchFrame::Upsert {
                    collection: RuntimeCollection::Machine,
                    key: String::from("machine-1"),
                    record: RuntimeRecord::Machine(new_machine.clone()),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::MachineRemoved {
                    id: new_machine.id.clone(),
                }),
                RuntimeWatchFrame::Remove {
                    collection: RuntimeCollection::Machine,
                    key: String::from("machine-1"),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::RevisionUpdated {
                    old: old_revision.clone(),
                    new: new_revision.clone(),
                }),
                RuntimeWatchFrame::Upsert {
                    collection: RuntimeCollection::Revision,
                    key: String::from("prod:api:rev-1"),
                    record: RuntimeRecord::Revision(new_revision.clone()),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::RevisionRemoved {
                    namespace: new_revision.namespace.clone(),
                    service: new_revision.service.clone(),
                    revision_hash: new_revision.revision_hash.clone(),
                }),
                RuntimeWatchFrame::Remove {
                    collection: RuntimeCollection::Revision,
                    key: String::from("prod:api:rev-1"),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::ReleaseUpdated {
                    old: old_release.clone(),
                    new: new_release.clone(),
                }),
                RuntimeWatchFrame::Upsert {
                    collection: RuntimeCollection::Release,
                    key: String::from("prod:api"),
                    record: RuntimeRecord::Release(new_release.clone()),
                },
            ),
            (
                runtime_frame_from_event(RoutingEvent::ReleaseRemoved {
                    namespace: new_release.namespace.clone(),
                    service: new_release.service.clone(),
                }),
                RuntimeWatchFrame::Remove {
                    collection: RuntimeCollection::Release,
                    key: String::from("prod:api"),
                },
            ),
        ];

        for (actual, expected) in cases {
            assert_eq!(actual, expected);
        }
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
            collection: RuntimeCollection::Instance,
            key: String::from("instance-1"),
            record: RuntimeRecord::Instance(instance_record("instance-1", "prod", "api")),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime watch frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "upsert",
                "collection": "instance",
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

    #[test]
    fn runtime_watch_remove_frame_serialization_is_key_only() {
        let frame = RuntimeWatchFrame::Remove {
            collection: RuntimeCollection::Release,
            key: String::from("prod:api"),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime remove frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "remove",
                "collection": "release",
                "key": "prod:api"
            })
        );

        let decoded: RuntimeWatchFrame =
            serde_json::from_value(json).expect("deserialize runtime remove frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn runtime_watch_error_frame_serialization_roundtrips() {
        let frame = RuntimeWatchFrame::Error {
            code: String::from("RUNTIME_SUBSCRIPTION_FAILED"),
            message: String::from("routing event 'event-1' ack receiver closed"),
        };

        let json = serde_json::to_value(&frame).expect("serialize runtime error frame");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "error",
                "code": "RUNTIME_SUBSCRIPTION_FAILED",
                "message": "routing event 'event-1' ack receiver closed"
            })
        );

        let decoded: RuntimeWatchFrame =
            serde_json::from_value(json).expect("deserialize runtime error frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn daemon_error_response_preserves_structured_payload() {
        let response = DaemonResponse {
            ok: false,
            code: String::from("MACHINE_UPDATE_FAILED"),
            message: String::from("machine 'peer-1' update failed: refused"),
            payload: Some(DaemonPayload::MachineUpdate(MachineUpdatePayload {
                operation_id: String::from("update-1"),
                updated: Vec::new(),
                failed: vec![MachineUpdateRow {
                    id: String::from("peer-1"),
                    version: String::from("0.5.6"),
                    message: String::from("refused"),
                }],
            })),
        };

        let json = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": false,
                "code": "MACHINE_UPDATE_FAILED",
                "message": "machine 'peer-1' update failed: refused",
                "payload": {
                    "kind": "machine-update",
                    "operation_id": "update-1",
                    "failed": [{
                        "id": "peer-1",
                        "version": "0.5.6",
                        "message": "refused"
                    }]
                }
            })
        );
        let decoded: DaemonResponse = serde_json::from_value(json).expect("deserialize response");
        assert!(!decoded.ok);
        assert_eq!(decoded.code, "MACHINE_UPDATE_FAILED");
        let Some(DaemonPayload::MachineUpdate(payload)) = decoded.payload else {
            panic!("expected machine update payload");
        };
        assert_eq!(payload.operation_id, "update-1");
        let [failed] = payload.failed.as_slice() else {
            panic!("expected one failed row");
        };
        assert_eq!(failed.id, "peer-1");
        assert_eq!(failed.message, "refused");
    }

    #[test]
    fn daemon_operation_response_preserves_structured_failure_status() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("operation details"),
            payload: Some(DaemonPayload::MachineOperation(MachineOperationPayload {
                operation: MachineOperationInfo {
                    id: String::from("machine-add-1"),
                    kind: String::from("add"),
                    network_name: Some(String::from("alpha")),
                    targets: vec![String::from("host-a")],
                    status: String::from("interrupted"),
                    stage: String::from("bootstrap"),
                    started_at: 10,
                    updated_at: 20,
                    last_error: Some(String::from("daemon restarted before operation completed")),
                    machine_id: Some(MachineId("machine-a".into())),
                    invite_id: Some(String::from("invite-1")),
                    allocated_subnet: Some(String::from("10.210.1.0/24")),
                },
            })),
        };

        let json = serde_json::to_value(&response).expect("serialize operation response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": true,
                "code": "OK",
                "message": "operation details",
                "payload": {
                    "kind": "machine-operation",
                    "operation": {
                        "id": "machine-add-1",
                        "kind": "add",
                        "network_name": "alpha",
                        "targets": ["host-a"],
                        "status": "interrupted",
                        "stage": "bootstrap",
                        "started_at": 10,
                        "updated_at": 20,
                        "last_error": "daemon restarted before operation completed",
                        "machine_id": "machine-a",
                        "invite_id": "invite-1",
                        "allocated_subnet": "10.210.1.0/24"
                    }
                }
            })
        );

        let decoded: DaemonResponse =
            serde_json::from_value(json).expect("deserialize operation response");
        let Some(DaemonPayload::MachineOperation(payload)) = decoded.payload else {
            panic!("expected machine operation payload");
        };
        assert_eq!(payload.operation.status, "interrupted");
        assert_eq!(
            payload.operation.last_error.as_deref(),
            Some("daemon restarted before operation completed")
        );
    }

    #[test]
    fn daemon_status_response_preserves_edge_and_control_plane_uncertainty() {
        let response = DaemonResponse {
            ok: true,
            code: String::from("OK"),
            message: String::from("status"),
            payload: Some(DaemonPayload::Status(StatusPayload {
                machine_id: String::from("founder"),
                public_key: PublicKey([1; 32]),
                version: String::from("0.5.5"),
                network: Some(String::from("alpha")),
                overlay_ip: Some(String::from("fd00::1")),
                network_lifecycle: Some(NetworkLifecycle::Running),
                local_machine_lifecycle: Some(MachineLifecycle::Active),
                mesh_phase: String::from("Running"),
                edge_sync: vec![EdgeSyncStatus {
                    service: String::from("gateway"),
                    stream: String::from("routing"),
                    state: EdgeSyncHealthState::Stale {
                        stale_since_unix_secs: 1_777_646_000,
                        failures_total: 7,
                    },
                }],
                nats_assets: Vec::new(),
                control_plane: vec![ControlPlaneStatus {
                    component: String::from("mesh_peer_sync"),
                    state: ControlPlaneHealthState::Unknown {
                        error: String::from("machine subscription closed"),
                    },
                }],
            })),
        };

        let json = serde_json::to_value(&response).expect("serialize status response");

        assert_eq!(
            json,
            serde_json::json!({
                "ok": true,
                "code": "OK",
                "message": "status",
                "payload": {
                    "kind": "status",
                    "machine_id": "founder",
                    "public_key": PublicKey([1; 32]),
                    "version": "0.5.5",
                    "network": "alpha",
                    "overlay_ip": "fd00::1",
                    "network_lifecycle": "Running",
                    "local_machine_lifecycle": "Active",
                    "mesh_phase": "Running",
                    "edge_sync": [{
                        "service": "gateway",
                        "stream": "routing",
                        "state": "stale",
                        "stale_since_unix_secs": 1777646000u64,
                        "failures_total": 7u64
                    }],
                    "control_plane": [{
                        "component": "mesh_peer_sync",
                        "state": "unknown",
                        "error": "machine subscription closed"
                    }]
                }
            })
        );

        let decoded: DaemonResponse =
            serde_json::from_value(json).expect("deserialize status response");
        let Some(DaemonPayload::Status(payload)) = decoded.payload else {
            panic!("expected status payload");
        };
        let [edge] = payload.edge_sync.as_slice() else {
            panic!("expected one edge sync row");
        };
        match &edge.state {
            EdgeSyncHealthState::Stale {
                stale_since_unix_secs,
                failures_total,
            } => {
                assert_eq!(*stale_since_unix_secs, 1_777_646_000);
                assert_eq!(*failures_total, 7);
            }
            other => panic!("expected stale edge sync state, got {other:?}"),
        }
        let [control] = payload.control_plane.as_slice() else {
            panic!("expected one control-plane row");
        };
        match &control.state {
            ControlPlaneHealthState::Unknown { error } => {
                assert_eq!(error, "machine subscription closed");
            }
            other => panic!("expected unknown control-plane state, got {other:?}"),
        }
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: vec![String::from("127.0.0.1:51820")],
            lifecycle: MachineLifecycle::Active,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
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
