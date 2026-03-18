use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use ipnet::Ipv4Net;
use ployz_orchestrator::{ContainerNetwork, WireguardDriver};
use ployz_state::{Identity, StoreDriver};
use ployz_types::model::{DeployId, MachineId, OverlayIp};
use ployz_types::{Error, Result as PloyzResult, spec::Namespace};

pub struct MeshRuntimeComponents {
    pub network: WireguardDriver,
    pub store: StoreDriver,
    pub container_network: Option<ContainerNetwork>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartableWorkload {
    pub container_name: String,
    pub was_running: bool,
}

#[async_trait]
pub trait RuntimeHandle: Send + Sync {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String>;

    async fn detach(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct NamespaceLockManager {
    held: Arc<Mutex<std::collections::HashMap<String, DeployId>>>,
}

impl NamespaceLockManager {
    pub fn try_acquire(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> PloyzResult<NamespaceLock> {
        let mut guard = self.lock_inner();
        if let Some(current) = guard.get(&namespace.0) {
            return Err(Error::operation(
                "namespace_lock",
                format!(
                    "namespace '{}' is already locked by deploy '{}'",
                    namespace, current
                ),
            ));
        }
        guard.insert(namespace.0.clone(), deploy_id.clone());
        Ok(NamespaceLock {
            held: Arc::clone(&self.held),
            namespace: namespace.clone(),
        })
    }

    fn lock_inner(&self) -> MutexGuard<'_, std::collections::HashMap<String, DeployId>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct NamespaceLock {
    held: Arc<Mutex<std::collections::HashMap<String, DeployId>>>,
    namespace: Namespace,
}

impl Drop for NamespaceLock {
    fn drop(&mut self) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.remove(&self.namespace.0);
    }
}

#[async_trait]
pub trait RuntimeOps: Send + Sync {
    fn is_memory_test(&self) -> bool;
    fn overlay_network_name(&self, network_name: &str) -> Option<String>;
    async fn build_mesh_components(
        &self,
        identity: &Identity,
        overlay_ip: OverlayIp,
        network_dir: &Path,
        network_name: &str,
        subnet: Ipv4Net,
        exposed_tcp_ports: &[u16],
        bootstrap: &[String],
        network_id: &str,
    ) -> std::result::Result<MeshRuntimeComponents, String>;
    fn remote_control_bind_addr(&self, remote_control_port: u16, overlay_ip: OverlayIp)
        -> SocketAddr;
    async fn start_remote_control(
        &self,
        bind_addr: SocketAddr,
        store: StoreDriver,
        namespace_locks: NamespaceLockManager,
        machine_id: MachineId,
        overlay_network_name: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
    ) -> std::result::Result<Box<dyn RuntimeHandle>, String>;
    async fn stop_local_workloads_for_subnet_heal(
        &self,
        machine_id: &MachineId,
        network_name: &str,
        target_subnet: Ipv4Net,
    ) -> std::result::Result<Vec<RestartableWorkload>, String>;
    async fn start_local_workloads_after_subnet_heal(
        &self,
        network_name: &str,
        target_subnet: Ipv4Net,
        workloads: &[RestartableWorkload],
    ) -> std::result::Result<(), String>;
}
