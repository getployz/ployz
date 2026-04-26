use ipnet::Ipv4Net;
use ployz_types::model::{
    InstanceStatusRecord, MachineId, MachineLifecycle, MachineRecord, NetworkId, NetworkLifecycle,
};
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
pub enum ResourceKey {
    /// Subnet reservation: missing peers are acceptable only if the strict
    /// majority quorum still allows the reservation.
    Subnet(Ipv4Net),
    /// Namespace deploy lock: all required deploy participants must be
    /// reachable and session-locked.
    DeployNamespace(String),
    /// ACME hostname issuance lock: peer inventory must be available;
    /// unreachable known peers abstain, explicit denials veto.
    CertIssuance(String),
    /// ACME account creation lock: peer inventory must be available;
    /// unreachable known peers abstain, explicit denials veto.
    AcmeAccount(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VoteConflict {
    HeldBy {
        holder: MachineId,
        reservation_id: String,
    },
    AlreadyCommitted,
    OwnerMismatch,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Vote {
    Allow,
    Deny(VoteConflict),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoordOutcome {
    SubnetClaimed { subnet: Ipv4Net, owner: MachineId },
    SubnetReleased { subnet: Ipv4Net, owner: MachineId },
    DeployCommitted { deploy_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoordOp {
    Prepare {
        id: String,
        key: ResourceKey,
        owner: MachineId,
        nonce: String,
        ttl_secs: u64,
    },
    Renew {
        id: String,
        key: ResourceKey,
        nonce: String,
        ttl_secs: u64,
    },
    Commit {
        id: String,
        key: ResourceKey,
        nonce: String,
        outcome: CoordOutcome,
    },
    Release {
        id: String,
        key: ResourceKey,
        nonce: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
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
        allow_disconnected_bootstrap: bool,
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
    Coord {
        op: CoordOp,
    },
    AcmeChallengeReady {
        hostname: String,
        token: String,
    },
    MeshSelfRecord,
    MeshAccept {
        response: String,
    },
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DaemonPayload {
    Doctor(DoctorPayload),
    Status(StatusPayload),
    MachineList(MachineListPayload),
    MachineRtt(MachineRttPayload),
    MachineAdd(MachineAddPayload),
    MachineRemove(MachineRemovePayload),
    MeshList(MeshListPayload),
    MeshStatus(MeshStatusPayload),
    MeshReady(MeshReadyPayload),
    MeshSelfRecord(MeshSelfRecordPayload),
    MachineInviteList(MachineInviteListPayload),
    MachineOperationList(MachineOperationListPayload),
    MachineOperation(MachineOperationPayload),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_lifecycle: Option<NetworkLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_machine_lifecycle: Option<MachineLifecycle>,
    pub mesh_phase: String,
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
    pub blocking: bool,
    pub store_lifecycle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    pub wg_state: String,
    pub probe_state: String,
    pub corrosion_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrosion_actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrosion_timestamp: Option<u64>,
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
    pub encoded: String,
    pub record: MachineRecord,
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
    pub bootstrap_peers: Vec<MachineRecord>,
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
pub struct DaemonResponse {
    pub ok: bool,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<DaemonPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeployFrame {
    Open {
        namespace: String,
        deploy_id: String,
        coordinator_id: String,
    },
    InspectNamespace,
    StartCandidate {
        service: String,
        slot_id: String,
        instance_id: String,
        spec_json: String,
    },
    DrainInstance {
        instance_id: String,
    },
    RemoveInstance {
        instance_id: String,
    },
    Close,
    Opened {
        instances: Vec<InstanceStatusRecord>,
    },
    NamespaceSnapshot {
        instances: Vec<InstanceStatusRecord>,
    },
    CandidateStarted {
        status: Box<InstanceStatusRecord>,
    },
    Ack {
        message: String,
    },
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::DeployFrame;

    #[test]
    fn start_candidate_roundtrip_is_session_scoped() {
        let frame = DeployFrame::StartCandidate {
            service: String::from("api"),
            slot_id: String::from("slot-1"),
            instance_id: String::from("inst-1"),
            spec_json: String::from("{\"name\":\"api\"}"),
        };

        let json = serde_json::to_value(&frame).expect("serialize frame");
        let start_candidate = json
            .get("StartCandidate")
            .expect("enum variant payload should exist");

        assert!(start_candidate.get("deploy_id").is_none());

        let decoded: DeployFrame = serde_json::from_value(json).expect("deserialize frame");
        let DeployFrame::StartCandidate {
            service,
            slot_id,
            instance_id,
            spec_json,
        } = decoded
        else {
            panic!("unexpected frame");
        };
        assert_eq!(service, "api");
        assert_eq!(slot_id, "slot-1");
        assert_eq!(instance_id, "inst-1");
        assert_eq!(spec_json, "{\"name\":\"api\"}");
    }
}
