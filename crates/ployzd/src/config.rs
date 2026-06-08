//! Role-specific daemon configuration.

use ployz_core::ha::CoreTopology;
use ployz_core::ids::NodeId;
use ployz_core::install::{InstallContractError, MachineBootstrapUrl};
use std::fmt;

use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};
use std::time::Duration;

use crate::iroh_tunnel::PreparedTunnelService;
use crate::nats_process::NatsServerRuntime;
use crate::role::{DaemonProcessRole, TunnelSide};

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const PLOYZ_MACHINE_BOOTSTRAP_URL_ENV: &str = "PLOYZ_MACHINE_BOOTSTRAP_URL";
pub const DEFAULT_MACHINE_BOOTSTRAP_URL: &str = "https://get.ployz.dev/ployz.sh";
pub const PLOYZ_EBPF_BYTECODE_ENV: &str = "PLOYZ_EBPF_BYTECODE";
pub const DEFAULT_EBPF_BYTECODE_PATH: &str = "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc";
pub const DEFAULT_DEPLOY_STEP_TIMEOUT: Duration = Duration::from_secs(60);

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
    Configured(Box<DaemonProcessConfig>),
    TunnelConfigPending { side: TunnelSide },
}

pub fn load_daemon_process_config(
    role: DaemonProcessRole,
    env: impl Fn(&str) -> Option<String>,
) -> Result<LoadedDaemonProcessConfig, DaemonProcessConfigError> {
    match &role {
        DaemonProcessRole::Control => {
            let nats_url = load_nats_url(&role, &env)?;
            let control = ControlProcessConfig::new(
                NatsServerRuntime::External(nats_url),
                NodeId::try_new("core_1").expect("default single-core node id is valid"),
            )
            .with_machine_bootstrap_url(load_machine_bootstrap_url(&env)?);
            Ok(LoadedDaemonProcessConfig::Configured(Box::new(
                DaemonProcessConfig::Control(control),
            )))
        }
        DaemonProcessRole::Node(node_id) => {
            let nats_url = load_nats_url(&role, &env)?;
            Ok(LoadedDaemonProcessConfig::Configured(Box::new(
                DaemonProcessConfig::Node(NodeProcessConfig::new(
                    node_id.clone(),
                    nats_url,
                    load_ebpf_bytecode_path(env),
                )),
            )))
        }
        DaemonProcessRole::Gateway => {
            let nats_url = load_nats_url(&role, &env)?;
            Ok(LoadedDaemonProcessConfig::Configured(Box::new(
                DaemonProcessConfig::Gateway(GatewayProcessConfig::new(nats_url)),
            )))
        }
        DaemonProcessRole::Dns => {
            let nats_url = load_nats_url(&role, &env)?;
            Ok(LoadedDaemonProcessConfig::Configured(Box::new(
                DaemonProcessConfig::Dns(DnsProcessConfig::new(nats_url)),
            )))
        }
        DaemonProcessRole::Tunnel(side) => {
            Ok(LoadedDaemonProcessConfig::TunnelConfigPending { side: *side })
        }
    }
}

fn load_ebpf_bytecode_path(env: impl Fn(&str) -> Option<String>) -> std::path::PathBuf {
    env(PLOYZ_EBPF_BYTECODE_ENV)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_EBPF_BYTECODE_PATH))
}

fn load_nats_url(
    role: &DaemonProcessRole,
    env: &impl Fn(&str) -> Option<String>,
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

fn load_machine_bootstrap_url(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<MachineBootstrapUrl, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_MACHINE_BOOTSTRAP_URL_ENV).filter(|value| !value.is_empty()) else {
        return Ok(default_machine_bootstrap_url());
    };
    MachineBootstrapUrl::try_new(value.clone())
        .map_err(|source| DaemonProcessConfigError::InvalidMachineBootstrapUrl { value, source })
}

fn default_machine_bootstrap_url() -> MachineBootstrapUrl {
    MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
        .expect("default machine bootstrap URL is valid")
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
    InvalidMachineBootstrapUrl {
        value: String,
        source: InstallContractError,
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
            Self::InvalidMachineBootstrapUrl { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_MACHINE_BOOTSTRAP_URL_ENV
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
    pub deploy_nodes: Vec<NodeId>,
    pub deploy_step_timeout: Duration,
    pub machine_bootstrap_url: MachineBootstrapUrl,
}

impl ControlProcessConfig {
    #[must_use]
    pub fn new(nats: NatsServerRuntime, core_node_id: NodeId) -> Self {
        let core_topology = CoreTopology::from_nodes(vec![core_node_id.clone()])
            .expect("single-core process config uses a valid topology");
        Self {
            nats,
            core_node_id: core_node_id.clone(),
            core_topology,
            deploy_nodes: vec![core_node_id],
            deploy_step_timeout: DEFAULT_DEPLOY_STEP_TIMEOUT,
            machine_bootstrap_url: default_machine_bootstrap_url(),
        }
    }

    #[must_use]
    pub fn with_deploy_nodes(mut self, deploy_nodes: Vec<NodeId>) -> Self {
        self.deploy_nodes = deploy_nodes;
        self
    }

    #[must_use]
    pub const fn with_deploy_step_timeout(mut self, deploy_step_timeout: Duration) -> Self {
        self.deploy_step_timeout = deploy_step_timeout;
        self
    }

    #[must_use]
    pub fn with_machine_bootstrap_url(
        mut self,
        machine_bootstrap_url: MachineBootstrapUrl,
    ) -> Self {
        self.machine_bootstrap_url = machine_bootstrap_url;
        self
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
    pub ebpf_bytecode_path: std::path::PathBuf,
}

impl NodeProcessConfig {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        nats_url: NatsClientUrl,
        ebpf_bytecode_path: std::path::PathBuf,
    ) -> Self {
        Self {
            node_id,
            nats_url,
            ebpf_bytecode_path,
        }
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
