use crate::coord::locks::{Lease, LeaseValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderLease {
    lease: Lease,
}

impl LeaderLease {
    #[must_use]
    pub fn new(key: impl Into<String>, revision: u64, value: LeaseValue) -> Self {
        Self {
            lease: Lease::new(key, revision, value),
        }
    }

    #[must_use]
    pub fn lease(&self) -> &Lease {
        &self.lease
    }
}
