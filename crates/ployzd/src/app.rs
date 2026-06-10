//! Daemon application composition.

use ployz_core::ids::NodeId;
use ployz_nats::connect::NatsClientUrl;
use std::net::SocketAddr;

use crate::config::{
    ControlProcessConfig, DaemonProcessConfig, DnsProcessConfig, GatewayProcessConfig,
    NodeProcessConfig,
};
use crate::nats_process::NatsServerRuntime;
use crate::services::DaemonServiceCatalog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleProcessPlan {
    Control(ControlProcessPlan),
    Node(NodeProcessPlan),
    Gateway(GatewayProcessPlan),
    Dns(DnsProcessPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProcessPlan {
    pub nats: NatsServerRuntime,
    pub service_catalog: DaemonServiceCatalog,
    pub work: &'static [ControlWork],
}

impl ControlProcessPlan {
    #[must_use]
    pub fn nats_url(&self) -> NatsClientUrl {
        self.nats.client_url()
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
    pub nats_url: NatsClientUrl,
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
    pub node_id: NodeId,
    pub nats_url: NatsClientUrl,
    pub listen_addr: SocketAddr,
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
    pub node_id: NodeId,
    pub nats_url: NatsClientUrl,
    pub work: &'static [DnsWork],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsWork {
    WatchServices,
    WatchNodeAddresses,
    ServeLastKnownGoodAnswers,
}

#[must_use]
pub fn plan_configured_process(config: &DaemonProcessConfig) -> RoleProcessPlan {
    match config {
        DaemonProcessConfig::Control(config) => plan_control_process(config),
        DaemonProcessConfig::Node(config) => plan_node_process(config),
        DaemonProcessConfig::Gateway(config) => plan_gateway_process(config),
        DaemonProcessConfig::Dns(config) => plan_dns_process(config),
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
        nats_url: config.nats_url.clone(),
        service_catalog: DaemonServiceCatalog::for_node(&config.node_id),
        work: &[NodeWork::ServeNodeRpc, NodeWork::PublishDockerObservations],
    })
}

fn plan_gateway_process(config: &GatewayProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Gateway(GatewayProcessPlan {
        node_id: config.node_id.clone(),
        nats_url: config.nats_url.clone(),
        listen_addr: config.listen_addr,
        work: &[
            GatewayWork::WatchRoutes,
            GatewayWork::WatchContainerHealth,
            GatewayWork::ServeLastKnownGoodRoutes,
        ],
    })
}

fn plan_dns_process(config: &DnsProcessConfig) -> RoleProcessPlan {
    RoleProcessPlan::Dns(DnsProcessPlan {
        node_id: config.node_id.clone(),
        nats_url: config.nats_url.clone(),
        work: &[
            DnsWork::WatchServices,
            DnsWork::WatchNodeAddresses,
            DnsWork::ServeLastKnownGoodAnswers,
        ],
    })
}
