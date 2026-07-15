//! Compatibility exports for daemon recovery mirrors.

pub use crate::recovery::{
    IntentMirror as MachineIntentMirror, IntentMirrorStoreOutcome as StoreOutcome,
    PendingMachineJoinMirror as MachinePendingJoinMirror,
};
