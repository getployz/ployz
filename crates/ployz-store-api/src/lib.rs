//! Store-facing contracts for machine state, deploy state, and routing snapshots.
//!
//! The orchestrator depends on these traits rather than a concrete storage
//! implementation so the core remains independent from the backing database.

mod traits;

pub use traits::{
    ClusterStore, DeployCommit, DeployCommitStore, DeployReadStore, DeployWriteStore, InviteStore,
    MachineEventReceiver, MachineEventSubscription, MachineStore, MachineSubscription,
    MembershipCommitStore, RoutingInvalidationReceiver, RoutingInvalidationSubscription,
    RoutingStore, SubscriptionPoll, SyncProbe, SyncStatus,
};
