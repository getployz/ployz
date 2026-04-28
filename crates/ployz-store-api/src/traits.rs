use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent,
    CertificateRecord, DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord,
    MachineEvent, MachineId, MachineMembership, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use std::future::Future;
use std::net::SocketAddr;
use tokio::sync::mpsc;

pub type MachineSubscription = (Vec<MachineMembership>, mpsc::Receiver<MachineEvent>);
pub type CertificateSubscription = (Vec<CertificateRecord>, mpsc::Receiver<CertificateEvent>);
pub type AcmeChallengeSubscription = (Vec<AcmeChallengeRecord>, mpsc::Receiver<AcmeChallengeEvent>);
pub type RoutingInvalidationSubscription = mpsc::Receiver<()>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommit {
    pub namespace: Namespace,
    pub removed_services: Vec<String>,
    pub removed_volumes: Vec<String>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub volumes: Vec<VolumeRecord>,
    pub deploy: DeployRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRevisionUpsert {
    pub revision: ServiceRevisionRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRecordUpdate {
    pub deploy: DeployRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySnapshot {
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

pub trait MachineRegistry: Send + Sync {
    fn init(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async { Ok(()) }
    }

    fn list_machines(&self) -> impl Future<Output = Result<Vec<MachineMembership>>> + Send + '_;

    fn upsert_self_machine<'a>(
        &'a self,
        record: &'a MachineMembership,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn subscribe_machines(&self) -> impl Future<Output = Result<MachineSubscription>> + Send + '_;
}

pub trait InviteRepository: Send + Sync {
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

pub trait RoutingSnapshotReader: Send + Sync {
    fn load_routing_state(&self) -> impl Future<Output = Result<RoutingState>> + Send + '_;

    fn subscribe_routing_invalidations(
        &self,
    ) -> impl Future<Output = Result<RoutingInvalidationSubscription>> + Send + '_;
}

pub trait DeployRepository: Send + Sync {
    fn list_deploy_releases<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceReleaseRecord>>> + Send + 'a;

    fn load_deploy_snapshot<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<DeploySnapshot>> + Send + 'a;

    fn list_volumes<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<VolumeRecord>>> + Send + 'a;

    fn get_volume<'a>(
        &'a self,
        namespace: &'a Namespace,
        volume_name: &'a str,
    ) -> impl Future<Output = Result<Option<VolumeRecord>>> + Send + 'a;

    fn record_service_revision<'a>(
        &'a self,
        command: &'a DeployRevisionUpsert,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn commit_deploy<'a>(
        &'a self,
        command: &'a DeployCommit,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn update_deploy_record<'a>(
        &'a self,
        command: &'a DeployRecordUpdate,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn get_deploy<'a>(
        &'a self,
        deploy_id: &'a DeployId,
    ) -> impl Future<Output = Result<Option<DeployRecord>>> + Send + 'a;
}

pub trait InstanceStatusRepository: Send + Sync {
    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a;

    fn record_instance_status<'a>(
        &'a self,
        record: &'a InstanceStatusRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn remove_instance_status<'a>(
        &'a self,
        instance_id: &'a InstanceId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRttObservation {
    pub addr: SocketAddr,
    pub rtts_ms: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerMembershipState {
    Alive,
    Suspect,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerMembershipObservation {
    pub addr: SocketAddr,
    pub actor_id: String,
    pub state: PeerMembershipState,
    pub timestamp: u64,
}

pub trait PeerRttStore: Send + Sync {
    fn peer_rtt_observations(
        &self,
    ) -> impl Future<Output = Result<Vec<PeerRttObservation>>> + Send + '_ {
        async { Ok(Vec::new()) }
    }
}

pub trait PeerMembershipStore: Send + Sync {
    fn peer_membership_observations(
        &self,
    ) -> impl Future<Output = Result<Vec<PeerMembershipObservation>>> + Send + '_ {
        async { Ok(Vec::new()) }
    }
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
