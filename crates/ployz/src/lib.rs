pub mod adapters;
pub mod daemon;
pub mod drivers;
pub mod mesh;
pub mod network;
pub mod node;
pub mod store;
pub mod transport;
pub mod workload;

// Re-export ployz-sdk types so internal `crate::model`, `crate::error`, etc. still resolve.
pub use ployz_sdk::config;
pub use ployz_sdk::error;
pub use ployz_sdk::model;
pub use ployz_sdk::spec;

// Re-export public API from SDK
pub use ployz_sdk::model::*;
pub use ployz_sdk::{
    Affordances, ClientConfig, ConfigLoadError, DaemonConfig, Mode, Os, default_config_path,
    default_data_dir, default_socket_path, load_client_config, load_daemon_config,
    resolve_config_path, validate_mode,
};
pub use ployz_sdk::{Error, Result};

// Re-export daemon-internal public API
pub use adapters::corrosion::client::{CorrClient, Transport as CorrTransport};
pub use adapters::corrosion::docker::DockerCorrosion;
pub use adapters::corrosion::host::HostCorrosion;
pub use adapters::corrosion::{CorrosionStore, SCHEMA_SQL, config as corrosion_config};
pub use adapters::docker_network::DockerBridgeNetwork;
pub use adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
pub use adapters::wireguard::config as wireguard_config;
pub use adapters::wireguard::{DEFAULT_LISTEN_PORT, DockerWireGuard, HostWireGuard, WgSidecar};
pub use drivers::{StoreDriver, WireguardDriver};
pub use mesh::orchestrator::{Mesh, MeshError};
pub use mesh::peer::{PeerStatus, WireGuardPeer};
pub use mesh::phase::{Phase, PhaseEvent, TransitionError, transition};
pub use mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
pub use node::identity::{Identity, IdentityError};
pub use node::invite::InviteClaims;
pub use store::network::{NetworkConfig, NetworkConfigError};
pub use store::{InviteStore, MachineStore, ServiceControl, SyncProbe, SyncStatus};
