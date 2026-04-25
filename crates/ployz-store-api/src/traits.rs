use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent,
    CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord,
    MachineEvent, MachineId, MachineRecord, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::future::Future;
use tokio::sync::mpsc;

pub type MachineSubscription = (Vec<MachineRecord>, mpsc::Receiver<MachineEvent>);
pub type CertificateSubscription = (Vec<CertificateRecord>, mpsc::Receiver<CertificateEvent>);
pub type AcmeChallengeSubscription = (Vec<AcmeChallengeRecord>, mpsc::Receiver<AcmeChallengeEvent>);
pub type RoutingInvalidationSubscription = mpsc::Receiver<()>;

pub trait MachineStore: Send + Sync {
    fn init(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async { Ok(()) }
    }

    fn list_machines(&self) -> impl Future<Output = Result<Vec<MachineRecord>>> + Send + '_;

    fn upsert_self_machine<'a>(
        &'a self,
        record: &'a MachineRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_machines(&self) -> impl Future<Output = Result<MachineSubscription>> + Send + '_;
}

pub trait InviteStore: Send + Sync {
    fn create_invite<'a>(
        &'a self,
        invite: &'a InviteRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_invite<'a>(
        &'a self,
        invite_id: &'a str,
    ) -> impl Future<Output = Result<Option<InviteRecord>>> + Send + 'a;

    fn list_invites(&self) -> impl Future<Output = Result<Vec<InviteRecord>>> + Send + '_;

    fn redeem_invite<'a>(
        &'a self,
        invite_id: &'a str,
        machine_id: &'a MachineId,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<InviteRecord>> + Send + 'a;

    fn revoke_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<InviteRecord>> + Send + 'a;
}

pub trait RoutingStore: Send + Sync {
    fn load_routing_state(&self) -> impl Future<Output = Result<RoutingState>> + Send + '_;

    fn subscribe_routing_invalidations(
        &self,
    ) -> impl Future<Output = Result<RoutingInvalidationSubscription>> + Send + '_;
}

pub trait DeployStore: Send + Sync {
    fn list_service_revisions<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceRevisionRecord>>> + Send + 'a;

    fn list_service_releases<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceReleaseRecord>>> + Send + 'a;

    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a;

    fn upsert_service_revision<'a>(
        &'a self,
        record: &'a ServiceRevisionRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn upsert_service_release<'a>(
        &'a self,
        record: &'a ServiceReleaseRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_service_release<'a>(
        &'a self,
        namespace: &'a Namespace,
        service: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn upsert_instance_status<'a>(
        &'a self,
        record: &'a InstanceStatusRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_instance_status<'a>(
        &'a self,
        instance_id: &'a InstanceId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn upsert_deploy<'a>(
        &'a self,
        record: &'a DeployRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn commit_deploy<'a>(
        &'a self,
        namespace: &'a Namespace,
        removed_services: &'a [String],
        releases: &'a [ServiceReleaseRecord],
        deploy: &'a DeployRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_deploy<'a>(
        &'a self,
        deploy_id: &'a DeployId,
    ) -> impl Future<Output = Result<Option<DeployRecord>>> + Send + 'a;
}

pub trait CertificateStore: Send + Sync {
    fn get_acme_account<'a>(
        &'a self,
        issuer_url: &'a str,
    ) -> impl Future<Output = Result<Option<AcmeAccountRecord>>> + Send + 'a;

    fn upsert_acme_account<'a>(
        &'a self,
        record: &'a AcmeAccountRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_certificates(&self)
    -> impl Future<Output = Result<Vec<CertificateRecord>>> + Send + '_;

    fn get_certificate<'a>(
        &'a self,
        hostname: &'a str,
    ) -> impl Future<Output = Result<Option<CertificateRecord>>> + Send + 'a;

    fn upsert_certificate<'a>(
        &'a self,
        record: &'a CertificateRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn list_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeRecord>>> + Send + '_;

    fn upsert_acme_challenge<'a>(
        &'a self,
        record: &'a AcmeChallengeRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_acme_challenge<'a>(
        &'a self,
        hostname: &'a str,
        token: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_certificates(
        &self,
    ) -> impl Future<Output = Result<CertificateSubscription>> + Send + '_;

    fn subscribe_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<AcmeChallengeSubscription>> + Send + '_;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Syncing { gaps: u64 },
    Synced,
}

pub trait SyncProbe: Send + Sync {
    fn sync_status(&self) -> impl Future<Output = Result<SyncStatus>> + Send + '_ {
        async { Ok(SyncStatus::Synced) }
    }
}

#[async_trait]
pub trait StoreRuntimeControl: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn wipe_data(&self) -> Result<()> {
        Ok(())
    }
    async fn healthy(&self) -> bool;
}
