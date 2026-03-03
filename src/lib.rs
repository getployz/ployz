pub mod adapters;
pub mod config;
pub mod control;
pub mod dataplane;
pub mod domain;
pub mod error;
pub mod transport;

pub use adapters::memory::{MemoryService, MemoryStore, MemorySyncProbe, MemoryWireGuard};
pub use config::{
    default_data_dir, default_socket_path, resolve_profile, Affordances, BridgeBackend, Mode, Os,
    Profile, ServiceBackend, WireGuardBackend,
};
pub use control::machine::Machine;
pub use control::reconcile::{ConvergenceConfig, HealthSummary, Mesh, MeshError, PeerHealth};
pub use dataplane::traits::{
    MembershipStore, MeshNetwork, PeerProbe, PortError, PortResult, ServiceControl, SyncProbe,
};
pub use domain::identity::{Identity, IdentityError};
pub use domain::network::{NetworkConfig, NetworkConfigError};
pub use domain::model::*;
pub use domain::phase::{transition, Phase, PhaseEvent, TransitionError};
