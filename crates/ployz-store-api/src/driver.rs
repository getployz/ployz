use crate::memory::MemoryStore;
use crate::{
    DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore, InviteStore,
    MachineStore, MachineSubscription, RoutingInvalidationSubscription, RoutingStore, SyncProbe,
    SyncStatus,
};
use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::sync::Arc;

#[async_trait]
trait MachineBackend: Send + Sync {
    async fn init(&self) -> Result<()>;
    async fn list_machines(&self) -> Result<Vec<MachineRecord>>;
    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()>;
    async fn delete_machine(&self, id: &MachineId) -> Result<()>;
    async fn subscribe_machines(&self) -> Result<MachineSubscription>;
}

#[async_trait]
impl<T> MachineBackend for T
where
    T: MachineStore + Send + Sync,
{
    async fn init(&self) -> Result<()> {
        MachineStore::init(self).await
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        MachineStore::list_machines(self).await
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()> {
        MachineStore::upsert_self_machine(self, record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        MachineStore::delete_machine(self, id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        MachineStore::subscribe_machines(self).await
    }
}

#[async_trait]
trait InviteBackend: Send + Sync {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()>;
    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()>;
}

#[async_trait]
impl<T> InviteBackend for T
where
    T: InviteStore + Send + Sync,
{
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        InviteStore::create_invite(self, invite).await
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        InviteStore::consume_invite(self, invite_id, now_unix_secs).await
    }
}

#[async_trait]
trait RoutingBackend: Send + Sync {
    async fn load_routing_state(&self) -> Result<RoutingState>;
    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription>;
}

#[async_trait]
impl<T> RoutingBackend for T
where
    T: RoutingStore + Send + Sync,
{
    async fn load_routing_state(&self) -> Result<RoutingState> {
        RoutingStore::load_routing_state(self).await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription> {
        RoutingStore::subscribe_routing_invalidations(self).await
    }
}

#[async_trait]
trait DeployBackend: Send + Sync {
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
    async fn commit_deploy(&self, commit: &DeployCommit) -> Result<()>;
    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>>;
}

#[async_trait]
impl<T> DeployBackend for T
where
    T: DeployReadStore + DeployWriteStore + DeployCommitStore + Send + Sync,
{
    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        DeployReadStore::list_service_revisions(self, namespace).await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        DeployReadStore::list_service_releases(self, namespace).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        DeployReadStore::list_instance_status(self, namespace).await
    }

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        DeployWriteStore::upsert_service_revision(self, record).await
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        DeployWriteStore::upsert_service_release(self, record).await
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        DeployWriteStore::delete_service_release(self, namespace, service).await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        DeployWriteStore::upsert_instance_status(self, record).await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        DeployWriteStore::delete_instance_status(self, instance_id).await
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        DeployWriteStore::upsert_deploy(self, record).await
    }

    async fn commit_deploy(&self, commit: &DeployCommit) -> Result<()> {
        DeployCommitStore::apply_deploy_commit(self, commit).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        DeployReadStore::get_deploy(self, deploy_id).await
    }
}

#[async_trait]
trait SyncBackend: Send + Sync {
    async fn sync_status(&self) -> Result<SyncStatus>;
}

#[async_trait]
impl<T> SyncBackend for T
where
    T: SyncProbe + Send + Sync,
{
    async fn sync_status(&self) -> Result<SyncStatus> {
        SyncProbe::sync_status(self).await
    }
}

#[derive(Clone)]
pub struct StoreDriver {
    machine: Arc<dyn MachineBackend>,
    invite: Arc<dyn InviteBackend>,
    routing: Arc<dyn RoutingBackend>,
    deploy: Arc<dyn DeployBackend>,
    sync: Arc<dyn SyncBackend>,
    memory_store: Option<Arc<MemoryStore>>,
}

impl StoreDriver {
    #[must_use]
    pub fn memory() -> Self {
        Self::memory_with(Arc::new(MemoryStore::new()))
    }

    #[must_use]
    pub fn memory_with(store: Arc<MemoryStore>) -> Self {
        Self {
            machine: Arc::clone(&store) as Arc<dyn MachineBackend>,
            invite: Arc::clone(&store) as Arc<dyn InviteBackend>,
            routing: Arc::clone(&store) as Arc<dyn RoutingBackend>,
            deploy: Arc::clone(&store) as Arc<dyn DeployBackend>,
            sync: Arc::clone(&store) as Arc<dyn SyncBackend>,
            memory_store: Some(store),
        }
    }

    #[must_use]
    pub fn from_store<T>(store: Arc<T>) -> Self
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
            machine: Arc::clone(&store) as Arc<dyn MachineBackend>,
            invite: Arc::clone(&store) as Arc<dyn InviteBackend>,
            routing: Arc::clone(&store) as Arc<dyn RoutingBackend>,
            deploy: Arc::clone(&store) as Arc<dyn DeployBackend>,
            sync: store as Arc<dyn SyncBackend>,
            memory_store: None,
        }
    }

    #[must_use]
    pub fn memory_store(&self) -> Option<Arc<MemoryStore>> {
        self.memory_store.as_ref().map(Arc::clone)
    }
}

impl MachineStore for StoreDriver {
    fn init(&self) -> impl std::future::Future<Output = Result<()>> + Send + '_ {
        async move { self.machine.init().await }
    }

    fn list_machines(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<MachineRecord>>> + Send + '_ {
        async move { self.machine.list_machines().await }
    }

    fn upsert_self_machine<'a>(
        &'a self,
        record: &'a MachineRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.machine.upsert_self_machine(record).await }
    }

    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.machine.delete_machine(id).await }
    }

    fn subscribe_machines(
        &self,
    ) -> impl std::future::Future<Output = Result<MachineSubscription>> + Send + '_ {
        async move { self.machine.subscribe_machines().await }
    }
}

impl InviteStore for StoreDriver {
    fn create_invite<'a>(
        &'a self,
        invite: &'a InviteRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.invite.create_invite(invite).await }
    }

    fn consume_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.invite.consume_invite(invite_id, now_unix_secs).await }
    }
}

impl RoutingStore for StoreDriver {
    fn load_routing_state(
        &self,
    ) -> impl std::future::Future<Output = Result<RoutingState>> + Send + '_ {
        async move { self.routing.load_routing_state().await }
    }

    fn subscribe_routing_invalidations(
        &self,
    ) -> impl std::future::Future<Output = Result<RoutingInvalidationSubscription>> + Send + '_
    {
        async move { self.routing.subscribe_routing_invalidations().await }
    }
}

impl DeployReadStore for StoreDriver {
    fn list_service_revisions<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl std::future::Future<Output = Result<Vec<ServiceRevisionRecord>>> + Send + 'a {
        async move { self.deploy.list_service_revisions(namespace).await }
    }

    fn list_service_releases<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl std::future::Future<Output = Result<Vec<ServiceReleaseRecord>>> + Send + 'a {
        async move { self.deploy.list_service_releases(namespace).await }
    }

    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl std::future::Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a {
        async move { self.deploy.list_instance_status(namespace).await }
    }

    fn get_deploy<'a>(
        &'a self,
        deploy_id: &'a DeployId,
    ) -> impl std::future::Future<Output = Result<Option<DeployRecord>>> + Send + 'a {
        async move { self.deploy.get_deploy(deploy_id).await }
    }
}

impl DeployWriteStore for StoreDriver {
    fn upsert_service_revision<'a>(
        &'a self,
        record: &'a ServiceRevisionRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.upsert_service_revision(record).await }
    }

    fn upsert_service_release<'a>(
        &'a self,
        record: &'a ServiceReleaseRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.upsert_service_release(record).await }
    }

    fn delete_service_release<'a>(
        &'a self,
        namespace: &'a Namespace,
        service: &'a str,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.delete_service_release(namespace, service).await }
    }

    fn upsert_instance_status<'a>(
        &'a self,
        record: &'a InstanceStatusRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.upsert_instance_status(record).await }
    }

    fn delete_instance_status<'a>(
        &'a self,
        instance_id: &'a InstanceId,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.delete_instance_status(instance_id).await }
    }

    fn upsert_deploy<'a>(
        &'a self,
        record: &'a DeployRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.upsert_deploy(record).await }
    }
}

impl DeployCommitStore for StoreDriver {
    fn apply_deploy_commit<'a>(
        &'a self,
        commit: &'a DeployCommit,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.deploy.commit_deploy(commit).await }
    }
}

impl SyncProbe for StoreDriver {
    fn sync_status(&self) -> impl std::future::Future<Output = Result<SyncStatus>> + Send + '_ {
        async move { self.sync.sync_status().await }
    }
}
