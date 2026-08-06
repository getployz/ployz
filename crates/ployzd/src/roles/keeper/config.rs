//! Environment-backed configuration for the Keeper role.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::join::JOIN_DOOR_PORT;
use ployz_host_runner::SupervisorBackend;
use ployz_host_runner::builtin_wireguard::{
    BuiltinWireguardConfigError, BuiltinWireguardEbpfConfig, BuiltinWireguardHostConfig,
    BuiltinWireguardPorts, DEFAULT_PRIVATE_KEY_PATH, DEFAULT_WIREGUARD_IFNAME,
    DEFAULT_WIREGUARD_MTU,
};
use ployz_host_runner::container_isolation::{
    ContainerIsolationConfigError, ContainerIsolationHostConfig, DEFAULT_ISOLATION_CGROUP_ROOT,
    DEFAULT_ISOLATION_ROWS_PATH,
};
use ployz_host_runner::{ArtifactStoreError, CONTROL_SOCKET_PATH, PloyzdArtifactStore};

use crate::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};

const CORROSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CORROSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CORROSION_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const CORROSION_MAX_NDJSON_FRAME_BYTES: usize = 1_048_576;
const CORROSION_MAX_ERROR_BODY_BYTES: usize = 65_536;

const CORROSION_API_ADDR_ENV: &str = "PLOYZ_CORROSION_API_ADDR";
const CORROSION_BEARER_TOKEN_ENV: &str = "PLOYZ_CORROSION_BEARER_TOKEN";
const CLUSTER_ID_ENV: &str = "PLOYZ_CLUSTER_ID";
const MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const WIREGUARD_PRIVATE_KEY_PATH_ENV: &str = "PLOYZ_WIREGUARD_PRIVATE_KEY_PATH";
const WIREGUARD_INTERFACE_ENV: &str = "PLOYZ_WIREGUARD_INTERFACE";
const WIREGUARD_LISTEN_PORT_ENV: &str = "PLOYZ_WIREGUARD_LISTEN_PORT";
const CORROSION_GOSSIP_PORT_ENV: &str = "PLOYZ_CORROSION_GOSSIP_PORT";
const API_LISTEN_ADDR_ENV: &str = "PLOYZ_API_LISTEN_ADDR";
const WIREGUARD_MTU_ENV: &str = "PLOYZ_WIREGUARD_MTU";
const BRIDGE_INTERFACE_ENV: &str = "PLOYZ_BRIDGE_INTERFACE";
const EBPF_CTL_PATH_ENV: &str = "PLOYZ_EBPF_CTL_PATH";
const EBPF_BYTECODE_PATH_ENV: &str = "PLOYZ_EBPF_BYTECODE_PATH";
const EBPF_PIN_PATH_ENV: &str = "PLOYZ_EBPF_PIN_PATH";
const ISOLATION_CGROUP_ROOT_ENV: &str = "PLOYZ_ISOLATION_CGROUP_ROOT";
const CORROSION_VERSION_ENV: &str = "PLOYZ_CORROSION_VERSION";
const RECONCILE_INTERVAL_MS_ENV: &str = "PLOYZ_KEEPER_RECONCILE_INTERVAL_MS";
const RETRY_INITIAL_MS_ENV: &str = "PLOYZ_KEEPER_RETRY_INITIAL_MS";
const RETRY_MAX_MS_ENV: &str = "PLOYZ_KEEPER_RETRY_MAX_MS";
const HOST_COMMAND_TIMEOUT_MS_ENV: &str = "PLOYZ_KEEPER_HOST_COMMAND_TIMEOUT_MS";
const HOST_FOLD_TIMEOUT_MS_ENV: &str = "PLOYZ_KEEPER_HOST_FOLD_TIMEOUT_MS";
const SUPERVISOR_BACKEND_ENV: &str = "PLOYZ_SUPERVISOR_BACKEND";
const UPGRADE_STATE_DIR_ENV: &str = "PLOYZ_UPGRADE_STATE_DIR";
const UPGRADE_SOCKET_PATH_ENV: &str = "PLOYZ_UPGRADE_SOCKET_PATH";
const CONTROL_SOCKET_PATH_ENV: &str = "PLOYZ_CONTROL_SOCKET_PATH";

const DEFAULT_WIREGUARD_LISTEN_PORT: u16 = 51_820;
const DEFAULT_CORROSION_GOSSIP_PORT: u16 = 8_787;
const DEFAULT_BRIDGE_INTERFACE: &str = "br-ployz";
const DEFAULT_EBPF_CTL_PATH: &str = "/usr/local/bin/ployz-ebpf-ctl";
const DEFAULT_EBPF_BYTECODE_PATH: &str = "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc";
const DEFAULT_EBPF_PIN_PATH: &str = "/sys/fs/bpf/ployz";
const DEFAULT_CORROSION_VERSION: &str = "0.2.0-beta.0";
const DEFAULT_RECONCILE_INTERVAL_MS: u64 = 30_000;
const DEFAULT_RETRY_INITIAL_MS: u64 = 250;
const DEFAULT_RETRY_MAX_MS: u64 = 10_000;
const DEFAULT_HOST_COMMAND_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_HOST_FOLD_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_SUPERVISOR_BACKEND: &str = "systemd";
const DEFAULT_UPGRADE_STATE_DIR: &str = "/var/lib/ployz";
const DEFAULT_UPGRADE_SOCKET_PATH: &str = "/run/ployz/keeper-upgrade.sock";
const MAX_DIAGNOSTIC_BYTES: usize = 512;

/// Validated process-local inputs for one Keeper instance.
#[derive(Debug, Clone)]
pub struct KeeperRoleConfig {
    corrosion: CorrosionClientConfig,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    host: BuiltinWireguardHostConfig,
    isolation_host: ContainerIsolationHostConfig,
    corrosion_version: String,
    reconcile_interval: Duration,
    retry_initial: Duration,
    retry_max: Duration,
    host_fold_timeout: Duration,
    upgrade: KeeperUpgradeConfig,
    control_socket_path: PathBuf,
}

/// Validated machine-local inputs for Keeper's systemd-only upgrade handoff.
#[derive(Debug, Clone)]
pub struct KeeperUpgradeConfig {
    store: PloyzdArtifactStore,
    socket_path: PathBuf,
    supervisor: SupervisorBackend,
}

impl KeeperUpgradeConfig {
    #[must_use]
    pub const fn new(
        store: PloyzdArtifactStore,
        socket_path: PathBuf,
        supervisor: SupervisorBackend,
    ) -> Self {
        Self {
            store,
            socket_path,
            supervisor,
        }
    }
}

impl KeeperRoleConfig {
    /// Parses Keeper inputs from the supervisor-owned environment file.
    pub fn from_environment() -> Result<Self, KeeperRoleConfigError> {
        let corrosion_api_addr = required_environment(CORROSION_API_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| KeeperRoleConfigError::InvalidSocketAddress {
                name: CORROSION_API_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let bearer_token = BearerToken::new(required_environment(CORROSION_BEARER_TOKEN_ENV)?)
            .map_err(KeeperRoleConfigError::CorrosionConfig)?;
        let corrosion = CorrosionClientConfig::new(
            corrosion_api_addr,
            bearer_token,
            CorrosionClientBounds {
                connect_timeout: CORROSION_CONNECT_TIMEOUT,
                request_timeout: CORROSION_REQUEST_TIMEOUT,
                stream_idle_timeout: CORROSION_STREAM_IDLE_TIMEOUT,
                max_ndjson_frame_bytes: CORROSION_MAX_NDJSON_FRAME_BYTES,
                max_error_body_bytes: CORROSION_MAX_ERROR_BODY_BYTES,
            },
        )
        .map_err(KeeperRoleConfigError::CorrosionConfig)?;
        let cluster_id =
            ClusterId::try_new(required_environment(CLUSTER_ID_ENV)?).map_err(|error| {
                KeeperRoleConfigError::InvalidClusterId {
                    detail: error.to_string(),
                }
            })?;
        let local_machine_id = MachineRowId::try_new(required_environment(MACHINE_ID_ENV)?)
            .map_err(|error| KeeperRoleConfigError::InvalidMachineId {
                detail: error.to_string(),
            })?;
        let host_command_timeout =
            duration_environment(HOST_COMMAND_TIMEOUT_MS_ENV, DEFAULT_HOST_COMMAND_TIMEOUT_MS)?;
        let ebpf_pin_path = PathBuf::from(optional_environment(
            EBPF_PIN_PATH_ENV,
            DEFAULT_EBPF_PIN_PATH,
        )?);
        let ebpf = BuiltinWireguardEbpfConfig::try_new(
            optional_environment(BRIDGE_INTERFACE_ENV, DEFAULT_BRIDGE_INTERFACE)?,
            PathBuf::from(optional_environment(
                EBPF_CTL_PATH_ENV,
                DEFAULT_EBPF_CTL_PATH,
            )?),
            PathBuf::from(optional_environment(
                EBPF_BYTECODE_PATH_ENV,
                DEFAULT_EBPF_BYTECODE_PATH,
            )?),
            ebpf_pin_path.clone(),
        )?;
        let isolation_host = ContainerIsolationHostConfig::try_new(
            PathBuf::from(optional_environment(
                EBPF_CTL_PATH_ENV,
                DEFAULT_EBPF_CTL_PATH,
            )?),
            PathBuf::from(optional_environment(
                EBPF_BYTECODE_PATH_ENV,
                DEFAULT_EBPF_BYTECODE_PATH,
            )?),
            ebpf_pin_path,
            optional_path_environment(ISOLATION_CGROUP_ROOT_ENV, DEFAULT_ISOLATION_CGROUP_ROOT)?,
            PathBuf::from(DEFAULT_ISOLATION_ROWS_PATH),
            host_command_timeout,
        )?;
        let ports = builtin_wireguard_ports(
            u16_environment(WIREGUARD_LISTEN_PORT_ENV, DEFAULT_WIREGUARD_LISTEN_PORT)?,
            u16_environment(CORROSION_GOSSIP_PORT_ENV, DEFAULT_CORROSION_GOSSIP_PORT)?,
            required_environment(API_LISTEN_ADDR_ENV)?
                .parse::<SocketAddr>()
                .map_err(|error| KeeperRoleConfigError::InvalidSocketAddress {
                    name: API_LISTEN_ADDR_ENV,
                    detail: error.to_string(),
                })?
                .port(),
        )?;
        let supervisor = supervisor_environment()?;
        let host = BuiltinWireguardHostConfig::try_new(
            PathBuf::from(optional_environment(
                WIREGUARD_PRIVATE_KEY_PATH_ENV,
                DEFAULT_PRIVATE_KEY_PATH,
            )?),
            optional_environment(WIREGUARD_INTERFACE_ENV, DEFAULT_WIREGUARD_IFNAME)?,
            ports,
            u16_environment(WIREGUARD_MTU_ENV, DEFAULT_WIREGUARD_MTU)?,
            ebpf,
            supervisor,
            host_command_timeout,
        )?;

        let mut config = Self::new(
            corrosion,
            cluster_id,
            local_machine_id,
            KeeperHostConfig {
                mesh: host,
                isolation: isolation_host,
            },
            optional_environment(CORROSION_VERSION_ENV, DEFAULT_CORROSION_VERSION)?,
            KeeperTimingConfig {
                reconcile_interval: duration_environment(
                    RECONCILE_INTERVAL_MS_ENV,
                    DEFAULT_RECONCILE_INTERVAL_MS,
                )?,
                retry_initial: duration_environment(
                    RETRY_INITIAL_MS_ENV,
                    DEFAULT_RETRY_INITIAL_MS,
                )?,
                retry_max: duration_environment(RETRY_MAX_MS_ENV, DEFAULT_RETRY_MAX_MS)?,
                host_fold_timeout: duration_environment(
                    HOST_FOLD_TIMEOUT_MS_ENV,
                    DEFAULT_HOST_FOLD_TIMEOUT_MS,
                )?,
            },
            KeeperUpgradeConfig::new(
                PloyzdArtifactStore::new(optional_path_environment(
                    UPGRADE_STATE_DIR_ENV,
                    DEFAULT_UPGRADE_STATE_DIR,
                )?)
                .map_err(KeeperRoleConfigError::UpgradeStore)?,
                optional_path_environment(UPGRADE_SOCKET_PATH_ENV, DEFAULT_UPGRADE_SOCKET_PATH)?,
                supervisor,
            ),
        )?;
        config.control_socket_path =
            optional_path_environment(CONTROL_SOCKET_PATH_ENV, CONTROL_SOCKET_PATH)?;
        Ok(config)
    }

    /// Builds Keeper configuration without touching the process environment.
    pub fn new(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        hosts: KeeperHostConfig,
        corrosion_version: String,
        timing: KeeperTimingConfig,
        upgrade: KeeperUpgradeConfig,
    ) -> Result<Self, KeeperRoleConfigError> {
        let KeeperHostConfig {
            mesh: host,
            isolation: isolation_host,
        } = hosts;
        validate_diagnostic(CORROSION_VERSION_ENV, &corrosion_version)?;
        if timing.reconcile_interval.is_zero() {
            return Err(KeeperRoleConfigError::ZeroDuration {
                name: RECONCILE_INTERVAL_MS_ENV,
            });
        }
        if timing.retry_initial.is_zero() {
            return Err(KeeperRoleConfigError::ZeroDuration {
                name: RETRY_INITIAL_MS_ENV,
            });
        }
        if timing.retry_max < timing.retry_initial {
            return Err(KeeperRoleConfigError::RetryCapBeforeInitial {
                initial: timing.retry_initial,
                maximum: timing.retry_max,
            });
        }
        if timing.host_fold_timeout.is_zero() {
            return Err(KeeperRoleConfigError::ZeroDuration {
                name: HOST_FOLD_TIMEOUT_MS_ENV,
            });
        }
        validate_absolute_path(UPGRADE_SOCKET_PATH_ENV, &upgrade.socket_path)?;
        let control_socket_path = PathBuf::from(CONTROL_SOCKET_PATH);
        validate_absolute_path(CONTROL_SOCKET_PATH_ENV, &control_socket_path)?;

        Ok(Self {
            corrosion,
            cluster_id,
            local_machine_id,
            host,
            isolation_host,
            corrosion_version,
            reconcile_interval: timing.reconcile_interval,
            retry_initial: timing.retry_initial,
            retry_max: timing.retry_max,
            host_fold_timeout: timing.host_fold_timeout,
            upgrade,
            control_socket_path,
        })
    }

    #[must_use]
    pub fn corrosion(&self) -> &CorrosionClientConfig {
        &self.corrosion
    }
    #[must_use]
    pub fn cluster_id(&self) -> &ClusterId {
        &self.cluster_id
    }
    #[must_use]
    pub fn local_machine_id(&self) -> &MachineRowId {
        &self.local_machine_id
    }
    #[must_use]
    pub fn host(&self) -> &BuiltinWireguardHostConfig {
        &self.host
    }
    #[must_use]
    pub fn isolation_host(&self) -> &ContainerIsolationHostConfig {
        &self.isolation_host
    }
    #[must_use]
    pub fn corrosion_version(&self) -> &str {
        &self.corrosion_version
    }
    #[must_use]
    pub const fn reconcile_interval(&self) -> Duration {
        self.reconcile_interval
    }
    #[must_use]
    pub const fn retry_initial(&self) -> Duration {
        self.retry_initial
    }
    #[must_use]
    pub const fn retry_max(&self) -> Duration {
        self.retry_max
    }
    #[must_use]
    pub const fn host_fold_timeout(&self) -> Duration {
        self.host_fold_timeout
    }

    #[must_use]
    pub fn upgrade_store(&self) -> &PloyzdArtifactStore {
        &self.upgrade.store
    }
    #[must_use]
    pub fn upgrade_socket_path(&self) -> &std::path::Path {
        &self.upgrade.socket_path
    }
    #[must_use]
    pub fn control_socket_path(&self) -> &std::path::Path {
        &self.control_socket_path
    }
    #[must_use]
    pub const fn supervisor(&self) -> SupervisorBackend {
        self.upgrade.supervisor
    }
}

fn builtin_wireguard_ports(
    wireguard_listen: u16,
    corrosion_gossip: u16,
    api_http: u16,
) -> Result<BuiltinWireguardPorts, BuiltinWireguardConfigError> {
    BuiltinWireguardPorts::try_new(wireguard_listen, corrosion_gossip, api_http, JOIN_DOOR_PORT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeeperTimingConfig {
    pub reconcile_interval: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub host_fold_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperHostConfig {
    pub mesh: BuiltinWireguardHostConfig,
    pub isolation: ContainerIsolationHostConfig,
}

fn required_environment(name: &'static str) -> Result<String, KeeperRoleConfigError> {
    env::var(name).map_err(|error| match error {
        env::VarError::NotPresent => KeeperRoleConfigError::MissingEnvironment { name },
        env::VarError::NotUnicode(_) => KeeperRoleConfigError::NonUnicodeEnvironment { name },
    })
}

fn optional_environment(
    name: &'static str,
    default: &'static str,
) -> Result<String, KeeperRoleConfigError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.to_owned()),
        Err(env::VarError::NotUnicode(_)) => {
            Err(KeeperRoleConfigError::NonUnicodeEnvironment { name })
        }
    }
}

fn optional_path_environment(
    name: &'static str,
    default: &'static str,
) -> Result<PathBuf, KeeperRoleConfigError> {
    let path = PathBuf::from(optional_environment(name, default)?);
    validate_absolute_path(name, &path)?;
    Ok(path)
}

fn validate_absolute_path(
    name: &'static str,
    path: &std::path::Path,
) -> Result<(), KeeperRoleConfigError> {
    if path.as_os_str().is_empty() {
        Err(KeeperRoleConfigError::EmptyPath { name })
    } else if !path.is_absolute() {
        Err(KeeperRoleConfigError::RelativePath {
            name,
            value: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn u16_environment(name: &'static str, default: u16) -> Result<u16, KeeperRoleConfigError> {
    parsed_environment(name, default)
}

fn duration_environment(
    name: &'static str,
    default_ms: u64,
) -> Result<Duration, KeeperRoleConfigError> {
    let milliseconds = parsed_environment::<u64>(name, default_ms)?;
    if milliseconds == 0 {
        return Err(KeeperRoleConfigError::ZeroDuration { name });
    }
    Ok(Duration::from_millis(milliseconds))
}

fn parsed_environment<T>(name: &'static str, default: T) -> Result<T, KeeperRoleConfigError>
where
    T: std::str::FromStr + ToString,
    T::Err: std::fmt::Display,
{
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => default.to_string(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(KeeperRoleConfigError::NonUnicodeEnvironment { name });
        }
    };
    value
        .parse::<T>()
        .map_err(|error| KeeperRoleConfigError::InvalidInteger {
            name,
            detail: error.to_string(),
        })
}

fn validate_diagnostic(name: &'static str, value: &str) -> Result<(), KeeperRoleConfigError> {
    if value.trim().is_empty()
        || value.len() > MAX_DIAGNOSTIC_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(KeeperRoleConfigError::InvalidDiagnostic { name });
    }
    Ok(())
}

fn supervisor_environment() -> Result<SupervisorBackend, KeeperRoleConfigError> {
    let value = optional_environment(SUPERVISOR_BACKEND_ENV, DEFAULT_SUPERVISOR_BACKEND)?;
    match value.as_str() {
        "systemd" => Ok(SupervisorBackend::Systemd),
        "openrc" => Ok(SupervisorBackend::OpenRc),
        _ => Err(KeeperRoleConfigError::InvalidSupervisorBackend { value }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeeperRoleConfigError {
    #[error("required Keeper role environment variable {name} is missing")]
    MissingEnvironment { name: &'static str },
    #[error("Keeper role environment variable {name} is not valid Unicode")]
    NonUnicodeEnvironment { name: &'static str },
    #[error("Keeper role environment variable {name} is not a socket address: {detail}")]
    InvalidSocketAddress { name: &'static str, detail: String },
    #[error("PLOYZ_CLUSTER_ID is invalid: {detail}")]
    InvalidClusterId { detail: String },
    #[error("PLOYZ_MACHINE_ID is invalid: {detail}")]
    InvalidMachineId { detail: String },
    #[error("Keeper integer from {name} is invalid: {detail}")]
    InvalidInteger { name: &'static str, detail: String },
    #[error("Keeper duration from {name} must be nonzero")]
    ZeroDuration { name: &'static str },
    #[error("Keeper retry maximum {maximum:?} is shorter than initial delay {initial:?}")]
    RetryCapBeforeInitial {
        initial: Duration,
        maximum: Duration,
    },
    #[error(
        "Keeper diagnostic from {name} must be nonempty, at most 512 bytes, and contain no control characters"
    )]
    InvalidDiagnostic { name: &'static str },
    #[error("invalid local Corrosion configuration: {0}")]
    CorrosionConfig(#[source] crate::corrosion::CorrosionClientConfigError),
    #[error("invalid builtin WireGuard host configuration: {0}")]
    HostConfiguration(#[from] BuiltinWireguardConfigError),
    #[error("invalid container isolation host configuration: {0}")]
    IsolationHostConfiguration(#[from] ContainerIsolationConfigError),
    #[error("PLOYZ_SUPERVISOR_BACKEND must be systemd or openrc, got {value:?}")]
    InvalidSupervisorBackend { value: String },
    #[error("Keeper path from {name} must not be empty")]
    EmptyPath { name: &'static str },
    #[error("Keeper path from {name} must be absolute: {value:?}")]
    RelativePath { name: &'static str, value: PathBuf },
    #[error("invalid local upgrade artifact store: {0}")]
    UpgradeStore(#[source] ArtifactStoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeper_ports_take_the_join_door_port_from_core() {
        let ports = builtin_wireguard_ports(51_820, 8_787, 2_020).expect("valid fixed ports");

        assert!(format!("{ports:?}").contains(&format!("join_door_https: {JOIN_DOOR_PORT}")));
    }

    #[test]
    fn isolation_cgroup_root_default_is_absolute_and_relative_values_are_rejected() {
        assert!(std::path::Path::new(DEFAULT_ISOLATION_CGROUP_ROOT).is_absolute());
        assert!(matches!(
            validate_absolute_path(
                ISOLATION_CGROUP_ROOT_ENV,
                std::path::Path::new("sys/fs/cgroup")
            ),
            Err(KeeperRoleConfigError::RelativePath {
                name: ISOLATION_CGROUP_ROOT_ENV,
                ..
            })
        ));
    }
}
