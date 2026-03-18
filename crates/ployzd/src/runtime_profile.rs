use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use crate::built_in_images::{BuiltInImage, BuiltInImages};
use crate::daemon::deploy_control::NamespaceLockManager;
use crate::daemon::deploy_control::remote::{RemoteControlHandle, start_remote_control_listener};
use crate::services::dns::{DnsHandle, start_managed_dns};
use crate::services::gateway::{GatewayHandle, start_managed_gateway};
use crate::services::supervisor::ServiceSupervision;
use ipnet::Ipv4Net;
use ployz_config::{RuntimeTarget, ServiceMode};
use ployz_corrosion::config as corrosion_config;
use ployz_corrosion::runtime::{docker as corrosion_docker, host as corrosion_host};
use ployz_corrosion::ManagedCorrosionStore;
use ployz_dns::DnsConfig;
use ployz_gateway::GatewayConfig;
use ployz_runtime_api::Identity;
use ployz_runtime_api::{
    ContainerNetwork, DisconnectMode, RestartableWorkload, ServiceRuntime, WireguardDriver,
};
use ployz_runtime_backends::mesh::driver as mesh_backends;
use ployz_runtime_backends::network::docker_bridge_network;
use ployz_runtime_backends::runtime::{
    ContainerEngine,
    labels::{LABEL_KIND, LABEL_MACHINE, LABEL_MANAGED},
};
use ployz_store_api::{StoreDriver, memory::MemoryService};
use ployz_types::model::{MachineId, OverlayIp};

const HEAL_WORKLOAD_STOP_GRACE: tokio::time::Duration = tokio::time::Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionBackend {
    Memory,
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlPlaneBinding {
    Loopback,
    Overlay,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RuntimeProfile {
    execution_backend: ExecutionBackend,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    control_plane_binding: ControlPlaneBinding,
    sidecar_supervision: Option<ServiceSupervision>,
    built_in_images: BuiltInImages,
}

pub(crate) struct MeshRuntimeComponents {
    pub(crate) network: WireguardDriver,
    pub(crate) store: StoreDriver,
    pub(crate) store_runtime: Arc<dyn ServiceRuntime>,
    pub(crate) container_network: Option<ContainerNetwork>,
}

impl RuntimeProfile {
    #[must_use]
    pub(crate) fn from_runtime(
        runtime_target: RuntimeTarget,
        service_mode: ServiceMode,
        built_in_images: BuiltInImages,
    ) -> Self {
        match runtime_target {
            RuntimeTarget::Docker => Self {
                execution_backend: ExecutionBackend::Docker,
                runtime_target,
                service_mode,
                control_plane_binding: ControlPlaneBinding::Loopback,
                sidecar_supervision: Some(ServiceSupervision::DockerContainer),
                built_in_images,
            },
            RuntimeTarget::Host => Self {
                execution_backend: ExecutionBackend::Host,
                runtime_target,
                service_mode,
                control_plane_binding: ControlPlaneBinding::Overlay,
                sidecar_supervision: Some(match service_mode {
                    ServiceMode::User => ServiceSupervision::ChildProcess,
                    ServiceMode::System => ServiceSupervision::Systemd,
                }),
                built_in_images,
            },
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn memory_for_tests() -> Self {
        let built_in_images =
            BuiltInImages::load(None).expect("embedded built-in images manifest should parse");
        Self {
            execution_backend: ExecutionBackend::Memory,
            runtime_target: RuntimeTarget::Host,
            service_mode: ServiceMode::User,
            control_plane_binding: ControlPlaneBinding::Loopback,
            sidecar_supervision: None,
            built_in_images,
        }
    }

    #[must_use]
    pub(crate) fn is_memory_test(&self) -> bool {
        self.execution_backend == ExecutionBackend::Memory
    }

    #[must_use]
    pub(crate) fn overlay_network_name(&self, network_name: &str) -> Option<String> {
        if self.is_memory_test() {
            return None;
        }
        Some(format!("ployz-{network_name}"))
    }

    pub(crate) async fn build_mesh_components(
        &self,
        identity: &Identity,
        overlay_ip: OverlayIp,
        network_dir: &Path,
        network_name: &str,
        subnet: Ipv4Net,
        exposed_tcp_ports: &[u16],
        bootstrap: &[String],
        network_id: &str,
    ) -> Result<MeshRuntimeComponents, String> {
        let network = match self.execution_backend {
            ExecutionBackend::Memory => WireguardDriver::memory(),
            ExecutionBackend::Docker => {
                mesh_backends::docker(
                    identity,
                    overlay_ip,
                    network_dir,
                    exposed_tcp_ports,
                    corrosion_config::DEFAULT_API_PORT,
                    self.built_in_images.resolve(BuiltInImage::Networking),
                )
                .await?
            }
            ExecutionBackend::Host => {
                mesh_backends::host(identity, overlay_ip, network_name, subnet)?
            }
        };

        let managed_store = match self.execution_backend {
            ExecutionBackend::Memory => ManagedCorrosionStore {
                store: StoreDriver::memory(),
                runtime: Arc::new(MemoryService::new()),
            },
            ExecutionBackend::Docker => {
                corrosion_docker(
                    overlay_ip,
                    network_dir,
                    bootstrap,
                    network_id,
                    self.built_in_images.resolve(BuiltInImage::Corrosion),
                )
                .await?
            }
            ExecutionBackend::Host => {
                corrosion_host(overlay_ip, network_dir, bootstrap, network_id)?
            }
        };
        let ManagedCorrosionStore {
            store,
            runtime: store_runtime,
        } = managed_store;

        let container_network = match self.execution_backend {
            ExecutionBackend::Memory => None,
            ExecutionBackend::Docker | ExecutionBackend::Host => Some(
                docker_bridge_network(network_name, subnet)
                    .await
                    .map_err(|error| error.to_string())?,
            ),
        };

        Ok(MeshRuntimeComponents {
            network,
            store,
            store_runtime,
            container_network,
        })
    }

    #[must_use]
    pub(crate) fn remote_control_bind_addr(
        &self,
        remote_control_port: u16,
        overlay_ip: OverlayIp,
    ) -> SocketAddr {
        match self.control_plane_binding {
            ControlPlaneBinding::Loopback => {
                SocketAddr::from(([127, 0, 0, 1], remote_control_port))
            }
            ControlPlaneBinding::Overlay => {
                SocketAddr::new(IpAddr::V6(overlay_ip.0), remote_control_port)
            }
        }
    }

    pub(crate) async fn start_remote_control(
        &self,
        bind_addr: SocketAddr,
        store: StoreDriver,
        namespace_locks: NamespaceLockManager,
        machine_id: MachineId,
        overlay_network_name: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
    ) -> Result<RemoteControlHandle, String> {
        if self.is_memory_test() {
            return Ok(RemoteControlHandle::noop());
        }
        start_remote_control_listener(
            bind_addr,
            store,
            namespace_locks,
            machine_id,
            overlay_network_name,
            overlay_dns_server,
        )
        .await
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn start_gateway(
        &self,
        config: GatewayConfig,
    ) -> Result<GatewayHandle, String> {
        start_managed_gateway(
            self.sidecar_supervision,
            config,
            self.built_in_images.resolve(BuiltInImage::Gateway),
        )
        .await
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn start_dns(&self, config: DnsConfig) -> Result<DnsHandle, String> {
        start_managed_dns(
            self.sidecar_supervision,
            config,
            self.built_in_images.resolve(BuiltInImage::Dns),
        )
        .await
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn stop_local_workloads_for_subnet_heal(
        &self,
        machine_id: &MachineId,
        network_name: &str,
        target_subnet: Ipv4Net,
    ) -> Result<Vec<RestartableWorkload>, String> {
        if self.is_memory_test() {
            return Ok(Vec::new());
        }

        let engine = ContainerEngine::connect()
            .await
            .map_err(|err| format!("connect docker engine for subnet heal: {err}"))?;
        let bridge_name = format!("ployz-{network_name}");
        let observed = engine
            .list_by_labels(&[
                (LABEL_MANAGED, "true"),
                (LABEL_KIND, "workload"),
                (LABEL_MACHINE, &machine_id.0),
            ])
            .await
            .map_err(|err| format!("list local workloads for subnet heal: {err}"))?;

        let bridge = docker_bridge_network(network_name, target_subnet)
            .await
            .map_err(|err| format!("build bridge handle for subnet heal: {err}"))?;

        let mut restartable = Vec::new();
        for container in observed {
            if !container.networks.contains_key(&bridge_name) {
                continue;
            }

            if container.running {
                engine
                    .stop(&container.container_name, HEAL_WORKLOAD_STOP_GRACE)
                    .await
                    .map_err(|err| {
                        format!(
                            "stop workload '{}' for subnet heal: {err}",
                            container.container_name
                        )
                    })?;
            }

            bridge
                .disconnect(&container.container_name, DisconnectMode::Force)
                .await
                .map_err(|err| {
                    format!(
                        "disconnect workload '{}' from old bridge: {err}",
                        container.container_name
                    )
                })?;

            restartable.push(RestartableWorkload {
                container_name: container.container_name,
                was_running: container.running,
            });
        }

        Ok(restartable)
    }

    pub(crate) async fn start_local_workloads_after_subnet_heal(
        &self,
        network_name: &str,
        target_subnet: Ipv4Net,
        workloads: &[RestartableWorkload],
    ) -> Result<(), String> {
        if self.is_memory_test() || workloads.is_empty() {
            return Ok(());
        }

        let engine = ContainerEngine::connect()
            .await
            .map_err(|err| format!("connect docker engine after subnet heal: {err}"))?;
        let bridge = docker_bridge_network(network_name, target_subnet)
            .await
            .map_err(|err| format!("build target bridge handle after subnet heal: {err}"))?;

        for workload in workloads {
            bridge
                .connect(&workload.container_name, None)
                .await
                .map_err(|err| {
                    format!(
                        "reconnect workload '{}' to healed bridge: {err}",
                        workload.container_name
                    )
                })?;
            if workload.was_running {
                engine
                    .start(&workload.container_name)
                    .await
                    .map_err(|err| {
                        format!(
                            "restart workload '{}' after subnet heal: {err}",
                            workload.container_name
                        )
                    })?;
            }
        }

        Ok(())
    }
}
