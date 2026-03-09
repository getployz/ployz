use crate::error::Result;
use crate::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineRecord, RoutingState, ServiceHeadRecord, ServiceRevisionRecord,
    ServiceSlotRecord,
};
use crate::spec::Namespace;
use std::future::Future;
use tokio::sync::mpsc;

pub trait MachineStore: Send + Sync {
    fn init(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async { Ok(()) }
    }
    fn list_machines(&self) -> impl Future<Output = Result<Vec<MachineRecord>>> + Send + '_;
    fn upsert_machine<'a>(
        &'a self,
        record: &'a MachineRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn subscribe_machines(
        &self,
    ) -> impl Future<Output = Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)>> + Send + '_;
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
    ) -> impl Future<Output = Result<mpsc::Receiver<()>>> + Send + '_;
}

pub trait DeployStore: Send + Sync {
    fn list_service_heads<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceHeadRecord>>> + Send + 'a;

    fn list_service_slots<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<ServiceSlotRecord>>> + Send + 'a;

    fn list_instance_status<'a>(
        &'a self,
        namespace: &'a Namespace,
    ) -> impl Future<Output = Result<Vec<InstanceStatusRecord>>> + Send + 'a;

    fn upsert_service_revision<'a>(
        &'a self,
        record: &'a ServiceRevisionRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn upsert_service_head<'a>(
        &'a self,
        record: &'a ServiceHeadRecord,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn delete_service_head<'a>(
        &'a self,
        namespace: &'a Namespace,
        service: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn replace_service_slots<'a>(
        &'a self,
        namespace: &'a Namespace,
        service: &'a str,
        records: &'a [ServiceSlotRecord],
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
        heads: &'a [ServiceHeadRecord],
        slots: &'a [ServiceSlotRecord],
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

pub trait StoreRuntimeControl: Send + Sync {
    fn start(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn stop(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn healthy(&self) -> impl Future<Output = bool> + Send + '_;
}
