pub mod bridge;
pub mod config;
pub mod docker;
pub mod host;
pub mod sidecar;

pub use docker::DockerWireGuard;
pub use host::HostWireGuard;
pub use sidecar::WgSidecar;
