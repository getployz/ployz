//! Domain and wire contracts shared byte-for-byte by the Ployz CLI and daemon.

pub mod config;
mod container_metadata;
pub mod decode;
pub mod domain;
mod enrollment;
pub mod error_chain;
pub mod framing;
pub use enrollment::*;
mod host_config;
mod log_level;
mod machine_telemetry;
pub mod namespace;
mod ports;
mod release;
#[cfg(not(target_arch = "wasm32"))]
pub mod routing;
pub mod rpc;
mod rpc_catalog;
pub mod service;
mod storage_capacity;
pub mod stream;
pub mod value;
mod volume_switch;

pub use container_metadata::*;
pub use domain::*;
pub use framing::*;
pub use host_config::*;
pub use log_level::*;
pub use machine_telemetry::*;
pub use namespace::*;
pub use ports::*;
pub use release::*;
#[cfg(not(target_arch = "wasm32"))]
pub use routing::*;
pub use rpc::*;
pub use service::*;
pub use storage_capacity::*;
pub use stream::*;
pub use value::*;
pub use volume_switch::*;

/// How long a Machine may take to become ready after start or restart,
/// including a first Corrosion image pull. systemd start is extended up to
/// this long; clients waiting for Participating must wait at least this long.
pub const MACHINE_START_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Daemon exit status for an operator-actionable Docker network or WireGuard
/// interface refusal.
pub const NETWORK_CONFLICT_EXIT_STATUS: u8 = 78;

/// Safe operator recovery shared by daemon diagnostics and lifecycle commands.
pub const DOCKER_NETWORK_CONFLICT_RECOVERY: &str = "run `systemctl stop ployz.socket ployz`; run `docker network inspect ployz` and identify the network owner from its labels and attached containers; safely remove or migrate every attached container through its owning deployment; after confirming the network is empty and no longer needed, run `docker network rm ployz`; run `systemctl reset-failed ployz.socket ployz`; run `systemctl start ployz`";
