//! Serving product ports.

use crate::acme::Hostname;
use crate::error::ServingFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteId(String);

impl RouteId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServingTarget(String);

impl ServingTarget {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingSnapshot {
    pub route: RouteId,
    pub hostname: Hostname,
    pub target: ServingTarget,
    pub generation: ServingGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServingGeneration(u64);

impl ServingGeneration {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCheckpoint {
    pub generation: ServingGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingActivationStatus {
    Acknowledged(ServingCheckpoint),
    Failed(ServingFailure),
    Unknown,
}

pub trait ServingPort {
    fn commit_snapshot(
        &self,
        snapshot: ServingSnapshot,
    ) -> Result<ServingCheckpoint, ServingFailure>;

    fn activation_status(
        &self,
        target: &ServingTarget,
    ) -> Result<ServingActivationStatus, ServingFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_snapshot_is_not_activation() {
        let checkpoint = ServingCheckpoint {
            generation: ServingGeneration::new(7),
        };
        let status = ServingActivationStatus::Unknown;

        assert_ne!(status, ServingActivationStatus::Acknowledged(checkpoint));
    }
}
