//! Compatibility exports for role-neutral daemon recovery failover.

pub use crate::recovery::IntentFailover;
pub(crate) use crate::recovery::{mirrored_server_pool, spawn_intent_failover_mirror};
