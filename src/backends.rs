use crate::adapters::corrosion::CorrosionStore;
use crate::adapters::corrosion::docker::DockerCorrosion;
use crate::adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
use crate::adapters::wireguard::{DockerWireGuard, HostWireGuard};
use crate::error::Result;
use crate::mesh::MeshNetwork;
use crate::store::{InviteStore, MachineStore, ServiceControl, SyncProbe, SyncStatus};
use crate::store::model::{InviteRecord, MachineEvent, MachineId, MachineRecord};
use std::sync::Arc;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum WireguardDriver {
    Memory(Arc<MemoryWireGuard>),
    Docker(Arc<DockerWireGuard>),
    Host(Arc<HostWireGuard>),
}

impl MeshNetwork for WireguardDriver {
    async fn up(&self) -> Result<()> {
        match self {
            Self::Memory(n) => n.up().await,
            Self::Docker(n) => n.up().await,
            Self::Host(n) => n.up().await,
        }
    }

    async fn down(&self) -> Result<()> {
        match self {
            Self::Memory(n) => n.down().await,
            Self::Docker(n) => n.down().await,
            Self::Host(n) => n.down().await,
        }
    }

    async fn set_peers<'a>(&'a self, peers: &'a [MachineRecord]) -> Result<()> {
        match self {
            Self::Memory(n) => n.set_peers(peers).await,
            Self::Docker(n) => n.set_peers(peers).await,
            Self::Host(n) => n.set_peers(peers).await,
        }
    }
}

// ---------------------------------------------------------------------------
// DistributedStore — service lifecycle + data layer
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum StoreDriver {
    Memory {
        store: Arc<MemoryStore>,
        service: Arc<MemoryService>,
    },
    Corrosion {
        store: CorrosionStore,
        service: Arc<DockerCorrosion>,
    },
}

impl ServiceControl for StoreDriver {
    async fn start(&self) -> Result<()> {
        match self {
            Self::Memory { service, .. } => service.start().await,
            Self::Corrosion { service, .. } => service.start().await,
        }
    }

    async fn stop(&self) -> Result<()> {
        match self {
            Self::Memory { service, .. } => service.stop().await,
            Self::Corrosion { service, .. } => service.stop().await,
        }
    }

    async fn healthy(&self) -> bool {
        match self {
            Self::Memory { service, .. } => service.healthy().await,
            Self::Corrosion { service, .. } => service.healthy().await,
        }
    }
}

impl MachineStore for StoreDriver {
    async fn init(&self) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.init().await,
            Self::Corrosion { store, .. } => store.init().await,
        }
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        match self {
            Self::Memory { store, .. } => store.list_machines().await,
            Self::Corrosion { store, .. } => store.list_machines().await,
        }
    }

    async fn upsert_machine<'a>(&'a self, record: &'a MachineRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.upsert_machine(record).await,
            Self::Corrosion { store, .. } => store.upsert_machine(record).await,
        }
    }

    async fn delete_machine<'a>(&'a self, id: &'a MachineId) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.delete_machine(id).await,
            Self::Corrosion { store, .. } => store.delete_machine(id).await,
        }
    }

    async fn subscribe_machines(
        &self,
    ) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        match self {
            Self::Memory { store, .. } => store.subscribe_machines().await,
            Self::Corrosion { store, .. } => store.subscribe_machines().await,
        }
    }
}

impl InviteStore for StoreDriver {
    async fn create_invite<'a>(&'a self, invite: &'a InviteRecord) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.create_invite(invite).await,
            Self::Corrosion { store, .. } => store.create_invite(invite).await,
        }
    }

    async fn consume_invite<'a>(
        &'a self,
        invite_id: &'a str,
        now_unix_secs: u64,
    ) -> Result<()> {
        match self {
            Self::Memory { store, .. } => store.consume_invite(invite_id, now_unix_secs).await,
            Self::Corrosion { store, .. } => store.consume_invite(invite_id, now_unix_secs).await,
        }
    }
}

impl SyncProbe for StoreDriver {
    async fn sync_status(&self) -> Result<SyncStatus> {
        match self {
            Self::Memory { store, .. } => store.sync_status().await,
            Self::Corrosion { store, .. } => store.sync_status().await,
        }
    }
}
