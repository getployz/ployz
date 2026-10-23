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
