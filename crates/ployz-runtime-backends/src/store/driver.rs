use crate::store::corrosion::docker::DockerCorrosion;
use crate::store::corrosion::host::HostCorrosion;
use async_trait::async_trait;
use ployz_core::store::driver::StoreBackend;
use ployz_core::store::{DeployStore, InviteStore, MachineStore, RoutingStore, SyncProbe, SyncStatus};
use ployz_core::{
    StoreDriver,
    corrosion_config,
    error::Result,
    model::{
        DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
        MachineId, MachineRecord, OverlayIp, RoutingState, ServiceReleaseRecord,
        ServiceRevisionRecord,
    },
    spec::Namespace,
    store::StoreRuntimeControl,
};
use ployz_corrosion::client::Transport;
use ployz_corrosion::{CorrosionStore, SCHEMA_SQL};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

fn which_corrosion() -> std::result::Result<PathBuf, String> {
    let candidates = ["/usr/local/bin/corrosion", "/usr/bin/corrosion"];
    for path in candidates {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(String::from(
        "corrosion binary not found (expected at /usr/local/bin/corrosion)",
    ))
}

pub async fn corrosion_docker(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
    image: &str,
) -> std::result::Result<StoreDriver, String> {
    let paths = corrosion_config::Paths::new(network_dir);
    let gossip_addr = SocketAddr::new(
        IpAddr::V6(overlay_ip.0),
        corrosion_config::DEFAULT_GOSSIP_PORT,
    );
    let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);
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
    .map_err(|error| format!("write corrosion config: {error}"))?;

    let local_api = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        corrosion_config::DEFAULT_API_PORT,
    );
    let store = CorrosionStore::new(
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
        .map_err(|error| format!("docker service: {error}"))?;

    Ok(StoreDriver::from_backend(Arc::new(CorrosionBackend {
        store,
        service: Arc::new(service),
    })))
}

pub fn corrosion_host(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
) -> std::result::Result<StoreDriver, String> {
    let paths = corrosion_config::Paths::new(network_dir);
    let gossip_addr = SocketAddr::new(
        IpAddr::V6(overlay_ip.0),
        corrosion_config::DEFAULT_GOSSIP_PORT,
    );
    let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

    corrosion_config::write_config(
        &paths,
        &paths,
        SCHEMA_SQL,
        gossip_addr,
        api_addr,
        bootstrap,
        Some(network_id),
    )
    .map_err(|error| format!("write corrosion config: {error}"))?;

    let store = CorrosionStore::new(api_addr, Transport::Direct, Some(paths.admin.clone()));
    let binary = which_corrosion()?;
    let service = HostCorrosion::new(binary, &paths.config);

    Ok(StoreDriver::from_backend(Arc::new(CorrosionBackend {
        store,
        service: Arc::new(service),
    })))
}

struct CorrosionBackend<S> {
    store: CorrosionStore,
    service: Arc<S>,
}

#[async_trait]
impl<S> StoreBackend for CorrosionBackend<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn start(&self) -> Result<()> {
        self.service.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.service.stop().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }

    async fn init(&self) -> Result<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        self.store.subscribe_machines().await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.store.create_invite(invite).await
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        self.store.consume_invite(invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>> {
        self.store.subscribe_routing_invalidations().await
    }

    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        self.store.list_service_revisions(namespace).await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.store.list_service_releases(namespace).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        self.store.upsert_service_revision(record).await
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        self.store.upsert_service_release(record).await
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        self.store.delete_service_release(namespace, service).await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.store.upsert_instance_status(record).await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.store.delete_instance_status(instance_id).await
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        self.store.upsert_deploy(record).await
    }

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        releases: &[ServiceReleaseRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        self.store
            .commit_deploy(namespace, removed_services, releases, deploy)
            .await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        self.store.sync_status().await
    }
}
