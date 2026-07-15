//! Live machine and machine-role testimony contracts.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::ids::MachineId;

/// Machine-owned endpoint facts reported with machine facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineEndpointObservation {
    pub machine_id: MachineId,
    pub control_endpoints: Vec<IpAddr>,
    pub mesh_endpoints: Vec<SocketAddr>,
}

/// Gateway role status fact reported by the gateway process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusObservation {
    pub machine_id: MachineId,
    pub listen_addr: SocketAddr,
    pub serving: GatewayServingStatus,
    pub route_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayServingStatus {
    Current,
    LastKnownGood,
    Unavailable,
}
