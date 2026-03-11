// Module declarations
pub mod daemon;
pub mod deploy;
mod machine_liveness;
pub mod mesh;
pub mod network;
pub mod node;
pub mod runtime;
pub mod services;
pub mod store;
pub mod transport;

// SDK path aliases (so crate::model, crate::error, etc. resolve internally)
pub use ployz_sdk::config;
pub use ployz_sdk::error;
pub use ployz_sdk::model;
pub use ployz_sdk::paths;
pub use ployz_sdk::spec;

// Corrosion path alias (used internally via crate::corrosion_config)
pub use ployz_corrosion::config as corrosion_config;

// Narrow public surface
pub use ployz_sdk::{Error, Mode, Result};
pub use ployz_sdk::{Affordances, ConfigLoadError, DaemonConfig, load_daemon_config};
pub use mesh::driver::WireguardDriver;
pub use store::driver::StoreDriver;
pub use mesh::orchestrator::{Mesh, MeshError};
pub use mesh::phase::Phase;
pub use node::identity::{Identity, IdentityError};
pub use store::network::{NetworkConfig, NetworkConfigError};
