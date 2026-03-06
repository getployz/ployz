use crate::adapters::docker_network::DockerBridgeNetwork;
use crate::drivers::{StoreDriver, WireguardDriver};
use crate::error::Error as PortError;
use crate::mesh::MeshNetwork;
use crate::mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
use crate::model::{MachineId, MachineRecord, MachineStatus};
use crate::store::{MachineStore, ServiceControl, SyncProbe, SyncStatus};
use crate::tasks::{TaskSet, TaskSetError, run_endpoint_refresh_task, run_heartbeat_task, run_peer_sync_task};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tracing::{debug, info, warn};

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

pub struct Mesh {
    phase: Phase,
    pub network: WireguardDriver,
    pub store: StoreDriver,
    container_network: Option<DockerBridgeNetwork>,
    tasks: Option<TaskSet>,
    bootstrap_interval: Duration,
    connection_timeout: Duration,
    service_ready_timeout: Duration,
    machine_id: MachineId,
    listen_port: u16,
    seed_records: Vec<MachineRecord>,
    allow_disconnected_bootstrap: bool,
}

impl Mesh {
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
            bootstrap_interval: Duration::from_millis(500),
            connection_timeout: Duration::from_secs(30),
            service_ready_timeout: Duration::from_secs(15),
            machine_id,
            listen_port,
            seed_records: Vec::new(),
            allow_disconnected_bootstrap: false,
        }
    }

    pub fn with_bootstrap_timing(
        mut self,
        interval: Duration,
        connection_timeout: Duration,
    ) -> Self {
        self.bootstrap_interval = interval;
        self.connection_timeout = connection_timeout;
        self
    }

    pub fn with_seed_records(mut self, seed_records: Vec<MachineRecord>) -> Self {
        self.seed_records = seed_records;
        self
    }

    pub fn with_disconnected_bootstrap_allowed(
        mut self,
        allow_disconnected_bootstrap: bool,
    ) -> Self {
        self.allow_disconnected_bootstrap = allow_disconnected_bootstrap;
        self
    }

    pub fn phase(&self) -> Phase {
        self.phase
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

        // 2. Container network (Docker only)
        if let Some(cn) = &self.container_network {
            cn.ensure().await?;
            cn.connect("ployz-wireguard", Some(cn.container_v4())).await?;
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

        // 5. Patch bridge IP and seed records
        if let Some(bridge_ip) = self.network.bridge_ip().await {
            if let Some(rec) = self.seed_records.iter_mut().find(|m| m.id == self.machine_id) {
                debug!(%bridge_ip, "persisting bridge_ip to self record");
                rec.bridge_ip = Some(bridge_ip);
            }
        }
        for record in &self.seed_records {
            self.store.upsert_machine(record).await?;
        }

        // 6. Initial peer sync from store
        let peers: Vec<_> = self
            .store
            .list_machines()
            .await?
            .into_iter()
            .filter(|m| m.id != self.machine_id)
            .collect();
        if let Err(e) = self.network.set_peers(&peers).await {
            warn!(?e, "initial peer sync failed");
        }

        self.apply(PhaseEvent::ComponentsStarted)?;

        // 7. Bootstrap gate
        self.bootstrap_gate().await?;

        // 8. Spawn background tasks
        self.spawn_background_tasks().await?;

        info!(phase = %self.phase, "mesh up");
        Ok(())
    }

    async fn spawn_background_tasks(&mut self) -> Result<()> {
        let (mut task_set, cancel) = TaskSet::new();

        let (snapshot, events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;

        let cancel2 = cancel.clone();
        task_set.spawn(run_peer_sync_task(
            snapshot,
            events,
            self.network.clone(),
            self.machine_id.clone(),
            cancel,
        ));

        let cancel3 = cancel2.clone();
        task_set.spawn(run_endpoint_refresh_task(
            self.machine_id.clone(),
            self.listen_port,
            self.store.clone(),
            cancel2,
        ));

        task_set.spawn(run_heartbeat_task(
            self.machine_id.clone(),
            self.store.clone(),
            cancel3,
        ));

        self.tasks = Some(task_set);
        Ok(())
    }

    /// Idempotent teardown — stops whatever was started, ignores errors on
    /// things not yet started.
    async fn teardown(&mut self) {
        if let Some(mut tasks) = self.tasks.take() {
            if let Err(e) = tasks.stop().await {
                warn!(?e, "task stop failed during teardown");
            }
        }
        if let Err(e) = self.store.stop().await {
            warn!(?e, "service stop failed during teardown");
        }
        if let Err(e) = self.network.down().await {
            warn!(?e, "network down failed during teardown");
        }
        if let Some(cn) = &self.container_network {
            if let Err(e) = cn.remove().await {
                warn!(?e, "container network remove failed during teardown");
            }
        }
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DetachRequested)?;
        if let Some(mut tasks) = self.tasks.take() {
            tasks.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Err(e) = self
            .store
            .update_heartbeat(&self.machine_id, MachineStatus::Down, now)
            .await
        {
            warn!(?e, "failed to set status=down on destroy");
        }

        let mut first_err: Option<MeshError> = None;

        if let Some(mut tasks) = self.tasks.take() {
            if let Err(e) = tasks.stop().await {
                warn!(?e, "task stop failed during destroy");
                first_err.get_or_insert(e.into());
            }
        }

        if let Err(e) = self.store.stop().await {
            warn!(?e, "service stop failed during destroy");
            first_err.get_or_insert(e.into());
        }

        if let Err(e) = self.network.down().await {
            warn!(?e, "network down failed during destroy");
            first_err.get_or_insert(e.into());
        }

        if let Some(cn) = &self.container_network {
            if let Err(e) = cn.remove().await {
                warn!(?e, "container network remove failed during destroy");
                first_err.get_or_insert(e.into());
            }
        }

        self.apply(PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed");

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Wait for at least one WG peer to complete a handshake.
    /// Handshakes typically complete in 1-3s; prevents 503 storm when the tunnel is slow.
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
    ///
    /// Corrosion is an eventually-consistent system. The /v1/health endpoint
    /// reports gossip membership and bookkeeping gaps, but gaps == 0 before
    /// the first sync handshake is vacuously true — it means "no known missing
    /// versions," not "we've compared state with a peer and converged."
    ///
    /// We gate only on membership (members >= 1 = gossip can see a remote peer).
    /// After that, data convergence happens in the background. The subscription
    /// snapshot taken after this gate may be stale; the subscribe loop will
    /// deliver corrections as sync completes.
    ///
    /// If workload scheduling data is ever stored in corrosion, this gate is
    /// NOT sufficient — a stronger readiness primitive (e.g. verifying a remote
    /// actor_id exists in __corro_bookkeeping) would be needed to ensure the
    /// snapshot reflects post-sync state.
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

        // Wait for corrosion gossip to see at least one remote peer.
        // Track whether we ever got a healthy-but-disconnected response vs
        // only errors (503 / transport failure), so we can give the user
        // an actionable message on timeout.
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
            .unwrap_or_else(|_| Ok(false));

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
