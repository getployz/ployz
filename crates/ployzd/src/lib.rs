//! Minimal daemon lifecycle owner for Ployz substrate boot.
//!
//! `ployzd` owns process lifecycle, local substrate adoption, startup reporting, and
//! shutdown. Product semantics stay in `ployz`; product-neutral primitives stay
//! in `polis`.

pub mod commands;
pub mod config;
pub mod daemon;
mod operations;
pub mod report;
mod substrate;

pub use commands::{
    DaemonCommandError, DaemonCommandRequest, DaemonCommandResponse, DaemonCommandService,
    DaemonOperationFailure, DaemonStatus, OperationStatusResponse,
};
pub use config::DaemonConfig;
pub use daemon::{DaemonError, DaemonErrorKind, DaemonRuntime};
pub use report::{
    CorrosionCleanup, CorrosionFailure, CorrosionPhase, CorrosionStartup, CorrosionStop,
    PeerFailure, PeerPhase, PeerStartup, SchemaFailure, SchemaPhase, SchemaStartup, StartupReport,
};

#[cfg(test)]
mod tests;
