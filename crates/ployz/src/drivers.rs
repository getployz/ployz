//! Concrete driver enums that dispatch to backend-specific adapters.
//!
//! These are closed enums rather than `dyn Trait` objects because the set of
//! backends is fixed at compile time (`Mode` picks once at startup), exhaustive
//! matching catches unimplemented variants when a new backend is added, and
//! there is no vtable/`Arc` overhead on the hot dispatch paths.

use crate::adapters::corrosion::docker::DockerCorrosion;
use crate::adapters::corrosion::host::HostCorrosion;
use ployz_corrosion::client::Transport;
use ployz_corrosion::CorrosionStore;
use crate::adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
use crate::adapters::wireguard::{DockerWireGuard, HostWireGuard};
use crate::config::Mode;
use crate::error::Result;
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, OverlayIp, PublicKey, RoutingState, ServiceHeadRecord,
    ServiceRevisionRecord, ServiceSlotRecord,
};
use crate::node::identity::Identity;
use crate::spec::Namespace;
use crate::store::{
    DeployStore, InviteStore, MachineStore, RoutingStore, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};
use ployz_corrosion::config as corrosion_config;
use ployz_corrosion::SCHEMA_SQL;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum WireguardDriver {
    Memory(Arc<MemoryWireGuard>),
    Docker(Arc<DockerWireGuard>),
    Host(Arc<HostWireGuard>),
}

impl WireguardDriver {
    pub async fn from_mode(
        mode: Mode,
        identity: &Identity,
        overlay_ip: OverlayIp,
        network_dir: &Path,
        network_name: &str,
        subnet: ipnet::Ipv4Net,
        exposed_tcp_ports: &[u16],
    ) -> std::result::Result<Self, String> {
        match mode {
            Mode::Memory => Ok(Self::Memory(Arc::new(MemoryWireGuard::new()))),
            Mode::Docker => {
                let api_port = corrosion_config::DEFAULT_API_PORT;
                let overlay_api = SocketAddr::new(IpAddr::V6(overlay_ip.0), api_port);
                let local_api = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), api_port);

                let mut builder = DockerWireGuard::new(
                    "ployz-networking",
                    network_dir,
                    identity.private_key.clone(),
                    overlay_ip,
                )
                .with_bridge_forward(local_api, overlay_api);
                for &port in exposed_tcp_ports {
                    builder = builder.expose_tcp(port);
                }
                let wg = builder
                    .build()
                    .await
                    .map_err(|e| format!("docker wireguard: {e}"))?;
                Ok(Self::Docker(Arc::new(wg)))
            }
            Mode::HostExec | Mode::HostService => {
                let ifname = format!("plz-{network_name}");
                #[cfg(target_os = "linux")]
                let wg = HostWireGuard::kernel(
                    &ifname,
                    identity.private_key.clone(),
                    overlay_ip,
                    subnet,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                #[cfg(not(target_os = "linux"))]
                let wg = HostWireGuard::userspace(
                    &ifname,
                    identity.private_key.clone(),
                    overlay_ip,
                    subnet,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                Ok(Self::Host(Arc::new(wg)))
            }
        }
    }
}

impl MeshNetwork for WireguardDriver {
    async fn up(&self) -> Result<()> {
        match self {
            Self::Memory(n) => n.up().await,
            Self::Docker(n) => n.up().await,
            Self::Host(n) => n.up().await,
        }
    }

    async fn down(&self) -> Result<()> {
        match self {
            Self::Memory(n) => n.down().await,
            Self::Docker(n) => n.down().await,
            Self::Host(n) => n.down().await,
        }
    }

    async fn set_peers<'a>(&'a self, peers: &'a [MachineRecord]) -> Result<()> {
        match self {
            Self::Memory(n) => n.set_peers(peers).await,
            Self::Docker(n) => n.set_peers(peers).await,
            Self::Host(n) => n.set_peers(peers).await,
        }
    }

    async fn has_remote_handshake(&self) -> bool {
        match self {
            Self::Memory(n) => n.has_remote_handshake().await,
            Self::Docker(n) => n.has_remote_handshake().await,
            Self::Host(n) => n.has_remote_handshake().await,
        }
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        match self {
            Self::Memory(n) => n.bridge_ip().await,
            Self::Docker(n) => n.bridge_ip().await,
            Self::Host(n) => n.bridge_ip().await,
        }
    }
}

impl WireGuardDevice for WireguardDriver {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        match self {
            Self::Memory(n) => n.read_peers().await,
            Self::Docker(n) => n.read_peers().await,
            Self::Host(n) => n.read_peers().await,
        }
    }

    async fn set_peer_endpoint<'a>(&'a self, key: &'a PublicKey, endpoint: &'a str) -> Result<()> {
        match self {
            Self::Memory(n) => n.set_peer_endpoint(key, endpoint).await,
            Self::Docker(n) => n.set_peer_endpoint(key, endpoint).await,
            Self::Host(n) => n.set_peer_endpoint(key, endpoint).await,
        }
    }
}

// ---------------------------------------------------------------------------
// DistributedStore — service lifecycle + data layer
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum StoreDriver {
    Memory {
        store: Arc<MemoryStore>,
        service: Arc<MemoryService>,
    },
    Corrosion {
        store: CorrosionStore,
        service: Arc<DockerCorrosion>,
    },
    CorrosionHost {
        store: CorrosionStore,
        service: Arc<HostCorrosion>,
    },
}

fn which_corrosion() -> std::result::Result<PathBuf, String> {
    let candidates = ["/usr/local/bin/corrosion", "/usr/bin/corrosion"];
    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }
    Err("corrosion binary not found (expected at /usr/local/bin/corrosion)".into())
}

impl StoreDriver {
    pub async fn from_mode(
        mode: Mode,
        overlay_ip: OverlayIp,
        network_dir: &Path,
        bootstrap: &[String],
        network_id: &str,
    ) -> std::result::Result<Self, String> {
        match mode {
            Mode::Memory => Ok(Self::Memory {
                store: Arc::new(MemoryStore::new()),
                service: Arc::new(MemoryService::new()),
            }),
            Mode::Docker | Mode::HostExec | Mode::HostService => {
                let paths = corrosion_config::Paths::new(network_dir);
                let gossip_addr = SocketAddr::new(
                    IpAddr::V6(overlay_ip.0),
                    corrosion_config::DEFAULT_GOSSIP_PORT,
                );
                let api_addr =
                    SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

                corrosion_config::write_config(
                    &paths,
                    SCHEMA_SQL,
                    gossip_addr,
                    api_addr,
                    bootstrap,
                    Some(network_id),
                )
                .map_err(|e| format!("write corrosion config: {e}"))?;

                let corrosion = CorrosionStore::new(
                    api_addr,
                    match mode {
                        Mode::Docker => {
                            let local_api = SocketAddr::new(
                                IpAddr::V4(Ipv4Addr::LOCALHOST),
                                corrosion_config::DEFAULT_API_PORT,
                            );
                            Transport::Bridge {
                                local_addr: local_api,
                            }
                        }
                        Mode::HostExec | Mode::HostService | Mode::Memory => Transport::Direct,
                    },
                );

                match mode {
                    Mode::Docker => {
                        let config_path = paths.config.to_string_lossy().into_owned();
                        let dir_mount = paths.dir.to_string_lossy().into_owned();

                        let service =
                            DockerCorrosion::new("ployz-corrosion", "ghcr.io/getployz/corrosion")
                                .cmd(vec!["agent".into(), "-c".into(), config_path])
                                .volume(&dir_mount, &dir_mount)
                                .network_mode("container:ployz-networking")
                                .build()
                                .await
                                .map_err(|e| format!("docker service: {e}"))?;

                        tracing::info!(endpoint = %api_addr, "store backend: corrosion (docker)");
                        Ok(Self::Corrosion {
                            store: corrosion,
                            service: Arc::new(service),
                        })
                    }
                    Mode::HostExec | Mode::HostService => {
                        let binary = which_corrosion()?;
                        let service = HostCorrosion::new(binary, &paths.config);

                        tracing::info!(endpoint = %api_addr, "store backend: corrosion (host)");
                        Ok(Self::CorrosionHost {
                            store: corrosion,
                            service: Arc::new(service),
                        })
                    }
                    Mode::Memory => unreachable!("Memory mode handled above"),
                }
            }
        }
    }
}

impl StoreRuntimeControl for StoreDriver {
    async fn start(&self) -> Result<()> {
        match self {
            Self::Memory { service, .. } => service.start().await,
            Self::Corrosion { service, .. } => service.start().await,
            Self::CorrosionHost { service, .. } => service.start().await,
        }
    }

    async fn stop(&self) -> Result<()> {
        match self {
            Self::Memory { service, .. } => service.stop().await,
            Self::Corrosion { service, .. } => service.stop().await,
            Self::CorrosionHost { service, .. } => service.stop().await,
        }
    }

    async fn healthy(&self) -> bool {
        match self {
            Self::Memory { service, .. } => service.healthy().await,
            Self::Corrosion { service, .. } => service.healthy().await,
            Self::CorrosionHost { service, .. } => service.healthy().await,
        }
    }
}

impl MachineStore for StoreDriver {
    async fn init(&self) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.init().await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => store.init().await,
        }
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_machines().await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.list_machines().await
            }
        }
    }

    async fn upsert_machine<'a>(&'a self, record: &'a MachineRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_machine(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_machine(record).await
            }
        }
    }

    async fn delete_machine<'a>(&'a self, id: &'a MachineId) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.delete_machine(id).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.delete_machine(id).await
            }
        }
    }

    async fn subscribe_machines(
        &self,
    ) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        match self {
            Self::Memory { store, .. } => store.subscribe_machines().await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.subscribe_machines().await
            }
        }
    }
}

impl InviteStore for StoreDriver {
    async fn create_invite<'a>(&'a self, invite: &'a InviteRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.create_invite(invite).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.create_invite(invite).await
            }
        }
    }

    async fn consume_invite<'a>(&'a self, invite_id: &'a str, now_unix_secs: u64) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.consume_invite(invite_id, now_unix_secs).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.consume_invite(invite_id, now_unix_secs).await
            }
        }
    }
}

impl RoutingStore for StoreDriver {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        match self {
            Self::Memory { store, .. } => store.load_routing_state().await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.load_routing_state().await
            }
        }
    }

    async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>> {
        match self {
            Self::Memory { store, .. } => store.subscribe_routing_invalidations().await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.subscribe_routing_invalidations().await
            }
        }
    }
}

impl DeployStore for StoreDriver {
    async fn list_service_heads(&self, namespace: &Namespace) -> Result<Vec<ServiceHeadRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_service_heads(namespace).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.list_service_heads(namespace).await
            }
        }
    }

    async fn list_service_slots(&self, namespace: &Namespace) -> Result<Vec<ServiceSlotRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_service_slots(namespace).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.list_service_slots(namespace).await
            }
        }
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_instance_status(namespace).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.list_instance_status(namespace).await
            }
        }
    }

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_service_revision(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_service_revision(record).await
            }
        }
    }

    async fn upsert_service_head(&self, record: &ServiceHeadRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_service_head(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_service_head(record).await
            }
        }
    }

    async fn delete_service_head(&self, namespace: &Namespace, service: &str) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.delete_service_head(namespace, service).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.delete_service_head(namespace, service).await
            }
        }
    }

    async fn replace_service_slots(
        &self,
        namespace: &Namespace,
        service: &str,
        records: &[ServiceSlotRecord],
    ) -> Result<()> {
        match self {
            Self::Memory { store, .. } => {
                store
                    .replace_service_slots(namespace, service, records)
                    .await
            }
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store
                    .replace_service_slots(namespace, service, records)
                    .await
            }
        }
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_instance_status(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_instance_status(record).await
            }
        }
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.delete_instance_status(instance_id).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.delete_instance_status(instance_id).await
            }
        }
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_deploy(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_deploy(record).await
            }
        }
    }

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        heads: &[ServiceHeadRecord],
        slots: &[ServiceSlotRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        match self {
            Self::Memory { store, .. } => {
                store
                    .commit_deploy(namespace, removed_services, heads, slots, deploy)
                    .await
            }
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store
                    .commit_deploy(namespace, removed_services, heads, slots, deploy)
                    .await
            }
        }
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        match self {
            Self::Memory { store, .. } => store.get_deploy(deploy_id).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.get_deploy(deploy_id).await
            }
        }
    }
}

impl SyncProbe for StoreDriver {
    async fn sync_status(&self) -> Result<SyncStatus> {
        match self {
            Self::Memory { store, .. } => store.sync_status().await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.sync_status().await
            }
        }
    }
}

impl ployz_gateway::RoutingStore for StoreDriver {
    async fn load_routing_state(
        &self,
    ) -> std::result::Result<RoutingState, ployz_gateway::GatewayError> {
        RoutingStore::load_routing_state(self)
            .await
            .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))
    }

    async fn subscribe_routing_invalidations(
        &self,
    ) -> std::result::Result<mpsc::Receiver<()>, ployz_gateway::GatewayError> {
        RoutingStore::subscribe_routing_invalidations(self)
            .await
            .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))
    }
}

impl ployz_dns::DnsStore for StoreDriver {
    async fn load_routing_state(
        &self,
    ) -> std::result::Result<RoutingState, ployz_dns::DnsError> {
        RoutingStore::load_routing_state(self)
            .await
            .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
    }

    async fn subscribe_routing_invalidations(
        &self,
    ) -> std::result::Result<mpsc::Receiver<()>, ployz_dns::DnsError> {
        RoutingStore::subscribe_routing_invalidations(self)
            .await
            .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
    }
}
