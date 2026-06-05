//! Role-specific daemon configuration.

use ployz_core::ids::NodeId;
use ployz_nats::connect::NatsClientEndpoint;

use crate::iroh_tunnel::PreparedTunnelService;
use crate::nats_process::NatsServerRuntime;
use crate::role::{DaemonProcessRole, TunnelSide};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonProcessConfig {
    Control(ControlProcessConfig),
    Node(NodeProcessConfig),
    Gateway(GatewayProcessConfig),
    Dns(DnsProcessConfig),
    Tunnel(TunnelProcessConfig),
}

impl DaemonProcessConfig {
    #[must_use]
    pub fn role(&self) -> DaemonProcessRole {
        match self {
            Self::Control(_) => DaemonProcessRole::Control,
            Self::Node(config) => DaemonProcessRole::Node(config.node_id.clone()),
            Self::Gateway(_) => DaemonProcessRole::Gateway,
            Self::Dns(_) => DaemonProcessRole::Dns,
            Self::Tunnel(config) => DaemonProcessRole::Tunnel(config.side()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProcessConfig {
    pub nats: NatsServerRuntime,
}

impl ControlProcessConfig {
    #[must_use]
    pub fn new(nats: NatsServerRuntime) -> Self {
        Self { nats }
    }

    #[must_use]
    pub fn nats_endpoint(&self) -> NatsClientEndpoint {
        self.nats.client_endpoint()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProcessConfig {
    pub node_id: NodeId,
    pub nats_endpoint: NatsClientEndpoint,
}

impl NodeProcessConfig {
    #[must_use]
    pub fn new(node_id: NodeId, nats_endpoint: NatsClientEndpoint) -> Self {
        Self {
            node_id,
            nats_endpoint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessConfig {
    pub nats_endpoint: NatsClientEndpoint,
}

impl GatewayProcessConfig {
    #[must_use]
    pub fn new(nats_endpoint: NatsClientEndpoint) -> Self {
        Self { nats_endpoint }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessConfig {
    pub nats_endpoint: NatsClientEndpoint,
}

impl DnsProcessConfig {
    #[must_use]
    pub fn new(nats_endpoint: NatsClientEndpoint) -> Self {
        Self { nats_endpoint }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelProcessConfig {
    pub service: PreparedTunnelService,
}

impl TunnelProcessConfig {
    #[must_use]
    pub fn new(service: PreparedTunnelService) -> Self {
        Self { service }
    }

    #[must_use]
    pub const fn side(&self) -> TunnelSide {
        self.service.side()
    }
}
