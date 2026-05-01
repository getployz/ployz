mod driver;
pub mod memory;
mod traits;

use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateRecord,
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineMembership, RoutingState, ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;

pub use driver::StoreDriver;
pub use traits::{
    AcmeChallengeSubscription, AcmeChallengeSubscriptionUpdate, CertificateStore,
    CertificateSubscription, CertificateSubscriptionUpdate, DeployCommit, DeployRecordUpdate,
    DeployRepository, DeployRevisionUpsert, DeploySnapshot, InstanceStatusRepository,
    InviteRepository, MachineRegistry, MachineSubscription, MachineSubscriptionUpdate,
    PeerRttObservation, PeerRttStore, RoutingBatchSubscription, RoutingBatchSubscriptionUpdate,
    RoutingEventBatch, RoutingSnapshotReader, RoutingSubscription, StoreRuntimeControl, SyncProbe,
    SyncStatus, apply_routing_event, apply_routing_events,
};

#[async_trait]
pub trait StoreBackend: Send + Sync {
    async fn init(&self) -> Result<()>;
    async fn list_machines(&self) -> Result<Vec<MachineMembership>>;
    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()>;
    async fn delete_machine(&self, id: &MachineId) -> Result<()>;
    async fn subscribe_machines(&self) -> Result<MachineSubscription>;

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()>;
    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>>;
    async fn list_invites(&self) -> Result<Vec<InviteRecord>>;
    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord>;
    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord>;

    async fn load_routing_state(&self) -> Result<RoutingState>;
    async fn subscribe_routing_batches(
        &self,
        subscription: RoutingSubscription,
    ) -> Result<RoutingBatchSubscription>;

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>>;
    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot>;
    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>>;
    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>>;
    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()>;
    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()>;
    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()>;
    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>>;

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>>;
    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()>;
    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()>;

    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>>;
    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()>;
    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>>;
    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>>;
    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()>;
    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>>;
    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()>;
    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()>;
    async fn subscribe_certificates(&self) -> Result<CertificateSubscription>;
    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription>;
    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()>;
    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>>;

    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }

    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        Ok(Vec::new())
    }
}
