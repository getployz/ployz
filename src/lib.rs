pub mod adapters;
pub mod config;
pub mod control;
pub mod dataplane;
pub mod domain;
pub mod error;
pub mod transport;

pub use adapters::memory::{MemoryService, MemoryStore, MemorySyncProbe, MemoryWireGuard};
pub use config::{
    Affordances, BridgeBackend, ClientConfig, ConfigLoadError, DaemonConfig, Mode, Os, Profile,
    ServiceBackend, WireGuardBackend, default_config_path, default_data_dir, default_socket_path,
    load_client_config, load_daemon_config, resolve_config_path, resolve_profile,
};
pub use control::machine::Machine;
pub use control::reconcile::{ConvergenceConfig, HealthSummary, Mesh, MeshError, PeerHealth};
pub use dataplane::traits::{
    InviteStore, MachineStore, MeshNetwork, PeerProbe, PortError, PortResult, ServiceControl,
    SyncProbe,
};
pub use domain::identity::{Identity, IdentityError};
pub use domain::model::*;
pub use domain::network::{NetworkConfig, NetworkConfigError};
pub use domain::phase::{Phase, PhaseEvent, TransitionError, transition};
