//! Role-specific daemon configuration.

use ployz_core::dataplane::{MachineEndpointSupernet, default_endpoint_subnet};
use ployz_core::ids::MachineId;
use ployz_core::install::{
    InstallContractError, MachineBootstrapUrl, MachineJoinSecretDelivery, MachineJoinTemplate,
    NatsMachineMaterialPaths,
};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::security::NatsPrincipal;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::{fmt, fs};

use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsClientUrlError, NatsConnectConfig, NatsTlsTrust,
};
use std::time::Duration;

use crate::adapters::nats_server::NatsServerLaunch;
use crate::lease::{LeaseWorkerUrl, LeaseWorkerUrlError};
use crate::operation_api::admission::MachineAddBootstrapConfig;
use crate::role_cli::DaemonProcessRole;
pub use ployz_core::cert::DEFAULT_LEASE_WORKER_URL;
pub use ployz_core::install::DEFAULT_MACHINE_BOOTSTRAP_URL;

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
pub const PLOYZ_NATS_NKEY_SEED_FILE_ENV: &str = "PLOYZ_NATS_NKEY_SEED_FILE";
pub const PLOYZ_NATS_AUTHORIZED_USERS_FILE_ENV: &str = "PLOYZ_NATS_AUTHORIZED_USERS_FILE";
pub const PLOYZ_CORE_DB_ENV: &str = "PLOYZ_CORE_DB";
pub const PLOYZ_LEASE_WORKER_URL_ENV: &str = "PLOYZ_LEASE_WORKER_URL";
pub const DEFAULT_CORE_DB: &str = "/var/lib/ployz/ployz-core.db";
/// Set by `core-promote` on the new core's control unit: the machine's local
/// intent mirror to seed a fresh core store from on first startup (ADR 0031).
pub const PLOYZ_SEED_FROM_MIRROR_ENV: &str = "PLOYZ_SEED_FROM_MIRROR";
pub const PLOYZ_NATS_MACHINE_SEED_FILE_ENV: &str = "PLOYZ_NATS_MACHINE_SEED_FILE";
pub const PLOYZ_JOIN_NKEY_SEED_FILE_ENV: &str = "PLOYZ_JOIN_NKEY_SEED_FILE";
pub const DEFAULT_NATS_AUTHORIZED_USERS_FILE: &str = "/etc/nats/authorized-users.conf";
pub const PLOYZ_MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
pub const PLOYZ_GATEWAY_LISTEN_ADDR_ENV: &str = "PLOYZ_GATEWAY_LISTEN_ADDR";
pub const PLOYZ_DEPLOY_MACHINES_ENV: &str = "PLOYZ_DEPLOY_MACHINES";
pub const PLOYZ_MACHINE_BOOTSTRAP_URL_ENV: &str = "PLOYZ_MACHINE_BOOTSTRAP_URL";
pub const PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV: &str = "PLOYZ_MACHINE_JOIN_TEMPLATE_FILE";
pub const PLOYZ_EBPF_BYTECODE_ENV: &str = "PLOYZ_EBPF_BYTECODE";
pub const DEFAULT_EBPF_BYTECODE_PATH: &str = "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc";
pub const PLOYZ_EBPF_CTL_ENV: &str = "PLOYZ_EBPF_CTL";
pub const DEFAULT_EBPF_CTL_PATH: &str = "/usr/local/bin/ployz-ebpf-ctl";
pub const PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV: &str = "PLOYZ_DATAPLANE_BRIDGE_IFNAME";
pub const DEFAULT_DATAPLANE_BRIDGE_IFNAME: &str = "br-ployz";
pub const PLOYZ_DATAPLANE_WG_IFNAME_ENV: &str = "PLOYZ_DATAPLANE_WG_IFNAME";
pub const DEFAULT_DATAPLANE_WG_IFNAME: &str = "ployz-wg0";
pub const PLOYZ_DATAPLANE_WG_MTU_ENV: &str = "PLOYZ_DATAPLANE_WG_MTU";
pub const PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV: &str = "PLOYZ_DATAPLANE_ENDPOINT_SUBNET";
pub const PLOYZ_DATAPLANE_ENDPOINT_SUPERNET_ENV: &str = "PLOYZ_DATAPLANE_ENDPOINT_SUPERNET";
pub const DEFAULT_DEPLOY_STEP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonProcessConfig {
    Control(ControlProcessConfig),
    Machine(MachineProcessConfig),
    Gateway(GatewayProcessConfig),
    Dns(DnsProcessConfig),
}

impl DaemonProcessConfig {
    #[must_use]
    pub fn role(&self) -> DaemonProcessRole {
        match self {
            Self::Control(_) => DaemonProcessRole::Control,
            Self::Machine(config) => DaemonProcessRole::Machine(config.machine_id.clone()),
            Self::Gateway(_) => DaemonProcessRole::Gateway,
            Self::Dns(_) => DaemonProcessRole::Dns,
        }
    }
}

pub fn load_daemon_process_config(
    role: DaemonProcessRole,
    env: impl Fn(&str) -> Option<String>,
) -> Result<DaemonProcessConfig, DaemonProcessConfigError> {
    match &role {
        DaemonProcessRole::Control => {
            let machine_id = load_process_machine_id(&role, &env)?;
            let connect = load_nats_connect_config(&role, &env)?;
            let epoch_fence_mirror = load_seed_from_mirror(&env).or_else(|| {
                Some(
                    connect
                        .seed_file
                        .with_file_name(ployz_core::install::INTENT_MIRROR_FILE_NAME),
                )
            });
            let nats_connect = read_connect_config_now(&role, &connect)?;
            let mut control = ControlProcessConfig::new(
                NatsServerLaunch::External(connect.url),
                machine_id,
                nats_connect,
            )
            .with_nats_authorization(load_control_nats_authorization(&env));
            if let Some(deploy_machines) = load_deploy_machines(&env)? {
                control = control.with_deploy_machines(deploy_machines);
            }
            control = control.with_core_db_path(load_core_db_path(&env));
            control = control.with_lease_worker_url(load_lease_worker_url(&env)?);
            control = control.with_seed_from_mirror(load_seed_from_mirror(&env));
            control = control.with_epoch_fence_mirror(epoch_fence_mirror);
            control = control.with_machine_bootstrap(load_machine_bootstrap(&env)?);
            control =
                control.with_dataplane_endpoint_supernet(load_dataplane_endpoint_supernet(&env)?);
            Ok(DaemonProcessConfig::Control(control))
        }
        DaemonProcessRole::Machine(machine_id) => {
            let connect = load_nats_connect_config(&role, &env)?;
            let artifacts = MachineProcessArtifacts::new(
                load_ebpf_bytecode_path(&env),
                load_ebpf_ctl_path(&env),
            );
            let ployz_native_mesh = MachinePloyzNativeMeshConfig::new(
                load_dataplane_bridge_ifname(&env),
                load_dataplane_wg_ifname(&env),
                load_dataplane_wg_mtu(&env)?,
                load_dataplane_endpoint_subnet(&env, machine_id),
            );
            Ok(DaemonProcessConfig::Machine(MachineProcessConfig::new(
                machine_id.clone(),
                connect,
                artifacts,
                ployz_native_mesh,
            )))
        }
        DaemonProcessRole::Gateway => {
            let machine_id = load_process_machine_id(&role, &env)?;
            let connect = load_nats_connect_config(&role, &env)?;
            Ok(DaemonProcessConfig::Gateway(GatewayProcessConfig::new(
                machine_id,
                connect,
                load_gateway_listen_addr(&env)?,
            )))
        }
        DaemonProcessRole::Dns => {
            let machine_id = load_process_machine_id(&role, &env)?;
            let connect = load_nats_connect_config(&role, &env)?;
            Ok(DaemonProcessConfig::Dns(DnsProcessConfig::new(
                machine_id, connect,
            )))
        }
    }
}

/// Reads `key` from the environment, treating empty values as unset.
fn env_value(env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key).filter(|value| !value.is_empty())
}

/// Everything a role needs to make its authenticated NATS connection,
/// before the seed file is read. A configured-but-absent seed **file** is
/// not a config error: machine and gateway enter the bounded
/// `AwaitingSeedFile` startup state and re-read the path until ployzd
/// control mints and writes the machine credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleNatsConnect {
    pub url: NatsClientUrl,
    pub ca_file: PathBuf,
    pub seed_file: PathBuf,
    pub principal: NatsPrincipal,
}

impl RoleNatsConnect {
    /// One read attempt of the seed file. Missing files are a typed state,
    /// not a panic or a config error.
    pub fn read_connect_config(&self) -> Result<NatsConnectConfig, SeedFileReadError> {
        let raw = match fs::read_to_string(&self.seed_file) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SeedFileReadError::Missing {
                    path: self.seed_file.clone(),
                });
            }
            Err(error) => {
                return Err(SeedFileReadError::Unreadable {
                    path: self.seed_file.clone(),
                    message: error.to_string(),
                });
            }
        };
        let seed = NatsUserSeed::try_new(raw.trim()).map_err(|_| SeedFileReadError::Invalid {
            path: self.seed_file.clone(),
        })?;

        Ok(NatsConnectConfig {
            url: self.url.clone(),
            auth: NatsClientAuth::NkeySeed(seed),
            trust: NatsTlsTrust::ClusterCa(self.ca_file.clone()),
            principal: self.principal.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeedFileReadError {
    #[error("NKey seed file {} is missing", path.display())]
    Missing { path: PathBuf },
    #[error("NKey seed file {} is unreadable: {message}", path.display())]
    Unreadable { path: PathBuf, message: String },
    #[error("NKey seed file {} does not contain an SU-prefixed user seed", path.display())]
    Invalid { path: PathBuf },
}

/// Reads `PLOYZ_NATS_URL`, `PLOYZ_NATS_CA_FILE`, and
/// `PLOYZ_NATS_NKEY_SEED_FILE` for the given role. Each missing or invalid
/// input is a distinct typed error; the seed file's *contents* are read
/// later (eagerly for control, awaited for machine/gateway/DNS).
pub fn load_nats_connect_config(
    role: &DaemonProcessRole,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<RoleNatsConnect, DaemonProcessConfigError> {
    let url = load_nats_url(role, env)?;
    let ca_file = env_value(env, PLOYZ_NATS_CA_FILE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| DaemonProcessConfigError::MissingNatsCaFile { role: role.clone() })?;
    let seed_file = env_value(env, PLOYZ_NATS_NKEY_SEED_FILE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| DaemonProcessConfigError::MissingNatsSeedFile { role: role.clone() })?;
    let principal = match role {
        DaemonProcessRole::Control => NatsPrincipal::Controller,
        // Gateway and DNS authenticate as the machine's Machine user in v1 —
        // there is no Gateway principal (ADR: Committed Auth Scheme).
        DaemonProcessRole::Machine(machine_id) => NatsPrincipal::Machine {
            machine_id: machine_id.clone(),
        },
        DaemonProcessRole::Gateway | DaemonProcessRole::Dns => NatsPrincipal::Machine {
            machine_id: load_process_machine_id(role, env)?,
        },
    };

    Ok(RoleNatsConnect {
        url,
        ca_file,
        seed_file,
        principal,
    })
}

/// Control reads its seed eagerly: Host Runner wrote `controller.seed` at
/// install, so a missing file is a configuration error, not an awaiting
/// state.
fn read_connect_config_now(
    role: &DaemonProcessRole,
    connect: &RoleNatsConnect,
) -> Result<NatsConnectConfig, DaemonProcessConfigError> {
    connect
        .read_connect_config()
        .map_err(|source| DaemonProcessConfigError::ReadNatsSeedFile {
            role: role.clone(),
            source,
        })
}

fn load_control_nats_authorization(
    env: &impl Fn(&str) -> Option<String>,
) -> ControlNatsAuthorizationConfig {
    let defaults = ControlNatsAuthorizationConfig::in_default_paths();
    ControlNatsAuthorizationConfig {
        authorized_users_file: env_value(env, PLOYZ_NATS_AUTHORIZED_USERS_FILE_ENV)
            .map(PathBuf::from)
            .unwrap_or(defaults.authorized_users_file),
        machine_seed_file: env_value(env, PLOYZ_NATS_MACHINE_SEED_FILE_ENV)
            .map(PathBuf::from)
            .unwrap_or(defaults.machine_seed_file),
    }
}

fn load_core_db_path(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    env_value(env, PLOYZ_CORE_DB_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CORE_DB))
}

pub fn load_lease_worker_url(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<LeaseWorkerUrl, DaemonProcessConfigError> {
    let Some(value) = env_value(env, PLOYZ_LEASE_WORKER_URL_ENV) else {
        return Ok(LeaseWorkerUrl::default_worker());
    };
    LeaseWorkerUrl::try_new(value.clone())
        .map_err(|source| DaemonProcessConfigError::InvalidLeaseWorkerUrl { value, source })
}

fn load_seed_from_mirror(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env_value(env, PLOYZ_SEED_FROM_MIRROR_ENV).map(PathBuf::from)
}

/// Control-owned file paths: the rendered authority include the nats-server
/// reloads, and the first machine's locally written `machine.seed`. Durable
/// control state lives in the core database, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlNatsAuthorizationConfig {
    pub authorized_users_file: PathBuf,
    pub machine_seed_file: PathBuf,
}

impl ControlNatsAuthorizationConfig {
    #[must_use]
    pub fn in_default_paths() -> Self {
        Self {
            authorized_users_file: PathBuf::from(DEFAULT_NATS_AUTHORIZED_USERS_FILE),
            machine_seed_file: NatsMachineMaterialPaths::in_default_state_dir().machine_seed_file(),
        }
    }
}

fn load_ebpf_bytecode_path(env: &impl Fn(&str) -> Option<String>) -> std::path::PathBuf {
    env_value(env, PLOYZ_EBPF_BYTECODE_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_EBPF_BYTECODE_PATH))
}

fn load_ebpf_ctl_path(env: &impl Fn(&str) -> Option<String>) -> std::path::PathBuf {
    env_value(env, PLOYZ_EBPF_CTL_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_EBPF_CTL_PATH))
}

fn load_dataplane_bridge_ifname(env: &impl Fn(&str) -> Option<String>) -> String {
    env_value(env, PLOYZ_DATAPLANE_BRIDGE_IFNAME_ENV)
        .unwrap_or_else(|| DEFAULT_DATAPLANE_BRIDGE_IFNAME.to_owned())
}

fn load_dataplane_wg_ifname(env: &impl Fn(&str) -> Option<String>) -> String {
    env_value(env, PLOYZ_DATAPLANE_WG_IFNAME_ENV)
        .unwrap_or_else(|| DEFAULT_DATAPLANE_WG_IFNAME.to_owned())
}

fn load_dataplane_wg_mtu(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<u32>, DaemonProcessConfigError> {
    env_value(env, PLOYZ_DATAPLANE_WG_MTU_ENV)
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|source| DaemonProcessConfigError::InvalidDataplaneWgMtu { value, source })
        })
        .transpose()
}

fn load_dataplane_endpoint_subnet(
    env: &impl Fn(&str) -> Option<String>,
    machine_id: &MachineId,
) -> String {
    env_value(env, PLOYZ_DATAPLANE_ENDPOINT_SUBNET_ENV)
        .unwrap_or_else(|| default_endpoint_subnet(machine_id))
}

fn load_dataplane_endpoint_supernet(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<MachineEndpointSupernet, DaemonProcessConfigError> {
    let Some(value) = env_value(env, PLOYZ_DATAPLANE_ENDPOINT_SUPERNET_ENV) else {
        return Ok(MachineEndpointSupernet::default_v1());
    };
    MachineEndpointSupernet::try_new(&value).map_err(|source| {
        DaemonProcessConfigError::InvalidDataplaneEndpointSupernet { value, source }
    })
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

fn load_process_machine_id(
    role: &DaemonProcessRole,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<MachineId, DaemonProcessConfigError> {
    let value = env(PLOYZ_MACHINE_ID_ENV)
        .ok_or_else(|| DaemonProcessConfigError::MissingMachineId { role: role.clone() })?;
    MachineId::try_new(value.clone()).map_err(|_source| {
        DaemonProcessConfigError::InvalidMachineId {
            role: role.clone(),
            value,
        }
    })
}

fn load_deploy_machines(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<Vec<MachineId>>, DaemonProcessConfigError> {
    let Some(value) = env(PLOYZ_DEPLOY_MACHINES_ENV).filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|machine| !machine.is_empty())
        .map(|machine| {
            MachineId::try_new(machine.to_owned()).map_err(|_source| {
                DaemonProcessConfigError::InvalidDeployMachine {
                    value: machine.to_owned(),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn load_machine_bootstrap_url(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<MachineBootstrapUrl, DaemonProcessConfigError> {
    let Some(value) = env_value(env, PLOYZ_MACHINE_BOOTSTRAP_URL_ENV) else {
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
    let Some(join_secret_delivery) = load_machine_join_secret_delivery(env)? else {
        return Ok(MachineAddBootstrapConfig::new(bootstrap_url));
    };
    Ok(MachineAddBootstrapConfig::new(bootstrap_url)
        .with_join_material(join_template, join_secret_delivery))
}

fn load_machine_join_template(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<MachineJoinTemplate>, DaemonProcessConfigError> {
    let Some(path) = env_value(env, PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV) else {
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

fn load_machine_join_secret_delivery(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<MachineJoinSecretDelivery>, DaemonProcessConfigError> {
    let Some(path) = env_value(env, PLOYZ_JOIN_NKEY_SEED_FILE_ENV) else {
        return Ok(None);
    };
    let raw = fs::read_to_string(&path).map_err(|source| {
        DaemonProcessConfigError::ReadMachineJoinSeed {
            path: path.clone(),
            source: source.to_string(),
        }
    })?;
    let nats_credentials = NatsUserSeed::try_new(raw.trim())
        .map_err(|_| DaemonProcessConfigError::InvalidMachineJoinSeed { path: path.clone() })?;
    Ok(Some(MachineJoinSecretDelivery { nats_credentials }))
}

fn load_gateway_listen_addr(
    env: &impl Fn(&str) -> Option<String>,
) -> Result<SocketAddr, DaemonProcessConfigError> {
    let Some(value) = env_value(env, PLOYZ_GATEWAY_LISTEN_ADDR_ENV) else {
        return Ok(default_gateway_listen_addr());
    };
    value
        .parse()
        .map_err(|source| DaemonProcessConfigError::InvalidGatewayListenAddr { value, source })
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
    MissingMachineId {
        role: DaemonProcessRole,
    },
    InvalidMachineId {
        role: DaemonProcessRole,
        value: String,
    },
    InvalidDeployMachine {
        value: String,
    },
    MissingNatsUrl {
        role: DaemonProcessRole,
    },
    InvalidNatsUrl {
        role: DaemonProcessRole,
        value: String,
        source: NatsClientUrlError,
    },
    MissingNatsCaFile {
        role: DaemonProcessRole,
    },
    MissingNatsSeedFile {
        role: DaemonProcessRole,
    },
    ReadNatsSeedFile {
        role: DaemonProcessRole,
        source: SeedFileReadError,
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
    ReadMachineJoinSeed {
        path: String,
        source: String,
    },
    InvalidMachineJoinSeed {
        path: String,
    },
    InvalidGatewayListenAddr {
        value: String,
        source: std::net::AddrParseError,
    },
    InvalidDataplaneWgMtu {
        value: String,
        source: std::num::ParseIntError,
    },
    InvalidDataplaneEndpointSupernet {
        value: String,
        source: ployz_core::dataplane::MachineEndpointSupernetError,
    },
    InvalidLeaseWorkerUrl {
        value: String,
        source: LeaseWorkerUrlError,
    },
}

impl fmt::Display for DaemonProcessConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMachineId { role } => write!(
                formatter,
                "{} is required for ployzd {}",
                PLOYZ_MACHINE_ID_ENV,
                role.process_name()
            ),
            Self::InvalidMachineId { role, value } => write!(
                formatter,
                "{}={value:?} is invalid for ployzd {}",
                PLOYZ_MACHINE_ID_ENV,
                role.process_name()
            ),
            Self::InvalidDeployMachine { value } => {
                write!(
                    formatter,
                    "{} contains invalid machine id {value:?}",
                    PLOYZ_DEPLOY_MACHINES_ENV
                )
            }
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
            Self::MissingNatsCaFile { role } => write!(
                formatter,
                "{} is required for ployzd {}",
                PLOYZ_NATS_CA_FILE_ENV,
                role.process_name()
            ),
            Self::MissingNatsSeedFile { role } => write!(
                formatter,
                "{} is required for ployzd {}",
                PLOYZ_NATS_NKEY_SEED_FILE_ENV,
                role.process_name()
            ),
            Self::ReadNatsSeedFile { role, source } => {
                write!(formatter, "ployzd {}: {source}", role.process_name())
            }
            Self::InvalidMachineBootstrapUrl { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_MACHINE_BOOTSTRAP_URL_ENV
            ),
            Self::ReadMachineJoinTemplate { path, source } => {
                write!(
                    formatter,
                    "{} points to unreadable file {path:?}: {source}",
                    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV
                )
            }
            Self::InvalidMachineJoinTemplate { path, source } => {
                write!(
                    formatter,
                    "{} file {path:?} is invalid: {source}",
                    PLOYZ_MACHINE_JOIN_TEMPLATE_FILE_ENV
                )
            }
            Self::ReadMachineJoinSeed { path, source } => {
                write!(
                    formatter,
                    "{} points to unreadable file {path:?}: {source}",
                    PLOYZ_JOIN_NKEY_SEED_FILE_ENV
                )
            }
            Self::InvalidMachineJoinSeed { path } => {
                write!(
                    formatter,
                    "{} file {path:?} does not contain an SU-prefixed user seed",
                    PLOYZ_JOIN_NKEY_SEED_FILE_ENV
                )
            }
            Self::InvalidGatewayListenAddr { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_GATEWAY_LISTEN_ADDR_ENV
            ),
            Self::InvalidDataplaneWgMtu { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_DATAPLANE_WG_MTU_ENV
            ),
            Self::InvalidDataplaneEndpointSupernet { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_DATAPLANE_ENDPOINT_SUPERNET_ENV
            ),
            Self::InvalidLeaseWorkerUrl { value, .. } => write!(
                formatter,
                "{}={value:?} is invalid",
                PLOYZ_LEASE_WORKER_URL_ENV
            ),
        }
    }
}

impl std::error::Error for DaemonProcessConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlProcessConfig {
    pub nats: NatsServerLaunch,
    pub nats_connect: NatsConnectConfig,
    pub nats_authorization: ControlNatsAuthorizationConfig,
    pub core_db_path: PathBuf,
    /// A machine-local intent mirror to seed a fresh core store from at startup,
    /// set only by `core-promote` (ADR 0031); `None` for an ordinary core.
    pub seed_from_mirror: Option<PathBuf>,
    /// Machine-local intent mirror used to refuse stale core startup after this
    /// machine has observed a higher Control-Plane Epoch.
    pub epoch_fence_mirror: Option<PathBuf>,
    pub deploy_machines: Vec<MachineId>,
    pub deploy_step_timeout: Duration,
    pub machine_bootstrap: MachineAddBootstrapConfig,
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
    pub lease_worker_url: LeaseWorkerUrl,
}

impl ControlProcessConfig {
    #[must_use]
    pub fn new(
        nats: NatsServerLaunch,
        first_deploy_machine: MachineId,
        nats_connect: NatsConnectConfig,
    ) -> Self {
        Self {
            nats,
            nats_connect,
            nats_authorization: ControlNatsAuthorizationConfig::in_default_paths(),
            core_db_path: PathBuf::from(DEFAULT_CORE_DB),
            seed_from_mirror: None,
            epoch_fence_mirror: None,
            deploy_machines: vec![first_deploy_machine],
            deploy_step_timeout: DEFAULT_DEPLOY_STEP_TIMEOUT,
            machine_bootstrap: MachineAddBootstrapConfig::new(default_machine_bootstrap_url()),
            dataplane_endpoint_supernet: MachineEndpointSupernet::default_v1(),
            lease_worker_url: LeaseWorkerUrl::default_worker(),
        }
    }

    #[must_use]
    pub fn with_nats_authorization(
        mut self,
        nats_authorization: ControlNatsAuthorizationConfig,
    ) -> Self {
        self.nats_authorization = nats_authorization;
        self
    }

    #[must_use]
    pub fn with_deploy_machines(mut self, deploy_machines: Vec<MachineId>) -> Self {
        self.deploy_machines = deploy_machines;
        self
    }

    #[must_use]
    pub fn with_core_db_path(mut self, core_db_path: PathBuf) -> Self {
        self.core_db_path = core_db_path;
        self
    }

    #[must_use]
    pub fn with_seed_from_mirror(mut self, seed_from_mirror: Option<PathBuf>) -> Self {
        self.seed_from_mirror = seed_from_mirror;
        self
    }

    #[must_use]
    pub fn with_epoch_fence_mirror(mut self, epoch_fence_mirror: Option<PathBuf>) -> Self {
        self.epoch_fence_mirror = epoch_fence_mirror;
        self
    }

    #[must_use]
    pub const fn with_deploy_step_timeout(mut self, deploy_step_timeout: Duration) -> Self {
        self.deploy_step_timeout = deploy_step_timeout;
        self
    }

    #[must_use]
    pub fn with_machine_bootstrap(mut self, machine_bootstrap: MachineAddBootstrapConfig) -> Self {
        self.machine_bootstrap = machine_bootstrap;
        self
    }

    #[must_use]
    pub fn with_dataplane_endpoint_supernet(
        mut self,
        dataplane_endpoint_supernet: MachineEndpointSupernet,
    ) -> Self {
        self.dataplane_endpoint_supernet = dataplane_endpoint_supernet;
        self
    }

    #[must_use]
    pub fn with_lease_worker_url(mut self, lease_worker_url: LeaseWorkerUrl) -> Self {
        self.lease_worker_url = lease_worker_url;
        self
    }

    #[must_use]
    pub fn nats_url(&self) -> NatsClientUrl {
        self.nats.client_url()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineProcessConfig {
    pub machine_id: MachineId,
    pub nats: RoleNatsConnect,
    pub artifacts: MachineProcessArtifacts,
    pub ployz_native_mesh: MachinePloyzNativeMeshConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineProcessArtifacts {
    pub ebpf_bytecode_path: std::path::PathBuf,
    pub ebpf_ctl_path: std::path::PathBuf,
}

impl MachineProcessArtifacts {
    #[must_use]
    pub fn new(ebpf_bytecode_path: std::path::PathBuf, ebpf_ctl_path: std::path::PathBuf) -> Self {
        Self {
            ebpf_bytecode_path,
            ebpf_ctl_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinePloyzNativeMeshConfig {
    pub bridge_ifname: String,
    pub wg_ifname: String,
    pub wg_mtu: Option<u32>,
    pub endpoint_subnet: String,
}

impl MachinePloyzNativeMeshConfig {
    #[must_use]
    pub fn new(
        bridge_ifname: String,
        wg_ifname: String,
        wg_mtu: Option<u32>,
        endpoint_subnet: String,
    ) -> Self {
        Self {
            bridge_ifname,
            wg_ifname,
            wg_mtu,
            endpoint_subnet,
        }
    }
}

impl MachineProcessConfig {
    #[must_use]
    pub fn new(
        machine_id: MachineId,
        nats: RoleNatsConnect,
        artifacts: MachineProcessArtifacts,
        ployz_native_mesh: MachinePloyzNativeMeshConfig,
    ) -> Self {
        Self {
            machine_id,
            nats,
            artifacts,
            ployz_native_mesh,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessConfig {
    pub machine_id: MachineId,
    pub nats: RoleNatsConnect,
    pub listen_addr: SocketAddr,
}

impl GatewayProcessConfig {
    #[must_use]
    pub fn new(machine_id: MachineId, nats: RoleNatsConnect, listen_addr: SocketAddr) -> Self {
        Self {
            machine_id,
            nats,
            listen_addr,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessConfig {
    pub machine_id: MachineId,
    pub nats: RoleNatsConnect,
}

impl DnsProcessConfig {
    #[must_use]
    pub fn new(machine_id: MachineId, nats: RoleNatsConnect) -> Self {
        Self { machine_id, nats }
    }
}
