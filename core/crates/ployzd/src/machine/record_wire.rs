//! Admit persisted Local Machine records before exposing lifecycle state.

use super::{
    LocalMachineBody, LocalMachineRecord, ManagementClientSlot, StoreError, WireGuardPrivateKey,
};
use crate::management::ManagementSecret;
use ployz_core::{MachineId, ManagementClientLabel, SelectedEndpoint};
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
}

impl TryFrom<LocalMachineRecordWire> for LocalMachineRecord {
    type Error = StoreError;

    fn try_from(wire: LocalMachineRecordWire) -> Result<Self, Self::Error> {
        let mut record = Self::parse(wire.body, wire.wireguard_private_key)?;
        record.management_secret = wire.management_secret;
        record.management_clients = wire.management_clients;
        record.wireguard_mtu = wire.wireguard_mtu;
        record.selected_endpoints = wire.selected_endpoints;
        Ok(record)
    }
}
