use crate::error::Error as PortError;
use crate::mesh::MeshNetwork;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
use crate::mesh::tasks::{
    PeerSyncCommand, TaskSet, TaskSetError, run_ebpf_sync_task, run_endpoint_refresh_task,
    run_heartbeat_task, run_peer_sync_task,
};
use crate::model::{MachineId, MachineRecord, MachineStatus};
use crate::network::docker_bridge::DockerBridgeNetwork;
use crate::network::ebpf::EbpfDataplane;
use crate::store::driver::StoreDriver;
use crate::store::{MachineStore, StoreRuntimeControl, SyncProbe, SyncStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

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
    container_network: Option<DockerBridgeNetwork>,
    tasks: Option<TaskSet>,
    task_cancel: Option<tokio_util::sync::CancellationToken>,
    peer_sync_tx: Option<mpsc::Sender<PeerSyncCommand>>,
    bootstrap_interval: Duration,
    connection_timeout: Duration,
    service_ready_timeout: Duration,
    machine_id: MachineId,
    listen_port: u16,
    seed_records: Vec<MachineRecord>,
    allow_disconnected_bootstrap: bool,
    dataplane: Option<Arc<EbpfDataplane>>,
    wg_ifindex: u32,
    heartbeat_started: Arc<AtomicBool>,
}

impl Mesh {
    #[must_use]
    pub fn new(
        network: WireguardDriver,
        store: StoreDriver,
        container_network: Option<DockerBridgeNetwork>,
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
            bootstrap_interval: Duration::from_millis(500),
            connection_timeout: Duration::from_secs(30),
            service_ready_timeout: Duration::from_secs(15),
            machine_id,
            listen_port,
            seed_records: Vec::new(),
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

    pub fn peer_sync_sender(&self) -> Option<mpsc::Sender<PeerSyncCommand>> {
        self.peer_sync_tx.clone()
    }

    pub async fn ready_status(&self) -> MeshReadyStatus {
        let phase = self.phase;
        let store_healthy = self.store.healthy().await;
        let has_remote_peer = self
            .store
            .list_machines()
            .await
            .map(|machines| {
                machines
                    .into_iter()
                    .any(|machine| machine.id != self.machine_id)
            })
            .unwrap_or(false);
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

    fn apply(&mut self, event: PhaseEvent) -> Result<()> {
        let next = transition(self.phase, event)?;
        info!(from = %self.phase, to = %next, ?event, "phase transition");
        self.phase = next;
        Ok(())
    }

    pub async fn up(&mut self) -> Result<()> {
        self.apply(PhaseEvent::UpRequested)?;

        match self.bring_up().await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(?e, "startup failed, tearing down");
                self.teardown().await;
                self.apply(PhaseEvent::ComponentFailed)?;
                Err(e)
            }
        }
    }

    async fn bring_up(&mut self) -> Result<()> {
        // 1. Network
        self.network.up().await?;
        self.apply(PhaseEvent::NetworkReady)?;

        // 2. Container network — create bridge for all modes that use Docker.
        if let Some(cn) = &self.container_network {
            cn.ensure().await?;
            if let WireguardDriver::Docker(_) = &self.network {
                cn.connect("ployz-networking", Some(cn.container_v4()))
                    .await?;
            }

            // Attach eBPF TC classifiers to the bridge for WG↔Docker forwarding.
            self.attach_ebpf_dataplane().await?;
        }

        // 3. Pre-configure WG peers from seed records
        let pre_start_peers: Vec<_> = self
            .seed_records
            .iter()
            .filter(|m| m.id != self.machine_id)
            .cloned()
            .collect();
        if !pre_start_peers.is_empty() {
            if let Err(e) = self.network.set_peers(&pre_start_peers).await {
                warn!(?e, "pre-start peer sync failed");
            }
            if self.wait_for_handshake().await.is_err() {
                warn!("no WG handshake within timeout, continuing anyway");
            }
        }

        // 4. Start store service
        self.store.start().await?;
        self.wait_service_ready().await?;
        self.wait_store_init().await?;

        // 5. Initial peer sync from store
        self.start_peer_sync_task().await?;

        self.apply(PhaseEvent::ComponentsStarted)?;

        // 7. Bootstrap gate
        self.bootstrap_gate().await?;

        // 8. Spawn background tasks
        self.spawn_background_tasks().await?;

        info!(phase = %self.phase, "mesh up");
        Ok(())
    }

    async fn start_peer_sync_task(&mut self) -> Result<()> {
        if self.peer_sync_tx.is_some() {
            return Ok(());
        }

        let (snapshot, events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;
        let (peer_sync_tx, peer_sync_rx) = mpsc::channel(64);
        let (mut task_set, cancel) = TaskSet::new();
        task_set.spawn(run_peer_sync_task(
            snapshot,
            events,
            peer_sync_rx,
            self.network.clone(),
            self.machine_id.clone(),
            cancel.clone(),
        ));

        self.peer_sync_tx = Some(peer_sync_tx);
        self.task_cancel = Some(cancel);
        self.tasks = Some(task_set);
        Ok(())
    }

    /// Attach the eBPF dataplane to the Docker bridge.
    /// On Linux: loads BPF directly in-process via aya (no container overhead).
    /// On macOS (Docker Desktop / OrbStack): starts a privileged sidecar container
    /// that runs `ployz-bpfctl` inside the VM where TC hooks live.
    async fn attach_ebpf_dataplane(&mut self) -> Result<()> {
        let Some(cn) = self.container_network.as_ref() else {
            return Err(MeshError::Port(PortError::operation(
                "attach_ebpf",
                "container_network not configured".to_string(),
            )));
        };
        let bridge_ifname = cn.resolve_bridge_ifname().await?;

        #[cfg(feature = "ebpf-native")]
        {
            let wg_ifname = match &self.network {
                WireguardDriver::Host(wg) => wg.ifname().to_string(),
                WireguardDriver::Docker(_) | WireguardDriver::Memory(_) => bridge_ifname.clone(),
            };
            let wg_ifindex = resolve_ifindex(&wg_ifname)?;
            let dp = EbpfDataplane::attach_native(&bridge_ifname)?;
            self.wg_ifindex = wg_ifindex;
            self.dataplane = Some(Arc::new(dp));
        }

        #[cfg(not(feature = "ebpf-native"))]
        {
            // Exec ployz-bpfctl inside the WG container (same image).
            let dp =
                EbpfDataplane::attach_container("ployz-networking", &bridge_ifname, &bridge_ifname)
                    .await?;
            self.wg_ifindex = 0;
            self.dataplane = Some(Arc::new(dp));
        }

        Ok(())
    }

    async fn spawn_background_tasks(&mut self) -> Result<()> {
        if self.tasks.is_none() {
            return Err(TaskSetError::Subscribe(crate::error::Error::operation(
                "peer sync task",
                "peer sync task not started".to_string(),
            ))
            .into());
        }
        let Some(cancel) = self.task_cancel.clone() else {
            return Err(TaskSetError::Subscribe(crate::error::Error::operation(
                "peer sync task",
                "peer sync cancellation token missing".to_string(),
            ))
            .into());
        };

        let cancel2 = cancel.clone();
        if let Some(task_set) = self.tasks.as_mut() {
            task_set.spawn(run_endpoint_refresh_task(
                self.machine_id.clone(),
                self.listen_port,
                self.store.clone(),
                cancel2,
            ));
        }

        let cancel3 = cancel.clone();
        self.heartbeat_started.store(false, Ordering::SeqCst);
        let self_seed = self
            .seed_records
            .iter()
            .find(|m| m.id == self.machine_id)
            .cloned();
        if let Some(task_set) = self.tasks.as_mut() {
            task_set.spawn(run_heartbeat_task(
                self.machine_id.clone(),
                self_seed,
                self.store.clone(),
                self.network.clone(),
                self.heartbeat_started.clone(),
                cancel3,
            ));
        }

        if let Some(ref dataplane) = self.dataplane {
            let (ebpf_snapshot, ebpf_events) = self
                .store
                .subscribe_machines()
                .await
                .map_err(TaskSetError::Subscribe)?;
            let cancel4 = cancel.clone();

            if let Some(task_set) = self.tasks.as_mut() {
                task_set.spawn(run_ebpf_sync_task(
                    ebpf_snapshot,
                    ebpf_events,
                    dataplane.clone(),
                    self.wg_ifindex,
                    self.machine_id.clone(),
                    cancel4,
                ));
            }
        }

        Ok(())
    }

    /// Idempotent teardown — stops whatever was started, ignores errors on
    /// things not yet started.
    async fn teardown(&mut self) {
        self.peer_sync_tx = None;
        self.task_cancel = None;
        self.heartbeat_started.store(false, Ordering::SeqCst);
        if let Some(mut tasks) = self.tasks.take()
            && let Err(e) = tasks.stop().await
        {
            warn!(?e, "task stop failed during teardown");
        }
        if let Some(dp) = self.dataplane.take()
            && let Ok(dp) = Arc::try_unwrap(dp)
            && let Err(e) = dp.detach().await
        {
            warn!(?e, "ebpf detach failed during teardown");
        }
        if let Err(e) = self.store.stop().await {
            warn!(?e, "service stop failed during teardown");
        }
        if let Err(e) = self.network.down().await {
            warn!(?e, "network down failed during teardown");
        }
        if let Some(cn) = &self.container_network
            && let Err(e) = cn.remove().await
        {
            warn!(?e, "container network remove failed during teardown");
        }
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DetachRequested)?;
        self.peer_sync_tx = None;
        self.task_cancel = None;
        self.heartbeat_started.store(false, Ordering::SeqCst);
        if let Some(mut tasks) = self.tasks.take() {
            tasks.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        // Mark self as down before tearing down infra
        if let Ok(machines) = self.store.list_machines().await
            && let Some(mut record) = machines.into_iter().find(|m| m.id == self.machine_id)
        {
            let now = crate::time::now_unix_secs();
            record.status = MachineStatus::Down;
            record.last_heartbeat = now;
            record.updated_at = now;
            if let Err(e) = self.store.upsert_machine(&record).await {
                warn!(?e, "failed to set status=down on destroy");
            }
        }

        let mut first_err: Option<MeshError> = None;

        if let Some(mut tasks) = self.tasks.take()
            && let Err(e) = tasks.stop().await
        {
            warn!(?e, "task stop failed during destroy");
            first_err.get_or_insert(e.into());
        }
        self.task_cancel = None;

        if let Some(dp) = self.dataplane.take()
            && let Ok(dp) = Arc::try_unwrap(dp)
            && let Err(e) = dp.detach().await
        {
            warn!(?e, "ebpf detach failed during destroy");
        }

        if let Err(e) = self.store.stop().await {
            warn!(?e, "service stop failed during destroy");
            first_err.get_or_insert(e.into());
        }

        if let Err(e) = self.network.down().await {
            warn!(?e, "network down failed during destroy");
            first_err.get_or_insert(e.into());
        }

        if let Some(cn) = &self.container_network
            && let Err(e) = cn.remove().await
        {
            warn!(?e, "container network remove failed during destroy");
            first_err.get_or_insert(e.into());
        }

        self.apply(PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed");

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Wait for at least one WG peer to complete a handshake.
    async fn wait_for_handshake(&self) -> Result<()> {
        poll_until(
            Duration::from_secs(10),
            Duration::from_millis(200),
            Duration::from_millis(200),
            || async { self.network.has_remote_handshake().await },
        )
        .await
        .then_some(())
        .ok_or_else(|| {
            MeshError::Port(PortError::operation(
                "handshake wait",
                "no WG handshake within 10s".to_string(),
            ))
        })?;
        info!("WG remote handshake confirmed, proceeding with store start");
        Ok(())
    }

    /// Wait for the service to report healthy after starting.
    async fn wait_service_ready(&self) -> Result<()> {
        let timeout = self.service_ready_timeout;
        let ok = poll_until(
            timeout,
            Duration::from_millis(50),
            Duration::from_secs(1),
            || async { self.store.healthy().await },
        )
        .await;
        if !ok {
            return Err(MeshError::Port(PortError::operation(
                "service ready",
                format!("service did not become ready within {timeout:?}"),
            )));
        }
        Ok(())
    }

    /// Wait for the store to accept its schema and serve queries.
    async fn wait_store_init(&self) -> Result<()> {
        let timeout = Duration::from_secs(30);
        let init_ok = poll_until(
            timeout,
            Duration::from_millis(100),
            Duration::from_secs(2),
            || async {
                match self.store.init().await {
                    Ok(()) => true,
                    Err(e) => {
                        info!(?e, "store not ready, retrying");
                        false
                    }
                }
            },
        )
        .await;
        if !init_ok {
            return Err(MeshError::Port(PortError::operation(
                "store init",
                format!("store did not become ready within {timeout:?}"),
            )));
        }

        let query_ok = poll_until(
            timeout,
            Duration::from_millis(100),
            Duration::from_secs(2),
            || async {
                match self.store.list_machines().await {
                    Ok(_) => true,
                    Err(e) => {
                        info!(?e, "store not queryable yet, retrying");
                        false
                    }
                }
            },
        )
        .await;
        if !query_ok {
            return Err(MeshError::Port(PortError::operation(
                "store init",
                format!("store queries did not succeed within {timeout:?}"),
            )));
        }
        Ok(())
    }

    /// Bootstrap gate: wait for gossip membership, then proceed.
    async fn bootstrap_gate(&mut self) -> Result<()> {
        let machines = self.store.list_machines().await?;
        let has_remote_peer = machines.iter().any(|m| m.id != self.machine_id);
        if !has_remote_peer {
            self.apply(PhaseEvent::SyncComplete)?;
            return Ok(());
        }

        if self.allow_disconnected_bootstrap {
            info!("skipping bootstrap wait because disconnected bootstrap is allowed");
            self.apply(PhaseEvent::SyncComplete)?;
            return Ok(());
        }

        let interval = self.bootstrap_interval;
        let connection_timeout = self.connection_timeout;
        let store = self.store.clone();

        let result: std::result::Result<bool, String> =
            tokio::time::timeout(connection_timeout, async {
                let mut consecutive_errors = 0u32;
                loop {
                    match store.sync_status().await {
                        Ok(SyncStatus::Disconnected) => {
                            consecutive_errors = 0;
                        }
                        Ok(_) => return Ok(true),
                        Err(e) => {
                            consecutive_errors += 1;
                            if consecutive_errors <= 3 {
                                warn!(?e, "sync probe failed during bootstrap");
                            } else if consecutive_errors == 4 {
                                warn!(
                                    ?e,
                                    consecutive_errors,
                                    "sync probe keeps failing — corrosion transport may be stuck"
                                );
                            }
                        }
                    }
                    tokio::time::sleep(interval).await;
                }
            })
            .await
            .unwrap_or(Ok(false));

        let connected = matches!(result, Ok(true));

        if !connected {
            let reason = match result {
                Ok(_) => {
                    "corrosion gossip could not reach any remote peer within the timeout. \
                     The gossip transport (QUIC) may be stuck — try restarting the mesh on both nodes"
                        .to_string()
                }
                Err(e) => {
                    format!(
                        "corrosion API never became healthy: {e}. \
                         The gossip transport (QUIC) may be stuck — try restarting the mesh on both nodes"
                    )
                }
            };
            return Err(TransitionError::BootstrapTimeout { reason }.into());
        }

        self.apply(PhaseEvent::SyncComplete)?;
        Ok(())
    }
}

#[cfg(feature = "ebpf-native")]
fn resolve_ifindex(ifname: &str) -> std::result::Result<u32, PortError> {
    let c_name = std::ffi::CString::new(ifname)
        .map_err(|e| PortError::operation("if_nametoindex", e.to_string()))?;
    let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if idx == 0 {
        return Err(PortError::operation(
            "if_nametoindex",
            format!("interface {ifname} not found"),
        ));
    }
    Ok(idx)
}

/// Poll `check` with exponential backoff until it returns `true` or `timeout` expires.
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
