pub mod adapters;
pub mod config;
pub mod control;
pub mod dataplane;
pub mod domain;
pub mod error;
pub mod transport;

pub use adapters::corrosion::CorrosionStore;
pub use adapters::corrosion::SCHEMA_SQL;
pub use adapters::corrosion::config as corrosion_config;
pub use adapters::corrosion::docker::DockerCorrosion;
pub use adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
pub use config::{
    Affordances, BridgeBackend, ClientConfig, ConfigLoadError, DaemonConfig, Mode, Os, Profile,
    ServiceBackend, WireGuardBackend, default_config_path, default_data_dir, default_socket_path,
    load_client_config, load_daemon_config, resolve_config_path, resolve_profile,
};
pub use control::backends::{Network, Service, Store};
pub use control::machine::Machine;
pub use control::runtime::{Mesh, MeshError};
pub use dataplane::traits::{
    DevicePeer, InviteStore, MachineStore, MeshNetwork, PortError, PortResult, ServiceControl,
    SyncProbe, SyncStatus, WireGuardDevice,
};
pub use dataplane::wireguard::{PeerStatus, WireGuardPeer};
pub use domain::identity::{Identity, IdentityError};
pub use domain::model::*;
pub use domain::network::{NetworkConfig, NetworkConfigError};
pub use domain::phase::{Phase, PhaseEvent, TransitionError, transition};
