use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    Status,
    MeshList,
    MeshStatus { network: String },
    MeshJoin { token: String },
    MeshCreate { network: String },
    MeshInit { network: String },
    MeshUp { network: String },
    MeshDown,
    MeshDestroy { network: String },
    MachineList,
    MachineInit { target: String, network: String },
    MachineAdd { target: String },
    MachineInviteCreate { ttl_secs: u64 },
    MachineInviteImport { token: String },
    MeshSelfRecord,
    MeshAccept { response: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub code: String,
    pub message: String,
}
