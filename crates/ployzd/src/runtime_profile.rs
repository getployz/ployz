use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use crate::built_in_images::{BuiltInImage, BuiltInImages};
use crate::daemon::store::StoreDriver;
use crate::mesh_state::bootstrap::load_bootstrap_peer_records;
use ipnet::Ipv4Net;
use ployz_config::{RuntimeTarget, ServiceMode, corrosion as corrosion_config};
use ployz_corrosion::{
    CorrosionStore, Transport, docker_corrosion_runtime, host_corrosion_runtime,
};
use ployz_runtime_api::{
    ContainerNetwork, DataplaneFactory, EndpointDiscovery, Result as RuntimeResult, RuntimeError,
    ServiceRuntime, WireguardDriver,
};
use ployz_runtime_backends::mesh::driver as mesh_backends;
use ployz_runtime_backends::sidecar::ServiceSupervision;
use ployz_test_support::{
    MemoryServiceRuntime, MemoryStore, MemoryWireGuard, StaticEndpointDiscovery,
    memory_wireguard_driver,
};
use ployz_types::model::{Identity, MachineId, MachineRecord, OverlayIp};

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
    control_plane_binding: ControlPlaneBinding,
    sidecar_supervision: Option<ServiceSupervision>,
    built_in_images: BuiltInImages,
}

pub(crate) struct MeshRuntimeComponents {
    pub(crate) network: WireguardDriver,
    pub(crate) store: StoreDriver,
    pub(crate) bootstrap_seed_records: Vec<MachineRecord>,
    pub(crate) store_runtime: Arc<dyn ServiceRuntime>,
    pub(crate) container_network: Option<ContainerNetwork>,
    pub(crate) endpoint_discovery: Arc<dyn EndpointDiscovery>,
    pub(crate) dataplane_factory: Option<Arc<dyn DataplaneFactory>>,
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
                control_plane_binding: ControlPlaneBinding::Loopback,
                sidecar_supervision: Some(ServiceSupervision::DockerContainer),
                built_in_images,
            },
            RuntimeTarget::Host => Self {
                execution_backend: ExecutionBackend::Host,
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

    #[must_use]
    pub(crate) fn sidecar_supervision(&self) -> Option<ServiceSupervision> {
        self.sidecar_supervision
    }

    #[must_use]
    pub(crate) fn resolve_built_in_image(&self, image: BuiltInImage) -> &str {
        self.built_in_images.resolve(image)
    }

    pub(crate) fn load_bootstrap_addrs(
        &self,
        network_dir: &Path,
        local_machine_id: &MachineId,
    ) -> RuntimeResult<Vec<String>> {
        match self.execution_backend {
            ExecutionBackend::Memory => Ok(Vec::new()),
            ExecutionBackend::Docker | ExecutionBackend::Host => {
                let peers = load_bootstrap_peer_records(network_dir)
                    .map_err(|error| RuntimeError::operation("load_bootstrap_addrs", error))?;
                Ok(peers
                    .into_iter()
                    .filter(|peer| peer.machine_id != *local_machine_id)
                    .map(|peer| {
                        format!(
                            "[{}]:{}",
                            peer.overlay_ip.0,
                            ployz_config::corrosion::DEFAULT_GOSSIP_PORT
                        )
                    })
                    .collect())
            }
        }
    }

    pub(crate) fn load_bootstrap_seed_records(&self, network_dir: &Path) -> Vec<MachineRecord> {
        match self.execution_backend {
            ExecutionBackend::Memory => Vec::new(),
            ExecutionBackend::Docker | ExecutionBackend::Host => {
                load_bootstrap_peer_records(network_dir)
                    .unwrap_or_else(|error| {
                        tracing::warn!(
                            ?error,
                            "failed to load bootstrap peers, continuing without seeds"
                        );
                        Vec::new()
                    })
                    .into_iter()
                    .map(|peer| peer.into_machine_record())
                    .collect()
            }
        }
    }

    // Startup assembly needs the full runtime wiring input in one place.
    #[expect(clippy::too_many_arguments)]
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
    ) -> RuntimeResult<MeshRuntimeComponents> {
        let mesh_components = match self.execution_backend {
            ExecutionBackend::Memory => {
                let store = Arc::new(MemoryStore::new());
                MeshRuntimeComponents {
                    network: memory_wireguard_driver(Arc::new(MemoryWireGuard::new())),
                    store: StoreDriver::memory_with(Arc::clone(&store)),
                    bootstrap_seed_records: Vec::new(),
                    store_runtime: Arc::new(MemoryServiceRuntime::new()),
                    container_network: None,
                    endpoint_discovery: Arc::new(StaticEndpointDiscovery::empty()),
                    dataplane_factory: None,
                }
            }
            ExecutionBackend::Docker => {
                let mesh = mesh_backends::docker_components(
                    identity,
                    overlay_ip,
                    network_dir,
                    network_name,
                    subnet,
                    exposed_tcp_ports,
                    corrosion_config::DEFAULT_API_PORT,
                    self.built_in_images.resolve(BuiltInImage::Networking),
                )
                .await?;
                let api_addr =
                    SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);
                let local_api = SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    corrosion_config::DEFAULT_API_PORT,
                );
                let store = Arc::new(CorrosionStore::new(
                    api_addr,
                    Transport::Bridge {
                        local_addr: local_api,
                    },
                    None,
                ));
                MeshRuntimeComponents {
                    network: mesh.network,
                    store: StoreDriver::from_store(store),
                    bootstrap_seed_records: self.load_bootstrap_seed_records(network_dir),
                    store_runtime: docker_corrosion_runtime(
                        overlay_ip,
                        network_dir,
                        bootstrap,
                        network_id,
                        self.built_in_images.resolve(BuiltInImage::Corrosion),
                    )
                    .await?,
                    container_network: mesh.container_network,
                    endpoint_discovery: mesh.endpoint_discovery,
                    dataplane_factory: mesh.dataplane_factory,
                }
            }
            ExecutionBackend::Host => {
                let mesh =
                    mesh_backends::host_components(identity, overlay_ip, network_name, subnet)
                        .await?;
                let paths = corrosion_config::Paths::new(network_dir);
                let api_addr =
                    SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);
                let store = Arc::new(CorrosionStore::new(
                    api_addr,
                    Transport::Direct,
                    Some(paths.admin),
                ));
                MeshRuntimeComponents {
                    network: mesh.network,
                    store: StoreDriver::from_store(store),
                    bootstrap_seed_records: self.load_bootstrap_seed_records(network_dir),
                    store_runtime: host_corrosion_runtime(
                        overlay_ip,
                        network_dir,
                        bootstrap,
                        network_id,
                    )?,
                    container_network: mesh.container_network,
                    endpoint_discovery: mesh.endpoint_discovery,
                    dataplane_factory: mesh.dataplane_factory,
                }
            }
        };

        Ok(mesh_components)
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
}
