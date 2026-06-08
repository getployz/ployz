//! Role-specific daemon configuration.

use ployz_core::ha::CoreTopology;
use ployz_core::ids::NodeId;
use std::fmt;

use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};

use crate::iroh_tunnel::PreparedTunnelService;
use crate::nats_process::NatsServerRuntime;
use crate::role::{DaemonProcessRole, TunnelSide};

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";

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
pub enum LoadedDaemonProcessConfig {
    Configured(DaemonProcessConfig),
    TunnelConfigPending { side: TunnelSide },
}

pub fn load_daemon_process_config(
    role: DaemonProcessRole,
    env: impl Fn(&str) -> Option<String>,
) -> Result<LoadedDaemonProcessConfig, DaemonProcessConfigError> {
    match &role {
        DaemonProcessRole::Control => {
            let nats_url = load_nats_url(&role, env)?;
            Ok(LoadedDaemonProcessConfig::Configured(
                DaemonProcessConfig::Control(ControlProcessConfig::new(
                    NatsServerRuntime::External(nats_url),
                    NodeId::try_new("core_1").expect("default single-core node id is valid"),
                )),
            ))
        }
        DaemonProcessRole::Node(node_id) => {
            let nats_url = load_nats_url(&role, env)?;
            Ok(LoadedDaemonProcessConfig::Configured(
                DaemonProcessConfig::Node(NodeProcessConfig::new(node_id.clone(), nats_url)),
            ))
        }
        DaemonProcessRole::Gateway => {
            let nats_url = load_nats_url(&role, env)?;
            Ok(LoadedDaemonProcessConfig::Configured(
                DaemonProcessConfig::Gateway(GatewayProcessConfig::new(nats_url)),
            ))
        }
        DaemonProcessRole::Dns => {
            let nats_url = load_nats_url(&role, env)?;
            Ok(LoadedDaemonProcessConfig::Configured(
                DaemonProcessConfig::Dns(DnsProcessConfig::new(nats_url)),
            ))
        }
        DaemonProcessRole::Tunnel(side) => {
            Ok(LoadedDaemonProcessConfig::TunnelConfigPending { side: *side })
        }
    }
}

fn load_nats_url(
    role: &DaemonProcessRole,
    env: impl Fn(&str) -> Option<String>,
) -> Result<NatsClientUrl, DaemonProcessConfigError> {
    let value = env(PLOYZ_NATS_URL_ENV)
        .ok_or_else(|| DaemonProcessConfigError::MissingNatsUrl { role: role.clone() })?;
    NatsClientUrl::try_new(value.clone()).map_err(|source| {
        DaemonProcessConfigError::InvalidNatsUrl {
            role: role.clone(),
            value,
            source,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonProcessConfigError {
    MissingNatsUrl {
        role: DaemonProcessRole,
    },
    InvalidNatsUrl {
        role: DaemonProcessRole,
        value: String,
        source: NatsClientUrlError,
    },
}

impl fmt::Display for DaemonProcessConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNatsUrl { role } => write!(
                formatter,
                "{} is required for ployzd {}",
                PLOYZ_NATS_URL_ENV,
                role.process_name()
            ),
            Self::InvalidNatsUrl { role, value, .. } => write!(
                formatter,
                "{}={value:?} is invalid for ployzd {}",
                PLOYZ_NATS_URL_ENV,
                role.process_name()
            ),
        }
    }
}

impl std::error::Error for DaemonProcessConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProcessConfig {
    pub nats: NatsServerRuntime,
    pub core_node_id: NodeId,
    pub core_topology: CoreTopology,
}

impl ControlProcessConfig {
    #[must_use]
    pub fn new(nats: NatsServerRuntime, core_node_id: NodeId) -> Self {
        let core_topology = CoreTopology::from_nodes(vec![core_node_id.clone()])
            .expect("single-core process config uses a valid topology");
        Self {
            nats,
            core_node_id,
            core_topology,
        }
    }

    #[must_use]
    pub fn nats_url(&self) -> NatsClientUrl {
        self.nats.client_url()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProcessConfig {
    pub node_id: NodeId,
    pub nats_url: NatsClientUrl,
}

impl NodeProcessConfig {
    #[must_use]
    pub fn new(node_id: NodeId, nats_url: NatsClientUrl) -> Self {
        Self { node_id, nats_url }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessConfig {
    pub nats_url: NatsClientUrl,
}

impl GatewayProcessConfig {
    #[must_use]
    pub fn new(nats_url: NatsClientUrl) -> Self {
        Self { nats_url }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessConfig {
    pub nats_url: NatsClientUrl,
}

impl DnsProcessConfig {
    #[must_use]
    pub fn new(nats_url: NatsClientUrl) -> Self {
        Self { nats_url }
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
