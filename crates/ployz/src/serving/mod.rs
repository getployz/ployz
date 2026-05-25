//! Serving product ports.

use std::future::Future;
use std::time::SystemTime;

use crate::acme::Hostname;
use crate::error::ServingFailure;
use crate::operation::MutationContext;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteId(String);

impl RouteId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ServingFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServingTarget(String);

impl ServingTarget {
    pub fn parse(value: impl Into<String>) -> Result<Self, ServingFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServingCommitId(String);

impl ServingCommitId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ServingFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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
    pub(crate) fn replace_current_route(
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

    #[must_use]
    pub fn commit_identity(&self) -> ServingCommitIdentity {
        ServingCommitIdentity::from_request(self)
    }

    pub(crate) fn commit_id(&self) -> Result<ServingCommitId, ServingFailure> {
        commit_id_for_identity(&self.commit_identity())
    }
}

/// Route switch identity carried to DNS/gateway subscribers.
///
/// This is not a stale-write fence. Pre-v1 serving state is a latest-current
/// row, so rollback or same-version route switches are explicit overwrites by
/// the product writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServingGeneration(u64);

impl ServingGeneration {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCheckpoint {
    generation: ServingGeneration,
}

impl ServingCheckpoint {
    #[must_use]
    pub fn new(generation: ServingGeneration) -> Self {
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServingCommitIdentity {
    route: RouteId,
    hostname: Hostname,
    target: ServingTarget,
    generation: ServingGeneration,
}

impl ServingCommitIdentity {
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
    fn from_request(request: &ServingCommitRequest) -> Self {
        Self::new(
            request.route.clone(),
            request.hostname.clone(),
            request.target.clone(),
            request.generation,
        )
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
    pub fn matches_request(&self, request: &ServingCommitRequest) -> bool {
        self == &request.commit_identity()
    }
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
    pub fn route(&self) -> &RouteId {
        self.checkpoint.route()
    }

    #[must_use]
    pub fn hostname(&self) -> &Hostname {
        self.checkpoint.hostname()
    }

    #[must_use]
    pub fn target(&self) -> &ServingTarget {
        self.checkpoint.target()
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
    pub(crate) fn try_acknowledge_commit(
        &self,
        request: &ServingCommitRequest,
    ) -> Result<ServingActivationProof, ServingFailure> {
        self.try_acknowledge(&request.checkpoint())
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCommittedSnapshot {
    commit_id: ServingCommitId,
    identity: ServingCommitIdentity,
}

impl ServingCommittedSnapshot {
    pub fn from_request(request: &ServingCommitRequest) -> Result<Self, ServingFailure> {
        Self::from_identity(request.commit_identity())
    }

    pub(crate) fn from_identity(identity: ServingCommitIdentity) -> Result<Self, ServingFailure> {
        Ok(Self {
            commit_id: commit_id_for_identity(&identity)?,
            identity,
        })
    }

    pub(crate) fn from_stored_parts(
        commit_id: ServingCommitId,
        identity: ServingCommitIdentity,
    ) -> Result<Self, ServingFailure> {
        let snapshot = Self::from_identity(identity)?;
        if snapshot.commit_id == commit_id {
            Ok(snapshot)
        } else {
            Err(ServingFailure::SnapshotRejected)
        }
    }

    #[must_use]
    pub fn commit_id(&self) -> &ServingCommitId {
        &self.commit_id
    }

    #[must_use]
    pub fn identity(&self) -> &ServingCommitIdentity {
        &self.identity
    }

    #[must_use]
    pub fn matches_request(&self, request: &ServingCommitRequest) -> bool {
        self.identity.matches_request(request)
            && request
                .commit_id()
                .map(|commit_id| self.commit_id == commit_id)
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServingCommitObservation {
    Current(ServingCommittedSnapshot),
    Missing,
    Unknown,
}

impl ServingCommitObservation {
    pub fn try_confirm_commit(
        &self,
        request: &ServingCommitRequest,
    ) -> Result<ServingCommitMatch, ServingFailure> {
        match self {
            Self::Current(current) if current.matches_request(request) => {
                Ok(ServingCommitMatch::Current)
            }
            Self::Current(_) => Ok(ServingCommitMatch::DifferentCurrent),
            Self::Missing => Ok(ServingCommitMatch::Missing),
            Self::Unknown => Err(ServingFailure::LiveObservationUnknown),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServingCommitMatch {
    Current,
    Missing,
    DifferentCurrent,
}

pub trait ServingSnapshotPort {
    fn commit_snapshot<'a>(
        &'a self,
        context: &'a MutationContext,
        request: &'a ServingCommitRequest,
    ) -> impl Future<Output = Result<(), ServingFailure>> + 'a;

    fn commit_status<'a>(
        &'a self,
        context: &'a MutationContext,
        request: &'a ServingCommitRequest,
    ) -> impl Future<Output = Result<ServingCommitObservation, ServingFailure>> + 'a;
}

pub trait ServingActivationPort {
    /// Return the latest serving commit observed by the activation owner.
    ///
    /// Deploy uses this as the explicit boundary between writing the desired
    /// route row and trusting activation state from DNS/gateway-like observers.
    fn await_observed_commit<'a>(
        &'a self,
        context: &'a MutationContext,
        request: &'a ServingCommitRequest,
        deadline: SystemTime,
    ) -> impl Future<Output = Result<ServingCommitObservation, ServingFailure>> + 'a;

    fn activation_status(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
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

fn commit_id_for_identity(
    identity: &ServingCommitIdentity,
) -> Result<ServingCommitId, ServingFailure> {
    let payload = ServingCommitIdentityPayload {
        route_id: identity.route().as_str(),
        hostname: identity.hostname().as_str(),
        target: identity.target().as_str(),
        generation: identity.generation().value(),
    };
    let bytes = serde_json::to_vec(&payload).map_err(|_| ServingFailure::SnapshotRejected)?;
    let digest = Sha256::digest(bytes);
    ServingCommitId::parse(hex_prefix(&digest, 16))
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut value = String::with_capacity(len * 2);
    for byte in bytes.iter().take(len) {
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

#[derive(Serialize)]
struct ServingCommitIdentityPayload<'a> {
    route_id: &'a str,
    hostname: &'a str,
    target: &'a str,
    generation: u64,
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
        ServingCommitRequest::replace_current_route(
            RouteId::parse("route-a").expect("route"),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse("gateway-a").expect("target"),
            generation,
        )
    }
}
