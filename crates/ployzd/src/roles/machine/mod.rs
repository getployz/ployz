//! Machine-local RPC seam.
//!
//! - `protocol`: wire request/response types and the shared RPC envelope.
//! - `service`: server-side NATS handlers for machine-local commands.
//! - `client`: request-side NATS adapters used by deploy/control workers.
//! - `runner`: the `MachineContainerRunner` port and container-run decision.
//! - `process`: the machine role process and observation loop.
//! - `intent_mirror`: machine-local durable copy of core intent (ADR 0031).

pub mod client;
mod endpoints;
pub mod intent_mirror;
mod ployz_native_mesh;
pub mod process;
pub mod protocol;
pub mod runner;
pub mod service;
