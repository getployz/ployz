use crate::memory::{MemoryService, MemoryStore};
use crate::{
    DeployStore, InviteStore, MachineStore, MachineSubscription, RoutingInvalidationSubscription,
    RoutingStore, StoreBackend, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
use std::sync::Arc;

#[derive(Clone)]
pub struct StoreDriver {
    backend: Arc<dyn StoreBackend>,
    runtime_control: Arc<dyn StoreRuntimeControl>,
    memory_store: Option<Arc<MemoryStore>>,
    memory_service: Option<Arc<MemoryService>>,
}

impl StoreDriver {
    #[must_use]
    pub fn memory() -> Self {
        Self::memory_with(Arc::new(MemoryStore::new()), Arc::new(MemoryService::new()))
    }

    #[must_use]
    pub fn memory_with(store: Arc<MemoryStore>, service: Arc<MemoryService>) -> Self {
        let backend = Arc::new(MemoryStoreBackend {
            store: Arc::clone(&store),
            service: Arc::clone(&service),
        });
        Self {
            backend: Arc::clone(&backend) as Arc<dyn StoreBackend>,
            runtime_control: backend as Arc<dyn StoreRuntimeControl>,
            memory_store: Some(store),
            memory_service: Some(service),
        }
    }

    #[must_use]
    pub fn from_backend(
        backend: Arc<dyn StoreBackend>,
        runtime_control: Arc<dyn StoreRuntimeControl>,
    ) -> Self {
        Self {
            backend,
            runtime_control,
            memory_store: None,
            memory_service: None,
        }
    }

    #[must_use]
    pub fn memory_store(&self) -> Option<Arc<MemoryStore>> {
        self.memory_store.as_ref().map(Arc::clone)
    }

    #[must_use]
    pub fn memory_service(&self) -> Option<Arc<MemoryService>> {
        self.memory_service.as_ref().map(Arc::clone)
    }
}

#[async_trait]
impl StoreRuntimeControl for StoreDriver {
    async fn start(&self) -> Result<()> {
        self.runtime_control.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.runtime_control.stop().await
    }

    async fn healthy(&self) -> bool {
        self.runtime_control.healthy().await
    }
}

impl MachineStore for StoreDriver {
    fn init(&self) -> impl std::future::Future<Output = Result<()>> + Send + '_ {
        async move { self.backend.init().await }
    }

    fn list_machines(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<MachineRecord>>> + Send + '_ {
        async move { self.backend.list_machines().await }
    }

    fn upsert_self_machine<'a>(
        &'a self,
        record: &'a MachineRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.upsert_self_machine(record).await }
    }

    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.delete_machine(id).await }
    }

    fn subscribe_machines(
        &self,
    ) -> impl std::future::Future<Output = Result<MachineSubscription>> + Send + '_ {
        async move { self.backend.subscribe_machines().await }
    }
}

impl InviteStore for StoreDriver {
    fn create_invite<'a>(
        &'a self,
        invite: &'a InviteRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.create_invite(invite).await }
    }

    fn consume_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.consume_invite(invite_id, now_unix_secs).await }
    }
}

impl RoutingStore for StoreDriver {
    fn load_routing_state(
        &self,
    ) -> impl std::future::Future<Output = Result<RoutingState>> + Send + '_ {
        async move { self.backend.load_routing_state().await }
    }

    fn subscribe_routing_invalidations(
        &self,
    ) -> impl std::future::Future<Output = Result<RoutingInvalidationSubscription>> + Send + '_
    {
        async move { self.backend.subscribe_routing_invalidations().await }
    }
}

impl DeployStore for StoreDriver {
    fn list_service_revisions<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl std::future::Future<Output = Result<Vec<ServiceRevisionRecord>>> + Send + 'a {
        async move { self.backend.list_service_revisions(namespace).await }
    }

    fn list_service_releases<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl std::future::Future<Output = Result<Vec<ServiceReleaseRecord>>> + Send + 'a {
        async move { self.backend.list_service_releases(namespace).await }
    }

    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl std::future::Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a {
        async move { self.backend.list_instance_status(namespace).await }
    }

    fn upsert_service_revision<'a>(
        &'a self,
        record: &'a ServiceRevisionRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.upsert_service_revision(record).await }
    }

    fn upsert_service_release<'a>(
        &'a self,
        record: &'a ServiceReleaseRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.upsert_service_release(record).await }
    }

    fn delete_service_release<'a>(
        &'a self,
        namespace: &'a Namespace,
        service: &'a str,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move {
            self.backend
                .delete_service_release(namespace, service)
                .await
        }
    }

    fn upsert_instance_status<'a>(
        &'a self,
        record: &'a InstanceStatusRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.upsert_instance_status(record).await }
    }

    fn delete_instance_status<'a>(
        &'a self,
        instance_id: &'a InstanceId,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.delete_instance_status(instance_id).await }
    }

    fn upsert_deploy<'a>(
        &'a self,
        record: &'a DeployRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.upsert_deploy(record).await }
    }

    fn commit_deploy<'a>(
        &'a self,
        namespace: &'a Namespace,
        removed_services: &'a [String],
        releases: &'a [ServiceReleaseRecord],
        deploy: &'a DeployRecord,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move {
            self.backend
                .commit_deploy(namespace, removed_services, releases, deploy)
                .await
        }
    }

    fn get_deploy<'a>(
        &'a self,
        deploy_id: &'a DeployId,
    ) -> impl std::future::Future<Output = Result<Option<DeployRecord>>> + Send + 'a {
        async move { self.backend.get_deploy(deploy_id).await }
    }
}

impl SyncProbe for StoreDriver {
    fn sync_status(&self) -> impl std::future::Future<Output = Result<SyncStatus>> + Send + '_ {
        async move { self.backend.sync_status().await }
    }
}

struct MemoryStoreBackend {
    store: Arc<MemoryStore>,
    service: Arc<MemoryService>,
}

#[async_trait]
impl StoreBackend for MemoryStoreBackend {
    async fn init(&self) -> Result<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.store.subscribe_machines().await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.store.create_invite(invite).await
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        self.store.consume_invite(invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription> {
        self.store.subscribe_routing_invalidations().await
    }

    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        self.store.list_service_revisions(namespace).await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.store.list_service_releases(namespace).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
        self.store.upsert_service_revision(record).await
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> Result<()> {
        self.store.upsert_service_release(record).await
    }

    async fn delete_service_release(&self, namespace: &Namespace, service: &str) -> Result<()> {
        self.store.delete_service_release(namespace, service).await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.store.upsert_instance_status(record).await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.store.delete_instance_status(instance_id).await
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        self.store.upsert_deploy(record).await
    }

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        releases: &[ServiceReleaseRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        self.store
            .commit_deploy(namespace, removed_services, releases, deploy)
            .await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        self.store.sync_status().await
    }
}

#[async_trait]
impl StoreRuntimeControl for MemoryStoreBackend {
    async fn start(&self) -> Result<()> {
        self.service.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.service.stop().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }
}
