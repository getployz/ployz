use crate::memory::{MemoryService, MemoryStore};
use crate::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, MachineSubscription,
    PeerRttObservation, PeerRttStore, RoutingBatchSubscription, RoutingSnapshotReader,
    StoreBackend, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineMembership, RoutingState, ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::sync::Arc;

#[derive(Clone)]
pub struct StoreDriver {
    backend: Arc<dyn StoreBackend>,
    runtime_control: Arc<dyn StoreRuntimeControl>,
    memory_store: Option<Arc<MemoryStore>>,
    memory_service: Option<Arc<MemoryService>>,
}

impl StoreDriver {
    #[must_use]
    pub fn memory() -> Self {
        Self::memory_with(Arc::new(MemoryStore::new()), Arc::new(MemoryService::new()))
    }

    #[must_use]
    pub fn memory_with(store: Arc<MemoryStore>, service: Arc<MemoryService>) -> Self {
        let backend = Arc::new(MemoryStoreBackend {
            store: Arc::clone(&store),
            service: Arc::clone(&service),
        });
        Self {
            backend: Arc::clone(&backend) as Arc<dyn StoreBackend>,
            runtime_control: backend as Arc<dyn StoreRuntimeControl>,
            memory_store: Some(store),
            memory_service: Some(service),
        }
    }

    #[must_use]
    pub fn from_backend(
        backend: Arc<dyn StoreBackend>,
        runtime_control: Arc<dyn StoreRuntimeControl>,
    ) -> Self {
        Self {
            backend,
            runtime_control,
            memory_store: None,
            memory_service: None,
        }
    }

    #[must_use]
    pub fn memory_store(&self) -> Option<Arc<MemoryStore>> {
        self.memory_store.as_ref().map(Arc::clone)
    }

    #[must_use]
    pub fn memory_service(&self) -> Option<Arc<MemoryService>> {
        self.memory_service.as_ref().map(Arc::clone)
    }
}

#[async_trait]
impl StoreRuntimeControl for StoreDriver {
    async fn start(&self) -> Result<()> {
        self.runtime_control.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.runtime_control.stop().await
    }

    async fn wipe_data(&self) -> Result<()> {
        self.runtime_control.wipe_data().await
    }

    async fn healthy(&self) -> bool {
        self.runtime_control.healthy().await
    }
}

impl MachineRegistry for StoreDriver {
    async fn init(&self) -> Result<()> {
        self.backend.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        self.backend.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        self.backend.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.backend.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.backend.subscribe_machines().await
    }
}

impl InviteRepository for StoreDriver {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.backend.create_invite(invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        self.backend.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        self.backend.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        self.backend
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        self.backend.revoke_invite(invite_id, now_unix_secs).await
    }
}

impl RoutingSnapshotReader for StoreDriver {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.backend.load_routing_state().await
    }

    async fn subscribe_routing_batches(
        &self,
        consumer_id: &str,
    ) -> Result<RoutingBatchSubscription> {
        self.backend.subscribe_routing_batches(consumer_id).await
    }
}

impl DeployRepository for StoreDriver {
    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.backend.list_deploy_releases(namespace).await
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        self.backend.load_deploy_snapshot(namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        self.backend.list_volumes(namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        self.backend.get_volume(namespace, volume_name).await
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        self.backend.record_service_revision(command).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        self.backend.commit_deploy(command).await
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        self.backend.update_deploy_record(command).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.backend.get_deploy(deploy_id).await
    }
}

impl InstanceStatusRepository for StoreDriver {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.backend.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.backend.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.backend.remove_instance_status(instance_id).await
    }
}

impl SyncProbe for StoreDriver {
    async fn sync_status(&self) -> Result<SyncStatus> {
        self.backend.sync_status().await
    }
}

impl PeerRttStore for StoreDriver {
    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        self.backend.peer_rtt_observations().await
    }
}

impl CertificateStore for StoreDriver {
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        self.backend.get_acme_account(issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        self.backend.upsert_acme_account(record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        self.backend.list_certificates().await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        self.backend.get_certificate(hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        self.backend.upsert_certificate(record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        self.backend.list_acme_challenges().await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        self.backend.upsert_acme_challenge(record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        self.backend.delete_acme_challenge(hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        self.backend.subscribe_certificates().await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        self.backend.subscribe_acme_challenges().await
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        self.backend.upsert_acme_challenge_readiness(record).await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        self.backend
            .list_acme_challenge_readiness(hostname, token)
            .await
    }
}

struct MemoryStoreBackend {
    store: Arc<MemoryStore>,
    service: Arc<MemoryService>,
}

#[async_trait]
impl StoreBackend for MemoryStoreBackend {
    async fn init(&self) -> Result<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.store.subscribe_machines().await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.store.create_invite(invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        self.store.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        self.store.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        self.store
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        self.store.revoke_invite(invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_batches(
        &self,
        consumer_id: &str,
    ) -> Result<RoutingBatchSubscription> {
        self.store.subscribe_routing_batches(consumer_id).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.store.list_deploy_releases(namespace).await
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        self.store.load_deploy_snapshot(namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        self.store.list_volumes(namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        self.store.get_volume(namespace, volume_name).await
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        self.store.record_service_revision(command).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        self.store.commit_deploy(command).await
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        self.store.update_deploy_record(command).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.store.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.store.remove_instance_status(instance_id).await
    }

    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        self.store.get_acme_account(issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        self.store.upsert_acme_account(record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        self.store.list_certificates().await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        self.store.get_certificate(hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        self.store.upsert_certificate(record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        self.store.list_acme_challenges().await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        self.store.upsert_acme_challenge(record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        self.store.delete_acme_challenge(hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        self.store.subscribe_certificates().await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        self.store.subscribe_acme_challenges().await
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        self.store.upsert_acme_challenge_readiness(record).await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        self.store
            .list_acme_challenge_readiness(hostname, token)
            .await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        self.store.sync_status().await
    }
}

#[async_trait]
impl StoreRuntimeControl for MemoryStoreBackend {
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
