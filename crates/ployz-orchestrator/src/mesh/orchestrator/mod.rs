use crate::error::Error as PortError;
use crate::mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
use crate::mesh::probe::{ProbeListenerFamily, ProbeListenerReadiness};
use crate::mesh::tasks::{
    HeartbeatCommand, ParticipationCommand, PeerSyncCommand, SelfLivenessCommand, TaskSet,
    TaskSetError, TaskTimingConfig,
};
use crate::model::{MachineId, MachineRecord};
use ployz_runtime_api::{
    ContainerNetwork, DataplaneFactory, EndpointDiscovery, MeshDataplane, MeshNetwork,
    RuntimeError, ServiceRuntime, WireguardBackendMode, WireguardDriver,
};
use ployz_store_api::{MachineStore, SyncProbe};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{RwLock, mpsc};

mod lifecycle;
mod probe_wiring;
mod readiness;
mod self_record;
mod task_runtime;

pub use readiness::MeshReadyStatus;

pub type Result<T> = std::result::Result<T, MeshError>;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Task(#[from] TaskSetError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeStoreMode {
    Keep,
    Stop,
}

pub struct Mesh {
    phase: Phase,
    pub network: WireguardDriver,
    store: Arc<dyn MachineStore>,
    sync_probe: Arc<dyn SyncProbe>,
    store_runtime: Arc<dyn ServiceRuntime>,
    container_network: Option<ContainerNetwork>,
    endpoint_discovery: Arc<dyn EndpointDiscovery>,
    dataplane_factory: Option<Arc<dyn DataplaneFactory>>,
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
    probe_readiness: Arc<ProbeListenerReadiness>,
    task_timing: TaskTimingConfig,
}

impl Mesh {
    #[must_use]
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        network: WireguardDriver,
        store: Arc<dyn MachineStore>,
        sync_probe: Arc<dyn SyncProbe>,
        store_runtime: Arc<dyn ServiceRuntime>,
        container_network: Option<ContainerNetwork>,
        endpoint_discovery: Arc<dyn EndpointDiscovery>,
        dataplane_factory: Option<Arc<dyn DataplaneFactory>>,
        machine_id: MachineId,
        listen_port: u16,
    ) -> Self {
        let probe_required_family = if network.runs_probe_listener() {
            Some(ProbeListenerFamily::Ipv6)
        } else {
            None
        };
        Self {
            phase: Phase::Stopped,
            network,
            store,
            sync_probe,
            store_runtime,
            container_network,
            endpoint_discovery,
            dataplane_factory,
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
            probe_readiness: Arc::new(ProbeListenerReadiness::new(probe_required_family)),
            task_timing: TaskTimingConfig::production(),
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
    pub fn with_task_timing(mut self, config: TaskTimingConfig) -> Self {
        self.task_timing = config;
        self
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

    #[must_use]
    pub fn local_probe_ready(&self) -> bool {
        self.probe_readiness.local_probe_ready()
    }
}
