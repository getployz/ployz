//! Environment-backed configuration for the Keeper role.

use std::env;
use std::net::SocketAddr;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_host_runner::SupervisorBackend;

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
const WIREGUARD_MTU_ENV: &str = "PLOYZ_WIREGUARD_MTU";
const BRIDGE_INTERFACE_ENV: &str = "PLOYZ_BRIDGE_INTERFACE";
const EBPF_CTL_PATH_ENV: &str = "PLOYZ_EBPF_CTL_PATH";
const EBPF_BYTECODE_PATH_ENV: &str = "PLOYZ_EBPF_BYTECODE_PATH";
const EBPF_PIN_PATH_ENV: &str = "PLOYZ_EBPF_PIN_PATH";
const CORROSION_VERSION_ENV: &str = "PLOYZ_CORROSION_VERSION";
const RECONCILE_INTERVAL_MS_ENV: &str = "PLOYZ_KEEPER_RECONCILE_INTERVAL_MS";
const RETRY_INITIAL_MS_ENV: &str = "PLOYZ_KEEPER_RETRY_INITIAL_MS";
const RETRY_MAX_MS_ENV: &str = "PLOYZ_KEEPER_RETRY_MAX_MS";
const HOST_COMMAND_TIMEOUT_MS_ENV: &str = "PLOYZ_KEEPER_HOST_COMMAND_TIMEOUT_MS";
const HOST_FOLD_TIMEOUT_MS_ENV: &str = "PLOYZ_KEEPER_HOST_FOLD_TIMEOUT_MS";
const SUPERVISOR_BACKEND_ENV: &str = "PLOYZ_SUPERVISOR_BACKEND";

const DEFAULT_WIREGUARD_PRIVATE_KEY_PATH: &str = "/etc/ployz/wireguard.key";
const DEFAULT_WIREGUARD_INTERFACE: &str = "ployz0";
const DEFAULT_WIREGUARD_LISTEN_PORT: u16 = 51_820;
const DEFAULT_WIREGUARD_MTU: u16 = 1_420;
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
const MAX_DIAGNOSTIC_BYTES: usize = 512;

/// Validated process-local inputs for one Keeper instance.
#[derive(Debug, Clone)]
pub struct KeeperRoleConfig {
    corrosion: CorrosionClientConfig,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    private_key_path: PathBuf,
    wireguard_interface: String,
    wireguard_listen_port: NonZeroU16,
    wireguard_mtu: u16,
    bridge_interface: String,
    ebpf_ctl_path: PathBuf,
    ebpf_bytecode_path: PathBuf,
    ebpf_pin_path: PathBuf,
    corrosion_version: String,
    reconcile_interval: Duration,
    retry_initial: Duration,
    retry_max: Duration,
    host_command_timeout: Duration,
    host_fold_timeout: Duration,
    supervisor_backend: SupervisorBackend,
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

        Self::new(
            corrosion,
            cluster_id,
            local_machine_id,
            KeeperHostConfig {
                private_key_path: absolute_path_environment(
                    WIREGUARD_PRIVATE_KEY_PATH_ENV,
                    DEFAULT_WIREGUARD_PRIVATE_KEY_PATH,
                )?,
                wireguard_interface: interface_environment(
                    WIREGUARD_INTERFACE_ENV,
                    DEFAULT_WIREGUARD_INTERFACE,
                )?,
                wireguard_listen_port: nonzero_u16_environment(
                    WIREGUARD_LISTEN_PORT_ENV,
                    DEFAULT_WIREGUARD_LISTEN_PORT,
                )?,
                wireguard_mtu: u16_environment(WIREGUARD_MTU_ENV, DEFAULT_WIREGUARD_MTU)?,
                bridge_interface: interface_environment(
                    BRIDGE_INTERFACE_ENV,
                    DEFAULT_BRIDGE_INTERFACE,
                )?,
                ebpf_ctl_path: absolute_path_environment(EBPF_CTL_PATH_ENV, DEFAULT_EBPF_CTL_PATH)?,
                ebpf_bytecode_path: absolute_path_environment(
                    EBPF_BYTECODE_PATH_ENV,
                    DEFAULT_EBPF_BYTECODE_PATH,
                )?,
                ebpf_pin_path: absolute_path_environment(EBPF_PIN_PATH_ENV, DEFAULT_EBPF_PIN_PATH)?,
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
                host_command_timeout: duration_environment(
                    HOST_COMMAND_TIMEOUT_MS_ENV,
                    DEFAULT_HOST_COMMAND_TIMEOUT_MS,
                )?,
                host_fold_timeout: duration_environment(
                    HOST_FOLD_TIMEOUT_MS_ENV,
                    DEFAULT_HOST_FOLD_TIMEOUT_MS,
                )?,
            },
            supervisor_environment()?,
        )
    }

    /// Builds Keeper configuration without touching the process environment.
    pub fn new(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        host: KeeperHostConfig,
        corrosion_version: String,
        timing: KeeperTimingConfig,
        supervisor_backend: SupervisorBackend,
    ) -> Result<Self, KeeperRoleConfigError> {
        let KeeperHostConfig {
            private_key_path,
            wireguard_interface,
            wireguard_listen_port,
            wireguard_mtu,
            bridge_interface,
            ebpf_ctl_path,
            ebpf_bytecode_path,
            ebpf_pin_path,
        } = host;
        validate_absolute_path(WIREGUARD_PRIVATE_KEY_PATH_ENV, &private_key_path)?;
        validate_interface(WIREGUARD_INTERFACE_ENV, &wireguard_interface)?;
        if wireguard_mtu < 1_280 {
            return Err(KeeperRoleConfigError::WireguardMtuTooSmall { wireguard_mtu });
        }
        validate_interface(BRIDGE_INTERFACE_ENV, &bridge_interface)?;
        validate_absolute_path(EBPF_CTL_PATH_ENV, &ebpf_ctl_path)?;
        validate_absolute_path(EBPF_BYTECODE_PATH_ENV, &ebpf_bytecode_path)?;
        validate_absolute_path(EBPF_PIN_PATH_ENV, &ebpf_pin_path)?;
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
        if timing.host_command_timeout.is_zero() {
            return Err(KeeperRoleConfigError::ZeroDuration {
                name: HOST_COMMAND_TIMEOUT_MS_ENV,
            });
        }
        if timing.host_fold_timeout.is_zero() {
            return Err(KeeperRoleConfigError::ZeroDuration {
                name: HOST_FOLD_TIMEOUT_MS_ENV,
            });
        }

        Ok(Self {
            corrosion,
            cluster_id,
            local_machine_id,
            private_key_path,
            wireguard_interface,
            wireguard_listen_port,
            wireguard_mtu,
            bridge_interface,
            ebpf_ctl_path,
            ebpf_bytecode_path,
            ebpf_pin_path,
            corrosion_version,
            reconcile_interval: timing.reconcile_interval,
            retry_initial: timing.retry_initial,
            retry_max: timing.retry_max,
            host_command_timeout: timing.host_command_timeout,
            host_fold_timeout: timing.host_fold_timeout,
            supervisor_backend,
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
    pub fn private_key_path(&self) -> &Path {
        &self.private_key_path
    }
    #[must_use]
    pub fn wireguard_interface(&self) -> &str {
        &self.wireguard_interface
    }
    #[must_use]
    pub const fn wireguard_listen_port(&self) -> NonZeroU16 {
        self.wireguard_listen_port
    }
    #[must_use]
    pub const fn wireguard_mtu(&self) -> u16 {
        self.wireguard_mtu
    }
    #[must_use]
    pub fn bridge_interface(&self) -> &str {
        &self.bridge_interface
    }
    #[must_use]
    pub fn ebpf_ctl_path(&self) -> &Path {
        &self.ebpf_ctl_path
    }
    #[must_use]
    pub fn ebpf_bytecode_path(&self) -> &Path {
        &self.ebpf_bytecode_path
    }
    #[must_use]
    pub fn ebpf_pin_path(&self) -> &Path {
        &self.ebpf_pin_path
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
    pub const fn host_command_timeout(&self) -> Duration {
        self.host_command_timeout
    }
    #[must_use]
    pub const fn host_fold_timeout(&self) -> Duration {
        self.host_fold_timeout
    }
    #[must_use]
    pub const fn supervisor_backend(&self) -> SupervisorBackend {
        self.supervisor_backend
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperHostConfig {
    pub private_key_path: PathBuf,
    pub wireguard_interface: String,
    pub wireguard_listen_port: NonZeroU16,
    pub wireguard_mtu: u16,
    pub bridge_interface: String,
    pub ebpf_ctl_path: PathBuf,
    pub ebpf_bytecode_path: PathBuf,
    pub ebpf_pin_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeeperTimingConfig {
    pub reconcile_interval: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub host_command_timeout: Duration,
    pub host_fold_timeout: Duration,
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

fn absolute_path_environment(
    name: &'static str,
    default: &'static str,
) -> Result<PathBuf, KeeperRoleConfigError> {
    let path = PathBuf::from(optional_environment(name, default)?);
    validate_absolute_path(name, &path)?;
    Ok(path)
}

fn validate_absolute_path(name: &'static str, path: &Path) -> Result<(), KeeperRoleConfigError> {
    if !path.is_absolute() {
        return Err(KeeperRoleConfigError::PathNotAbsolute {
            name,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn interface_environment(
    name: &'static str,
    default: &'static str,
) -> Result<String, KeeperRoleConfigError> {
    let value = optional_environment(name, default)?;
    validate_interface(name, &value)?;
    Ok(value)
}

fn validate_interface(name: &'static str, value: &str) -> Result<(), KeeperRoleConfigError> {
    if value.is_empty()
        || value.len() > 15
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(KeeperRoleConfigError::InvalidInterface {
            name,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn nonzero_u16_environment(
    name: &'static str,
    default: u16,
) -> Result<NonZeroU16, KeeperRoleConfigError> {
    let value = parsed_environment::<u16>(name, default)?;
    NonZeroU16::new(value).ok_or(KeeperRoleConfigError::ZeroInteger { name })
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
    #[error("Keeper path from {name} must be absolute, got {path:?}")]
    PathNotAbsolute { name: &'static str, path: PathBuf },
    #[error(
        "Keeper interface from {name} must be a nonempty Linux interface name of at most 15 bytes, got {value:?}"
    )]
    InvalidInterface { name: &'static str, value: String },
    #[error("Keeper integer from {name} is invalid: {detail}")]
    InvalidInteger { name: &'static str, detail: String },
    #[error("Keeper integer from {name} must be nonzero")]
    ZeroInteger { name: &'static str },
    #[error("WireGuard MTU {wireguard_mtu} cannot carry IPv6; minimum is 1280")]
    WireguardMtuTooSmall { wireguard_mtu: u16 },
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
    #[error("PLOYZ_SUPERVISOR_BACKEND must be systemd or openrc, got {value:?}")]
    InvalidSupervisorBackend { value: String },
}
