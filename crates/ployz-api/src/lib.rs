use ployz_types::model::{MachineId, MachineRecord};
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
    Heartbeat,
    Heal,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshReadyOutput {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootstrapWaitMode {
    Wait,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineRemoveMode {
    DisabledOnly,
    Force,
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
        output: MeshReadyOutput,
    },
    MeshCreate {
        network: String,
    },
    MeshInit {
        network: String,
    },
    MeshUp {
        network: String,
        bootstrap_wait: BootstrapWaitMode,
    },
    MeshDown,
    MeshDestroy {
        network: String,
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
    MachineRemove {
        id: String,
        mode: MachineRemoveMode,
    },
    MachineOperationList,
    MachineOperationGet {
        id: String,
    },
    MachineInviteCreate {
        ttl_secs: u64,
    },
    MachineInviteImport {
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
    MachineList(MachineListPayload),
    MachineAdd(MachineAddPayload),
    MachineRemove(MachineRemovePayload),
    MeshReady(MeshReadyPayload),
    MeshSelfRecord(MeshSelfRecordPayload),
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
    pub status: String,
    pub participation: String,
    pub liveness: String,
    pub overlay_ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    pub last_heartbeat: u64,
    pub heartbeat_display: String,
    pub created_at: u64,
    pub created_display: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineAwaitingSelfPublication {
    pub target: String,
    pub joiner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRemovePayload {
    pub id: String,
    pub mode: MachineRemoveMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshReadyPayload {
    pub ready: bool,
    pub phase: String,
    pub store_healthy: bool,
    pub sync_connected: bool,
    pub heartbeat_started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSelfRecordPayload {
    pub encoded: String,
    pub record: MachineRecord,
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
