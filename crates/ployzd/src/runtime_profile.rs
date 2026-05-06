use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use crate::built_in_images::{BuiltInImage, BuiltInImages};
use crate::services::dns::{DnsHandle, start_managed_dns};
use crate::services::gateway::{GatewayHandle, start_managed_gateway};
use crate::services::nats::{nats_docker, nats_host};
use crate::services::supervisor::ServiceSupervision;
use ipnet::Ipv4Net;
use ployz_config::{RuntimeTarget, ServiceMode};
use ployz_dns_config::DnsConfig;
use ployz_gateway_config::GatewayConfig;
use ployz_nats::config as nats_config;
use ployz_orchestrator::WireguardDriver;
use ployz_runtime_api::Identity;
use ployz_runtime_backends::mesh::driver as mesh_backends;
use ployz_runtime_backends::network::docker_bridge_network;
use ployz_store_api::StoreDriver;
use ployz_types::model::OverlayIp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionBackend {
    Memory,
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZfsTransferBinding {
    Loopback,
    Overlay,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RuntimeProfile {
    execution_backend: ExecutionBackend,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    zfs_transfer_binding: ZfsTransferBinding,
    sidecar_supervision: Option<ServiceSupervision>,
    built_in_images: BuiltInImages,
}

pub(crate) struct MeshRuntimeComponents {
    pub(crate) network: WireguardDriver,
    pub(crate) store: StoreDriver,
    pub(crate) container_network: Option<ployz_orchestrator::ContainerNetwork>,
}

pub(crate) struct MeshBuildRequest<'a> {
    pub(crate) identity: &'a Identity,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) network_dir: &'a Path,
    pub(crate) network_name: &'a str,
    pub(crate) subnet: Option<Ipv4Net>,
    pub(crate) exposed_tcp_ports: &'a [u16],
    pub(crate) bootstrap: &'a [String],
    pub(crate) network_id: &'a str,
    pub(crate) storage_authority: bool,
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
                zfs_transfer_binding: ZfsTransferBinding::Loopback,
                sidecar_supervision: Some(ServiceSupervision::DockerContainer),
                built_in_images,
            },
            RuntimeTarget::Host => Self {
                execution_backend: ExecutionBackend::Host,
                runtime_target,
                service_mode,
                zfs_transfer_binding: ZfsTransferBinding::Overlay,
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
            zfs_transfer_binding: ZfsTransferBinding::Loopback,
            sidecar_supervision: None,
            built_in_images,
        }
    }

    #[must_use]
    pub(crate) fn is_memory_test(&self) -> bool {
        self.execution_backend == ExecutionBackend::Memory
    }

    pub(crate) async fn build_mesh_components(
        &self,
        request: MeshBuildRequest<'_>,
    ) -> Result<MeshRuntimeComponents, String> {
        let MeshBuildRequest {
            identity,
            overlay_ip,
            network_dir,
            network_name,
            subnet,
            exposed_tcp_ports,
            bootstrap,
            network_id,
            storage_authority,
        } = request;
        let network = match self.execution_backend {
            ExecutionBackend::Memory => WireguardDriver::memory(),
            ExecutionBackend::Docker => {
                mesh_backends::docker(
                    identity,
                    overlay_ip,
                    network_dir,
                    nats_config::CLIENT_PORT,
                    exposed_tcp_ports,
                    self.built_in_images.resolve(BuiltInImage::Networking),
                )
                .await?
            }
            ExecutionBackend::Host => {
                mesh_backends::host(identity, overlay_ip, network_name, subnet)?
            }
        };

        let store = match self.execution_backend {
            ExecutionBackend::Memory => StoreDriver::memory(),
            ExecutionBackend::Docker => {
                nats_docker(
                    overlay_ip,
                    network_dir,
                    bootstrap,
                    network_id,
                    storage_authority,
                    self.built_in_images.resolve(BuiltInImage::Nats),
                )
                .await?
            }
            ExecutionBackend::Host => nats_host(
                overlay_ip,
                network_dir,
                bootstrap,
                network_id,
                storage_authority,
            )?,
        };

        let container_network = match (self.execution_backend, subnet) {
            (ExecutionBackend::Memory, _) => None,
            (ExecutionBackend::Docker | ExecutionBackend::Host, Some(subnet)) => Some(
                docker_bridge_network(network_name, subnet)
                    .await
                    .map_err(|error| error.to_string())?,
            ),
            (ExecutionBackend::Docker | ExecutionBackend::Host, None) => None,
        };

        Ok(MeshRuntimeComponents {
            network,
            store,
            container_network,
        })
    }

    #[must_use]
    pub(crate) fn zfs_transfer_bind_addr(
        &self,
        zfs_transfer_port: u16,
        overlay_ip: OverlayIp,
    ) -> SocketAddr {
        match self.zfs_transfer_binding {
            ZfsTransferBinding::Loopback => SocketAddr::from(([127, 0, 0, 1], zfs_transfer_port)),
            ZfsTransferBinding::Overlay => {
                SocketAddr::new(IpAddr::V6(overlay_ip.0), zfs_transfer_port)
            }
        }
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
}
