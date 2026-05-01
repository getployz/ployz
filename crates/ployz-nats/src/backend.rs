use async_trait::async_trait;
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, MachineSubscription,
    PeerMembershipObservation, PeerMembershipStore, PeerRttObservation, PeerRttStore,
    RoutingSnapshotReader, RoutingSubscription, StoreBackend, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeRecord, CertificateRecord, DeployId, DeployRecord, InstanceId,
    InstanceStatusRecord, InviteRecord, MachineId, MachineMembership, RoutingState,
    ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use tokio::sync::mpsc;

use crate::NatsStore;
use crate::buckets::ensure_assets;
use crate::store::deploys::replay_projection;
use crate::store::instances::list_all_instance_status;

#[async_trait]
impl StoreBackend for NatsStore {
    async fn init(&self) -> Result<()> {
        ensure_assets(self.jetstream(), self.asset_policy()).await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        MachineRegistry::list_machines(self).await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        MachineRegistry::upsert_self_machine(self, record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        MachineRegistry::delete_machine(self, id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        MachineRegistry::subscribe_machines(self).await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        InviteRepository::create_invite(self, invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        InviteRepository::get_invite(self, invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        InviteRepository::list_invites(self).await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        InviteRepository::redeem_invite(self, invite_id, machine_id, now_unix_secs).await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        InviteRepository::revoke_invite(self, invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        RoutingSnapshotReader::load_routing_state(self).await
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingSubscription> {
        RoutingSnapshotReader::subscribe_routing_events(self).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        DeployRepository::list_deploy_releases(self, namespace).await
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        DeployRepository::load_deploy_snapshot(self, namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        DeployRepository::list_volumes(self, namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        DeployRepository::get_volume(self, namespace, volume_name).await
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        DeployRepository::record_service_revision(self, command).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        DeployRepository::commit_deploy(self, command).await
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        DeployRepository::update_deploy_record(self, command).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        DeployRepository::get_deploy(self, deploy_id).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        InstanceStatusRepository::list_instance_status(self, namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        InstanceStatusRepository::record_instance_status(self, record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        InstanceStatusRepository::remove_instance_status(self, instance_id).await
    }

    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        CertificateStore::get_acme_account(self, issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        CertificateStore::upsert_acme_account(self, record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        CertificateStore::list_certificates(self).await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        CertificateStore::get_certificate(self, hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        CertificateStore::upsert_certificate(self, record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        CertificateStore::list_acme_challenges(self).await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        CertificateStore::upsert_acme_challenge(self, record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        CertificateStore::delete_acme_challenge(self, hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        CertificateStore::subscribe_certificates(self).await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        CertificateStore::subscribe_acme_challenges(self).await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        SyncProbe::sync_status(self).await
    }

    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        PeerRttStore::peer_rtt_observations(self).await
    }

    async fn peer_membership_observations(&self) -> Result<Vec<PeerMembershipObservation>> {
        PeerMembershipStore::peer_membership_observations(self).await
    }
}

impl RoutingSnapshotReader for NatsStore {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        let projection = replay_projection(self.jetstream()).await?;
        Ok(RoutingState {
            machines: MachineRegistry::list_machines(self).await?,
            revisions: projection.all_revisions(),
            releases: projection.all_releases(),
            instances: list_all_instance_status(self).await?,
        })
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingSubscription> {
        let state = RoutingSnapshotReader::load_routing_state(self).await?;
        let (_tx, rx) = mpsc::channel(128);
        Ok((state, rx))
    }
}

impl SyncProbe for NatsStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }
}

impl PeerRttStore for NatsStore {}

impl PeerMembershipStore for NatsStore {}

#[async_trait]
impl StoreRuntimeControl for NatsStore {
    async fn start(&self) -> Result<()> {
        ensure_assets(self.jetstream(), self.asset_policy()).await
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    async fn wipe_data(&self) -> Result<()> {
        Ok(())
    }

    async fn healthy(&self) -> bool {
        self.client().flush().await.is_ok()
    }
}
