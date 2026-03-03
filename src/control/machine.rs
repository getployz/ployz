use super::reconcile::{Mesh, MeshError};
use crate::dataplane::traits::{MembershipStore, MeshNetwork, PeerProbe, ServiceControl, SyncProbe};
use crate::domain::identity::Identity;
use crate::domain::network::NetworkConfig;
use crate::domain::phase::Phase;
use tokio_util::sync::CancellationToken;
use tracing::info;

pub type Result<T> = std::result::Result<T, MeshError>;

pub struct Machine<N, S, Store, Probe, Sync> {
    pub identity: Identity,
    pub network: NetworkConfig,
    pub mesh: Mesh<N, S, Store, Probe, Sync>,
    cancel: CancellationToken,
}

impl<N, S, Store, Probe, Sync> Machine<N, S, Store, Probe, Sync> {
    pub fn new(identity: Identity, network: NetworkConfig, mesh: Mesh<N, S, Store, Probe, Sync>) -> Self {
        Self {
            identity,
            network,
            mesh,
            cancel: CancellationToken::new(),
        }
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn phase(&self) -> Phase {
        self.mesh.phase()
    }
}

impl<N, S, Store, Probe, Sy> Machine<N, S, Store, Probe, Sy>
where
    N: MeshNetwork + 'static,
    S: ServiceControl + 'static,
    Store: MembershipStore + 'static,
    Probe: PeerProbe + 'static,
    Sy: SyncProbe + 'static,
{
    pub async fn run(&mut self) -> Result<()> {
        self.mesh.up().await?;
        info!(machine = %self.identity.machine_id, "machine running");
        self.cancel.cancelled().await;
        info!(machine = %self.identity.machine_id, "control loop stopping");
        self.mesh.detach().await
    }

    pub async fn init_network(&mut self) -> Result<()> {
        self.mesh.up().await
    }
}
