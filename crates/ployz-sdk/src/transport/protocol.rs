use clap::ValueEnum;
use derive_more::Display;
use serde::{Deserialize, Serialize};

use crate::model::InstanceStatusRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Display)]
#[serde(rename_all = "snake_case")]
pub enum DeployManifestFormat {
    #[display("auto")]
    Auto,
    #[display("compose")]
    Compose,
    #[display("service")]
    Service,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployManifestInput {
    pub format: DeployManifestFormat,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_files: Vec<String>,
    #[serde(default)]
    pub prune: bool,
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
    },
    MachineAdd {
        targets: Vec<String>,
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
        namespace: String,
        manifest_json: String,
        options: DeployOptions,
    },
    DeployApply {
        namespace: String,
        manifest_json: String,
        options: DeployOptions,
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
