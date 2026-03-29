use async_trait::async_trait;
use ployz_store_api::memory::MemoryStore;
use ployz_store_api::{
    DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore, InviteStore,
    MachineStore, MachineSubscription, RoutingInvalidationSubscription, RoutingStore, SyncProbe,
    SyncStatus,
};
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct StoreDriver {
    machine: Arc<dyn MachineStore>,
    invite: Arc<dyn InviteStore>,
    routing: Arc<dyn RoutingStore>,
    deploy_read: Arc<dyn DeployReadStore>,
    deploy_write: Arc<dyn DeployWriteStore>,
    deploy_commit: Arc<dyn DeployCommitStore>,
    sync: Arc<dyn SyncProbe>,
}

impl StoreDriver {
    #[must_use]
    pub(crate) fn memory_with(store: Arc<MemoryStore>) -> Self {
        Self {
            machine: Arc::clone(&store) as Arc<dyn MachineStore>,
            invite: Arc::clone(&store) as Arc<dyn InviteStore>,
            routing: Arc::clone(&store) as Arc<dyn RoutingStore>,
            deploy_read: Arc::clone(&store) as Arc<dyn DeployReadStore>,
            deploy_write: Arc::clone(&store) as Arc<dyn DeployWriteStore>,
            deploy_commit: Arc::clone(&store) as Arc<dyn DeployCommitStore>,
            sync: Arc::clone(&store) as Arc<dyn SyncProbe>,
        }
    }

    #[must_use]
    pub(crate) fn from_store<T>(store: Arc<T>) -> Self
    where
        T: MachineStore
            + InviteStore
            + RoutingStore
            + DeployReadStore
            + DeployWriteStore
            + DeployCommitStore
            + SyncProbe
            + Send
            + Sync
            + 'static,
    {
        Self {
            machine: Arc::clone(&store) as Arc<dyn MachineStore>,
            invite: Arc::clone(&store) as Arc<dyn InviteStore>,
            routing: Arc::clone(&store) as Arc<dyn RoutingStore>,
            deploy_read: Arc::clone(&store) as Arc<dyn DeployReadStore>,
            deploy_write: Arc::clone(&store) as Arc<dyn DeployWriteStore>,
            deploy_commit: Arc::clone(&store) as Arc<dyn DeployCommitStore>,
            sync: store as Arc<dyn SyncProbe>,
        }
    }

    #[must_use]
    pub(crate) fn machine_store(&self) -> Arc<dyn MachineStore> {
        Arc::clone(&self.machine)
    }

    #[must_use]
    pub(crate) fn sync_probe(&self) -> Arc<dyn SyncProbe> {
        Arc::clone(&self.sync)
    }
}

#[async_trait]
impl MachineStore for StoreDriver {
    async fn init(&self) -> Result<()> {
        self.machine.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        self.machine.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()> {
        self.machine.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.machine.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.machine.subscribe_machines().await
    }
}

#[async_trait]
impl InviteStore for StoreDriver {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.invite.create_invite(invite).await
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        self.invite.consume_invite(invite_id, now_unix_secs).await
    }
}

#[async_trait]
impl RoutingStore for StoreDriver {
    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.routing.load_routing_state().await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription> {
        self.routing.subscribe_routing_invalidations().await
    }
}

#[async_trait]
impl DeployReadStore for StoreDriver {
    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        self.deploy_read.list_service_revisions(namespace).await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.deploy_read.list_service_releases(namespace).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.deploy_read.list_instance_status(namespace).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.deploy_read.get_deploy(deploy_id).await
    }
}

#[async_trait]
impl DeployWriteStore for StoreDriver {
    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        self.deploy_write.upsert_service_revision(record).await
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        self.deploy_write.upsert_service_release(record).await
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        self.deploy_write
            .delete_service_release(namespace, service)
            .await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.deploy_write.upsert_instance_status(record).await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.deploy_write.delete_instance_status(instance_id).await
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        self.deploy_write.upsert_deploy(record).await
    }
}

#[async_trait]
impl DeployCommitStore for StoreDriver {
    async fn apply_deploy_commit(&self, commit: &DeployCommit) -> Result<()> {
        self.deploy_commit.apply_deploy_commit(commit).await
    }
}

#[async_trait]
impl SyncProbe for StoreDriver {
    async fn sync_status(&self) -> Result<SyncStatus> {
        self.sync.sync_status().await
    }
}
