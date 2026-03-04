use crate::adapters::corrosion::CorrosionStore;
use crate::adapters::corrosion::docker::DockerCorrosion;
use crate::adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
use crate::dataplane::traits::{
    InviteStore, MachineStore, MeshNetwork, PortResult, ServiceControl, SyncProbe, SyncStatus,
};
use crate::domain::model::{InviteRecord, MachineEvent, MachineId, MachineRecord};
use std::sync::Arc;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum Network {
    Memory(Arc<MemoryWireGuard>),
}

impl MeshNetwork for Network {
    async fn up(&self) -> PortResult<()> {
        match self {
            Self::Memory(n) => n.up().await,
        }
    }

    async fn down(&self) -> PortResult<()> {
        match self {
            Self::Memory(n) => n.down().await,
        }
    }

    async fn set_peers<'a>(&'a self, peers: &'a [MachineRecord]) -> PortResult<()> {
        match self {
            Self::Memory(n) => n.set_peers(peers).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum Service {
    Memory(Arc<MemoryService>),
    Docker(Arc<DockerCorrosion>),
}

impl ServiceControl for Service {
    async fn start(&self) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.start().await,
            Self::Docker(s) => s.start().await,
        }
    }

    async fn stop(&self) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.stop().await,
            Self::Docker(s) => s.stop().await,
        }
    }

    async fn healthy(&self) -> bool {
        match self {
            Self::Memory(s) => s.healthy().await,
            Self::Docker(s) => s.healthy().await,
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum Store {
    Memory(Arc<MemoryStore>),
    Corrosion(CorrosionStore),
}

impl MachineStore for Store {
    async fn init(&self) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.init().await,
            Self::Corrosion(s) => s.init().await,
        }
    }

    async fn list_machines(&self) -> PortResult<Vec<MachineRecord>> {
        match self {
            Self::Memory(s) => s.list_machines().await,
            Self::Corrosion(s) => s.list_machines().await,
        }
    }

    async fn upsert_machine<'a>(&'a self, record: &'a MachineRecord) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.upsert_machine(record).await,
            Self::Corrosion(s) => s.upsert_machine(record).await,
        }
    }

    async fn delete_machine<'a>(&'a self, id: &'a MachineId) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.delete_machine(id).await,
            Self::Corrosion(s) => s.delete_machine(id).await,
        }
    }

    async fn subscribe_machines(
        &self,
    ) -> PortResult<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        match self {
            Self::Memory(s) => s.subscribe_machines().await,
            Self::Corrosion(s) => s.subscribe_machines().await,
        }
    }
}

impl InviteStore for Store {
    async fn create_invite<'a>(&'a self, invite: &'a InviteRecord) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.create_invite(invite).await,
            Self::Corrosion(s) => s.create_invite(invite).await,
        }
    }

    async fn consume_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> PortResult<()> {
        match self {
            Self::Memory(s) => s.consume_invite(invite_id, now_unix_secs).await,
            Self::Corrosion(s) => s.consume_invite(invite_id, now_unix_secs).await,
        }
    }
}

impl SyncProbe for Store {
    async fn sync_status(&self) -> PortResult<SyncStatus> {
        match self {
            Self::Memory(s) => s.sync_status().await,
            Self::Corrosion(s) => s.sync_status().await,
        }
    }
}
