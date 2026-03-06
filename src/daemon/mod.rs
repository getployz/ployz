pub mod handlers;
mod setup;
pub mod ssh;

use std::path::{Path, PathBuf};

use crate::config::Mode;
use crate::mesh::orchestrator::Mesh;
use crate::node::identity::Identity;
use crate::store::network::NetworkConfig;
use crate::transport::DaemonResponse;

pub struct ActiveMesh {
    pub config: NetworkConfig,
    pub mesh: Mesh,
}

pub struct DaemonState {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub mode: Mode,
    pub cluster_cidr: String,
    pub subnet_prefix_len: u8,
    pub active: Option<ActiveMesh>,
}

impl DaemonState {
    pub fn new(
        data_dir: &Path,
        identity: Identity,
        mode: Mode,
        cluster_cidr: String,
        subnet_prefix_len: u8,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            mode,
            cluster_cidr,
            subnet_prefix_len,
            active: None,
        }
    }

    pub fn active_marker_path(&self) -> PathBuf {
        self.data_dir.join("active_network")
    }

    pub fn network_dir(&self, network: &str) -> PathBuf {
        NetworkConfig::dir(&self.data_dir, network)
    }

    pub fn read_active_marker(&self) -> Option<String> {
        std::fs::read_to_string(self.active_marker_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_active_marker(&self, network: &str) {
        let _ = std::fs::write(self.active_marker_path(), network);
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
