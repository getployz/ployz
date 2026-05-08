use crate::deploy::DeployOptions;
use crate::machine::{
    MachineAddOptions, MachineInstallOptions, MachineStorageAuthorityPeer,
    MachineStoragePromoteRequest, MachineTransitionGoal,
};
use crate::mesh::MeshBootstrapRequest;
use ipnet::Ipv4Net;
use ployz_types::model::{MachineId, NetworkId, StorageReplicaPolicy};
use serde::{Deserialize, Serialize};

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
    MachineStoragePromote {
        request: MachineStoragePromoteRequest,
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
    MachineStoragePromoteSelf {
        replicas: StorageReplicaPolicy,
        authority_peers: Vec<MachineStorageAuthorityPeer>,
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
