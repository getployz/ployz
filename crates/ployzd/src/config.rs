use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    state_dir: PathBuf,
    corrosion: polis::CorrosionAgentConfig,
    peer_identity_path: PathBuf,
    peer_boot_timeout: Duration,
    schema_timeout: polis::StoreTimeout,
    peer_shutdown_timeout: Duration,
    corrosion_shutdown_timeout: Duration,
    corrosion_start_mode: CorrosionStartMode,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DaemonConfigError {
    #[error("environment variable {name} must be a socket address, got {value:?}: {source}")]
    InvalidSocketAddress {
        name: &'static str,
        value: String,
        source: std::net::AddrParseError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CorrosionStartMode {
    StartOrAdopt,
    #[cfg(test)]
    StartManaged,
    #[cfg(test)]
    AdoptExisting,
}

impl DaemonConfig {
    #[must_use]
    pub fn for_state_dir(
        state_dir: impl Into<PathBuf>,
        corrosion_addresses: polis::CorrosionAgentAddresses,
    ) -> Self {
        let state_dir = state_dir.into();
        let corrosion = polis::CorrosionAgentConfig::for_root_dir(
            state_dir.join("corrosion"),
            corrosion_addresses,
        )
        .with_owner_id(polis::CorrosionOwnerId::parse("ployzd").expect("ployzd owner id"))
        .with_schema_file("membership.sql", polis::membership_replication_schema_sql());
        Self {
            corrosion,
            peer_identity_path: state_dir.join("peer.key"),
            state_dir,
            peer_boot_timeout: Duration::from_secs(5),
            schema_timeout: polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            peer_shutdown_timeout: Duration::from_secs(5),
            corrosion_shutdown_timeout: Duration::from_secs(5),
            corrosion_start_mode: CorrosionStartMode::StartOrAdopt,
        }
    }

    pub fn from_env_defaults() -> Result<Self, DaemonConfigError> {
        Ok(Self::for_state_dir(
            default_state_dir(),
            default_corrosion_addresses()?,
        ))
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test_state_dir(
        state_dir: impl Into<PathBuf>,
        corrosion_addresses: polis::CorrosionAgentAddresses,
    ) -> Self {
        Self::for_state_dir(state_dir, corrosion_addresses)
    }

    #[must_use]
    pub fn with_corrosion_binary_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.corrosion = self.corrosion.with_binary_path(path);
        self
    }

    #[must_use]
    pub fn with_corrosion_bootstrap(mut self, bootstrap: Vec<SocketAddr>) -> Self {
        self.corrosion = self.corrosion.with_bootstrap(bootstrap);
        self
    }

    #[must_use]
    pub fn with_readiness_timeout(mut self, timeout: Duration) -> Self {
        self.corrosion = self.corrosion.with_readiness_timeout(timeout);
        self
    }

    #[must_use]
    pub fn with_peer_boot_timeout(mut self, timeout: Duration) -> Self {
        self.peer_boot_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_peer_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.peer_shutdown_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_corrosion_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.corrosion_shutdown_timeout = timeout;
        self
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_corrosion_start_mode(mut self, mode: CorrosionStartMode) -> Self {
        self.corrosion_start_mode = mode;
        self
    }

    pub(crate) fn with_persisted_corrosion_addresses(
        mut self,
    ) -> Result<Self, polis::CorrosionAgentError> {
        self.corrosion = self.corrosion.load_or_persist_addresses()?;
        Ok(self)
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    #[must_use]
    pub fn corrosion_config(&self) -> &polis::CorrosionAgentConfig {
        &self.corrosion
    }

    #[must_use]
    pub fn corrosion_binary_path(&self) -> &Path {
        self.corrosion.binary_path()
    }

    #[must_use]
    pub fn corrosion_root_dir(&self) -> &Path {
        self.corrosion.root_dir()
    }

    #[must_use]
    pub fn peer_identity_path(&self) -> &Path {
        &self.peer_identity_path
    }

    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.corrosion.api_addr()
    }

    #[must_use]
    pub fn gossip_addr(&self) -> SocketAddr {
        self.corrosion.gossip_addr()
    }

    #[must_use]
    pub fn prometheus_addr(&self) -> SocketAddr {
        self.corrosion.prometheus_addr()
    }

    #[must_use]
    pub fn corrosion_addresses(&self) -> polis::CorrosionAgentAddresses {
        polis::CorrosionAgentAddresses::new(
            self.api_addr(),
            self.gossip_addr(),
            self.prometheus_addr(),
        )
    }

    #[must_use]
    pub fn readiness_timeout(&self) -> Duration {
        self.corrosion.readiness_timeout()
    }

    #[must_use]
    pub fn peer_boot_timeout(&self) -> Duration {
        self.peer_boot_timeout
    }

    #[must_use]
    pub fn schema_timeout(&self) -> polis::StoreTimeout {
        self.schema_timeout
    }

    #[must_use]
    pub fn peer_shutdown_timeout(&self) -> Duration {
        self.peer_shutdown_timeout
    }

    #[must_use]
    pub fn corrosion_shutdown_timeout(&self) -> Duration {
        self.corrosion_shutdown_timeout
    }

    #[must_use]
    pub(crate) fn corrosion_start_mode(&self) -> CorrosionStartMode {
        self.corrosion_start_mode
    }
}

fn default_state_dir() -> PathBuf {
    if let Some(value) = std::env::var_os("PLOYZ_STATE_DIR") {
        return PathBuf::from(value);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".ployz");
    }
    PathBuf::from(".ployz")
}

fn default_corrosion_addresses() -> Result<polis::CorrosionAgentAddresses, DaemonConfigError> {
    Ok(polis::CorrosionAgentAddresses::new(
        env_socket_addr("PLOYZ_CORROSION_API_ADDR", "127.0.0.1:9781")?,
        env_socket_addr("PLOYZ_CORROSION_GOSSIP_ADDR", "127.0.0.1:9782")?,
        env_socket_addr("PLOYZ_CORROSION_PROMETHEUS_ADDR", "127.0.0.1:9783")?,
    ))
}

fn env_socket_addr(
    name: &'static str,
    default: &'static str,
) -> Result<SocketAddr, DaemonConfigError> {
    socket_addr_from_env_value(name, std::env::var_os(name), default)
}

fn socket_addr_from_env_value(
    name: &'static str,
    value: Option<OsString>,
    default: &'static str,
) -> Result<SocketAddr, DaemonConfigError> {
    let Some(value) = value else {
        return Ok(parse_builtin_socket_addr(default));
    };
    let value = value.to_string_lossy().into_owned();
    value
        .parse()
        .map_err(|source| DaemonConfigError::InvalidSocketAddress {
            name,
            value,
            source,
        })
}

fn parse_builtin_socket_addr(value: &'static str) -> SocketAddr {
    match value.parse() {
        Ok(address) => address,
        Err(error) => panic!("invalid built-in socket address {value}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provided_invalid_corrosion_address_is_config_error() {
        let error = socket_addr_from_env_value(
            "PLOYZ_CORROSION_API_ADDR",
            Some(OsString::from("not-a-socket")),
            "127.0.0.1:9781",
        )
        .expect_err("invalid env value");

        assert!(matches!(
            error,
            DaemonConfigError::InvalidSocketAddress {
                name: "PLOYZ_CORROSION_API_ADDR",
                ..
            }
        ));
    }
}
