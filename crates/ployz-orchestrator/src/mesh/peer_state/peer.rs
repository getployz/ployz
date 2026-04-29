use ipnet::Ipv4Net;

use crate::model::{
    MachineId, MachineIdentity, MachineMembership, MachineObservation, OverlayIp, PublicKey,
};

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
    pub(crate) fn from_record(record: &MachineMembership) -> Self {
        Self {
            id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            endpoints: record.endpoints.clone(),
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineMembership) {
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.subnet = record.subnet;
        self.bridge_ip = record.bridge_ip;
        self.endpoints = record.endpoints.clone();
    }

    pub(crate) fn from_observation(observation: &MachineObservation) -> Self {
        Self {
            id: observation.identity.id.clone(),
            public_key: observation.identity.public_key.clone(),
            overlay_ip: observation.identity.overlay_ip,
            subnet: observation.subnet,
            bridge_ip: observation.bridge_ip,
            endpoints: observation.endpoints.clone(),
        }
    }

    pub(crate) fn update_from_observation(&mut self, observation: &MachineObservation) {
        self.public_key = observation.identity.public_key.clone();
        self.overlay_ip = observation.identity.overlay_ip;
        self.subnet = observation.subnet;
        self.bridge_ip = observation.bridge_ip;
        self.endpoints = observation.endpoints.clone();
    }

    pub(crate) fn to_observation(&self) -> MachineObservation {
        MachineObservation {
            identity: MachineIdentity {
                id: self.id.clone(),
                public_key: self.public_key.clone(),
                overlay_ip: self.overlay_ip,
            },
            subnet: self.subnet,
            bridge_ip: self.bridge_ip,
            endpoints: self.endpoints.clone(),
        }
    }

    pub(super) fn planned_endpoints(&self, selected_endpoint: Option<&str>) -> Vec<String> {
        let Some(selected_endpoint) = selected_endpoint else {
            return self.endpoints.clone();
        };

        if !self
            .endpoints
            .iter()
            .any(|endpoint| endpoint == selected_endpoint)
        {
            return self.endpoints.clone();
        }

        let mut planned = Vec::with_capacity(self.endpoints.len());
        planned.push(selected_endpoint.to_string());
        planned.extend(
            self.endpoints
                .iter()
                .filter(|endpoint| endpoint.as_str() != selected_endpoint)
                .cloned(),
        );
        planned
    }
}
