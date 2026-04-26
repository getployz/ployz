mod driver;
pub mod memory;
mod traits;

use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeRecord, CertificateRecord, DeployId, DeployRecord, InstanceId,
    InstanceStatusRecord, InviteRecord, MachineId, MachineRecord, RoutingState,
    ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;

pub use driver::StoreDriver;
pub use traits::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployStore, InviteStore,
    MachineStore, MachineSubscription, PeerMembershipObservation, PeerMembershipState,
    PeerMembershipStore, PeerRttObservation, PeerRttStore, RoutingInvalidationSubscription,
    RoutingStore, StoreRuntimeControl, SyncProbe, SyncStatus,
};

#[async_trait]
pub trait StoreBackend: Send + Sync {
    async fn init(&self) -> Result<()>;
    async fn list_machines(&self) -> Result<Vec<MachineRecord>>;
    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()>;
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
    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription>;

    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>>;

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>>;

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>>;

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()>;
    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()>;
    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()>;
    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()>;
    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()>;
    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()>;
    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        releases: &[ServiceReleaseRecord],
        deploy: &DeployRecord,
    ) -> Result<()>;
    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>>;

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

    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }

    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        Ok(Vec::new())
    }

    async fn peer_membership_observations(&self) -> Result<Vec<PeerMembershipObservation>> {
        Ok(Vec::new())
    }
}
