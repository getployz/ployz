use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;
pub use ployz_types::store::{
    DeployStore, InviteStore, MachineStore, RoutingStore, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};
use tokio::sync::mpsc;

#[async_trait]
pub trait StoreBackend: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn healthy(&self) -> bool;

    async fn init(&self) -> Result<()>;
    async fn list_machines(&self) -> Result<Vec<MachineRecord>>;
    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()>;
    async fn delete_machine(&self, id: &MachineId) -> Result<()>;
    async fn subscribe_machines(&self) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)>;

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()>;
    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()>;

    async fn load_routing_state(&self) -> Result<RoutingState>;
    async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>>;

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

    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }
}
