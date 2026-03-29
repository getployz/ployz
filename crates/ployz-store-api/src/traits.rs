use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
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

#[async_trait]
pub trait MachineStore: Send + Sync {
    async fn init(&self) -> Result<()> {
        Ok(())
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>>;

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()>;

    async fn delete_machine(&self, id: &MachineId) -> Result<()>;

    async fn subscribe_machines(&self) -> Result<MachineSubscription>;
}

#[async_trait]
pub trait InviteStore: Send + Sync {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()>;

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()>;
}

#[async_trait]
pub trait RoutingStore: Send + Sync {
    async fn load_routing_state(&self) -> Result<RoutingState>;

    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription>;
}

#[derive(Debug, Clone)]
pub struct DeployCommit {
    pub namespace: Namespace,
    pub removed_services: Vec<String>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub deploy: DeployRecord,
}

#[async_trait]
pub trait DeployReadStore: Send + Sync {
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

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>>;
}

#[async_trait]
pub trait DeployWriteStore: Send + Sync {
    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()>;

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()>;

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()>;

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()>;

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()>;

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()>;
}

#[async_trait]
pub trait DeployCommitStore: Send + Sync {
    async fn apply_deploy_commit(&self, commit: &DeployCommit) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Syncing { gaps: u64 },
    Synced,
}

#[async_trait]
pub trait SyncProbe: Send + Sync {
    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }
}

#[async_trait]
pub trait BootstrapStateReader: Send + Sync {
    async fn seed_machine_records(&self) -> Result<Vec<MachineRecord>>;

    async fn bootstrap_addrs(&self, local_machine_id: &MachineId) -> Result<Vec<String>>;
}
