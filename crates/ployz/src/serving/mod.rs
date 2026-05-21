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
pub struct ServingCommitRequest {
    route: RouteId,
    hostname: Hostname,
    target: ServingTarget,
    generation: ServingGeneration,
}

impl ServingCommitRequest {
    #[must_use]
    pub(crate) fn new(
        route: RouteId,
        hostname: Hostname,
        target: ServingTarget,
        generation: ServingGeneration,
    ) -> Self {
        Self {
            route,
            hostname,
            target,
            generation,
        }
    }

    #[must_use]
    pub fn route(&self) -> &RouteId {
        &self.route
    }

    #[must_use]
    pub fn hostname(&self) -> &Hostname {
        &self.hostname
    }

    #[must_use]
    pub fn target(&self) -> &ServingTarget {
        &self.target
    }

    #[must_use]
    pub fn generation(&self) -> ServingGeneration {
        self.generation
    }

    #[must_use]
    pub fn checkpoint(&self) -> ServingActivationCheckpoint {
        ServingActivationCheckpoint::from_request(self)
    }
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
pub struct ServingActivationCheckpoint {
    route: RouteId,
    hostname: Hostname,
    target: ServingTarget,
    generation: ServingGeneration,
}

impl ServingActivationCheckpoint {
    #[must_use]
    fn from_request(request: &ServingCommitRequest) -> Self {
        Self {
            route: request.route.clone(),
            hostname: request.hostname.clone(),
            target: request.target.clone(),
            generation: request.generation,
        }
    }

    #[must_use]
    pub fn route(&self) -> &RouteId {
        &self.route
    }

    #[must_use]
    pub fn hostname(&self) -> &Hostname {
        &self.hostname
    }

    #[must_use]
    pub fn target(&self) -> &ServingTarget {
        &self.target
    }

    #[must_use]
    pub fn generation(&self) -> ServingGeneration {
        self.generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingActivationProof {
    checkpoint: ServingActivationCheckpoint,
}

impl ServingActivationProof {
    #[must_use]
    pub fn checkpoint(&self) -> &ServingActivationCheckpoint {
        &self.checkpoint
    }

    #[must_use]
    pub fn generation(&self) -> ServingGeneration {
        self.checkpoint.generation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingActivationObservation {
    Acknowledged {
        route: RouteId,
        hostname: Hostname,
        target: ServingTarget,
        generation: ServingGeneration,
    },
    Failed(ServingFailure),
    Unknown,
}

impl ServingActivationObservation {
    pub(crate) fn try_acknowledge(
        &self,
        checkpoint: &ServingActivationCheckpoint,
    ) -> Result<ServingActivationProof, ServingFailure> {
        match self {
            Self::Acknowledged {
                route,
                hostname,
                target,
                generation,
            } if route == checkpoint.route()
                && hostname == checkpoint.hostname()
                && target == checkpoint.target()
                && *generation == checkpoint.generation() =>
            {
                Ok(ServingActivationProof {
                    checkpoint: checkpoint.clone(),
                })
            }
            Self::Failed(error) => Err(error.clone()),
            Self::Acknowledged { .. } | Self::Unknown => {
                Err(ServingFailure::LiveObservationUnknown)
            }
        }
    }
}

pub trait ServingPort {
    fn commit_snapshot(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<(), ServingFailure>;

    fn activation_status(
        &self,
        checkpoint: &ServingActivationCheckpoint,
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
    fn committed_request_is_not_activation() {
        let request = request(ServingGeneration::new(7));
        let checkpoint = request.checkpoint();
        let status = ServingActivationObservation::Unknown;

        assert_eq!(
            status.try_acknowledge(&checkpoint),
            Err(ServingFailure::LiveObservationUnknown)
        );
    }

    #[test]
    fn acknowledged_activation_mints_proof_for_matching_checkpoint_identity() {
        let request = request(ServingGeneration::new(7));
        let checkpoint = request.checkpoint();
        let matching = ServingActivationObservation::Acknowledged {
            route: request.route().clone(),
            hostname: request.hostname().clone(),
            target: request.target().clone(),
            generation: ServingGeneration::new(7),
        };
        let stale = ServingActivationObservation::Acknowledged {
            route: request.route().clone(),
            hostname: request.hostname().clone(),
            target: request.target().clone(),
            generation: ServingGeneration::new(6),
        };
        let wrong_hostname = ServingActivationObservation::Acknowledged {
            route: request.route().clone(),
            hostname: Hostname::parse("other.example.com").expect("hostname"),
            target: request.target().clone(),
            generation: ServingGeneration::new(7),
        };

        assert_eq!(
            matching
                .try_acknowledge(&checkpoint)
                .expect("activation proof")
                .checkpoint(),
            &checkpoint
        );
        assert_eq!(
            stale.try_acknowledge(&checkpoint),
            Err(ServingFailure::LiveObservationUnknown)
        );
        assert_eq!(
            wrong_hostname.try_acknowledge(&checkpoint),
            Err(ServingFailure::LiveObservationUnknown)
        );
    }

    #[test]
    fn failed_activation_does_not_acknowledge_checkpoint() {
        let request = request(ServingGeneration::new(7));
        let checkpoint = request.checkpoint();
        let status = ServingActivationObservation::Failed(ServingFailure::ReloadFailed);

        assert_eq!(
            status.try_acknowledge(&checkpoint),
            Err(ServingFailure::ReloadFailed)
        );
    }

    #[test]
    fn empty_route_id_is_rejected() {
        assert_eq!(RouteId::parse(""), Err(ServingFailure::SnapshotRejected));
    }

    fn request(generation: ServingGeneration) -> ServingCommitRequest {
        ServingCommitRequest::new(
            RouteId::parse("route-a").expect("route"),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse("gateway-a").expect("target"),
            generation,
        )
    }
}
