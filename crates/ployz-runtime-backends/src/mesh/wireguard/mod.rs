#[cfg(feature = "userspace-wg")]
pub(crate) mod bridge;
pub(crate) mod config;
#[cfg(feature = "docker")]
pub(crate) mod docker;
pub(crate) mod host;
#[cfg(feature = "docker")]
pub(crate) mod sidecar;

#[cfg(feature = "docker")]
pub use docker::DockerWireGuard;
pub use host::HostWireGuard;
#[cfg(feature = "docker")]
pub use sidecar::WgSidecar;

pub const DEFAULT_LISTEN_PORT: u16 = 51820;
pub const PERSISTENT_KEEPALIVE_SECS: u16 = 5;
