mod cert_coordination;
mod cert_renewal_health;
mod deploy_probe;
pub mod handlers;
mod runtime;
mod setup;
pub mod ssh;
mod subnet_coordination;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::built_in_images::BuiltInImages;
use crate::ipc::listener::IncomingCommand;
use crate::mesh_state::bootstrap::BootstrapPeerSeedTask;
use crate::mesh_state::network::NetworkConfig;
use crate::runtime_profile::RuntimeProfile;
use ipnet::Ipv4Net;
use ployz_api::{DaemonPayload, DaemonResponse};
use ployz_config::{RuntimeTarget, ServiceMode, StorageConfig};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::certificates::CertificateRenewalTask;
use ployz_orchestrator::coordination::{MemorySubnetCoordinator, SubnetReservationCoordinator};
use ployz_runtime_api::Identity;
use ployz_runtime_api::RuntimeHandle;
use ployz_types::model::MachineTopology;
use serde::Serialize;
use tokio::sync::mpsc;

pub struct ActiveMesh {
    pub config: NetworkConfig,
    pub retained_subnet: RetainedSubnet,
    pub mesh: Mesh,
    pub nats_control: Box<dyn RuntimeHandle>,
    pub zfs_transfer: Box<dyn RuntimeHandle>,
    pub gateway: Box<dyn RuntimeHandle>,
    pub dns: Box<dyn RuntimeHandle>,
    pub certificate_renewal: Option<CertificateRenewalTask>,
    pub bootstrap_peer_seed: Option<BootstrapPeerSeedTask>,
}

#[derive(Clone, Copy, Debug)]
pub struct RetainedSubnet(Option<Ipv4Net>);

impl RetainedSubnet {
    #[must_use]
    pub fn from_running_config(subnet: Option<Ipv4Net>) -> Self {
        Self(subnet)
    }

    #[must_use]
    pub fn value(self) -> Option<Ipv4Net> {
        self.0
    }

    pub fn record_activation(&mut self, subnet: Ipv4Net) {
        self.0 = Some(subnet);
    }

    pub fn record_standby(&mut self, previous_subnet: Option<Ipv4Net>) {
        self.0 = previous_subnet;
    }
}

impl ActiveMesh {
    #[must_use]
    pub fn subnet_to_persist_after_stop(&self) -> Option<Ipv4Net> {
        self.config.subnet.or(self.retained_subnet.value())
    }

    pub async fn stop_certificate_renewal(&mut self) {
        if let Some(task) = self.certificate_renewal.take() {
            task.shutdown().await;
        }
    }

    pub async fn stop_bootstrap_peer_seed(&mut self) {
        if let Some(task) = self.bootstrap_peer_seed.take() {
            task.shutdown().await;
        }
    }
}

pub struct DaemonState {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub runtime_target: RuntimeTarget,
    pub service_mode: ServiceMode,
    pub storage: StorageConfig,
    runtime_profile: RuntimeProfile,
    pub cluster_cidr: String,
    pub subnet_prefix_len: u8,
    pub zfs_transfer_port: u16,
    pub gateway_listen_addr: String,
    pub gateway_https_listen_addr: Option<String>,
    pub gateway_threads: usize,
    pub configured_topology: Option<MachineTopology>,
    pub dns_metrics_listen_addr: Option<String>,
    pub gateway_metrics_listen_addr: Option<String>,
    pub active: Option<ActiveMesh>,
    pub subnet_coord: Arc<dyn SubnetReservationCoordinator>,
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
        storage: StorageConfig,
        built_in_images: BuiltInImages,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        zfs_transfer_port: u16,
        gateway_listen_addr: String,
        gateway_https_listen_addr: Option<String>,
        gateway_threads: usize,
        configured_topology: Option<MachineTopology>,
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
            storage,
            runtime_profile,
            cluster_cidr,
            subnet_prefix_len,
            zfs_transfer_port,
            gateway_listen_addr,
            gateway_https_listen_addr,
            gateway_threads,
            configured_topology,
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
        zfs_transfer_port: u16,
        gateway_listen_addr: String,
        gateway_https_listen_addr: Option<String>,
        gateway_threads: usize,
    ) -> Self {
        Self::new_with_runtime_profile(
            data_dir,
            identity,
            RuntimeTarget::Host,
            ServiceMode::User,
            StorageConfig::default(),
            RuntimeProfile::memory_for_tests(),
            cluster_cidr,
            subnet_prefix_len,
            zfs_transfer_port,
            gateway_listen_addr,
            gateway_https_listen_addr,
            gateway_threads,
            None,
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
        storage: StorageConfig,
        runtime_profile: RuntimeProfile,
        cluster_cidr: String,
        subnet_prefix_len: u8,
        zfs_transfer_port: u16,
        gateway_listen_addr: String,
        gateway_https_listen_addr: Option<String>,
        gateway_threads: usize,
        configured_topology: Option<MachineTopology>,
        dns_metrics_listen_addr: Option<String>,
        gateway_metrics_listen_addr: Option<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            runtime_target,
            service_mode,
            storage,
            runtime_profile,
            cluster_cidr,
            subnet_prefix_len,
            zfs_transfer_port,
            gateway_listen_addr,
            gateway_https_listen_addr,
            gateway_threads,
            configured_topology,
            dns_metrics_listen_addr,
            gateway_metrics_listen_addr,
            active: None,
            subnet_coord: Arc::new(MemorySubnetCoordinator::new()),
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

    pub(crate) async fn nats_node_rpc_client(
        &self,
    ) -> Result<ployz_nats::NatsNodeRpcClient, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        let client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let store = ployz_nats::NatsStore::connect_with_scope(&client_url, scope)
            .await
            .map_err(|error| error.to_string())?;
        ployz_store_api::StoreRuntimeControl::start(&store)
            .await
            .map_err(|error| error.to_string())?;
        Ok(ployz_nats::NatsNodeRpcClient::for_store(&store))
    }
}
