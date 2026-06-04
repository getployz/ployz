//! iroh endpoint identity and addressing.

use std::net::SocketAddr;

use ployz_core::ids::NodeId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrohEndpoint {
    pub node_id: NodeId,
    pub public_key: String,
    pub direct_addresses: Vec<SocketAddr>,
    pub relay_url: Option<String>,
}

impl IrohEndpoint {
    #[must_use]
    pub fn new(node_id: NodeId, public_key: impl Into<String>) -> Self {
        Self {
            node_id,
            public_key: public_key.into(),
            direct_addresses: Vec::new(),
            relay_url: None,
        }
    }

    #[must_use]
    pub fn with_direct_address(mut self, address: SocketAddr) -> Self {
        self.direct_addresses.push(address);
        self
    }

    #[must_use]
    pub fn with_relay_url(mut self, relay_url: impl Into<String>) -> Self {
        self.relay_url = Some(relay_url.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrohPath {
    Direct {
        remote_address: SocketAddr,
        latency_ms: u32,
    },
    Relayed {
        relay_url: String,
        latency_ms: u32,
    },
}
