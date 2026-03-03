use crate::domain::model::{MachineEvent, MachineId, MachineRecord, PublicKey};
use std::collections::HashMap;
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

pub trait PeerProbe: Send + Sync {
    fn peer_handshakes(
        &self,
    ) -> impl Future<Output = PortResult<HashMap<PublicKey, Option<Instant>>>> + Send + '_;
}

pub trait MembershipStore: Send + Sync {
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

pub trait SyncProbe: Send + Sync {
    fn sync_complete(&self) -> impl Future<Output = PortResult<bool>> + Send + '_;
}

pub trait ServiceControl: Send + Sync {
    fn start(&self) -> impl Future<Output = PortResult<()>> + Send + '_;
    fn stop(&self) -> impl Future<Output = PortResult<()>> + Send + '_;
    fn healthy(&self) -> impl Future<Output = bool> + Send + '_;
}
