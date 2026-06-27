//! Machine-local RPC seam.
//!
//! - `protocol`: wire request/response types and the shared RPC envelope.
//! - `service`: server-side NATS handlers for machine-local commands.
//! - `client`: request-side NATS adapters used by deploy/control workers.
//! - `runner`: the `MachineContainerRunner` port and container-run decision.
//! - `process`: the machine role process runtime and observation loop.

pub mod client;
pub mod process;
pub mod protocol;
pub mod runner;
pub mod service;
