use std::{collections::BTreeMap, net::IpAddr};

use serde::{Deserialize, Serialize};

use crate::{
    AdvertisedEndpoint, InspectTelemetry, LocalMachinePhase, Machine, MachineId,
    MachineStorageObservation, RttObservation, TelemetryObservation, WireGuardPublicKey,
};

use super::default_wireguard_port;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectRequest {
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub public_ip_override: Option<IpAddr>,
    #[serde(default = "default_wireguard_port")]
    pub wireguard_port: u16,
    #[serde(default)]
    pub include_rtts: bool,
    /// Collect current local storage evidence for this inspection.
    #[serde(default)]
    pub include_storage: bool,
    /// Fresh telemetry to collect for this inspection.
    #[serde(default)]
    pub telemetry: InspectTelemetry,
}

impl Default for InspectRequest {
    fn default() -> Self {
        Self {
            advertised_endpoints: Vec::new(),
            public_ip_override: None,
            wireguard_port: default_wireguard_port(),
            include_rtts: false,
            include_storage: false,
            telemetry: InspectTelemetry::None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineDetails {
    pub id: MachineId,
    pub phase: LocalMachinePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<Machine>,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub store_version: BTreeMap<String, i64>,
    #[serde(default)]
    pub rtts: Vec<RttObservation>,
    /// Stored Cloud Pairing is present. The Pairing Credential is not returned.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cloud_paired: bool,
    /// Fresh telemetry requested only by targeted inspect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryObservation>,
    /// Current local storage evidence when the daemon advertises support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<MachineStorageObservation>,
}
