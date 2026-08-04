use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::SupervisorBackend;

const MAX_HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinWireguardPorts {
    pub(super) listen: NonZeroU16,
    pub(super) corrosion_gossip: NonZeroU16,
    pub(super) api_http: NonZeroU16,
}

impl BuiltinWireguardPorts {
    pub fn try_new(
        listen: u16,
        corrosion_gossip: u16,
        api_http: u16,
    ) -> Result<Self, BuiltinWireguardConfigError> {
        let Some(listen) = NonZeroU16::new(listen) else {
            return Err(BuiltinWireguardConfigError::ZeroListenPort);
        };
        let Some(corrosion_gossip) = NonZeroU16::new(corrosion_gossip) else {
            return Err(BuiltinWireguardConfigError::ZeroCorrosionGossipPort);
        };
        let Some(api_http) = NonZeroU16::new(api_http) else {
            return Err(BuiltinWireguardConfigError::ZeroApiHttpPort);
        };
        Ok(Self {
            listen,
            corrosion_gossip,
            api_http,
        })
    }
}

/// Validated local-effect configuration. The bridge is supplied by the
/// separate bridge owner; this module reports its absence without inventing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardHostConfig {
    pub(super) private_key_path: PathBuf,
    pub(super) wg_ifname: String,
    pub(super) ports: BuiltinWireguardPorts,
    pub(super) mtu: u16,
    pub(super) ebpf: BuiltinWireguardEbpfConfig,
    pub(super) supervisor: SupervisorBackend,
    pub(super) command_timeout: Duration,
}

impl BuiltinWireguardHostConfig {
    pub fn try_new(
        private_key_path: PathBuf,
        wg_ifname: String,
        ports: BuiltinWireguardPorts,
        mtu: u16,
        ebpf: BuiltinWireguardEbpfConfig,
        supervisor: SupervisorBackend,
        command_timeout: Duration,
    ) -> Result<Self, BuiltinWireguardConfigError> {
        validate_path("private key", &private_key_path)?;
        validate_ifname("WireGuard", &wg_ifname)?;
        if !(1_280..=9_000).contains(&mtu) {
            return Err(BuiltinWireguardConfigError::InvalidMtu { mtu });
        }
        if command_timeout.is_zero() {
            return Err(BuiltinWireguardConfigError::ZeroCommandTimeout);
        }
        if command_timeout > MAX_HOST_COMMAND_TIMEOUT {
            return Err(BuiltinWireguardConfigError::CommandTimeoutTooLong {
                milliseconds: command_timeout.as_millis(),
            });
        }
        Ok(Self {
            private_key_path,
            wg_ifname,
            ports,
            mtu,
            ebpf,
            supervisor,
            command_timeout,
        })
    }

    #[must_use]
    pub const fn command_timeout(&self) -> Duration {
        self.command_timeout
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardEbpfConfig {
    pub(super) bridge_ifname: String,
    pub(super) ctl_path: PathBuf,
    pub(super) bytecode_path: PathBuf,
    pub(super) pin_path: PathBuf,
}

impl BuiltinWireguardEbpfConfig {
    pub fn try_new(
        bridge_ifname: String,
        ctl_path: PathBuf,
        bytecode_path: PathBuf,
        pin_path: PathBuf,
    ) -> Result<Self, BuiltinWireguardConfigError> {
        validate_ifname("bridge", &bridge_ifname)?;
        validate_path("eBPF control program", &ctl_path)?;
        validate_path("eBPF bytecode", &bytecode_path)?;
        validate_path("eBPF pin", &pin_path)?;
        Ok(Self {
            bridge_ifname,
            ctl_path,
            bytecode_path,
            pin_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuiltinWireguardConfigError {
    #[error("{field} path must not be empty")]
    EmptyPath { field: &'static str },
    #[error("{field} path must be absolute: {value:?}")]
    RelativePath { field: &'static str, value: PathBuf },
    #[error("{field} interface name is invalid: {value:?}")]
    InvalidInterfaceName { field: &'static str, value: String },
    #[error("WireGuard listen port must not be zero")]
    ZeroListenPort,
    #[error("Corrosion gossip port must not be zero")]
    ZeroCorrosionGossipPort,
    #[error("API HTTP port must not be zero")]
    ZeroApiHttpPort,
    #[error("WireGuard MTU must be between 1280 and 9000, got {mtu}")]
    InvalidMtu { mtu: u16 },
    #[error("host command timeout must not be zero")]
    ZeroCommandTimeout,
    #[error("host command timeout must not exceed 60 seconds, got {milliseconds}ms")]
    CommandTimeoutTooLong { milliseconds: u128 },
}

fn validate_path(field: &'static str, path: &Path) -> Result<(), BuiltinWireguardConfigError> {
    if path.as_os_str().is_empty() {
        Err(BuiltinWireguardConfigError::EmptyPath { field })
    } else if !path.is_absolute() {
        Err(BuiltinWireguardConfigError::RelativePath {
            field,
            value: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn validate_ifname(field: &'static str, value: &str) -> Result<(), BuiltinWireguardConfigError> {
    let valid = !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(BuiltinWireguardConfigError::InvalidInterfaceName {
            field,
            value: value.to_owned(),
        })
    }
}
