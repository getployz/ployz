pub mod corrosion_config;
pub mod machine_liveness;
pub mod mesh;
pub mod network;
pub mod node;
pub mod store;
pub mod time;

// SDK path aliases (so crate::model, crate::error, etc. resolve internally)
pub use ployz_types::error;
pub use ployz_types::model;
pub use ployz_types::paths;
pub use ployz_types::spec;

// Narrow public surface
pub use mesh::container_network::ContainerNetwork;
pub use mesh::driver::WireguardDriver;
pub use mesh::orchestrator::{Mesh, MeshError};
pub use mesh::phase::Phase;
pub use node::identity::{Identity, IdentityError};
pub use ployz_types::{Error, Result};
pub use store::driver::StoreDriver;
pub use store::network::{NetworkConfig, NetworkConfigError};
