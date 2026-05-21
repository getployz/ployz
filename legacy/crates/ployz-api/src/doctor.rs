use serde::{Deserialize, Serialize};

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
