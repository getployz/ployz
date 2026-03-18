use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::future::Future;
use tokio::sync::mpsc;

pub type MachineSubscription = (Vec<MachineRecord>, MachineEventSubscription);

pub struct MachineEventSubscription {
    inner: mpsc::Receiver<MachineEvent>,
}

impl MachineEventSubscription {
    #[must_use]
    pub fn new(inner: mpsc::Receiver<MachineEvent>) -> Self {
        Self { inner }
    }

    pub async fn recv(&mut self) -> Option<MachineEvent> {
        self.inner.recv().await
    }
}

pub struct RoutingInvalidationSubscription {
    inner: mpsc::Receiver<()>,
}

impl RoutingInvalidationSubscription {
    #[must_use]
    pub fn new(inner: mpsc::Receiver<()>) -> Self {
        Self { inner }
    }

    pub async fn recv(&mut self) -> Option<()> {
        self.inner.recv().await
    }

    pub fn try_recv(&mut self) -> std::result::Result<(), mpsc::error::TryRecvError> {
        self.inner.try_recv()
    }
}

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

    fn consume_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
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
    async fn healthy(&self) -> bool;
}
