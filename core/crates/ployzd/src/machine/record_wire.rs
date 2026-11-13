//! Admit persisted Local Machine records before exposing lifecycle state.

use super::{
    LocalMachineBody, LocalMachineRecord, ManagementClientSlot, StoreError, WireGuardPrivateKey,
};
use crate::management::ManagementSecret;
use ployz_core::{
    MachineId, MachineVersion, ManagementClientLabel, PinnedAddresses, SelectedEndpoint,
};
use reqwest::Url;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Untrusted persisted input; only checked conversion admits a local record.
#[derive(Deserialize)]
pub(super) struct LocalMachineRecordWire {
    body: LocalMachineBody,
    wireguard_private_key: WireGuardPrivateKey,
    management_secret: ManagementSecret,
    /// Required: a record without it predates Management Clients, and reading it
    /// as "no slots" would silently forget every admitted key.
    management_clients: BTreeMap<ManagementClientLabel, ManagementClientSlot>,
    #[serde(default)]
    wireguard_mtu: Option<u32>,
    #[serde(default)]
    selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    /// 0.3.0-beta.7 wrote nothing when nothing was pinned; earlier daemons never did.
    #[serde(default)]
    pinned_addresses: Option<PinnedAddresses>,
    #[serde(default)]
    push_target: Option<Url>,
    #[serde(default)]
    device_slots_vetted: bool,
    /// Read once, to keep an awaiting record's peer filter. It no longer decides whether
    /// this Machine enforces: every Machine does.
    #[serde(default)]
    device_enforcement: bool,
    #[serde(default)]
    born_enforcing: bool,
    #[serde(default)]
    trusted_machines: Vec<ployz_core::authority::Member>,
    /// Read one by one in [`readable_passkeys`], so an entry a newer daemon wrote never
    /// stops the record from loading.
    #[serde(default)]
    passkeys: Vec<serde_json::Value>,
    #[serde(default)]
    membership_cluster: Option<ployz_core::SealingPublicKey>,
    #[serde(default)]
    enrollment: Option<ployz_core::EnrollRequest>,
    #[serde(default)]
    trusted_snapshot: Option<super::TrustedSnapshot>,
    #[serde(default)]
    activation: Option<ployz_core::OperationId>,
}

impl TryFrom<LocalMachineRecordWire> for LocalMachineRecord {
    type Error = StoreError;

    fn try_from(wire: LocalMachineRecordWire) -> Result<Self, Self::Error> {
        let mut record = Self::parse(wire.body, wire.wireguard_private_key)?;
        record.management_secret = wire.management_secret;
        record.management_clients = wire.management_clients;
        record.wireguard_mtu = wire.wireguard_mtu;
        record.selected_endpoints = wire.selected_endpoints;
        record.pinned_addresses = wire
            .pinned_addresses
            .unwrap_or_else(|| pins_when_none_stored(&record));
        record.push_target = wire.push_target;
        record.device_slots_vetted = wire.device_slots_vetted;
        record.filters_peers = wire.device_enforcement;
        record.born_enforcing = wire.born_enforcing;
        record.trusted_machines = wire.trusted_machines;
        record.passkeys = readable_passkeys(wire.passkeys);
        record.membership_cluster = wire.membership_cluster;
        record.enrollment = wire.enrollment;
        record.trusted_snapshot = wire.trusted_snapshot;
        record.activation = wire.activation;
        if record.activation.is_some() {
            record.filters_peers = true;
        }
        Ok(record)
    }
}

/// The store saves the running daemon's version into the record on every open,
/// so a record whose version predates pins never had a chance to store them. Its
/// addresses may have been chosen, and only pinning both keeps a choice.
fn pins_when_none_stored(record: &LocalMachineRecord) -> PinnedAddresses {
    let Some(machine) = record.machine() else {
        return PinnedAddresses::default();
    };
    let first_pinning = MachineVersion::parse("0.3.0-beta.7").expect("a release version");
    let predates_pins = MachineVersion::parse(&machine.runtime.daemon_version)
        .ok()
        .is_none_or(|version| version < first_pinning);
    PinnedAddresses {
        public_ip: predates_pins,
        advertised_endpoints: predates_pins,
    }
}

/// The trusted passkeys this daemon can read. One it can't, such as one with an algorithm a
/// newer daemon added, is logged and left out: it approves nothing here, and the next save
/// drops it.
fn readable_passkeys(
    entries: Vec<serde_json::Value>,
) -> Vec<ployz_core::authority::TrustedPasskey> {
    entries
        .into_iter()
        .filter_map(|entry| match serde_json::from_value(entry) {
            Ok(trusted) => Some(trusted),
            Err(error) => {
                tracing::warn!(%error, "skipped a trusted passkey this daemon can't read");
                None
            }
        })
        .collect()
}
