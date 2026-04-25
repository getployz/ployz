use crate::admin::AdminClient;
use crate::client::{CorrClient, Transport};
use crate::config as corrosion_config;
use corro_api_types::{ExecResult, Statement};
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployStore, InviteStore,
    MachineStore, PeerMembershipObservation, PeerMembershipState, PeerMembershipStore,
    PeerRttObservation, PeerRttStore, RoutingStore, SyncProbe, SyncStatus,
};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeRecord, CertificateRecord, DeployId, DeployRecord, InstanceId,
    InstanceStatusRecord, InviteRecord, MachineEvent, MachineId, MachineRecord, OverlayIp,
    RoutingState, ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
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

impl PeerRttStore for CorrosionStore {
    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        let Some(admin) = &self.admin else {
            return Ok(Vec::new());
        };
        admin
            .cluster_member_rtts()
            .await
            .map_err(|e| {
                Error::operation(
                    "peer_rtt_observations",
                    format!("admin members request: {e}"),
                )
            })
            .map(|members| {
                members
                    .into_iter()
                    .map(|member| PeerRttObservation {
                        addr: member.addr,
                        rtts_ms: member.rtts_ms,
                    })
                    .collect()
            })
    }
}

impl PeerMembershipStore for CorrosionStore {
    async fn peer_membership_observations(&self) -> Result<Vec<PeerMembershipObservation>> {
        let Some(admin) = &self.admin else {
            return Ok(Vec::new());
        };
        admin
            .cluster_membership_states_latest()
            .await
            .map_err(|e| {
                Error::operation(
                    "peer_membership_observations",
                    format!("admin membership request: {e}"),
                )
            })
            .map(|members| {
                members
                    .into_iter()
                    .map(|member| PeerMembershipObservation {
                        addr: member.addr,
                        actor_id: member.id,
                        state: match member.state {
                            crate::admin::MembershipState::Alive => PeerMembershipState::Alive,
                            crate::admin::MembershipState::Suspect => PeerMembershipState::Suspect,
                            crate::admin::MembershipState::Down => PeerMembershipState::Down,
                        },
                        timestamp: member.timestamp,
                    })
                    .collect()
            })
    }
}

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

    async fn subscribe_machines(
        &self,
    ) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        tables::machines::subscribe_machines(&self.client).await
    }
}

impl InviteStore for CorrosionStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        tables::invites::create_invite(&self.client, invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        tables::invites::get_invite(&self.client, invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        tables::invites::list_invites(&self.client).await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        tables::invites::redeem_invite(&self.client, invite_id, machine_id, now_unix_secs).await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        tables::invites::revoke_invite(&self.client, invite_id, now_unix_secs).await
    }
}

impl RoutingStore for CorrosionStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        workflows::routing_state::load_routing_state(&self.client).await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>> {
        workflows::routing_state::subscribe_routing_invalidations(&self.client).await
    }
}

impl DeployStore for CorrosionStore {
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

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        tables::volumes::list_volumes(&self.client, namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        tables::volumes::get_volume(&self.client, namespace, volume_name).await
    }

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

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        releases: &[ServiceReleaseRecord],
        volumes: &[VolumeRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        workflows::deploy_commit::commit_deploy(
            &self.client,
            namespace,
            removed_services,
            releases,
            volumes,
            deploy,
        )
        .await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        tables::deploys::get_deploy(&self.client, deploy_id).await
    }
}

impl CertificateStore for CorrosionStore {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        tables::acme_accounts::get_acme_account(&self.client, issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        tables::acme_accounts::upsert_acme_account(&self.client, record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        tables::certificates::list_certificates(&self.client).await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        tables::certificates::get_certificate(&self.client, hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        tables::certificates::upsert_certificate(&self.client, record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        tables::acme_challenges::list_acme_challenges(&self.client).await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        tables::acme_challenges::upsert_acme_challenge(&self.client, record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        tables::acme_challenges::delete_acme_challenge(&self.client, hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        tables::certificates::subscribe_certificates(&self.client).await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        tables::acme_challenges::subscribe_acme_challenges(&self.client).await
    }
}
