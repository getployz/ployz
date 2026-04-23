use ipnet::Ipv4Net;

use crate::model::{MachineId, MachineRecord, OverlayIp, PublicKey};

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub(crate) id: MachineId,
    pub(crate) public_key: PublicKey,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) subnet: Option<Ipv4Net>,
    pub(crate) bridge_ip: Option<OverlayIp>,
    pub(crate) endpoints: Vec<String>,
}

impl PeerState {
    pub(crate) fn from_record(record: &MachineRecord) -> Self {
        Self {
            id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            endpoints: record.endpoints.clone(),
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineRecord) {
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.subnet = record.subnet;
        self.bridge_ip = record.bridge_ip;
        self.endpoints = record.endpoints.clone();
    }

    pub(super) fn planned_endpoints(&self) -> Vec<String> {
        self.endpoints.clone()
    }
}
