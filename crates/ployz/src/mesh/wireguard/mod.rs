pub(crate) mod bridge;
pub(crate) mod config;
pub(crate) mod docker;
pub(crate) mod host;
pub(crate) mod memory;
pub(crate) mod sidecar;

pub use docker::DockerWireGuard;
pub use host::HostWireGuard;
pub use memory::MemoryWireGuard;
pub use sidecar::WgSidecar;

pub const DEFAULT_LISTEN_PORT: u16 = 51820;
pub const PERSISTENT_KEEPALIVE_SECS: u16 = 5;
