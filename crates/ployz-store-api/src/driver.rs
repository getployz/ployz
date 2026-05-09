use crate::memory::{MemoryService, MemoryStore};
use crate::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployStore, InstanceStatusStore, InviteStore, MachineMembershipStore, MachineSubscription,
    PeerRttObservation, PeerRttStore, RoutingEventSubscription, RoutingStateStore,
    StoreRuntimeControl, SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployId, DeployPhaseId, DeployPhaseRecord, DeployRecord, InstanceId, InstanceStatusRecord,
    InviteRecord, MachineId, MachineMembership, RoutingState, ServiceBranchLineageRecord,
    ServiceReleaseRecord, ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::sync::Arc;

#[derive(Clone)]
pub struct StoreDriver {
    runtime: Arc<dyn StoreRuntimeControl>,
    machines: Arc<dyn MachineMembershipStore>,
    invites: Arc<dyn InviteStore>,
    routing: Arc<dyn RoutingStateStore>,
    deploys: Arc<dyn DeployStore>,
    instances: Arc<dyn InstanceStatusStore>,
    certificates: Arc<dyn CertificateStore>,
    sync: Arc<dyn SyncProbe>,
    peer_rtt: Arc<dyn PeerRttStore>,
}

impl StoreDriver {
    #[must_use]
    pub fn memory() -> Self {
        Self::memory_with(Arc::new(MemoryStore::new()), Arc::new(MemoryService::new()))
    }

    #[must_use]
    pub fn memory_with(store: Arc<MemoryStore>, service: Arc<MemoryService>) -> Self {
        let runtime = Arc::new(MemoryRuntime {
            store: Arc::clone(&store),
            service: Arc::clone(&service),
        });
        Self::new(
            runtime,
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store,
        )
    }

    #[must_use]
    pub fn new(
        runtime: Arc<dyn StoreRuntimeControl>,
        machines: Arc<dyn MachineMembershipStore>,
        invites: Arc<dyn InviteStore>,
        routing: Arc<dyn RoutingStateStore>,
        deploys: Arc<dyn DeployStore>,
        instances: Arc<dyn InstanceStatusStore>,
        certificates: Arc<dyn CertificateStore>,
        sync: Arc<dyn SyncProbe>,
        peer_rtt: Arc<dyn PeerRttStore>,
    ) -> Self {
        Self {
            runtime,
            machines,
            invites,
            routing,
            deploys,
            instances,
            certificates,
            sync,
            peer_rtt,
        }
    }
}

#[async_trait]
impl StoreRuntimeControl for StoreDriver {
    async fn start(&self) -> Result<()> {
        self.runtime.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.runtime.stop().await
    }

    async fn wipe_data(&self) -> Result<()> {
        self.runtime.wipe_data().await
    }

    async fn healthy(&self) -> bool {
        self.runtime.healthy().await
    }
}

#[async_trait]
impl MachineMembershipStore for StoreDriver {
    async fn init(&self) -> Result<()> {
        self.machines.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        self.machines.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        self.machines.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.machines.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.machines.subscribe_machines().await
    }
}

#[async_trait]
impl InviteStore for StoreDriver {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.invites.create_invite(invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        self.invites.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        self.invites.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        self.invites
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        self.invites.revoke_invite(invite_id, now_unix_secs).await
    }
}

#[async_trait]
impl RoutingStateStore for StoreDriver {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.routing.load_routing_state().await
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingEventSubscription> {
        self.routing.subscribe_routing_events().await
    }
}

#[async_trait]
impl DeployStore for StoreDriver {
    async fn list_deploy_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        self.deploys.list_deploy_revisions(namespace).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.deploys.list_deploy_releases(namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        self.deploys.list_volumes(namespace).await
    }

    async fn list_service_branch_lineage(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceBranchLineageRecord>> {
        self.deploys.list_service_branch_lineage(namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        self.deploys.get_volume(namespace, volume_name).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        self.deploys.commit_deploy(command).await
    }

    async fn write_deploy_status(&self, deploy: &DeployRecord) -> Result<()> {
        self.deploys.write_deploy_status(deploy).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.deploys.get_deploy(deploy_id).await
    }

    async fn upsert_deploy_phase(&self, phase: &DeployPhaseRecord) -> Result<()> {
        self.deploys.upsert_deploy_phase(phase).await
    }

    async fn get_deploy_phase(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &DeployPhaseId,
    ) -> Result<Option<DeployPhaseRecord>> {
        self.deploys
            .get_deploy_phase(namespace, deploy_id, phase_id)
            .await
    }

    async fn list_deploy_phases(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<Vec<DeployPhaseRecord>> {
        self.deploys.list_deploy_phases(namespace, deploy_id).await
    }
}

#[async_trait]
impl InstanceStatusStore for StoreDriver {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.instances.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.instances.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.instances.remove_instance_status(instance_id).await
    }
}

#[async_trait]
impl SyncProbe for StoreDriver {
    async fn sync_status(&self) -> Result<SyncStatus> {
        self.sync.sync_status().await
    }
}

#[async_trait]
impl PeerRttStore for StoreDriver {
    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        self.peer_rtt.peer_rtt_observations().await
    }
}

#[async_trait]
impl CertificateStore for StoreDriver {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        self.certificates.get_acme_account(issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        self.certificates.upsert_acme_account(record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        self.certificates.list_certificates().await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        self.certificates.get_certificate(hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        self.certificates.upsert_certificate(record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        self.certificates.list_acme_challenges().await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        self.certificates.upsert_acme_challenge(record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        self.certificates
            .delete_acme_challenge(hostname, token)
            .await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        self.certificates.subscribe_certificates().await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        self.certificates.subscribe_acme_challenges().await
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        self.certificates
            .upsert_acme_challenge_readiness(record)
            .await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        self.certificates
            .list_acme_challenge_readiness(hostname, token)
            .await
    }
}

struct MemoryRuntime {
    store: Arc<MemoryStore>,
    service: Arc<MemoryService>,
}

#[async_trait]
impl StoreRuntimeControl for MemoryRuntime {
    async fn start(&self) -> Result<()> {
        self.service.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.service.stop().await
    }

    async fn wipe_data(&self) -> Result<()> {
        self.store.wipe_data().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }
}
