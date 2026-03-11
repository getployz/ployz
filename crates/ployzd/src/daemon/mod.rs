pub mod handlers;
mod setup;
pub mod ssh;

use std::path::{Path, PathBuf};

use crate::config::Mode;
use crate::deploy::NamespaceLockManager;
use crate::deploy::remote::RemoteControlHandle;
use crate::mesh::orchestrator::Mesh;
use crate::node::identity::Identity;
use crate::services::dns::DnsHandle;
use crate::services::gateway::GatewayHandle;
use crate::store::network::NetworkConfig;
use ployz_sdk::transport::DaemonResponse;

pub struct ActiveMesh {
    pub config: NetworkConfig,
    pub mesh: Mesh,
    pub remote_control: RemoteControlHandle,
    pub gateway: GatewayHandle,
    pub dns: DnsHandle,
}

pub struct DaemonState {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub mode: Mode,
    pub cluster_cidr: String,
    pub subnet_prefix_len: u8,
    pub remote_control_port: u16,
    pub gateway_listen_addr: String,
    pub gateway_threads: usize,
    pub active: Option<ActiveMesh>,
    pub namespace_locks: NamespaceLockManager,
}

impl DaemonState {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: &Path,
        identity: Identity,
        mode: Mode,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        remote_control_port: u16,
        gateway_listen_addr: String,
        gateway_threads: usize,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            mode,
            cluster_cidr,
            subnet_prefix_len,
            remote_control_port,
            gateway_listen_addr,
            gateway_threads,
            active: None,
            namespace_locks: NamespaceLockManager::default(),
        }
    }

    pub fn active_marker_path(&self) -> PathBuf {
        self.data_dir.join("active_network")
    }

    pub fn network_dir(&self, network: &str) -> PathBuf {
        NetworkConfig::dir(&self.data_dir, network)
    }

    pub fn read_active_marker(&self) -> Option<String> {
        NetworkConfig::read_active_network(&self.data_dir)
    }

    pub fn write_active_marker(&self, network: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::write(self.active_marker_path(), network)
    }

    pub fn clear_active_marker(&self) {
        let _ = std::fs::remove_file(self.active_marker_path());
    }

    pub fn ok(&self, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: message.into(),
        }
    }

    pub fn err(&self, code: &str, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse {
            ok: false,
            code: code.into(),
            message: message.into(),
        }
    }
}
