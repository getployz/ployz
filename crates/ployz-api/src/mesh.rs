use ipnet::Ipv4Net;
use ployz_model::{MachineMembership, NetworkId};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSelfRecordPayload {
    pub record: MachineMembership,
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
