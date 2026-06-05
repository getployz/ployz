//! Daemon application composition.

use ployz_core::ids::NodeId;

use crate::role::{DaemonProcessRole, TunnelSide};
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
    pub service_catalog: DaemonServiceCatalog,
    pub work: &'static [ControlWork],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlWork {
    AssureNatsResources,
    ServeOperationApi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProcessPlan {
    pub node_id: NodeId,
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
    pub side: TunnelSide,
    pub work: &'static [TunnelWork],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelWork {
    ExposeLoopbackNats,
    OpenIrohToCore,
    AcceptIrohFromEdges,
    ForwardCoreNatsBytes,
}

#[must_use]
pub fn plan_process(role: &DaemonProcessRole) -> RoleProcessPlan {
    match role {
        DaemonProcessRole::Control => RoleProcessPlan::Control(ControlProcessPlan {
            service_catalog: DaemonServiceCatalog::for_control(),
            work: &[
                ControlWork::AssureNatsResources,
                ControlWork::ServeOperationApi,
            ],
        }),
        DaemonProcessRole::Node(node_id) => RoleProcessPlan::Node(NodeProcessPlan {
            node_id: node_id.clone(),
            service_catalog: DaemonServiceCatalog::for_node(node_id),
            work: &[NodeWork::ServeNodeRpc, NodeWork::PublishDockerObservations],
        }),
        DaemonProcessRole::Gateway => RoleProcessPlan::Gateway(GatewayProcessPlan {
            work: &[
                GatewayWork::WatchRoutes,
                GatewayWork::WatchContainerHealth,
                GatewayWork::ServeLastKnownGoodRoutes,
            ],
        }),
        DaemonProcessRole::Dns => RoleProcessPlan::Dns(DnsProcessPlan {
            work: &[
                DnsWork::WatchServices,
                DnsWork::WatchNodeAddresses,
                DnsWork::ServeLastKnownGoodAnswers,
            ],
        }),
        DaemonProcessRole::Tunnel(TunnelSide::Edge) => RoleProcessPlan::Tunnel(TunnelProcessPlan {
            side: TunnelSide::Edge,
            work: &[TunnelWork::ExposeLoopbackNats, TunnelWork::OpenIrohToCore],
        }),
        DaemonProcessRole::Tunnel(TunnelSide::Core) => RoleProcessPlan::Tunnel(TunnelProcessPlan {
            side: TunnelSide::Core,
            work: &[
                TunnelWork::AcceptIrohFromEdges,
                TunnelWork::ForwardCoreNatsBytes,
            ],
        }),
    }
}
