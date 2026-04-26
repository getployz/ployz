use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use ployz_dns_config::DnsConfig;
use ployz_gateway_config::GatewayConfig;
use ployz_runtime_api::{NamespaceLockManager, RuntimeHandle};
use ployz_runtime_backends::storage::{TokioShellRunner, ZfsDriver};
use ployz_store_api::StoreDriver;
use ployz_types::model::{MachineId, OverlayIp};

use super::DaemonState;
use crate::runtime_profile::{MeshBuildRequest, MeshRuntimeComponents};

impl DaemonState {
    pub(crate) async fn build_runtime_mesh_components(
        &self,
        request: MeshBuildRequest<'_>,
    ) -> Result<MeshRuntimeComponents, String> {
        self.runtime_profile.build_mesh_components(request).await
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
        let storage_driver = self.zfs_storage_driver().await?;
        self.runtime_profile
            .start_remote_control(
                bind_addr,
                store,
                namespace_locks,
                machine_id,
                overlay_network_name,
                overlay_dns_server,
                storage_driver,
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

    pub(crate) async fn zfs_storage_driver(
        &self,
    ) -> Result<Option<Arc<ZfsDriver<TokioShellRunner>>>, String> {
        let Some(root) = self.storage.zfs_root.as_ref() else {
            return Ok(None);
        };
        let root = root
            .to_str()
            .ok_or_else(|| format!("storage zfs_root is not valid UTF-8: {}", root.display()))?;
        ZfsDriver::new(TokioShellRunner, root, self.storage.overcommit_ratio)
            .await
            .map(Arc::new)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}
