use crate::domain::model::{InviteRecord, MachineEvent, MachineId, MachineRecord, PublicKey};
use std::future::Future;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub type PortResult<T> = std::result::Result<T, PortError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PortError {
    #[error("{operation}: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
}

impl PortError {
    #[must_use]
    pub fn operation(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            message: message.into(),
        }
    }
}

pub trait MeshNetwork: Send + Sync {
    fn up(&self) -> impl Future<Output = PortResult<()>> + Send + '_;
    fn down(&self) -> impl Future<Output = PortResult<()>> + Send + '_;
    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl Future<Output = PortResult<()>> + Send + 'a;
}

pub trait MachineStore: Send + Sync {
    fn init(&self) -> impl Future<Output = PortResult<()>> + Send + '_ {
        async { Ok(()) }
    }
    fn list_machines(&self) -> impl Future<Output = PortResult<Vec<MachineRecord>>> + Send + '_;
    fn upsert_machine<'a>(
        &'a self,
        record: &'a MachineRecord,
    ) -> impl Future<Output = PortResult<()>> + Send + 'a;
    fn delete_machine<'a>(
        &'a self,
        id: &'a MachineId,
    ) -> impl Future<Output = PortResult<()>> + Send + 'a;
    fn subscribe_machines(
        &self,
    ) -> impl Future<Output = PortResult<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)>> + Send + '_;
}

pub trait InviteStore: Send + Sync {
    fn create_invite<'a>(
        &'a self,
        invite: &'a InviteRecord,
    ) -> impl Future<Output = PortResult<()>> + Send + 'a;
    fn consume_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> impl Future<Output = PortResult<()>> + Send + 'a;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Syncing { gaps: u64 },
    Synced,
}

pub trait SyncProbe: Send + Sync {
    fn sync_status(&self) -> impl Future<Output = PortResult<SyncStatus>> + Send + '_ {
        async { Ok(SyncStatus::Synced) }
    }
}

pub trait ServiceControl: Send + Sync {
    fn start(&self) -> impl Future<Output = PortResult<()>> + Send + '_;
    fn stop(&self) -> impl Future<Output = PortResult<()>> + Send + '_;
    fn healthy(&self) -> impl Future<Output = bool> + Send + '_;
}

// --- WireGuard device abstraction ---

pub struct DevicePeer {
    pub public_key: PublicKey,
    pub endpoint: Option<String>,
    pub last_handshake: Option<Instant>,
}

pub trait WireGuardDevice: Send + Sync {
    fn read_peers(&self) -> impl Future<Output = PortResult<Vec<DevicePeer>>> + Send + '_;
    fn set_peer_endpoint<'a>(
        &'a self,
        key: &'a PublicKey,
        endpoint: &'a str,
    ) -> impl Future<Output = PortResult<()>> + Send + 'a;
}
