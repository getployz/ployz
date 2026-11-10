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
    /// The passkeys this Machine trusts to approve workflows. A daemon from before
    /// passkeys omits it, and an entry this build can't read is left out.
    #[serde(
        default,
        deserialize_with = "crate::authority::trusted_key::readable_passkeys"
    )]
    pub passkeys: Vec<crate::authority::TrustedPasskey>,
    /// The public keys of the Devices this Machine trusts, so a person's client can check a
    /// statement another Device signed. A daemon from before 0.4 omits it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<crate::DeviceKey>>")]
    pub devices: Vec<crate::DeviceKey>,
    /// How this Machine came to its Cluster. A daemon from before 0.3.15 omits it.
    #[serde(default)]
    pub founding: crate::Founding,
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
    /// Absent from a daemon that predates the Sealing Key, or before enrollment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub sealing: Option<crate::Sealing>,
    /// Present only when the request asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub token: Option<MachineToken>,
}
