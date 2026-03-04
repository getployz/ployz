use super::peer_state::{PeerStateMap, plan_mesh_peers};
use super::reconcile::{ConvergenceConfig, HealthSummary};
use crate::dataplane::traits::{MachineStore, MeshNetwork, PeerProbe, PortError};
use crate::domain::model::{MachineEvent, MachineRecord};
use std::sync::{Arc, Mutex, MutexGuard};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

type Result<T> = std::result::Result<T, ConvergenceError>;

#[derive(Debug, Error)]
pub(crate) enum ConvergenceError {
    #[error("subscribe machines failed: {0}")]
    Subscribe(PortError),
    #[error("convergence loop task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub(crate) struct ConvergenceLoop<N, Store, Probe> {
    membership: Arc<Store>,
    network: Arc<N>,
    prober: Option<Arc<Probe>>,
    config: ConvergenceConfig,
    cancel: CancellationToken,
    health: Arc<Mutex<HealthSummary>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl<N, Store, Probe> ConvergenceLoop<N, Store, Probe> {
    pub(crate) fn new(
        membership: Arc<Store>,
        network: Arc<N>,
        prober: Option<Arc<Probe>>,
        config: ConvergenceConfig,
    ) -> Self {
        Self {
            membership,
            network,
            prober,
            config,
            cancel: CancellationToken::new(),
            health: Arc::new(Mutex::new(HealthSummary::empty())),
            handle: None,
        }
    }

    pub(crate) async fn stop(&mut self) -> Result<()> {
        self.cancel.cancel();
        if let Some(h) = self.handle.take() {
            h.await?;
        }
        Ok(())
    }

    pub(crate) fn health(&self) -> HealthSummary {
        lock_health(&self.health).clone()
    }
}

impl<N, Store, Probe> ConvergenceLoop<N, Store, Probe>
where
    N: MeshNetwork + 'static,
    Store: MachineStore + 'static,
    Probe: PeerProbe + 'static,
{
    pub(crate) async fn start(&mut self) -> Result<()> {
        let (snapshot, events) = self
            .membership
            .subscribe_machines()
            .await
            .map_err(ConvergenceError::Subscribe)?;
        let cancel = self.cancel.clone();
        let network = self.network.clone();
        let prober = self.prober.clone();
        let config = self.config.clone();
        let health = self.health.clone();

        self.handle = Some(tokio::spawn(async move {
            run_loop(
                snapshot, events, cancel, network, prober, config, health,
            )
            .await;
        }));
        Ok(())
    }
}

async fn run_loop<N: MeshNetwork, Probe: PeerProbe>(
    snapshot: Vec<MachineRecord>,
    mut events: mpsc::Receiver<MachineEvent>,
    cancel: CancellationToken,
    network: Arc<N>,
    prober: Option<Arc<Probe>>,
    config: ConvergenceConfig,
    health: Arc<Mutex<HealthSummary>>,
) {
    let mut state = PeerStateMap::new();
    state.init_from_snapshot(&snapshot);
    reconcile(&state, &*network).await;
    update_health(&state, &health);

    let mut probe_interval = tokio::time::interval(config.probe_interval);
    probe_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    probe_interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("convergence loop cancelled");
                break;
            }
            Some(event) = events.recv() => {
                debug!(?event, "machine event");
                match event {
                    MachineEvent::Added(r) | MachineEvent::Updated(r) => state.upsert(&r),
                    MachineEvent::Removed { id } => state.remove(&id),
                }
                reconcile(&state, &*network).await;
                update_health(&state, &health);
            }
            _ = probe_interval.tick(), if prober.is_some() => {
                if let Some(prober) = prober.as_ref() {
                    probe_and_rotate(&mut state, &**prober, &*network, &config).await;
                    update_health(&state, &health);
                }
            }
        }
    }
}

async fn reconcile<N: MeshNetwork>(state: &PeerStateMap, network: &N) {
    let planned = plan_mesh_peers(state);
    if let Err(e) = network.set_peers(&planned).await {
        warn!(?e, "set_peers failed");
    }
}

async fn probe_and_rotate<N: MeshNetwork, Probe: PeerProbe>(
    state: &mut PeerStateMap,
    prober: &Probe,
    network: &N,
    config: &ConvergenceConfig,
) {
    let handshakes = match prober.peer_handshakes().await {
        Ok(hs) => hs,
        Err(e) => {
            warn!(?e, "peer_handshakes failed");
            return;
        }
    };

    state.apply_handshakes(&handshakes);

    let now = Instant::now();
    let mut rotated = false;
    for ps in state.peers.values_mut() {
        ps.classify(now, config.handshake_timeout);
        if ps.should_rotate(now, config.rotation_timeout) {
            debug!(peer = %ps.id, "rotating endpoint");
            ps.next_endpoint();
            rotated = true;
        }
    }

    if rotated {
        reconcile(state, network).await;
    }
}

fn update_health(state: &PeerStateMap, health: &Arc<Mutex<HealthSummary>>) {
    let details: Vec<_> = state
        .peers
        .values()
        .map(|ps| (ps.id.clone(), ps.health))
        .collect();
    *lock_health(health) = HealthSummary::from_details(details);
}

fn lock_health(health: &Arc<Mutex<HealthSummary>>) -> MutexGuard<'_, HealthSummary> {
    health.lock().unwrap_or_else(|poisoned| {
        warn!("health mutex poisoned, recovering inner state");
        poisoned.into_inner()
    })
}
