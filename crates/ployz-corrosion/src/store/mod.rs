use crate::admin::AdminClient;
use crate::client::{CorrClient, Transport};
use corro_api_types::{ExecResult, Statement};
use ployz_config::corrosion as corrosion_config;
use ployz_store_api::{
    DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore, InviteStore,
    MachineEventSubscription, MachineStore, RoutingInvalidationSubscription, RoutingStore,
    SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineRecord, OverlayIp, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::info;

mod shared;
mod tables;
mod workflows;

pub const SCHEMA_SQL: &str = include_str!("../schema.sql");

#[derive(Clone)]
pub struct CorrosionStore {
    client: CorrClient,
    admin: Option<AdminClient>,
    gossip_addr: SocketAddr,
}

impl CorrosionStore {
    #[must_use]
    pub fn new(api_addr: SocketAddr, transport: Transport, admin_path: Option<PathBuf>) -> Self {
        let client = CorrClient::new(api_addr, transport);
        Self {
            client,
            admin: admin_path.map(AdminClient::new),
            gossip_addr: SocketAddr::new(api_addr.ip(), corrosion_config::DEFAULT_GOSSIP_PORT),
        }
    }

    #[must_use]
    pub fn client(&self) -> &CorrClient {
        &self.client
    }

    pub async fn connect_for_network(data_dir: &Path, network: &str) -> Result<Self> {
        let network_dir = ployz_config::network_dir(data_dir, network);
        let admin_path = corrosion_config::Paths::new(&network_dir).admin;
        let network_path = ployz_config::network_config_path(data_dir, network);
        let raw = std::fs::read_to_string(&network_path).map_err(|e| {
            Error::operation(
                "connect_for_network",
                format!(
                    "reading network config from {}: {e}",
                    network_path.display()
                ),
            )
        })?;

        #[derive(serde::Deserialize)]
        struct NetworkConfigMinimal {
            overlay_ip: OverlayIp,
        }

        let network_config: NetworkConfigMinimal = serde_json::from_str(&raw).map_err(|e| {
            Error::operation(
                "connect_for_network",
                format!("parsing network config: {e}"),
            )
        })?;

        let api_addr = SocketAddr::new(
            IpAddr::V6(network_config.overlay_ip.0),
            corrosion_config::DEFAULT_API_PORT,
        );
        let bridge_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            corrosion_config::DEFAULT_API_PORT,
        );

        let bridge = Self::new(
            api_addr,
            Transport::Bridge {
                local_addr: bridge_addr,
            },
            Some(admin_path.clone()),
        );
        if bridge.client.health().await.is_ok() {
            info!(%api_addr, %bridge_addr, "using local bridge transport for corrosion");
            return Ok(bridge);
        }

        let direct = Self::new(api_addr, Transport::Direct, Some(admin_path));
        if direct.client.health().await.is_ok() {
            info!(%api_addr, "using direct overlay transport for corrosion");
            return Ok(direct);
        }

        Err(Error::operation(
            "connect_for_network",
            format!("failed to reach corrosion via bridge {bridge_addr} or direct {api_addr}"),
        ))
    }

    pub async fn subscribe_routing_state(
        &self,
    ) -> Result<(RoutingState, mpsc::Receiver<RoutingState>)> {
        workflows::routing_state::subscribe_routing_state_inner(&self.client).await
    }
}

#[async_trait]
impl SyncProbe for CorrosionStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        if let Some(admin) = &self.admin {
            let active_remote_members = admin
                .cluster_membership_states_latest()
                .await
                .map_err(|e| {
                    Error::operation("sync_status", format!("admin membership request: {e}"))
                })?
                .into_iter()
                .filter(|state| state.addr != self.gossip_addr)
                .filter(|state| state.state.is_active())
                .count();
            if active_remote_members < 1 {
                return Ok(SyncStatus::Disconnected);
            }
        }

        let health = self
            .client
            .health()
            .await
            .map_err(|e| Error::operation("sync_status", format!("health request: {e}")))?;

        let status = if health.gaps > 0 {
            SyncStatus::Syncing {
                gaps: health.gaps as u64,
            }
        } else {
            SyncStatus::Synced
        };

        Ok(status)
    }
}

#[async_trait]
impl MachineStore for CorrosionStore {
    async fn init(&self) -> Result<()> {
        let res = self
            .client
            .schema(&[Statement::Simple(SCHEMA_SQL.to_string())])
            .await
            .map_err(|e| Error::operation("schema", e.to_string()))?;
        if let Some(ExecResult::Error { error }) = res.results.first() {
            return Err(Error::operation("schema", error.clone()));
        }
        Ok(())
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        tables::machines::list_machines(&self.client).await
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()> {
        tables::machines::upsert_self_machine(&self.client, record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        tables::machines::delete_machine(&self.client, id).await
    }

    async fn subscribe_machines(&self) -> Result<(Vec<MachineRecord>, MachineEventSubscription)> {
        let (snapshot, receiver) = tables::machines::subscribe_machines(&self.client).await?;
        Ok((snapshot, MachineEventSubscription::new(receiver)))
    }
}

#[async_trait]
impl InviteStore for CorrosionStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        tables::invites::create_invite(&self.client, invite).await
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        tables::invites::consume_invite(&self.client, invite_id, now_unix_secs).await
    }
}

#[async_trait]
impl RoutingStore for CorrosionStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        workflows::routing_state::load_routing_state(&self.client).await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription> {
        let receiver =
            workflows::routing_state::subscribe_routing_invalidations(&self.client).await?;
        Ok(RoutingInvalidationSubscription::new(receiver))
    }
}

#[async_trait]
impl DeployReadStore for CorrosionStore {
    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        tables::service_revisions::list_service_revisions(&self.client, namespace).await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        tables::service_releases::list_service_releases(&self.client, namespace).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        tables::instance_status::list_instance_status(&self.client, namespace).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        tables::deploys::get_deploy(&self.client, deploy_id).await
    }
}

#[async_trait]
impl DeployWriteStore for CorrosionStore {

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        tables::service_revisions::upsert_service_revision(&self.client, record).await
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        tables::service_releases::upsert_service_release(&self.client, record).await
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        tables::service_releases::delete_service_release(&self.client, namespace, service).await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        tables::instance_status::upsert_instance_status(&self.client, record).await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        tables::instance_status::delete_instance_status(&self.client, instance_id).await
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        tables::deploys::upsert_deploy(&self.client, record).await
    }
}

#[async_trait]
impl DeployCommitStore for CorrosionStore {
    async fn apply_deploy_commit(&self, commit: &DeployCommit) -> Result<()> {
        workflows::deploy_commit::apply_deploy_commit(&self.client, commit).await
    }
}
