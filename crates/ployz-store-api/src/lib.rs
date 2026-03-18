mod driver;
pub mod memory;
mod traits;

use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
};
use ployz_types::spec::Namespace;

pub use driver::StoreDriver;
pub use traits::{
    DeployStore, InviteStore, MachineStore, MachineSubscription, RoutingInvalidationSubscription,
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
    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()>;

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

    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }
}
