//! How long the Observe Store keeps marker segments, and the rollups an earlier
//! release wrote, before cleanup deletes them.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Retention {
    /// How long a raw hour is kept once it has closed.
    pub(crate) raw: Duration,
    /// How long a rollup is kept once its hour has closed.
    pub(crate) rollup: Duration,
}

pub(crate) const RETENTION: Retention = Retention {
    raw: Duration::from_secs(24 * 60 * 60),
    rollup: Duration::from_secs(90 * 24 * 60 * 60),
};
