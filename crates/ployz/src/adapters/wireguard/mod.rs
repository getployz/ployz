pub mod bridge;
pub mod config;
pub mod docker;
pub mod host;
pub mod sidecar;

pub use docker::DockerWireGuard;
pub use host::HostWireGuard;
pub use sidecar::WgSidecar;

pub const DEFAULT_LISTEN_PORT: u16 = 51820;
pub const PERSISTENT_KEEPALIVE_SECS: u16 = 5;
