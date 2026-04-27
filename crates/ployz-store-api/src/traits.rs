use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::future::Future;
use tokio::sync::mpsc;

pub type MachineSubscription = (Vec<MachineRecord>, mpsc::Receiver<MachineEvent>);
pub type RoutingInvalidationSubscription = mpsc::Receiver<()>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommit {
    pub namespace: Namespace,
    pub removed_services: Vec<String>,
    pub releases: Vec<ServiceReleaseRecord>,
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
    fn load_deploy_snapshot<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<DeploySnapshot>> + Send + 'a;

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
