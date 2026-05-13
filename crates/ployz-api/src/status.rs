use ployz_model::{
    AuthorityNodePosture, ControlPlaneDataBucket, ControlPlaneLossImpact, MachineLifecycle,
    NetworkLifecycle, PublicKey,
};
use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_authority: Option<AuthorityNodePosture>,
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
    pub data_bucket: ControlPlaneDataBucket,
    pub loss_impact: ControlPlaneLossImpact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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
