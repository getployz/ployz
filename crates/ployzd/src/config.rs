//! Role-specific daemon configuration.

use ployz_core::dataplane::default_endpoint_subnet;
use ployz_core::ids::NodeId;
use ployz_core::install::{InstallContractError, MachineBootstrapUrl, MachineJoinTemplate};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::{fmt, fs};

use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};
use ployz_transport::iroh_endpoint::IrohEndpoint;
use std::time::Duration;

use crate::controllers::MachineAddBootstrapConfig;
use crate::iroh_tunnel::PreparedTunnelService;
use crate::nats_process::NatsServerRuntime;
use crate::role::{DaemonProcessRole, TunnelSide};

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const PLOYZ_NODE_ID_ENV: &str = "PLOYZ_NODE_ID";
pub const PLOYZ_NODE_PUBLIC_IP_ENV: &str = "PLOYZ_NODE_PUBLIC_IP";
pub const PLOYZ_GATEWAY_LISTEN_ADDR_ENV: &str = "PLOYZ_GATEWAY_LISTEN_ADDR";
pub const PLOYZ_MACHINE_BOOTSTRAP_URL_ENV: &str = "PLOYZ_MACHINE_BOOTSTRAP_URL";
pub const PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV: &str = "PLOYZ_MACHINE_JOIN_TEMPLATE_FILE";
pub const DEFAULT_MACHINE_BOOTSTRAP_URL: &str = "https://get.ployz.dev/ployz.sh";
pub const PLOYZ_EBPF_BYTECODE_ENV: &str = "PLOYZ_EBPF_BYTECODE";
pub const DEFAULT_EBPF_BYTECODE_PATH: &str = "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc";
pub const PLOYZ_EBPF_CTL_ENV: &str = "PLOYZ_EBPF_CTL";
pub const DEFAULT_EBPF_CTL_PATH: &str = "/usr/local/bin/ployz-ebpf-ctl";
pub const PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV: &str = "PLOYZ_DATAPLANE_BRIDGE_IFNAME";
pub const DEFAULT_DATAPLANE_BRIDGE_IFNAME: &str = "docker0";
pub const PLOYZ_DATAPLANE_WG_IFNAME_ENV: &str = "PLOYZ_DATAPLANE_WG_IFNAME";
pub const DEFAULT_DATAPLANE_WG_IFNAME: &str = "ployz-wg0";
pub const PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV: &str = "PLOYZ_DATAPLANE_ENDPOINT_SUBNET";
pub const PLOYZ_TUNNEL_NATS_ADDR_ENV: &str = "PLOYZ_TUNNEL_NATS_ADDR";
pub const PLOYZ_TUNNEL_LISTEN_ADDR_ENV: &str = "PLOYZ_TUNNEL_LISTEN_ADDR";
pub const PLOYZ_TUNNEL_CORE_NODE_ENV: &str = "PLOYZ_TUNNEL_CORE_NODE";
pub const PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV: &str = "PLOYZ_TUNNEL_CORE_PUBLIC_KEY";
pub const PLOYZ_TUNNEL_CORE_DIRECT_ADDRS_ENV: &str = "PLOYZ_TUNNEL_CORE_DIRECT_ADDRS";
pub const PLOYZ_TUNNEL_CORE_RELAY_URL_ENV: &str = "PLOYZ_TUNNEL_CORE_RELAY_URL";
pub const PLOYZ_TUNNEL_IROH_BIND_ADDR_ENV: &str = "PLOYZ_TUNNEL_IROH_BIND_ADDR";
pub const PLOYZ_TUNNEL_SECRET_KEY_FILE_ENV: &str = "PLOYZ_TUNNEL_SECRET_KEY_FILE";
pub const PLOYZ_TUNNEL_PUBLIC_KEY_FILE_ENV: &str = "PLOYZ_TUNNEL_PUBLIC_KEY_FILE";
pub const DEFAULT_TUNNEL_SECRET_KEY_FILE: &str = "/var/lib/ployz/iroh/endpoint.key";
pub const DEFAULT_TUNNEL_PUBLIC_KEY_FILE: &str = "/var/lib/ployz/iroh/endpoint.public";
pub const DEFAULT_CORE_TUNNEL_IROH_PORT: u16 = 4433;
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

pub fn load_daemon_process_config(
    role: DaemonProcessRole,
    env: impl Fn(&str) -> Option<String>,
) -> Result<DaemonProcessConfig, DaemonProcessConfigError> {
    match &role {
        DaemonProcessRole::Control => {
            let nats_url = load_nats_url(&role, &env)?;
            let control = ControlProcessConfig::new(
                NatsServerRuntime::External(nats_url),
                NodeId::try_new("core_1").expect("default single-core node id is valid"),
            )
            .with_machine_bootstrap(load_machine_bootstrap(&env)?);
            Ok(DaemonProcessConfig::Control(control))
        }
        DaemonProcessRole::Node(node_id) => {
            let nats_url = load_nats_url(&role, &env)?;
            Ok(DaemonProcessConfig::Node(NodeProcessConfig::new(
                node_id.clone(),
                nats_url,
                load_ebpf_bytecode_path(&env),
                load_ebpf_ctl_path(&env),
                load_dataplane_bridge_ifname(&env),
                load_dataplane_wg_ifname(&env),
                load_dataplane_endpoint_subnet(&env, node_id),
                load_node_public_ip(&env)?,
            )))
        }
        DaemonProcessRole::Gateway => {
            let node_id = load_process_node_id(&role, &env)?;
            let nats_url = load_nats_url(&role, &env)?;
            Ok(DaemonProcessConfig::Gateway(GatewayProcessConfig::new(
                node_id,
                nats_url,
                load_gateway_listen_addr(&env)?,
            )))
        }
        DaemonProcessRole::Dns => {
            let node_id = load_process_node_id(&role, &env)?;
            let nats_url = load_nats_url(&role, &env)?;
            Ok(DaemonProcessConfig::Dns(DnsProcessConfig::new(
                node_id, nats_url,
            )))
        }
        DaemonProcessRole::Tunnel(side) => Ok(DaemonProcessConfig::Tunnel(load_tunnel_config(
            *side, &env,
        )?)),
    }
}

fn load_ebpf_bytecode_path(env: &impl Fn(&str) -> Option<String>) -> std::path::PathBuf {
    env(PLOYZ_EBPF_BYTECODE_ENV)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_EBPF_BYTECODE_PATH))
}

fn load_ebpf_ctl_path(env: &impl Fn(&str) -> Option<String>) -> std::path::PathBuf {
    env(PLOYZ_EBPF_CTL_ENV)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_EBPF_CTL_PATH))
}

fn load_dataplane_bridge_ifname(env: &impl Fn(&str) -> Option<String>) -> String {
    env(PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_DATAPLANE_BRIDGE_IFNAME.to_owned())
}

fn load_dataplane_wg_ifname(env: &impl Fn(&str) -> Option<String>) -> String {
    env(PLOYZ_DATAPLANE_WG_IFNAME_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_DATAPLANE_WG_IFNAME.to_owned())
}

fn load_dataplane_endpoint_subnet(
    env: &impl Fn(&str) -> Option<String>,
    node_id: &NodeId,
) -> String {
    env(PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_endpoint_subnet(node_id))
}

fn load_node_public_ip(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<IpAddr>, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_NODE_PUBLIC_IP_ENV).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    value
        .parse()
        .map(Some)
        .map_err(|source| DaemonProcessConfigError::InvalidNodePublicIp { value, source })
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

fn load_process_node_id(
    role: &DaemonProcessRole,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<NodeId, DaemonProcessConfigError> {
    let value = env(PLOYZ_NODE_ID_ENV)
        .ok_or_else(|| DaemonProcessConfigError::MissingNodeId { role: role.clone() })?;
    NodeId::try_new(value.clone()).map_err(|_source| DaemonProcessConfigError::InvalidNodeId {
        role: role.clone(),
        value,
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

fn load_machine_bootstrap(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<MachineAddBootstrapConfig, DaemonProcessConfigError> {
    let bootstrap_url = load_machine_bootstrap_url(env)?;
    let Some(join_template) = load_machine_join_template(env)? else {
        return Ok(MachineAddBootstrapConfig::new(bootstrap_url));
    };
    Ok(MachineAddBootstrapConfig::new(bootstrap_url).with_join_template(join_template))
}

fn load_machine_join_template(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<MachineJoinTemplate>, DaemonProcessConfigError> {
    let Some(path) = env(PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV).filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let json = fs::read_to_string(&path).map_err(|source| {
        DaemonProcessConfigError::ReadMachineJoinTemplate {
            path: path.clone(),
            source: source.to_string(),
        }
    })?;
    serde_json::from_str(&json).map(Some).map_err(|source| {
        DaemonProcessConfigError::InvalidMachineJoinTemplate {
            path,
            source: source.to_string(),
        }
    })
}

fn load_gateway_listen_addr(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<SocketAddr, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_GATEWAY_LISTEN_ADDR_ENV).filter(|value| !value.is_empty()) else {
        return Ok(default_gateway_listen_addr());
    };
    value
        .parse()
        .map_err(|source| DaemonProcessConfigError::InvalidGatewayListenAddr { value, source })
}

fn load_tunnel_config(
    side: TunnelSide,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<TunnelProcessConfig, DaemonProcessConfigError> {
    let service = match side {
        TunnelSide::Core => {
            let nats_socket = load_tunnel_socket(side, PLOYZ_TUNNEL_NATS_ADDR_ENV, env)?;
            PreparedTunnelService::core("ployzd-tunnel-core", nats_socket)
        }
        TunnelSide::Edge => {
            let local_listen = load_tunnel_socket(side, PLOYZ_TUNNEL_LISTEN_ADDR_ENV, env)?;
            if !local_listen.ip().is_loopback() {
                return Err(DaemonProcessConfigError::NonLoopbackTunnelListenAddr {
                    side,
                    value: local_listen,
                });
            }
            let core_node = load_tunnel_core_node(env)?;
            let public_key =
                load_required_tunnel_value(side, PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV, env)?;
            if !is_valid_tunnel_public_key(&public_key) {
                return Err(DaemonProcessConfigError::InvalidTunnelValue {
                    side,
                    env: PLOYZ_TUNNEL_CORE_PUBLIC_KEY_ENV,
                    value: public_key,
                });
            }
            let mut endpoint = IrohEndpoint::new(core_node, public_key);
            for address in load_tunnel_core_direct_addresses(env)? {
                endpoint = endpoint.with_direct_address(address);
            }
            if let Some(relay_url) = load_tunnel_core_relay_url(env)? {
                endpoint = endpoint.with_relay_url(relay_url);
            }
            PreparedTunnelService::edge("ployzd-tunnel-edge", local_listen, endpoint)
        }
    };

    Ok(TunnelProcessConfig::new(service)
        .with_iroh_bind_addr(load_tunnel_iroh_bind_addr(side, env)?)
        .with_identity_files(
            load_tunnel_identity_file(env, PLOYZ_TUNNEL_SECRET_KEY_FILE_ENV),
            load_tunnel_identity_file(env, PLOYZ_TUNNEL_PUBLIC_KEY_FILE_ENV),
        ))
}

fn load_tunnel_core_direct_addresses(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<SocketAddr>, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_TUNNEL_CORE_DIRECT_ADDRS_ENV).filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(|raw| {
            raw.parse::<SocketAddr>().map_err(|source| {
                DaemonProcessConfigError::InvalidTunnelDirectAddress {
                    value: raw.to_owned(),
                    source,
                }
            })
        })
        .collect()
}

fn load_tunnel_core_relay_url(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_TUNNEL_CORE_RELAY_URL_ENV).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if !value.starts_with("https://")
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(DaemonProcessConfigError::InvalidTunnelRelayUrl { value });
    }
    Ok(Some(value))
}

fn load_tunnel_socket(
    side: TunnelSide,
    env_name: &'static str,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<SocketAddr, DaemonProcessConfigError> {
    let value = load_required_tunnel_value(side, env_name, env)?;
    value
        .parse()
        .map_err(|source| DaemonProcessConfigError::InvalidTunnelSocket {
            side,
            env: env_name,
            value,
            source,
        })
}

fn load_tunnel_iroh_bind_addr(
    side: TunnelSide,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<SocketAddr, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_TUNNEL_IROH_BIND_ADDR_ENV).filter(|value| !value.is_empty()) else {
        return Ok(default_tunnel_iroh_bind_addr(side));
    };
    value
        .parse()
        .map_err(|source| DaemonProcessConfigError::InvalidTunnelSocket {
            side,
            env: PLOYZ_TUNNEL_IROH_BIND_ADDR_ENV,
            value,
            source,
        })
}

fn default_tunnel_iroh_bind_addr(side: TunnelSide) -> SocketAddr {
    match side {
        TunnelSide::Core => SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            DEFAULT_CORE_TUNNEL_IROH_PORT,
        ),
        TunnelSide::Edge => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
    }
}

fn load_tunnel_identity_file(
    env: &impl Fn(&str) -> Option<String>,
    name: &'static str,
) -> Option<std::path::PathBuf> {
    env(name)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| match name {
            PLOYZ_TUNNEL_SECRET_KEY_FILE_ENV => Some(DEFAULT_TUNNEL_SECRET_KEY_FILE.into()),
            PLOYZ_TUNNEL_PUBLIC_KEY_FILE_ENV => Some(DEFAULT_TUNNEL_PUBLIC_KEY_FILE.into()),
            _ => None,
        })
}

fn load_tunnel_core_node(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<NodeId, DaemonProcessConfigError> {
    let value = load_required_tunnel_value(TunnelSide::Edge, PLOYZ_TUNNEL_CORE_NODE_ENV, env)?;
    NodeId::try_new(value.clone())
        .map_err(|_| DaemonProcessConfigError::InvalidTunnelCoreNode { value })
}

fn load_required_tunnel_value(
    side: TunnelSide,
    env_name: &'static str,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<String, DaemonProcessConfigError> {
    let Some(value) = env(env_name).filter(|value| !value.is_empty()) else {
        return Err(DaemonProcessConfigError::MissingTunnelConfig {
            side,
            env: env_name,
        });
    };
    Ok(value)
}

fn is_valid_tunnel_public_key(value: &str) -> bool {
    !value.chars().any(char::is_control) && !value.chars().any(char::is_whitespace)
}

fn default_machine_bootstrap_url() -> MachineBootstrapUrl {
    MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
        .expect("default machine bootstrap URL is valid")
}

fn default_gateway_listen_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8080)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonProcessConfigError {
    MissingNodeId {
        role: DaemonProcessRole,
    },
    InvalidNodeId {
        role: DaemonProcessRole,
        value: String,
    },
    InvalidNodePublicIp {
        value: String,
        source: std::net::AddrParseError,
    },
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
    ReadMachineJoinTemplate {
        path: String,
        source: String,
    },
    InvalidMachineJoinTemplate {
        path: String,
        source: String,
    },
    InvalidGatewayListenAddr {
        value: String,
        source: std::net::AddrParseError,
    },
    MissingTunnelConfig {
        side: TunnelSide,
        env: &'static str,
    },
    InvalidTunnelSocket {
        side: TunnelSide,
        env: &'static str,
        value: String,
        source: std::net::AddrParseError,
    },
    NonLoopbackTunnelListenAddr {
        side: TunnelSide,
        value: SocketAddr,
    },
    InvalidTunnelCoreNode {
        value: String,
    },
    InvalidTunnelDirectAddress {
        value: String,
        source: std::net::AddrParseError,
    },
    InvalidTunnelRelayUrl {
        value: String,
    },
    InvalidTunnelValue {
        side: TunnelSide,
        env: &'static str,
        value: String,
    },
}

impl fmt::Display for DaemonProcessConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNodeId { role } => write!(
                formatter,
                "{} is required for ployzd {}",
                PLOYZ_NODE_ID_ENV,
                role.process_name()
            ),
            Self::InvalidNodeId { role, value } => write!(
                formatter,
                "{}={value:?} is invalid for ployzd {}",
                PLOYZ_NODE_ID_ENV,
                role.process_name()
            ),
            Self::InvalidNodePublicIp { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_NODE_PUBLIC_IP_ENV
            ),
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
            Self::ReadMachineJoinTemplate { path, .. } => {
                write!(
                    formatter,
                    "{} points to unreadable file {path:?}",
                    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV
                )
            }
            Self::InvalidMachineJoinTemplate { path, .. } => {
                write!(
                    formatter,
                    "{} file {path:?} is invalid",
                    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV
                )
            }
            Self::InvalidGatewayListenAddr { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_GATEWAY_LISTEN_ADDR_ENV
            ),
            Self::MissingTunnelConfig { side, env } => write!(
                formatter,
                "{env} is required for ployzd tunnel {}",
                side.as_str()
            ),
            Self::InvalidTunnelSocket {
                side, env, value, ..
            } => write!(
                formatter,
                "{env}={value:?} is invalid for ployzd tunnel {}",
                side.as_str()
            ),
            Self::NonLoopbackTunnelListenAddr { side, value } => write!(
                formatter,
                "{}={value} must be loopback for ployzd tunnel {}",
                PLOYZ_TUNNEL_LISTEN_ADDR_ENV,
                side.as_str()
            ),
            Self::InvalidTunnelCoreNode { value } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_TUNNEL_CORE_NODE_ENV
            ),
            Self::InvalidTunnelDirectAddress { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_TUNNEL_CORE_DIRECT_ADDRS_ENV
            ),
            Self::InvalidTunnelRelayUrl { value } => write!(
                formatter,
                "{}={value:?} must be an HTTPS URL without whitespace",
                PLOYZ_TUNNEL_CORE_RELAY_URL_ENV
            ),
            Self::InvalidTunnelValue { side, env, value } => write!(
                formatter,
                "{env}={value:?} is invalid for ployzd tunnel {}",
                side.as_str()
            ),
        }
    }
}

impl std::error::Error for DaemonProcessConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProcessConfig {
    pub nats: NatsServerRuntime,
    pub deploy_nodes: Vec<NodeId>,
    pub deploy_step_timeout: Duration,
    pub machine_bootstrap: MachineAddBootstrapConfig,
}

impl ControlProcessConfig {
    #[must_use]
    pub fn new(nats: NatsServerRuntime, first_deploy_node: NodeId) -> Self {
        Self {
            nats,
            deploy_nodes: vec![first_deploy_node],
            deploy_step_timeout: DEFAULT_DEPLOY_STEP_TIMEOUT,
            machine_bootstrap: MachineAddBootstrapConfig::new(default_machine_bootstrap_url()),
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
        self.machine_bootstrap.bootstrap_url = machine_bootstrap_url;
        self
    }

    #[must_use]
    pub fn with_machine_bootstrap(mut self, machine_bootstrap: MachineAddBootstrapConfig) -> Self {
        self.machine_bootstrap = machine_bootstrap;
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
    pub ebpf_ctl_path: std::path::PathBuf,
    pub dataplane_bridge_ifname: String,
    pub dataplane_wg_ifname: String,
    pub dataplane_endpoint_subnet: String,
    pub public_ip: Option<IpAddr>,
}

impl NodeProcessConfig {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        nats_url: NatsClientUrl,
        ebpf_bytecode_path: std::path::PathBuf,
        ebpf_ctl_path: std::path::PathBuf,
        dataplane_bridge_ifname: String,
        dataplane_wg_ifname: String,
        dataplane_endpoint_subnet: String,
        public_ip: Option<IpAddr>,
    ) -> Self {
        Self {
            node_id,
            nats_url,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            dataplane_bridge_ifname,
            dataplane_wg_ifname,
            dataplane_endpoint_subnet,
            public_ip,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessConfig {
    pub node_id: NodeId,
    pub nats_url: NatsClientUrl,
    pub listen_addr: SocketAddr,
}

impl GatewayProcessConfig {
    #[must_use]
    pub fn new(node_id: NodeId, nats_url: NatsClientUrl, listen_addr: SocketAddr) -> Self {
        Self {
            node_id,
            nats_url,
            listen_addr,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessConfig {
    pub node_id: NodeId,
    pub nats_url: NatsClientUrl,
}

impl DnsProcessConfig {
    #[must_use]
    pub fn new(node_id: NodeId, nats_url: NatsClientUrl) -> Self {
        Self { node_id, nats_url }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelProcessConfig {
    pub service: PreparedTunnelService,
    pub iroh_bind_addr: SocketAddr,
    pub secret_key_file: Option<std::path::PathBuf>,
    pub public_key_file: Option<std::path::PathBuf>,
}

impl TunnelProcessConfig {
    #[must_use]
    pub fn new(service: PreparedTunnelService) -> Self {
        let iroh_bind_addr = default_tunnel_iroh_bind_addr(service.side());
        Self {
            service,
            iroh_bind_addr,
            secret_key_file: None,
            public_key_file: None,
        }
    }

    #[must_use]
    pub fn with_iroh_bind_addr(mut self, iroh_bind_addr: SocketAddr) -> Self {
        self.iroh_bind_addr = iroh_bind_addr;
        self
    }

    #[must_use]
    pub fn with_identity_files(
        mut self,
        secret_key_file: Option<std::path::PathBuf>,
        public_key_file: Option<std::path::PathBuf>,
    ) -> Self {
        self.secret_key_file = secret_key_file;
        self.public_key_file = public_key_file;
        self
    }

    #[must_use]
    pub const fn side(&self) -> TunnelSide {
        self.service.side()
    }
}
