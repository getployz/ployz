use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    #[cfg(test)]
    corrosion_fixture: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrosionStartMode {
    StartOrAdopt,
    StartManaged,
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
        .with_schema_file("membership.sql", polis::membership_startup_schema_sql());
        Self {
            corrosion,
            peer_identity_path: state_dir.join("peer.key"),
            state_dir,
            peer_boot_timeout: Duration::from_secs(5),
            schema_timeout: polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            peer_shutdown_timeout: Duration::from_secs(5),
            corrosion_shutdown_timeout: Duration::from_secs(5),
            corrosion_start_mode: CorrosionStartMode::StartOrAdopt,
            #[cfg(test)]
            corrosion_fixture: false,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test_state_dir(
        state_dir: impl Into<PathBuf>,
        corrosion_addresses: polis::CorrosionAgentAddresses,
    ) -> Self {
        let mut config = Self::for_state_dir(state_dir, corrosion_addresses);
        config.corrosion_fixture = true;
        config
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
    pub fn with_corrosion_start_mode(mut self, mode: CorrosionStartMode) -> Self {
        self.corrosion_start_mode = mode;
        self
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
    pub fn corrosion_start_mode(&self) -> CorrosionStartMode {
        self.corrosion_start_mode
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn use_corrosion_fixture(&self) -> bool {
        self.corrosion_fixture
    }
}
