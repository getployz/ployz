use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    AdvertisedEndpoint, InspectTelemetry, LocalMachinePhase, Machine, MachineId,
    MachineStorageObservation, MachineToken, ManagementClientLabel, PublicIpDiscovery,
    RttObservation, TelemetryObservation, WireGuardPublicKey,
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default)]
pub struct InspectRequest {
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    pub include_rtts: bool,
    /// Collect current local storage evidence for this inspection.
    pub include_storage: bool,
    /// Fresh telemetry to collect for this inspection.
    pub telemetry: InspectTelemetry,
    /// Opt-in: on an enrolled Machine it costs an external public-IP lookup
    /// that Inspect otherwise skips. An old daemon ignores it and returns no
    /// token, so the IP choice rides inside it and is never ignored alone.
    pub token: Option<TokenRequest>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default)]
pub struct TokenRequest {
    pub public_ip: PublicIpDiscovery,
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
    /// Labels of Management Client slots holding an accepted or pending key.
    #[serde(default)]
    pub management_clients: Vec<ManagementClientLabel>,
    /// Fresh telemetry requested only by targeted inspect.
    #[serde(default)]
    pub telemetry: Option<TelemetryObservation>,
    /// Current local storage evidence when the daemon advertises support.
    #[serde(default)]
    pub storage: Option<MachineStorageObservation>,
    /// Latest upgrade attempt, reconciled against its worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub upgrade: Option<crate::MachineUpgradeAttempt>,
    /// Present only when the request asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub token: Option<MachineToken>,
}
