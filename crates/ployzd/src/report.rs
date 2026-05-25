use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::config::DaemonConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrosionFailure {
    Agent,
    Store,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaFailure {
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerFailure {
    Startup,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrosionPhase {
    Configured,
    Ready,
    Failed(CorrosionFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrosionStop {
    Graceful,
    Forced,
    UnexpectedExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrosionCleanup {
    NotRun,
    Stopped(CorrosionStop),
    Unmanaged,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaPhase {
    Configured,
    Ready,
    Failed(SchemaFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPhase {
    Configured,
    Ready,
    Failed(PeerFailure),
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrosionStartup {
    root_dir: PathBuf,
    addresses: polis::CorrosionAgentAddresses,
    phase: CorrosionPhase,
    cleanup: CorrosionCleanup,
}

impl CorrosionStartup {
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.addresses.api_addr()
    }

    #[must_use]
    pub fn gossip_addr(&self) -> SocketAddr {
        self.addresses.gossip_addr()
    }

    #[must_use]
    pub fn prometheus_addr(&self) -> SocketAddr {
        self.addresses.prometheus_addr()
    }

    #[must_use]
    pub fn phase(&self) -> CorrosionPhase {
        self.phase
    }

    #[must_use]
    pub fn cleanup(&self) -> CorrosionCleanup {
        self.cleanup
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStartup {
    phase: SchemaPhase,
}

impl SchemaStartup {
    #[must_use]
    pub fn phase(&self) -> SchemaPhase {
        self.phase
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStartup {
    identity_path: PathBuf,
    endpoint_id: Option<polis::IrohEndpointId>,
    phase: PeerPhase,
}

impl PeerStartup {
    #[must_use]
    pub fn identity_path(&self) -> &Path {
        &self.identity_path
    }

    #[must_use]
    pub fn endpoint_id(&self) -> Option<&polis::IrohEndpointId> {
        self.endpoint_id.as_ref()
    }

    #[must_use]
    pub fn phase(&self) -> PeerPhase {
        self.phase
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupReport {
    corrosion: CorrosionStartup,
    schema: SchemaStartup,
    peer: PeerStartup,
}

impl StartupReport {
    #[must_use]
    pub fn configured(config: &DaemonConfig) -> Self {
        Self {
            corrosion: CorrosionStartup {
                root_dir: config.corrosion_root_dir().to_path_buf(),
                addresses: config.corrosion_addresses(),
                phase: CorrosionPhase::Configured,
                cleanup: CorrosionCleanup::NotRun,
            },
            schema: SchemaStartup {
                phase: SchemaPhase::Configured,
            },
            peer: PeerStartup {
                identity_path: config.peer_identity_path().to_path_buf(),
                endpoint_id: None,
                phase: PeerPhase::Configured,
            },
        }
    }

    #[must_use]
    pub fn with_corrosion_shutdown(mut self, shutdown: &polis::CorrosionShutdown) -> Self {
        self.corrosion.cleanup = match shutdown {
            polis::CorrosionShutdown::Graceful => {
                CorrosionCleanup::Stopped(CorrosionStop::Graceful)
            }
            polis::CorrosionShutdown::Forced => CorrosionCleanup::Stopped(CorrosionStop::Forced),
            polis::CorrosionShutdown::Unmanaged => CorrosionCleanup::Unmanaged,
            polis::CorrosionShutdown::UnexpectedExit(_) => {
                CorrosionCleanup::Stopped(CorrosionStop::UnexpectedExit)
            }
        };
        self
    }

    #[must_use]
    pub fn with_corrosion_cleanup_failed(mut self) -> Self {
        self.corrosion.cleanup = CorrosionCleanup::Failed;
        self
    }

    #[must_use]
    pub fn with_peer_stopped(mut self) -> Self {
        self.peer.phase = PeerPhase::Stopped;
        self
    }

    pub fn mark_corrosion_ready(&mut self, agent: &polis::LocalCorrosionAgent) {
        self.corrosion.root_dir = agent.root_dir().to_path_buf();
        self.corrosion.addresses = polis::CorrosionAgentAddresses::new(
            agent.api_addr(),
            agent.gossip_addr(),
            agent.prometheus_addr(),
        );
        self.corrosion.phase = CorrosionPhase::Ready;
        self.corrosion.cleanup = CorrosionCleanup::NotRun;
    }

    pub fn mark_schema_ready(&mut self) {
        self.schema.phase = SchemaPhase::Ready;
    }

    pub fn mark_peer_ready(&mut self, endpoint_id: polis::IrohEndpointId) {
        self.peer.endpoint_id = Some(endpoint_id);
        self.peer.phase = PeerPhase::Ready;
    }

    #[must_use]
    pub fn with_corrosion_failed(mut self, failure: CorrosionFailure) -> Self {
        self.corrosion.phase = CorrosionPhase::Failed(failure);
        self
    }

    #[must_use]
    pub fn with_schema_failed(mut self, failure: SchemaFailure) -> Self {
        self.schema.phase = SchemaPhase::Failed(failure);
        self
    }

    #[must_use]
    pub fn with_peer_failed(mut self, failure: PeerFailure) -> Self {
        self.peer.phase = PeerPhase::Failed(failure);
        self
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.corrosion.phase == CorrosionPhase::Ready
            && self.corrosion.cleanup == CorrosionCleanup::NotRun
            && self.schema.phase == SchemaPhase::Ready
            && self.peer.phase == PeerPhase::Ready
    }

    #[must_use]
    pub fn corrosion(&self) -> &CorrosionStartup {
        &self.corrosion
    }

    #[must_use]
    pub fn schema(&self) -> &SchemaStartup {
        &self.schema
    }

    #[must_use]
    pub fn peer(&self) -> &PeerStartup {
        &self.peer
    }

    #[must_use]
    pub fn corrosion_root_dir(&self) -> &Path {
        self.corrosion.root_dir()
    }

    #[must_use]
    pub fn corrosion_api_addr(&self) -> SocketAddr {
        self.corrosion.api_addr()
    }

    #[must_use]
    pub fn peer_identity_path(&self) -> &Path {
        self.peer.identity_path()
    }

    #[must_use]
    pub fn endpoint_id(&self) -> Option<&polis::IrohEndpointId> {
        self.peer.endpoint_id()
    }
}
