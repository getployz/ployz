pub mod handlers;
mod setup;
pub mod ssh;

use std::path::{Path, PathBuf};

use crate::config::{RuntimeTarget, ServiceMode};
use crate::deploy::NamespaceLockManager;
use crate::deploy::remote::RemoteControlHandle;
use crate::mesh::orchestrator::Mesh;
use crate::node::identity::Identity;
use crate::runtime_profile::RuntimeProfile;
use crate::services::dns::DnsHandle;
use crate::services::gateway::GatewayHandle;
use crate::store::network::NetworkConfig;
use ipnet::Ipv4Net;
use ployz_sdk::transport::{DaemonPayload, DaemonResponse};

pub struct ActiveMesh {
    pub config: NetworkConfig,
    pub mesh: Mesh,
    pub remote_control: RemoteControlHandle,
    pub gateway: GatewayHandle,
    pub dns: DnsHandle,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SubnetHealAttempt {
    pub network_subnet: Ipv4Net,
    pub target_subnet: Ipv4Net,
    pub attempted_at: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingSubnetHeal {
    pub network_subnet: Ipv4Net,
    pub target_subnet: Ipv4Net,
    pub planned_at: u64,
}

pub struct DaemonState {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub runtime_target: RuntimeTarget,
    pub service_mode: ServiceMode,
    pub(crate) runtime_profile: RuntimeProfile,
    pub cluster_cidr: String,
    pub subnet_prefix_len: u8,
    pub remote_control_port: u16,
    pub gateway_listen_addr: String,
    pub gateway_threads: usize,
    pub active: Option<ActiveMesh>,
    pub namespace_locks: NamespaceLockManager,
    pub(crate) pending_subnet_heal: Option<PendingSubnetHeal>,
    pub(crate) last_subnet_heal_attempt: Option<SubnetHealAttempt>,
}

impl DaemonState {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: &Path,
        identity: Identity,
        runtime_target: RuntimeTarget,
        service_mode: ServiceMode,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        remote_control_port: u16,
        gateway_listen_addr: String,
        gateway_threads: usize,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            runtime_target,
            service_mode,
            runtime_profile: RuntimeProfile::from_runtime(runtime_target, service_mode),
            cluster_cidr,
            subnet_prefix_len,
            remote_control_port,
            gateway_listen_addr,
            gateway_threads,
            active: None,
            namespace_locks: NamespaceLockManager::default(),
            pending_subnet_heal: None,
            last_subnet_heal_attempt: None,
        }
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_for_tests(
        data_dir: &Path,
        identity: Identity,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        remote_control_port: u16,
        gateway_listen_addr: String,
        gateway_threads: usize,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            runtime_target: RuntimeTarget::Host,
            service_mode: ServiceMode::User,
            runtime_profile: RuntimeProfile::memory_for_tests(),
            cluster_cidr,
            subnet_prefix_len,
            remote_control_port,
            gateway_listen_addr,
            gateway_threads,
            active: None,
            namespace_locks: NamespaceLockManager::default(),
            pending_subnet_heal: None,
            last_subnet_heal_attempt: None,
        }
    }

    #[must_use]
    pub fn active_marker_path(&self) -> PathBuf {
        self.data_dir.join("active_network")
    }

    #[must_use]
    pub fn network_dir(&self, network: &str) -> PathBuf {
        NetworkConfig::dir(&self.data_dir, network)
    }

    #[must_use]
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
        self.ok_with_payload(message, None)
    }

    pub fn ok_with_payload(
        &self,
        message: impl Into<String>,
        payload: Option<DaemonPayload>,
    ) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: message.into(),
            payload,
        }
    }

    pub fn err(&self, code: &str, message: impl Into<String>) -> DaemonResponse {
        self.err_with_payload(code, message, None)
    }

    pub fn err_with_payload(
        &self,
        code: &str,
        message: impl Into<String>,
        payload: Option<DaemonPayload>,
    ) -> DaemonResponse {
        DaemonResponse {
            ok: false,
            code: code.into(),
            message: message.into(),
            payload,
        }
    }
}
