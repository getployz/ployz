//! Role-neutral daemon recovery mechanics.
//!
//! Machine, Gateway, and DNS independently persist the same daemon-local
//! intent mirror and derive NATS failover from it. The mirror remains local
//! recovery evidence; Core intent is authoritative.

mod intent_mirror;
mod nats_failover;

pub use intent_mirror::{IntentMirror, IntentMirrorStoreOutcome, PendingMachineJoinMirror};
pub use nats_failover::IntentFailover;
pub(crate) use nats_failover::{mirrored_server_pool, spawn_intent_failover_mirror};
