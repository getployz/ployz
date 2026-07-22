//! Machine-local RPC seam.
//!
//! - `protocol`: wire request/response types and the shared RPC envelope.
//! - `service`: server-side NATS handlers for machine-local commands.
//! - `execution`: machine-owned container, image, and host dataplane adapters.
//! - `runner`: the `MachineContainerRunner` port for machine-local effects.
//! - `deploy_container_run`: private Service and Hook Container run choreography.
//! - `process`: the machine role process and observation loop.
//!
//! Recovery mirroring and failover are owned by the role-neutral
//! [`crate::recovery`] module.

mod build;
mod containers;
mod dataplane;
mod deploy_container_run;
mod endpoints;
pub mod execution;
mod facts;
mod image_ensure;
mod images;
mod logs;
pub mod process;
pub(crate) mod projection;
pub mod protocol;
pub(crate) mod response;
pub mod runner;
pub mod service;
mod substrate;
mod unavailable;
mod volume;

pub(crate) use unavailable::MachineRequestFailure;
pub use unavailable::MachineRuntimeUnavailableReason;
#[cfg(test)]
pub(crate) use volume::{VOLUME_TESTIMONY_ENDPOINT_TIMEOUT, handle_volume_testimony};

pub(crate) fn current_unix_ms() -> u64 {
    let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return 0;
    };
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}
