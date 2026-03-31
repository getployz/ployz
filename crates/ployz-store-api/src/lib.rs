//! Store-facing contracts for machine state, deploy state, and routing snapshots.
//!
//! The orchestrator depends on these traits rather than a concrete storage
//! implementation so the core remains independent from the backing database.

pub mod memory;
mod traits;

pub use traits::{
    BootstrapStateReader, DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore,
    InviteStore, MachineEventSubscription, MachineStore, MachineSubscription,
    RoutingInvalidationSubscription, RoutingStore, SyncProbe, SyncStatus,
};
