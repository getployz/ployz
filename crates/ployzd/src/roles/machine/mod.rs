//! Machine-local RPC seam.
//!
//! - `protocol`: wire request/response types and the shared RPC envelope.
//! - `service`: server-side NATS handlers for machine-local commands.
//! - `client`: request-side NATS adapters used by deploy/control workers.
//! - `runner`: the `MachineContainerRunner` port and container-run decision.
//! - `process`: the machine role process and observation loop.
//! Recovery mirroring and failover are owned by the role-neutral
//! [`crate::recovery`] module.

pub mod client;
mod containers;
pub(crate) mod convergence;
mod dataplane;
mod endpoints;
mod facts;
mod images;
/// Compatibility facade for the former Machine-owned recovery mirror.
pub mod intent_mirror;
mod logs;
mod ployz_native_mesh;
pub mod process;
pub(crate) mod projection;
pub mod protocol;
pub mod registry_v2;
pub(crate) mod response;
pub mod runner;
pub mod service;
mod substrate;

pub(crate) fn current_unix_ms() -> u64 {
    let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return 0;
    };
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}
