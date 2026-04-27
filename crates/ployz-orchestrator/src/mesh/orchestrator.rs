mod dataplane;
mod lifecycle;
mod shutdown;
mod tasks;

use crate::error::Error as PortError;
use crate::mesh::MeshDataplane;
use crate::mesh::MeshNetwork;
use crate::mesh::container_network::ContainerNetwork;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::phase::{Phase, TransitionError};
use crate::mesh::tasks::{
    EndpointMaintainerCommand, PeerSyncCommand, SelfRecordMutation, TaskSet, TaskSetError,
    apply_self_record_mutation,
};
use crate::model::{MachineId, MachineRecord};
use ployz_store_api::StoreDriver;
use ployz_store_api::{MachineRegistry, StoreRuntimeControl, SyncProbe, SyncStatus};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{RwLock, mpsc};

pub type Result<T> = std::result::Result<T, MeshError>;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Task(#[from] TaskSetError),
}

#[derive(Debug, Clone, Copy)]
pub struct MeshReadyStatus {
    pub ready: bool,
    pub phase: Phase,
    pub store_healthy: bool,
    pub sync_connected: bool,
}

pub struct Mesh {
    phase: Phase,
    pub network: WireguardDriver,
    pub store: StoreDriver,
    container_network: Option<ContainerNetwork>,
    tasks: Option<TaskSet>,
    task_cancel: Option<tokio_util::sync::CancellationToken>,
    peer_sync_tx: Option<mpsc::Sender<PeerSyncCommand>>,
    endpoint_maintainer_tx: Option<mpsc::Sender<EndpointMaintainerCommand>>,
    self_record_tx: Option<mpsc::Sender<crate::mesh::tasks::SelfRecordCommand>>,
    bootstrap_interval: Duration,
    connection_timeout: Duration,
    service_ready_timeout: Duration,
    machine_id: MachineId,
    _listen_port: u16,
    seed_records: Vec<MachineRecord>,
    authoritative_self: Option<Arc<RwLock<MachineRecord>>>,
    allow_disconnected_bootstrap: bool,
    dataplane: Option<Arc<dyn MeshDataplane>>,
    wg_ifindex: u32,
}

impl Mesh {
    #[must_use]
    pub fn new(
        network: WireguardDriver,
        store: StoreDriver,
        container_network: Option<ContainerNetwork>,
        machine_id: MachineId,
        listen_port: u16,
    ) -> Self {
        Self {
            phase: Phase::Stopped,
            network,
            store,
            container_network,
            tasks: None,
            task_cancel: None,
            peer_sync_tx: None,
            endpoint_maintainer_tx: None,
            self_record_tx: None,
            bootstrap_interval: Duration::from_millis(500),
            connection_timeout: Duration::from_secs(30),
            service_ready_timeout: Duration::from_secs(15),
            machine_id,
            _listen_port: listen_port,
            seed_records: Vec::new(),
            authoritative_self: None,
            allow_disconnected_bootstrap: false,
            dataplane: None,
            wg_ifindex: 0,
        }
    }

    #[must_use]
    pub fn with_bootstrap_timing(
        mut self,
        interval: Duration,
        connection_timeout: Duration,
    ) -> Self {
        self.bootstrap_interval = interval;
        self.connection_timeout = connection_timeout;
        self
    }

    #[must_use]
    pub fn with_seed_records(mut self, seed_records: Vec<MachineRecord>) -> Self {
        self.seed_records = seed_records;
        self
    }

    #[must_use]
    pub fn container_dns_server(&self) -> Option<Ipv4Addr> {
        self.container_network
            .as_ref()
            .map(ContainerNetwork::container_v4)
    }

    #[must_use]
    pub fn with_disconnected_bootstrap_allowed(
        mut self,
        allow_disconnected_bootstrap: bool,
    ) -> Self {
        self.allow_disconnected_bootstrap = allow_disconnected_bootstrap;
        self
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub fn peer_sync_sender(&self) -> Option<mpsc::Sender<PeerSyncCommand>> {
        self.peer_sync_tx.clone()
    }

    pub async fn run_endpoint_maintenance_once(&self, force_rotate: bool) -> bool {
        let Some(endpoint_maintainer_tx) = self.endpoint_maintainer_tx.as_ref() else {
            return false;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        if endpoint_maintainer_tx
            .send(EndpointMaintainerCommand::TickNow {
                force_rotate,
                complete: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.is_ok()
    }

    pub async fn publish_up_once(&self) -> Option<MachineRecord> {
        let self_record_tx = self.self_record_tx.as_ref()?;
        let bridge_ip = self.network.bridge_ip().await;
        apply_self_record_mutation(self_record_tx, SelfRecordMutation::PublishUp { bridge_ip })
            .await
    }

    pub async fn authoritative_self_record(&self) -> Option<MachineRecord> {
        let authoritative_self = self.authoritative_self.as_ref()?.clone();
        Some(authoritative_self.read().await.clone())
    }

    pub async fn update_authoritative_self_record(
        &self,
        update: impl FnOnce(&mut MachineRecord),
    ) -> Option<MachineRecord> {
        let current = self.authoritative_self_record().await?;
        let mut next = current;
        update(&mut next);
        if let Some(self_record_tx) = &self.self_record_tx {
            return apply_self_record_mutation(self_record_tx, SelfRecordMutation::Replace(next))
                .await;
        }

        let authoritative_self = self.authoritative_self.as_ref()?.clone();
        let mut record = authoritative_self.write().await;
        *record = next;
        Some(record.clone())
    }

    pub async fn ready_status(&self) -> MeshReadyStatus {
        let phase = self.phase;
        let store_healthy = self.store.healthy().await;
        let has_remote_store_peer = self
            .store
            .list_machines()
            .await
            .map(|machines| {
                machines
                    .into_iter()
                    .any(|machine| machine.id != self.machine_id)
            })
            .unwrap_or(false);
        let has_remote_seed_peer = self
            .seed_records
            .iter()
            .any(|machine| machine.id != self.machine_id);
        let has_remote_peer = has_remote_store_peer || has_remote_seed_peer;
        let sync_connected = if has_remote_peer {
            match self.store.sync_status().await {
                Ok(SyncStatus::Disconnected) => false,
                Ok(SyncStatus::Syncing { .. }) | Ok(SyncStatus::Synced) => true,
                Err(_) => false,
            }
        } else {
            true
        };
        let ready = phase == Phase::Running && store_healthy && sync_connected;

        MeshReadyStatus {
            ready,
            phase,
            store_healthy,
            sync_connected,
        }
    }
}

async fn poll_until<F, Fut>(
    timeout: Duration,
    initial_interval: Duration,
    max_interval: Duration,
    mut check: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(timeout, async {
        let mut interval = initial_interval;
        loop {
            if check().await {
                return;
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(max_interval);
        }
    })
    .await
    .is_ok()
}
