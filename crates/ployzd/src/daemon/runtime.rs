use super::deploy_control::NamespaceLockManager;
use super::store::StoreDriver;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use ipnet::Ipv4Net;
use ployz_dns::DnsConfig;
use ployz_gateway::GatewayConfig;
use ployz_runtime_api::{RestartableWorkload, RuntimeHandle};
use ployz_types::model::MachineId;
use ployz_types::model::OverlayIp;

use super::DaemonState;
use crate::runtime_profile::MeshRuntimeComponents;

impl DaemonState {
    pub(crate) async fn build_runtime_mesh_components(
        &self,
        overlay_ip: OverlayIp,
        network_dir: &Path,
        network_name: &str,
        subnet: Ipv4Net,
        exposed_tcp_ports: &[u16],
        bootstrap: &[String],
        network_id: &str,
    ) -> Result<MeshRuntimeComponents, String> {
        self.runtime_profile
            .build_mesh_components(
                &self.identity,
                overlay_ip,
                network_dir,
                network_name,
                subnet,
                exposed_tcp_ports,
                bootstrap,
                network_id,
            )
            .await
    }

    #[must_use]
    pub(crate) fn remote_control_bind_addr(
        &self,
        remote_control_port: u16,
        overlay_ip: OverlayIp,
    ) -> SocketAddr {
        self.runtime_profile
            .remote_control_bind_addr(remote_control_port, overlay_ip)
    }

    #[must_use]
    pub(crate) fn runtime_overlay_network_name(&self, network_name: &str) -> Option<String> {
        self.runtime_profile.overlay_network_name(network_name)
    }

    pub(crate) async fn start_runtime_remote_control(
        &self,
        bind_addr: SocketAddr,
        store: StoreDriver,
        namespace_locks: NamespaceLockManager,
        machine_id: MachineId,
        overlay_network_name: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
    ) -> Result<Box<dyn RuntimeHandle>, String> {
        self.runtime_profile
            .start_remote_control(
                bind_addr,
                store,
                namespace_locks,
                machine_id,
                overlay_network_name,
                overlay_dns_server,
            )
            .await
            .map(|handle| Box::new(handle) as Box<dyn RuntimeHandle>)
    }

    pub(crate) async fn start_runtime_gateway(
        &self,
        config: GatewayConfig,
    ) -> Result<Box<dyn RuntimeHandle>, String> {
        self.runtime_profile
            .start_gateway(config)
            .await
            .map(|handle| Box::new(handle) as Box<dyn RuntimeHandle>)
    }

    pub(crate) async fn start_runtime_dns(
        &self,
        config: DnsConfig,
    ) -> Result<Box<dyn RuntimeHandle>, String> {
        self.runtime_profile
            .start_dns(config)
            .await
            .map(|handle| Box::new(handle) as Box<dyn RuntimeHandle>)
    }

    #[must_use]
    pub(crate) fn runtime_is_memory_test(&self) -> bool {
        self.runtime_profile.is_memory_test()
    }

    pub(crate) async fn stop_runtime_local_workloads_for_subnet_heal(
        &self,
        machine_id: &MachineId,
        network_name: &str,
        target_subnet: Ipv4Net,
    ) -> Result<Vec<RestartableWorkload>, String> {
        self.runtime_profile
            .stop_local_workloads_for_subnet_heal(machine_id, network_name, target_subnet)
            .await
    }

    pub(crate) async fn start_runtime_local_workloads_after_subnet_heal(
        &self,
        network_name: &str,
        target_subnet: Ipv4Net,
        workloads: &[RestartableWorkload],
    ) -> Result<(), String> {
        self.runtime_profile
            .start_local_workloads_after_subnet_heal(network_name, target_subnet, workloads)
            .await
    }
}
