//! Admit persisted Local Machine records before exposing lifecycle state.

use super::{LocalMachineBody, LocalMachineRecord, StoreError, WireGuardPrivateKey};
use ployz_core::{CloudPairing, MachineId, SelectedEndpoint};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Untrusted persisted input; only checked conversion admits a local record.
#[derive(Deserialize)]
pub(super) struct LocalMachineRecordWire {
    body: LocalMachineBody,
    wireguard_private_key: WireGuardPrivateKey,
    #[serde(default)]
    wireguard_mtu: Option<u32>,
    #[serde(default)]
    cloud_pairing: Option<CloudPairing>,
    #[serde(default)]
    selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
}

impl TryFrom<LocalMachineRecordWire> for LocalMachineRecord {
    type Error = StoreError;

    fn try_from(wire: LocalMachineRecordWire) -> Result<Self, Self::Error> {
        let mut record = Self::parse(wire.body, wire.wireguard_private_key)?;
        record.wireguard_mtu = wire.wireguard_mtu;
        record.cloud_pairing = wire.cloud_pairing;
        record.selected_endpoints = wire.selected_endpoints;
        Ok(record)
    }
}
