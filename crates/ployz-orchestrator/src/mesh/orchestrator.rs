mod dataplane;
mod lifecycle;
mod shutdown;
mod tasks;

use crate::error::Error as PortError;
use crate::mesh::container_network::ContainerNetwork;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::phase::{Phase, TransitionError};
use crate::mesh::tasks::{
    HeartbeatCommand, ParticipationCommand, PeerSyncCommand, SelfLivenessCommand,
    SelfRecordMutation, TaskSet, TaskSetError, apply_self_record_mutation,
};
use crate::mesh::MeshDataplane;
use crate::model::{MachineId, MachineRecord};
use ployz_store_api::StoreDriver;
use ployz_store_api::{MachineStore, StoreRuntimeControl, SyncProbe, SyncStatus};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub heartbeat_started: bool,
}

pub struct Mesh {
    phase: Phase,
    pub network: WireguardDriver,
    pub store: StoreDriver,
    container_network: Option<ContainerNetwork>,
    tasks: Option<TaskSet>,
    task_cancel: Option<tokio_util::sync::CancellationToken>,
    peer_sync_tx: Option<mpsc::Sender<PeerSyncCommand>>,
    heartbeat_tx: Option<mpsc::Sender<HeartbeatCommand>>,
    self_liveness_tx: Option<mpsc::Sender<SelfLivenessCommand>>,
    participation_tx: Option<mpsc::Sender<ParticipationCommand>>,
    self_record_tx: Option<mpsc::Sender<crate::mesh::tasks::SelfRecordCommand>>,
    bootstrap_interval: Duration,
    connection_timeout: Duration,
    service_ready_timeout: Duration,
    machine_id: MachineId,
    listen_port: u16,
    seed_records: Vec<MachineRecord>,
    authoritative_self: Option<Arc<RwLock<MachineRecord>>>,
    allow_disconnected_bootstrap: bool,
    dataplane: Option<Arc<dyn MeshDataplane>>,
    wg_ifindex: u32,
    heartbeat_started: Arc<AtomicBool>,
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
            heartbeat_tx: None,
            self_liveness_tx: None,
            participation_tx: None,
            self_record_tx: None,
            bootstrap_interval: Duration::from_millis(500),
            connection_timeout: Duration::from_secs(30),
            service_ready_timeout: Duration::from_secs(15),
            machine_id,
            listen_port,
            seed_records: Vec::new(),
            authoritative_self: None,
            allow_disconnected_bootstrap: false,
            dataplane: None,
            wg_ifindex: 0,
            heartbeat_started: Arc::new(AtomicBool::new(false)),
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

    #[must_use]
    pub fn heartbeat_sender(&self) -> Option<mpsc::Sender<HeartbeatCommand>> {
        self.heartbeat_tx.clone()
    }

    #[must_use]
    pub fn participation_sender(&self) -> Option<mpsc::Sender<ParticipationCommand>> {
        self.participation_tx.clone()
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
        let heartbeat_started = self.heartbeat_started.load(Ordering::SeqCst);
        let ready = phase == Phase::Running && store_healthy && sync_connected && heartbeat_started;

        MeshReadyStatus {
            ready,
            phase,
            store_healthy,
            sync_connected,
            heartbeat_started,
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
