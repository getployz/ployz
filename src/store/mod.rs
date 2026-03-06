pub mod network;

use crate::error::Result;
use crate::model::{InviteRecord, MachineEvent, MachineId, MachineRecord, MachineStatus};
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
    fn update_heartbeat<'a>(
        &'a self,
        id: &'a MachineId,
        status: MachineStatus,
        timestamp: u64,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
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

pub trait ServiceControl: Send + Sync {
    fn start(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn stop(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn healthy(&self) -> impl Future<Output = bool> + Send + '_;
}
