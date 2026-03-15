//! Concrete driver enum that dispatches to backend-specific store adapters.
//!
//! Closed enum rather than `dyn Trait` because the set of backends is fixed at
//! compile time, exhaustive matching catches new variants, and there is no
//! vtable/`Arc` overhead on hot dispatch paths.

use crate::error::Result;
use crate::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, OverlayIp, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord,
};
use crate::spec::Namespace;
use crate::store::backends::corrosion::docker::DockerCorrosion;
use crate::store::backends::corrosion::host::HostCorrosion;
use crate::store::backends::memory::{MemoryService, MemoryStore};
use crate::store::{
    DeployStore, InviteStore, MachineStore, RoutingStore, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};
use ployz_corrosion::client::Transport;
use ployz_corrosion::config as corrosion_config;
use ployz_corrosion::{CorrosionStore, SCHEMA_SQL};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

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
    #[must_use]
    pub fn memory() -> Self {
        Self::Memory {
            store: Arc::new(MemoryStore::new()),
            service: Arc::new(MemoryService::new()),
        }
    }

    pub async fn corrosion_docker(
        overlay_ip: OverlayIp,
        network_dir: &Path,
        bootstrap: &[String],
        network_id: &str,
        image: &str,
    ) -> std::result::Result<Self, String> {
        let paths = corrosion_config::Paths::new(network_dir);
        let gossip_addr = SocketAddr::new(
            IpAddr::V6(overlay_ip.0),
            corrosion_config::DEFAULT_GOSSIP_PORT,
        );
        let api_addr =
            SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);
        let config_paths = corrosion_config::Paths {
            db: PathBuf::from("/data/store.db"),
            admin: PathBuf::from("/data/admin.sock"),
            schema: PathBuf::from("/etc/corrosion/schema.sql"),
            ..paths.clone()
        };

        corrosion_config::write_config(
            &config_paths,
            &paths,
            SCHEMA_SQL,
            gossip_addr,
            api_addr,
            bootstrap,
            Some(network_id),
        )
        .map_err(|e| format!("write corrosion config: {e}"))?;

        let local_api = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            corrosion_config::DEFAULT_API_PORT,
        );
        let corrosion = CorrosionStore::new(
            api_addr,
            Transport::Bridge {
                local_addr: local_api,
            },
            None,
        );

        let config_host = paths.config.to_string_lossy().into_owned();
        let schema_host = paths.schema.to_string_lossy().into_owned();
        let config_container = "/etc/corrosion/config.toml";
        let schema_container = "/etc/corrosion/schema.sql";
        let service = DockerCorrosion::new("ployz-corrosion", image)
            .cmd(vec!["agent".into(), "-c".into(), config_container.into()])
            .volume(&format!("{config_host}:{config_container}:ro"))
            .volume(&format!("{schema_host}:{schema_container}:ro"))
            .volume("ployz-corrosion-data:/data")
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

    pub fn corrosion_host(
        overlay_ip: OverlayIp,
        network_dir: &Path,
        bootstrap: &[String],
        network_id: &str,
    ) -> std::result::Result<Self, String> {
        let paths = corrosion_config::Paths::new(network_dir);
        let gossip_addr = SocketAddr::new(
            IpAddr::V6(overlay_ip.0),
            corrosion_config::DEFAULT_GOSSIP_PORT,
        );
        let api_addr =
            SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(
            &paths,
            &paths,
            SCHEMA_SQL,
            gossip_addr,
            api_addr,
            bootstrap,
            Some(network_id),
        )
        .map_err(|e| format!("write corrosion config: {e}"))?;

        let corrosion = CorrosionStore::new(api_addr, Transport::Direct, Some(paths.admin.clone()));
        let binary = which_corrosion()?;
        let service = HostCorrosion::new(binary, &paths.config);

        tracing::info!(endpoint = %api_addr, "store backend: corrosion (host)");
        Ok(Self::CorrosionHost {
            store: corrosion,
            service: Arc::new(service),
        })
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

    async fn upsert_self_machine<'a>(&'a self, record: &'a MachineRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_self_machine(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_self_machine(record).await
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
    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_service_revisions(namespace).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.list_service_revisions(namespace).await
            }
        }
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_service_releases(namespace).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.list_service_releases(namespace).await
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

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_service_release(record).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.upsert_service_release(record).await
            }
        }
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.delete_service_release(namespace, service).await,
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store.delete_service_release(namespace, service).await
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
        releases: &[ServiceReleaseRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        match self {
            Self::Memory { store, .. } => {
                store
                    .commit_deploy(namespace, removed_services, releases, deploy)
                    .await
            }
            Self::Corrosion { store, .. } | Self::CorrosionHost { store, .. } => {
                store
                    .commit_deploy(namespace, removed_services, releases, deploy)
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
