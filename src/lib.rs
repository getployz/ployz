pub mod adapters;
pub mod drivers;
pub mod config;
pub mod daemon;
pub mod error;
pub mod mesh;
pub mod model;
pub mod node;
pub mod store;
mod tasks;
pub mod transport;

// Re-export public API
pub use error::{Error, Result};
pub use config::{
    Affordances, ClientConfig, ConfigLoadError, DaemonConfig, Mode, Os, default_config_path,
    default_data_dir, default_socket_path, load_client_config, load_daemon_config,
    resolve_config_path, validate_mode,
};
pub use node::identity::{Identity, IdentityError};
pub use node::invite::InviteClaims;
pub use model::*;
pub use store::network::{NetworkConfig, NetworkConfigError};
pub use store::{MachineStore, InviteStore, SyncProbe, SyncStatus, ServiceControl};
pub use mesh::{MeshNetwork, WireGuardDevice, DevicePeer};
pub use mesh::orchestrator::{Mesh, MeshError};
pub use mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
pub use mesh::peer::{PeerStatus, WireGuardPeer};
pub use drivers::{WireguardDriver, StoreDriver};
pub use adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
pub use adapters::corrosion::{CorrosionStore, SCHEMA_SQL, config as corrosion_config};
pub use adapters::corrosion::docker::DockerCorrosion;
pub use adapters::wireguard::{DockerWireGuard, HostWireGuard};
pub use adapters::wireguard::config as wireguard_config;
