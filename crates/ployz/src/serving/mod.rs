//! Serving product ports.

use crate::acme::Hostname;
use crate::error::ServingFailure;
use crate::operation::MutationContext;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteId(String);

impl RouteId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ServingFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServingTarget(String);

impl ServingTarget {
    pub fn parse(value: impl Into<String>) -> Result<Self, ServingFailure> {
        parse_non_empty(value, Self)
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
    generation: ServingGeneration,
}

impl ServingCheckpoint {
    #[must_use]
    pub(crate) fn new(generation: ServingGeneration) -> Self {
        Self { generation }
    }

    #[must_use]
    pub fn generation(&self) -> ServingGeneration {
        self.generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCommitReceipt {
    pub generation: ServingGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingActivationObservation {
    Acknowledged { generation: ServingGeneration },
    Failed(ServingFailure),
    Unknown,
}

pub trait ServingPort {
    fn commit_snapshot(
        &self,
        context: &MutationContext,
        snapshot: ServingSnapshot,
    ) -> Result<ServingCommitReceipt, ServingFailure>;

    fn activation_status(
        &self,
        target: &ServingTarget,
    ) -> Result<ServingActivationObservation, ServingFailure>;
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, ServingFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(ServingFailure::SnapshotRejected);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_snapshot_is_not_activation() {
        let checkpoint = ServingCheckpoint::new(ServingGeneration::new(7));
        let status = ServingActivationObservation::Unknown;

        assert_ne!(
            status,
            ServingActivationObservation::Acknowledged {
                generation: checkpoint.generation()
            }
        );
    }

    #[test]
    fn empty_route_id_is_rejected() {
        assert_eq!(RouteId::parse(""), Err(ServingFailure::SnapshotRejected));
    }
}
