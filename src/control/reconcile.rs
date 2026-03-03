use super::convergence::{ConvergenceError, ConvergenceLoop};
use crate::dataplane::traits::{
    MembershipStore, MeshNetwork, PeerProbe, PortError, ServiceControl, SyncProbe,
};
use crate::domain::model::MachineId;
use crate::domain::phase::{transition, Phase, PhaseEvent, TransitionError};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

pub type Result<T> = std::result::Result<T, MeshError>;

#[derive(Debug, Clone)]
pub struct ConvergenceConfig {
    pub probe_interval: Duration,
    pub handshake_timeout: Duration,
    pub rotation_timeout: Duration,
}

impl Default for ConvergenceConfig {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(15),
            rotation_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerHealth {
    New,
    Alive,
    Suspect,
}

#[derive(Debug, Clone)]
pub struct HealthSummary {
    pub total_peers: usize,
    pub alive: usize,
    pub suspect: usize,
    pub new: usize,
    pub details: Vec<(MachineId, PeerHealth)>,
}

impl HealthSummary {
    pub fn empty() -> Self {
        Self {
            total_peers: 0,
            alive: 0,
            suspect: 0,
            new: 0,
            details: Vec::new(),
        }
    }

    pub fn from_details(details: Vec<(MachineId, PeerHealth)>) -> Self {
        let alive = details
            .iter()
            .filter(|(_, h)| *h == PeerHealth::Alive)
            .count();
        let suspect = details
            .iter()
            .filter(|(_, h)| *h == PeerHealth::Suspect)
            .count();
        let new = details
            .iter()
            .filter(|(_, h)| *h == PeerHealth::New)
            .count();
        Self {
            total_peers: details.len(),
            alive,
            suspect,
            new,
            details,
        }
    }
}

#[derive(Debug, Error)]
pub enum MeshError {
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error("convergence subscribe failed: {0}")]
    ConvergenceSubscribe(PortError),
    #[error("convergence loop task failed: {0}")]
    ConvergenceJoin(tokio::task::JoinError),
}

impl From<ConvergenceError> for MeshError {
    fn from(value: ConvergenceError) -> Self {
        match value {
            ConvergenceError::Subscribe(err) => Self::ConvergenceSubscribe(err),
            ConvergenceError::Join(err) => Self::ConvergenceJoin(err),
        }
    }
}

pub struct Mesh<N, S, Store, Probe, Sync> {
    phase: Phase,
    network: Arc<N>,
    service: Arc<S>,
    membership: Arc<Store>,
    prober: Option<Arc<Probe>>,
    sync_prober: Option<Arc<Sync>>,
    convergence: Option<ConvergenceLoop<N, Store, Probe>>,
    convergence_config: ConvergenceConfig,
    bootstrap_interval: Duration,
    bootstrap_timeout: Duration,
    bootstrap_required_passes: u32,
}

impl<N, S, Store, Probe, Sync> Mesh<N, S, Store, Probe, Sync> {
    pub fn new(
        network: Arc<N>,
        service: Arc<S>,
        membership: Arc<Store>,
        prober: Option<Arc<Probe>>,
        sync_prober: Option<Arc<Sync>>,
    ) -> Self {
        Self {
            phase: Phase::Stopped,
            network,
            service,
            membership,
            prober,
            sync_prober,
            convergence: None,
            convergence_config: ConvergenceConfig::default(),
            bootstrap_interval: Duration::from_millis(500),
            bootstrap_timeout: Duration::from_secs(30),
            bootstrap_required_passes: 2,
        }
    }

    pub fn with_convergence_config(mut self, config: ConvergenceConfig) -> Self {
        self.convergence_config = config;
        self
    }

    pub fn with_bootstrap_timing(
        mut self,
        interval: Duration,
        timeout: Duration,
        required_passes: u32,
    ) -> Self {
        self.bootstrap_interval = interval;
        self.bootstrap_timeout = timeout;
        self.bootstrap_required_passes = required_passes;
        self
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn health(&self) -> HealthSummary {
        self.convergence
            .as_ref()
            .map(|c| c.health())
            .unwrap_or_else(HealthSummary::empty)
    }

    fn apply(&mut self, event: PhaseEvent) -> Result<()> {
        let next = transition(self.phase, event)?;
        info!(from = %self.phase, to = %next, ?event, "phase transition");
        self.phase = next;
        Ok(())
    }
}

impl<N, S, Store, Probe, Sy> Mesh<N, S, Store, Probe, Sy>
where
    N: MeshNetwork + 'static,
    S: ServiceControl + 'static,
    Store: MembershipStore + 'static,
    Probe: PeerProbe + 'static,
    Sy: SyncProbe + 'static,
{
    pub async fn up(&mut self) -> Result<()> {
        self.apply(PhaseEvent::UpRequested)?;

        if let Err(e) = self.network.up().await {
            warn!(?e, "network up failed");
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e.into());
        }

        if let Err(e) = self.service.start().await {
            warn!(?e, "service start failed");
            let _ = self.network.down().await;
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e.into());
        }

        self.apply(PhaseEvent::ComponentsStarted)?;

        if let Err(e) = self.bootstrap_gate().await {
            warn!(?e, "bootstrap failed");
            let _ = self.service.stop().await;
            let _ = self.network.down().await;
            self.apply(PhaseEvent::ComponentFailed)?;
            return Err(e);
        }

        let mut conv = ConvergenceLoop::new(
            self.membership.clone(),
            self.network.clone(),
            self.prober.clone(),
            self.convergence_config.clone(),
        );
        conv.start().await?;
        self.convergence = Some(conv);

        info!(phase = %self.phase, "mesh up");
        Ok(())
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DetachRequested)?;
        if let Some(mut conv) = self.convergence.take() {
            conv.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    /// Full reverse teardown. Continues on errors, returns first.
    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        let mut first_err: Option<MeshError> = None;

        if let Some(mut conv) = self.convergence.take() {
            if let Err(e) = conv.stop().await {
                warn!(?e, "convergence stop failed during destroy");
                first_err.get_or_insert(e.into());
            }
        }

        if let Err(e) = self.service.stop().await {
            warn!(?e, "service stop failed during destroy");
            first_err.get_or_insert(e.into());
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

    async fn bootstrap_gate(&mut self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + self.bootstrap_timeout;

        self.network_gate(deadline).await?;
        self.apply(PhaseEvent::NetworkConnected)?;

        self.sync_gate(deadline).await?;
        self.apply(PhaseEvent::SyncComplete)?;

        Ok(())
    }

    async fn network_gate(&self, deadline: tokio::time::Instant) -> Result<()> {
        let prober = match &self.prober {
            Some(p) => p,
            None => return Ok(()),
        };

        let interval = self.bootstrap_interval;
        let required = self.bootstrap_required_passes;

        tokio::time::timeout_at(deadline, async move {
            let mut consecutive = 0u32;
            loop {
                match prober.peer_handshakes().await {
                    Ok(handshakes) => {
                        let connected = handshakes.is_empty()
                            || handshakes.values().any(|hs| hs.is_some());
                        if connected {
                            consecutive += 1;
                            if consecutive >= required {
                                return;
                            }
                        } else {
                            consecutive = 0;
                        }
                    }
                    Err(e) => {
                        warn!(?e, "network gate probe failed");
                        consecutive = 0;
                    }
                }
                tokio::time::sleep(interval).await;
            }
        })
        .await
        .map_err(|_| TransitionError::BootstrapTimeout)?;
        Ok(())
    }

    async fn sync_gate(&self, deadline: tokio::time::Instant) -> Result<()> {
        let sync_prober = match &self.sync_prober {
            Some(p) => p,
            None => return Ok(()),
        };

        let interval = self.bootstrap_interval;
        let required = self.bootstrap_required_passes;

        tokio::time::timeout_at(deadline, async move {
            let mut consecutive = 0u32;
            loop {
                match sync_prober.sync_complete().await {
                    Ok(true) => {
                        consecutive += 1;
                        if consecutive >= required {
                            return;
                        }
                    }
                    Ok(false) => {
                        consecutive = 0;
                    }
                    Err(e) => {
                        warn!(?e, "sync gate probe failed");
                        consecutive = 0;
                    }
                }
                tokio::time::sleep(interval).await;
            }
        })
        .await
        .map_err(|_| TransitionError::BootstrapTimeout)?;
        Ok(())
    }
}
