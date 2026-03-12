use serde::{Deserialize, Serialize};

use crate::model::InstanceStatusRecord;

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
pub enum InstallSource {
    Release,
    Git,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallMode {
    Docker,
    HostExec,
    HostService,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInstallOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<InstallMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InstallSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    Status,
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
    MeshUp {
        network: String,
        skip_bootstrap_wait: bool,
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
    MachineDrain {
        id: String,
    },
    MachineRemove {
        id: String,
        force: bool,
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
    MachineLabel {
        id: String,
        set: Vec<(String, String)>,
        remove: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub code: String,
    pub message: String,
}

/// Deploy session wire protocol.
///
/// One TCP connection carries one session for one namespace.
/// The first frame must be `Open`; the server responds with `Opened`.
/// Subsequent frames are commands (client→server) and responses (server→client).
/// The session ends on `Close` or connection EOF, which releases the namespace lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeployFrame {
    // -- client → server --
    /// First frame: acquire namespace lock and return current instance snapshot.
    Open {
        namespace: String,
        deploy_id: String,
        coordinator_id: String,
    },
    /// Re-inspect live container state on this machine for the locked namespace.
    InspectNamespace,
    /// Start a new candidate instance.
    StartCandidate {
        service: String,
        slot_id: String,
        instance_id: String,
        deploy_id: String,
        spec_json: String,
    },
    /// Mark an instance as draining.
    DrainInstance { instance_id: String },
    /// Remove an instance.
    RemoveInstance { instance_id: String },
    /// Graceful session close (lock released).
    Close,

    // -- server → client --
    /// Response to `Open`: lock acquired, here are the current instances.
    Opened {
        instances: Vec<InstanceStatusRecord>,
    },
    /// Response to `InspectNamespace`.
    NamespaceSnapshot {
        instances: Vec<InstanceStatusRecord>,
    },
    /// Response to `StartCandidate`.
    CandidateStarted { status: Box<InstanceStatusRecord> },
    /// Generic success acknowledgement.
    Ack { message: String },
    /// Error response.
    Error { code: String, message: String },
}
