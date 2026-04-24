pub mod handlers;
mod runtime;
mod setup;
pub mod ssh;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::built_in_images::BuiltInImages;
use crate::ipc::listener::IncomingCommand;
use crate::mesh_state::network::NetworkConfig;
use crate::runtime_profile::RuntimeProfile;
use ipnet::Ipv4Net;
use ployz_api::{DaemonPayload, DaemonResponse};
use ployz_config::{RuntimeTarget, ServiceMode};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::coordination::PendingReservations;
use ployz_runtime_api::Identity;
use ployz_runtime_api::{NamespaceLockManager, RuntimeHandle};
use serde::Serialize;
use tokio::sync::mpsc;

pub struct ActiveMesh {
    pub config: NetworkConfig,
    pub cached_subnet: Option<Ipv4Net>,
    pub mesh: Mesh,
    pub remote_control: Box<dyn RuntimeHandle>,
    pub peer_control: Box<dyn RuntimeHandle>,
    pub gateway: Box<dyn RuntimeHandle>,
    pub dns: Box<dyn RuntimeHandle>,
}

pub struct DaemonState {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub runtime_target: RuntimeTarget,
    pub service_mode: ServiceMode,
    runtime_profile: RuntimeProfile,
    pub cluster_cidr: String,
    pub subnet_prefix_len: u8,
    pub remote_control_port: u16,
    pub peer_control_target: Option<String>,
    pub gateway_listen_addr: String,
    pub gateway_threads: usize,
    pub dns_metrics_listen_addr: Option<String>,
    pub gateway_metrics_listen_addr: Option<String>,
    pub active: Option<ActiveMesh>,
    pub namespace_locks: NamespaceLockManager,
    pub reservations: Arc<PendingReservations>,
    pub command_tx: Option<mpsc::Sender<IncomingCommand>>,
}

impl DaemonState {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: &Path,
        identity: Identity,
        runtime_target: RuntimeTarget,
        service_mode: ServiceMode,
        built_in_images: BuiltInImages,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        remote_control_port: u16,
        gateway_listen_addr: String,
        gateway_threads: usize,
        dns_metrics_listen_addr: Option<String>,
        gateway_metrics_listen_addr: Option<String>,
    ) -> Self {
        let runtime_profile =
            RuntimeProfile::from_runtime(runtime_target, service_mode, built_in_images);
        Self::new_with_runtime_profile(
            data_dir,
            identity,
            runtime_target,
            service_mode,
            runtime_profile,
            cluster_cidr,
            subnet_prefix_len,
            remote_control_port,
            gateway_listen_addr,
            gateway_threads,
            dns_metrics_listen_addr,
            gateway_metrics_listen_addr,
        )
    }

    #[cfg(test)]
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
        Self::new_with_runtime_profile(
            data_dir,
            identity,
            RuntimeTarget::Host,
            ServiceMode::User,
            RuntimeProfile::memory_for_tests(),
            cluster_cidr,
            subnet_prefix_len,
            remote_control_port,
            gateway_listen_addr,
            gateway_threads,
            None,
            None,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_runtime_profile(
        data_dir: &Path,
        identity: Identity,
        runtime_target: RuntimeTarget,
        service_mode: ServiceMode,
        runtime_profile: RuntimeProfile,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        remote_control_port: u16,
        gateway_listen_addr: String,
        gateway_threads: usize,
        dns_metrics_listen_addr: Option<String>,
        gateway_metrics_listen_addr: Option<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            runtime_target,
            service_mode,
            runtime_profile,
            cluster_cidr,
            subnet_prefix_len,
            remote_control_port,
            peer_control_target: None,
            gateway_listen_addr,
            gateway_threads,
            dns_metrics_listen_addr,
            gateway_metrics_listen_addr,
            active: None,
            namespace_locks: NamespaceLockManager::default(),
            reservations: Arc::new(PendingReservations::new()),
            command_tx: None,
        }
    }

    #[must_use]
    pub fn network_dir(&self, network: &str) -> PathBuf {
        NetworkConfig::dir(&self.data_dir, network)
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

    pub fn require_active(
        &self,
        code: &str,
        message: &'static str,
    ) -> Result<&ActiveMesh, Box<DaemonResponse>> {
        self.active
            .as_ref()
            .ok_or_else(|| Box::new(self.err(code, message)))
    }

    pub fn ok_json_pretty<T: Serialize>(
        &self,
        value: &T,
        encode_error_code: &str,
        context: &str,
    ) -> DaemonResponse {
        match serde_json::to_string_pretty(value) {
            Ok(json) => self.ok(json),
            Err(err) => self.err(encode_error_code, format!("{context}: {err}")),
        }
    }
}
