use crate::adapters::docker_network::DockerBridgeNetwork;
use crate::drivers::{StoreDriver, WireguardDriver};
use crate::error::Error as PortError;
use crate::mesh::MeshNetwork;
use crate::mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
use crate::store::{MachineStore, ServiceControl, SyncProbe, SyncStatus};
use crate::tasks::{TaskSet, TaskSetError, run_peer_sync_task};
use std::time::Duration;
use thiserror::Error;
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

pub struct Mesh {
    phase: Phase,
    network: WireguardDriver,
    store: StoreDriver,
    container_network: Option<DockerBridgeNetwork>,
    tasks: Option<TaskSet>,
    bootstrap_interval: Duration,
    connection_timeout: Duration,
    service_ready_timeout: Duration,
}

impl Mesh {
    pub fn new(
        network: WireguardDriver,
        store: StoreDriver,
        container_network: Option<DockerBridgeNetwork>,
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

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn store(&self) -> StoreDriver {
        self.store.clone()
    }

    fn apply(&mut self, event: PhaseEvent) -> Result<()> {
        let next = transition(self.phase, event)?;
        info!(from = %self.phase, to = %next, ?event, "phase transition");
        self.phase = next;
        Ok(())
    }

    pub async fn up(&mut self) -> Result<()> {
        self.apply(PhaseEvent::UpRequested)?;

        if let Err(e) = self.network.up().await {
            warn!(?e, "network up failed");
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e.into());
        }

        self.apply(PhaseEvent::NetworkReady)?;

        // Create dual-stack Docker bridge network and connect the WG container
        if let Some(cn) = &self.container_network {
            if let Err(e) = cn.ensure().await {
                warn!(?e, "container network ensure failed");
                let _ = self.network.down().await;
                self.apply(PhaseEvent::ComponentFailed)?;
                return Err(e.into());
            }
            if let Err(e) = cn.connect("ployz-wireguard", Some(cn.container_v4())).await {
                warn!(?e, "connect WG container to bridge failed");
                let _ = cn.remove().await;
                let _ = self.network.down().await;
                self.apply(PhaseEvent::ComponentFailed)?;
                return Err(e.into());
            }
        }

        if let Err(e) = self.store.start().await {
            warn!(?e, "service start failed");
            let _ = self.network.down().await;
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e.into());
        }

        if let Err(e) = self.wait_service_ready().await {
            warn!(?e, "service readiness check failed");
            let _ = self.store.stop().await;
            let _ = self.network.down().await;
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e);
        }

        if let Err(e) = self.wait_store_init().await {
            warn!(?e, "store init failed");
            let _ = self.store.stop().await;
            let _ = self.network.down().await;
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e.into());
        }

        self.apply(PhaseEvent::ComponentsStarted)?;

        if let Err(e) = self.bootstrap_gate().await {
            warn!(?e, "bootstrap failed");
            let _ = self.store.stop().await;
            let _ = self.network.down().await;
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e);
        }

        let (mut task_set, cancel) = TaskSet::new();

        let (snapshot, events) = self
            .store
            .subscribe_machines()
            .await
            .map_err(TaskSetError::Subscribe)?;

        task_set.spawn(run_peer_sync_task(
            snapshot,
            events,
            self.network.clone(),
            cancel,
        ));

        self.tasks = Some(task_set);

        info!(phase = %self.phase, "mesh up");
        Ok(())
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DetachRequested)?;
        if let Some(mut tasks) = self.tasks.take() {
            tasks.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    /// Full reverse teardown. Continues on errors, returns first.
    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        let mut first_err: Option<MeshError> = None;

        if let Some(mut tasks) = self.tasks.take()
            && let Err(e) = tasks.stop().await
        {
            warn!(?e, "task stop failed during destroy");
            first_err.get_or_insert(e.into());
        }

        if let Err(e) = self.store.stop().await {
            warn!(?e, "service stop failed during destroy");
            first_err.get_or_insert(e.into());
        }

        if let Some(cn) = &self.container_network {
            if let Err(e) = cn.remove().await {
                warn!(?e, "container network remove failed during destroy");
                first_err.get_or_insert(e.into());
            }
        }

        if let Err(e) = self.network.down().await {
            warn!(?e, "network down failed during destroy");
            first_err.get_or_insert(e.into());
        }

        self.apply(PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed");

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Wait for the service to report healthy after starting.
    /// Uses exponential backoff: 50ms → 1s, with a configurable timeout (default 15s).
    async fn wait_service_ready(&self) -> Result<()> {
        if self.store.healthy().await {
            return Ok(());
        }

        let timeout = self.service_ready_timeout;
        let mut interval = Duration::from_millis(50);
        let max_interval = Duration::from_secs(1);

        tokio::time::timeout(timeout, async {
            loop {
                if self.store.healthy().await {
                    return;
                }
                tokio::time::sleep(interval).await;
                interval = (interval * 2).min(max_interval);
            }
        })
        .await
        .map_err(|_| {
            MeshError::Port(PortError::operation(
                "service ready",
                format!("service did not become ready within {timeout:?}"),
            ))
        })
    }

    /// Wait for the store to accept its schema and serve queries.
    /// Uses exponential backoff: 100ms → 2s, 30s timeout.
    async fn wait_store_init(&self) -> Result<()> {
        let timeout = Duration::from_secs(30);
        let mut interval = Duration::from_millis(100);
        let max_interval = Duration::from_secs(2);

        let result = tokio::time::timeout(timeout, async {
            // Phase 1: apply schema.
            loop {
                match self.store.init().await {
                    Ok(()) => break,
                    Err(e) => {
                        info!(?e, "store not ready, retrying");
                        tokio::time::sleep(interval).await;
                        interval = (interval * 2).min(max_interval);
                    }
                }
            }
            // Phase 2: verify queries work.
            loop {
                match self.store.list_machines().await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        info!(?e, "store not queryable yet, retrying");
                        tokio::time::sleep(interval).await;
                        interval = (interval * 2).min(max_interval);
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner.map_err(MeshError::Port),
            Err(_) => Err(MeshError::Port(PortError::operation(
                "store init",
                format!("store did not become ready within {timeout:?}"),
            ))),
        }
    }

    async fn bootstrap_gate(&mut self) -> Result<()> {
        let machines = self.store.list_machines().await?;
        if machines.is_empty() {
            self.apply(PhaseEvent::SyncComplete)?;
            return Ok(());
        }

        let interval = self.bootstrap_interval;
        let connection_timeout = self.connection_timeout;
        let store = self.store.clone();

        // Connection phase: wait for corrosion to see any peer (short timeout).
        tokio::time::timeout(connection_timeout, async {
            loop {
                match store.sync_status().await {
                    Ok(SyncStatus::Disconnected) => {}
                    Ok(_) => return,
                    Err(e) => {
                        warn!(?e, "sync probe failed during connection phase");
                    }
                }
                tokio::time::sleep(interval).await;
            }
        })
        .await
        .map_err(|_| TransitionError::BootstrapTimeout)?;

        // Sync phase: wait for gaps to reach 0 (no timeout).
        loop {
            match self.store.sync_status().await {
                Ok(SyncStatus::Synced) => break,
                Ok(SyncStatus::Syncing { gaps }) => {
                    info!(gaps, "syncing: {gaps} gaps remaining");
                }
                Ok(SyncStatus::Disconnected) => {
                    warn!("corrosion disconnected during sync phase");
                }
                Err(e) => {
                    warn!(?e, "sync probe failed during sync phase");
                }
            }
            tokio::time::sleep(interval).await;
        }

        self.apply(PhaseEvent::SyncComplete)?;
        Ok(())
    }
}
