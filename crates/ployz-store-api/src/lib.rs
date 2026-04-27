mod driver;
pub mod memory;
mod traits;

use async_trait::async_trait;
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineId,
    MachineRecord, RoutingState, ServiceReleaseRecord,
};
use ployz_types::spec::Namespace;

pub use driver::StoreDriver;
pub use traits::{
    DeployCommit, DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, MachineSubscription,
    RoutingInvalidationSubscription, RoutingSnapshotReader, StoreRuntimeControl, SyncProbe,
    SyncStatus,
};

#[async_trait]
pub trait StoreBackend: Send + Sync {
    async fn init(&self) -> Result<()>;
    async fn list_machines(&self) -> Result<Vec<MachineRecord>>;
    async fn upsert_self_machine(&self, record: &MachineRecord) -> Result<()>;
    async fn delete_machine(&self, id: &MachineId) -> Result<()>;
    async fn subscribe_machines(&self) -> Result<MachineSubscription>;

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()>;
    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>>;
    async fn list_invites(&self) -> Result<Vec<InviteRecord>>;
    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord>;
    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord>;

    async fn load_routing_state(&self) -> Result<RoutingState>;
    async fn subscribe_routing_invalidations(&self) -> Result<RoutingInvalidationSubscription>;

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>>;
    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot>;
    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()>;
    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()>;
    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()>;
    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>>;

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>>;
    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()>;
    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()>;

    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(SyncStatus::Synced)
    }
}
