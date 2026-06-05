//! Daemon application composition.

use ployz_core::ids::NodeId;
use ployz_nats::connect::NatsClientEndpoint;

use crate::config::{
    ControlProcessConfig, DaemonProcessConfig, DnsProcessConfig, GatewayProcessConfig,
    NodeProcessConfig, TunnelProcessConfig,
};
use crate::iroh_tunnel::PreparedTunnelService;
use crate::nats_process::NatsServerRuntime;
use crate::role::TunnelSide;
use crate::services::DaemonServiceCatalog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleProcessPlan {
    Control(ControlProcessPlan),
    Node(NodeProcessPlan),
    Gateway(GatewayProcessPlan),
    Dns(DnsProcessPlan),
    Tunnel(TunnelProcessPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProcessPlan {
    pub nats: NatsServerRuntime,
    pub service_catalog: DaemonServiceCatalog,
    pub work: &'static [ControlWork],
}

impl ControlProcessPlan {
    #[must_use]
    pub fn nats_endpoint(&self) -> NatsClientEndpoint {
        self.nats.client_endpoint()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlWork {
    AssureNatsResources,
    ServeOperationApi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProcessPlan {
    pub node_id: NodeId,
    pub nats_endpoint: NatsClientEndpoint,
    pub service_catalog: DaemonServiceCatalog,
    pub work: &'static [NodeWork],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeWork {
    ServeNodeRpc,
    PublishDockerObservations,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessPlan {
    pub nats_endpoint: NatsClientEndpoint,
    pub work: &'static [GatewayWork],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayWork {
    WatchRoutes,
    WatchContainerHealth,
    ServeLastKnownGoodRoutes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessPlan {
    pub nats_endpoint: NatsClientEndpoint,
    pub work: &'static [DnsWork],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsWork {
    WatchServices,
    WatchNodeAddresses,
    ServeLastKnownGoodAnswers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelProcessPlan {
    pub service: PreparedTunnelService,
}

impl TunnelProcessPlan {
    #[must_use]
    pub const fn side(&self) -> TunnelSide {
        self.service.side()
    }

    #[must_use]
    pub const fn work(&self) -> &'static [TunnelWork] {
        match self.side() {
            TunnelSide::Edge => &[TunnelWork::ExposeLoopbackNats, TunnelWork::OpenIrohToCore],
            TunnelSide::Core => &[
                TunnelWork::AcceptIrohFromEdges,
                TunnelWork::ForwardCoreNatsBytes,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelWork {
    ExposeLoopbackNats,
    OpenIrohToCore,
    AcceptIrohFromEdges,
    ForwardCoreNatsBytes,
}

#[must_use]
pub fn plan_configured_process(config: &DaemonProcessConfig) -> RoleProcessPlan {
    match config {
        DaemonProcessConfig::Control(config) => plan_control_process(config),
        DaemonProcessConfig::Node(config) => plan_node_process(config),
        DaemonProcessConfig::Gateway(config) => plan_gateway_process(config),
        DaemonProcessConfig::Dns(config) => plan_dns_process(config),
        DaemonProcessConfig::Tunnel(config) => plan_tunnel_process(config),
    }
}

fn plan_control_process(config: &ControlProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Control(ControlProcessPlan {
        nats: config.nats.clone(),
        service_catalog: DaemonServiceCatalog::for_control(),
        work: &[
            ControlWork::AssureNatsResources,
            ControlWork::ServeOperationApi,
        ],
    })
}

fn plan_node_process(config: &NodeProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Node(NodeProcessPlan {
        node_id: config.node_id.clone(),
        nats_endpoint: config.nats_endpoint.clone(),
        service_catalog: DaemonServiceCatalog::for_node(&config.node_id),
        work: &[NodeWork::ServeNodeRpc, NodeWork::PublishDockerObservations],
    })
}

fn plan_gateway_process(config: &GatewayProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Gateway(GatewayProcessPlan {
        nats_endpoint: config.nats_endpoint.clone(),
        work: &[
            GatewayWork::WatchRoutes,
            GatewayWork::WatchContainerHealth,
            GatewayWork::ServeLastKnownGoodRoutes,
        ],
    })
}

fn plan_dns_process(config: &DnsProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Dns(DnsProcessPlan {
        nats_endpoint: config.nats_endpoint.clone(),
        work: &[
            DnsWork::WatchServices,
            DnsWork::WatchNodeAddresses,
            DnsWork::ServeLastKnownGoodAnswers,
        ],
    })
}

fn plan_tunnel_process(config: &TunnelProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Tunnel(TunnelProcessPlan {
        service: config.service.clone(),
    })
}
