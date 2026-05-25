use std::str::FromStr;

use iroh::EndpointAddr;
use iroh_tickets::endpoint::EndpointTicket;

use crate::IrohEndpointId;

use super::{PeerError, PeerProbeResult};

#[derive(Debug, Clone)]
pub struct PeerTicket {
    ticket: EndpointTicket,
}

impl PeerTicket {
    pub fn parse(value: &str) -> PeerProbeResult<Self> {
        import_ticket(value)
    }

    #[must_use]
    pub fn encoded(&self) -> String {
        self.ticket.to_string()
    }

    #[must_use]
    pub fn endpoint_id(&self) -> IrohEndpointId {
        IrohEndpointId::parse(self.ticket.endpoint_addr().id.to_string())
            .expect("iroh endpoint ids are non-empty")
    }

    #[must_use]
    pub fn endpoint_addr(&self) -> &EndpointAddr {
        self.ticket.endpoint_addr()
    }

    #[must_use]
    pub fn path(&self) -> PeerTicketPath {
        let has_direct_ip = self.ticket.endpoint_addr().ip_addrs().next().is_some();
        let has_relay = self.ticket.endpoint_addr().relay_urls().next().is_some();

        match (has_direct_ip, has_relay) {
            (true, true) => PeerTicketPath::DirectOrRelay,
            (true, false) => PeerTicketPath::DirectOnly,
            (false, true) => PeerTicketPath::RelayOnly,
            (false, false) => PeerTicketPath::DiscoveryOnly,
        }
    }

    #[must_use]
    pub fn redacted(&self) -> String {
        redact_ascii(&self.encoded(), 10, 8, "<redacted-ticket>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTicketPath {
    DirectOnly,
    RelayOnly,
    DirectOrRelay,
    DiscoveryOnly,
}

#[must_use]
pub fn issue_ticket(endpoint_addr: EndpointAddr) -> PeerTicket {
    PeerTicket {
        ticket: EndpointTicket::new(endpoint_addr),
    }
}

pub fn import_ticket(value: &str) -> PeerProbeResult<PeerTicket> {
    EndpointTicket::from_str(value)
        .map(|ticket| PeerTicket { ticket })
        .map_err(|_| PeerError::MalformedTicket)
}

fn redact_ascii(value: &str, prefix: usize, suffix: usize, fallback: &str) -> String {
    if value.len() <= prefix + suffix {
        return fallback.to_string();
    }

    format!("{}...{}", &value[..prefix], &value[value.len() - suffix..])
}
