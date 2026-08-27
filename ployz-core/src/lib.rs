//! Domain and wire contracts shared byte-for-byte by the Ployz CLI and daemon.

mod container_metadata;
pub mod domain;
pub mod framing;
mod host_config;
mod machine_telemetry;
mod ports;
pub mod project;
pub mod routing;
pub mod rpc;
mod rpc_catalog;
pub mod service;
pub mod stream;
pub mod value;

pub use container_metadata::*;
pub use domain::*;
pub use framing::*;
pub use host_config::*;
pub use machine_telemetry::*;
pub use ports::*;
pub use project::*;
pub use routing::*;
pub use rpc::*;
pub use service::*;
pub use stream::*;
pub use value::*;

/// Daemon exit status for an operator-actionable Docker network refusal.
pub const DOCKER_NETWORK_CONFLICT_EXIT_STATUS: u8 = 78;

/// Safe operator recovery shared by daemon diagnostics and lifecycle commands.
pub const DOCKER_NETWORK_CONFLICT_RECOVERY: &str = "run `systemctl stop ployz`; run `docker network inspect ployz` and identify the network owner from its labels and attached containers; safely remove or migrate every attached container through its owning deployment; after confirming the network is empty and no longer needed, run `docker network rm ployz`; run `systemctl start ployz`";
