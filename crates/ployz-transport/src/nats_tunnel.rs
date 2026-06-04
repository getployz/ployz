//! Local loopback-to-iroh byte tunnel for NATS client traffic.

use std::net::SocketAddr;

use crate::iroh_endpoint::{IrohEndpoint, IrohPath};

pub const NATS_TUNNEL_ALPN: &str = "plz/nats-tunnel/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsTunnelConfig {
    Core(CoreNatsTunnelConfig),
    Edge(EdgeNatsTunnelConfig),
}

impl NatsTunnelConfig {
    #[must_use]
    pub const fn local_socket(&self) -> SocketAddr {
        match self {
            Self::Core(core) => core.nats_socket,
            Self::Edge(edge) => edge.local_listen,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreNatsTunnelConfig {
    pub nats_socket: SocketAddr,
}

impl CoreNatsTunnelConfig {
    #[must_use]
    pub const fn new(nats_socket: SocketAddr) -> Self {
        Self { nats_socket }
    }

    #[must_use]
    pub const fn protocol(&self) -> &'static str {
        NATS_TUNNEL_ALPN
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeNatsTunnelConfig {
    pub local_listen: SocketAddr,
    pub core_endpoint: IrohEndpoint,
}

impl EdgeNatsTunnelConfig {
    #[must_use]
    pub fn new(local_listen: SocketAddr, core_endpoint: IrohEndpoint) -> Self {
        Self {
            local_listen,
            core_endpoint,
        }
    }

    #[must_use]
    pub const fn protocol(&self) -> &'static str {
        NATS_TUNNEL_ALPN
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsTunnelStatus {
    Connected { path: IrohPath },
    Reconnecting { last_error: String },
    Down { reason: String },
}

impl NatsTunnelStatus {
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        match self {
            Self::Connected { .. } => true,
            Self::Reconnecting { .. } | Self::Down { .. } => false,
        }
    }
}
