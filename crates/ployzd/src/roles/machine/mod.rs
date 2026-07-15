//! Machine-local RPC seam.
//!
//! - `protocol`: wire request/response types and the shared RPC envelope.
//! - `service`: server-side NATS handlers for machine-local commands.
//! - `execution`: machine-owned container, image, and host dataplane adapters.
//! - `runner`: the `MachineContainerRunner` port and container-run decision.
//! - `process`: the machine role process and observation loop.
//!
//! Recovery mirroring and failover are owned by the role-neutral
//! [`crate::recovery`] module.

mod containers;
mod dataplane;
mod endpoints;
pub mod execution;
mod facts;
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

pub(crate) fn current_unix_ms() -> u64 {
    let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return 0;
    };
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}
