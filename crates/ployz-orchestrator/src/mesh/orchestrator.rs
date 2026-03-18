use crate::error::Error as PortError;
use crate::mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
use crate::mesh::probe::run_probe_listener_task;
use crate::mesh::tasks::{
    HeartbeatCommand, ParticipationCommand, PeerSyncCommand, SelfLivenessCommand,
    SelfRecordMutation, TaskSet, TaskSetError, apply_self_record_mutation, run_ebpf_sync_task,
    run_endpoint_refresh_task, run_heartbeat_task, run_participation_task, run_peer_sync_task,
    run_self_liveness_task, run_self_record_writer_task, run_subnet_claim_monitor_task,
};
use crate::model::{MachineId, MachineRecord, MachineStatus};
use ployz_runtime_api::{
    ContainerNetwork, MeshDataplane, MeshNetwork, WireguardBackendMode, WireguardDriver,
};
use ployz_store_api::StoreDriver;
use ployz_store_api::{MachineStore, StoreRuntimeControl, SyncProbe, SyncStatus};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{RwLock, mpsc};
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
            if self.network.mode() == WireguardBackendMode::Docker {
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
        let bootstrap_peers: Vec<_> = self
            .seed_records
            .iter()
            .filter(|machine| machine.id != self.machine_id)
            .cloned()
            .collect();
        if self.network.runs_probe_listener() {
            task_set.spawn(run_probe_listener_task(cancel.clone()));
        }
        task_set.spawn(run_peer_sync_task(
            snapshot,
            events,
            peer_sync_rx,
            bootstrap_peers,
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
            let wg_ifname = self.network.ebpf_attachment_ifname(&bridge_ifname);
            let wg_ifindex = resolve_ifindex(&wg_ifname)?;
            let dp = crate::network::ebpf::EbpfDataplane::attach_native(&bridge_ifname)?;
            self.wg_ifindex = wg_ifindex;
            self.dataplane = Some(Arc::new(dp));
        }

        #[cfg(not(feature = "ebpf-native"))]
        {
            // Exec ployz-bpfctl inside the WG container (same image).
            let dp = crate::network::ebpf::EbpfDataplane::attach_container(
                "ployz-networking",
                &bridge_ifname,
                &bridge_ifname,
            )
            .await?;
            self.wg_ifindex = 0;
            self.dataplane = Some(Arc::new(dp));
        }

        Ok(())
    }

    async fn spawn_background_tasks(&mut self) -> Result<()> {
        let Some(cancel) = self.task_cancel.clone() else {
            return Err(TaskSetError::Subscribe(crate::error::Error::operation(
                "spawn_background_tasks",
                "peer sync task not started (no cancel token)".to_string(),
            ))
            .into());
        };

        if self.authoritative_self.is_none() {
            let store_self = self.store.list_machines().await.ok().and_then(|machines| {
                machines
                    .into_iter()
                    .find(|machine| machine.id == self.machine_id)
            });
            let authoritative = self
                .seed_records
                .iter()
                .find(|machine| machine.id == self.machine_id)
                .cloned()
                .or(store_self)
                .ok_or_else(|| {
                    TaskSetError::Subscribe(crate::error::Error::operation(
                        "self machine record",
                        "authoritative self record missing".to_string(),
                    ))
                })?;
            self.authoritative_self = Some(Arc::new(RwLock::new(authoritative)));
        }
        // Safe to unwrap: we just ensured `authoritative_self` is `Some` above.
        let authoritative_self = self.authoritative_self.clone().expect("set above");

        // Safe to unwrap: `start_peer_sync_task` always sets `self.tasks` before we get here.
        let task_set = self
            .tasks
            .as_mut()
            .expect("tasks set by start_peer_sync_task");

        let (self_record_tx, self_record_rx) = mpsc::channel(64);
        self.self_record_tx = Some(self_record_tx.clone());
        task_set.spawn(run_self_record_writer_task(
            authoritative_self.clone(),
            self.store.clone(),
            self_record_rx,
            cancel.clone(),
        ));

        task_set.spawn(run_endpoint_refresh_task(
            self.machine_id.clone(),
            self.listen_port,
            authoritative_self.clone(),
            self_record_tx.clone(),
            cancel.clone(),
        ));

        self.heartbeat_started.store(false, Ordering::SeqCst);
        let (self_liveness_tx, self_liveness_rx) = mpsc::channel(16);
        self.self_liveness_tx = Some(self_liveness_tx.clone());
        task_set.spawn(run_self_liveness_task(
            self.network.clone(),
            self.heartbeat_started.clone(),
            self_record_tx.clone(),
            self_liveness_rx,
            cancel.clone(),
        ));

        let (participation_tx, participation_rx) = mpsc::channel(16);
        self.participation_tx = Some(participation_tx.clone());
        task_set.spawn(run_participation_task(
            self.machine_id.clone(),
            authoritative_self.clone(),
            self.store.clone(),
            self.network.clone(),
            self_record_tx,
            participation_rx,
            cancel.clone(),
        ));

        let (heartbeat_tx, heartbeat_rx) = mpsc::channel(16);
        self.heartbeat_tx = Some(heartbeat_tx);
        task_set.spawn(run_heartbeat_task(
            self_liveness_tx,
            participation_tx,
            heartbeat_rx,
            cancel.clone(),
        ));

        let (subnet_snapshot, subnet_events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;
        task_set.spawn(run_subnet_claim_monitor_task(
            subnet_snapshot,
            subnet_events,
            cancel.clone(),
        ));

        if let Some(ref dataplane) = self.dataplane {
            let (ebpf_snapshot, ebpf_events) = self
                .store
                .subscribe_machines()
                .await
                .map_err(TaskSetError::Subscribe)?;
            task_set.spawn(run_ebpf_sync_task(
                ebpf_snapshot,
                ebpf_events,
                dataplane.clone(),
                self.wg_ifindex,
                self.machine_id.clone(),
                cancel.clone(),
            ));
        }

        Ok(())
    }

    fn clear_task_channels(&mut self) {
        self.peer_sync_tx = None;
        self.heartbeat_tx = None;
        self.self_liveness_tx = None;
        self.participation_tx = None;
        self.self_record_tx = None;
        self.task_cancel = None;
        self.heartbeat_started.store(false, Ordering::SeqCst);
    }

    async fn stop_runtime(&mut self, stop_store: bool) -> Option<MeshError> {
        let mut first_err: Option<MeshError> = None;

        self.clear_task_channels();

        if let Some(mut tasks) = self.tasks.take()
            && let Err(error) = tasks.stop().await
        {
            warn!(?error, "task stop failed during runtime stop");
            first_err.get_or_insert(error.into());
        }

        if let Some(dp) = self.dataplane.take()
            && let Err(error) = dp.detach().await
        {
            warn!(?error, "ebpf detach failed during runtime stop");
        }
        self.wg_ifindex = 0;

        if stop_store && let Err(error) = self.store.stop().await {
            warn!(?error, "service stop failed during runtime stop");
            first_err.get_or_insert(error.into());
        }

        if let Err(error) = self.network.down().await {
            warn!(?error, "network down failed during runtime stop");
            first_err.get_or_insert(error.into());
        }

        if let Some(container_network) = &self.container_network
            && let Err(error) = container_network.remove().await
        {
            warn!(
                ?error,
                "container network remove failed during runtime stop"
            );
            first_err.get_or_insert(error.into());
        }

        first_err
    }

    /// Idempotent teardown — stops whatever was started, ignores errors on
    /// things not yet started.
    async fn teardown(&mut self) {
        let _ = self.stop_runtime(true).await;
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DetachRequested)?;
        self.clear_task_channels();
        if let Some(mut tasks) = self.tasks.take() {
            tasks.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        // Mark self as down before tearing down infra
        let now = crate::time::now_unix_secs();
        if self
            .update_authoritative_self_record(|record| {
                record.status = MachineStatus::Down;
                record.last_heartbeat = now;
                record.updated_at = now;
            })
            .await
            .is_none()
            && self.authoritative_self_record().await.is_some()
        {
            warn!(timestamp = now, "failed to set status=down on destroy");
        }

        let first_err = self.stop_runtime(true).await;

        self.apply(PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed");

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub async fn restart_runtime_for_subnet_change(
        &mut self,
        network: WireguardDriver,
        container_network: Option<ContainerNetwork>,
    ) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        let stop_err = self.stop_runtime(false).await;
        self.network = network;
        self.container_network = container_network;

        self.apply(PhaseEvent::TeardownComplete)?;
        self.apply(PhaseEvent::UpRequested)?;

        match self.bring_up().await {
            Ok(()) => {
                if let Some(error) = stop_err {
                    warn!(
                        ?error,
                        "runtime stop reported an error before subnet restart"
                    );
                }
                Ok(())
            }
            Err(error) => {
                warn!(
                    ?error,
                    "subnet runtime restart failed, tearing down runtime"
                );
                let _ = self.stop_runtime(false).await;
                self.apply(PhaseEvent::ComponentFailed)?;
                Err(error)
            }
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
