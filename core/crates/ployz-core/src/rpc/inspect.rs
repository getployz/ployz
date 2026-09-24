use std::{collections::BTreeMap, net::IpAddr};

use serde::{Deserialize, Serialize};

use crate::{
    AdvertisedEndpoint, InspectTelemetry, LocalMachinePhase, Machine, MachineId,
    MachineStorageObservation, RttObservation, TelemetryObservation, WireGuardPublicKey,
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InspectRequest {
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    pub public_ip_override: Option<IpAddr>,
    pub include_rtts: bool,
    /// Collect current local storage evidence for this inspection.
    pub include_storage: bool,
    /// Fresh telemetry to collect for this inspection.
    pub telemetry: InspectTelemetry,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct MachineDetails {
    pub id: MachineId,
    pub phase: LocalMachinePhase,
    #[serde(default)]
    pub machine: Option<Machine>,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub store_version: BTreeMap<String, i64>,
    #[serde(default)]
    pub rtts: Vec<RttObservation>,
    /// Stored Cloud Pairing is present. The Pairing Credential is not returned.
    #[serde(default)]
    pub cloud_paired: bool,
    /// Fresh telemetry requested only by targeted inspect.
    #[serde(default)]
    pub telemetry: Option<TelemetryObservation>,
    /// Current local storage evidence when the daemon advertises support.
    #[serde(default)]
    pub storage: Option<MachineStorageObservation>,
}
