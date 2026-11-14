//! Domain and wire contracts shared byte-for-byte by the Ployz CLI and daemon.

pub mod authority;
pub mod cloud_contract;
pub mod config;
mod container_metadata;
pub mod decode;
pub mod deploy_progress;
mod dockerfile;
pub mod domain;
mod enrollment;
pub mod error_chain;
pub mod framing;
pub use enrollment::*;
mod founding;
pub mod hex;
mod hint;
mod host_config;
pub mod host_preparation;
pub mod labels;
mod log_level;
mod machine_layout;
mod machine_telemetry;
pub mod memory_size;
pub mod metrics;
pub mod namespace;
#[cfg(not(target_arch = "wasm32"))]
pub mod port_holder;
mod ports;
#[cfg(unix)]
pub mod private_fs;
pub mod probe_document;
mod release;
#[cfg(not(target_arch = "wasm32"))]
mod release_mirror;
#[cfg(not(target_arch = "wasm32"))]
pub mod routing;
pub mod rpc;
mod rpc_catalog;
pub mod scrub;
mod sealed;
mod sealed_record;
mod sealing;
#[cfg(not(target_arch = "wasm32"))]
pub mod sealing_backup;
pub mod service;
pub mod shell;
pub mod startup_refusal;
mod storage_capacity;
pub mod stream;
pub mod value;
mod volume_copy;
mod volume_size;
mod volume_switch;

pub use container_metadata::*;
pub use dockerfile::DockerfileFacts;
pub use domain::*;
pub use founding::*;
pub use framing::*;
pub use hint::*;
pub use host_config::*;
pub use log_level::*;
pub use machine_layout::*;
pub use machine_telemetry::*;
pub use namespace::*;
pub use ports::*;
pub use release::*;
#[cfg(not(target_arch = "wasm32"))]
pub use release_mirror::*;
#[cfg(not(target_arch = "wasm32"))]
pub use routing::*;
pub use rpc::*;
pub use sealed::*;
pub use sealed_record::*;
pub use sealing::*;
pub use service::*;
pub use startup_refusal::{STARTUP_REFUSAL_FILE, STARTUP_REFUSED};
pub use storage_capacity::*;
pub use stream::*;
pub use value::*;
pub use volume_copy::*;
pub use volume_size::*;
pub use volume_switch::*;

/// How long a Machine may take to become ready after start or restart,
/// including a first Corrosion image pull. systemd start is extended up to
/// this long; clients waiting for Participating must wait at least this long.
pub const MACHINE_START_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// What a Machine that enforces Device approval says when an Operator asks it to `Build`
/// or `MintBuildGrant`. Clients compare against it to show the refusal as it is.
pub const BUILD_REFUSED_BY_ENFORCEMENT: &str =
    "This server runs only builds an approved device starts.";

/// What a Machine that enforces Device approval says when an Operator asks it to
/// `SeedVolume`, as restoring a point into a new volume does.
pub const SEED_REFUSED_BY_ENFORCEMENT: &str = "This server restores into a new volume only what you approve, so it can't run this restore yet.";

/// What a Machine that enforces Device approval says when an Operator asks it to `Leave`.
/// Clients compare against it to show the refusal as it is.
pub const LEAVE_REFUSED_BY_ENFORCEMENT: &str = "This server leaves only when you approve it, with ployz server rm from an approved device or your passkey in the dashboard";

/// What a Machine that enforces Device approval says when an Operator asks it to `Forget`
/// another Machine.
pub const FORGET_REFUSED_BY_ENFORCEMENT: &str = "This server forgets another server only when you approve it, with ployz server forget from an approved device or your passkey in the dashboard";

/// Daemon exit status for an operator-actionable Docker network or WireGuard
/// interface refusal.
pub const NETWORK_CONFLICT_EXIT_STATUS: u8 = 78;

/// How long a route change waits for every live proxy's ack: the Deploy executor's
/// wait, and the daemon's boot pass under one deadline.
pub const ACK_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Safe operator recovery shared by daemon diagnostics and lifecycle commands.
pub const DOCKER_NETWORK_CONFLICT_RECOVERY: &str = "run `systemctl stop ployz.socket ployz`; run `docker network inspect ployz` and identify the network owner from its labels and attached containers; safely remove or migrate every attached container through its owning deployment; after confirming the network is empty and no longer needed, run `docker network rm ployz`; run `systemctl reset-failed ployz.socket ployz`; run `systemctl start ployz`";
