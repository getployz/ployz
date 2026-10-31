//! Domain and wire contracts shared byte-for-byte by the Ployz CLI and daemon.

pub mod config;
mod container_metadata;
pub mod decode;
mod dockerfile;
pub mod domain;
mod enrollment;
pub mod error_chain;
pub mod framing;
pub use enrollment::*;
mod host_config;
pub mod labels;
mod log_level;
mod machine_telemetry;
pub mod metrics;
pub mod namespace;
#[cfg(not(target_arch = "wasm32"))]
pub mod port_holder;
mod ports;
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
pub mod service;
mod storage_capacity;
pub mod stream;
pub mod value;
mod volume_copy;
mod volume_size;
mod volume_switch;

pub use container_metadata::*;
pub use dockerfile::DockerfileFacts;
pub use domain::*;
pub use framing::*;
pub use host_config::*;
pub use log_level::*;
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

/// Daemon exit status for an operator-actionable Docker network or WireGuard
/// interface refusal.
pub const NETWORK_CONFLICT_EXIT_STATUS: u8 = 78;

/// The file beside the Machine API socket that holds the refusal of a start that
/// exited with [`NETWORK_CONFLICT_EXIT_STATUS`]. Each start removes it first.
pub const STARTUP_REFUSAL_FILE: &str = "refused";

/// How a startup refusal begins wherever a client reads it, including the
/// stderr of a remote `ployzd dial-stdio`. Clients of other releases stop
/// waiting on this text, so it must not change.
pub const STARTUP_REFUSED: &str = "ployzd refused to start: ";

/// Why the daemon whose socket is in `run_dir` refused its last start, or `None`
/// when it did not. A refusing start closes the connection that started it
/// unanswered, so a client reads this to say why.
#[must_use]
pub fn startup_refusal(run_dir: &std::path::Path) -> Option<String> {
    let line = std::fs::read_to_string(run_dir.join(STARTUP_REFUSAL_FILE)).ok()?;
    let line = line.trim();
    (!line.is_empty()).then(|| format!("{STARTUP_REFUSED}{line}"))
}

/// Safe operator recovery shared by daemon diagnostics and lifecycle commands.
pub const DOCKER_NETWORK_CONFLICT_RECOVERY: &str = "run `systemctl stop ployz.socket ployz`; run `docker network inspect ployz` and identify the network owner from its labels and attached containers; safely remove or migrate every attached container through its owning deployment; after confirming the network is empty and no longer needed, run `docker network rm ployz`; run `systemctl reset-failed ployz.socket ployz`; run `systemctl start ployz`";
