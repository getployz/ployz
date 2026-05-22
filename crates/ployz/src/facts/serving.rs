//! Product-owned serving commit facts and reducer.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::acme::Hostname;
use crate::facts::{
    ProductFact, ProductFactEnvelope, ProductFactKey, ProductFactKind, ProductFactPayload,
    ProductFactResource, ProductFactTarget,
};
use crate::serving::{
    RouteId, ServingCommitId, ServingCommitRequest, ServingCommittedSnapshot, ServingGeneration,
    ServingRouteState, ServingTarget,
};

pub const SERVING_COMMIT_KIND: &str = "ployz.serving.commit.v1";
pub const SERVING_GENERATION_SLOT_KIND: &str = "ployz.serving.generation-slot.v1";

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ServingFactError {
    #[error("serving fact payload is invalid")]
    InvalidPayload,
    #[error("serving fact target does not match payload")]
    TargetMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCommitFact {
    commit_id: ServingCommitId,
    request: ServingCommitRequest,
}

impl ServingCommitFact {
    pub fn from_request(request: ServingCommitRequest) -> Result<Self, ServingFactError> {
        let identity = ServingCommitIdentityPayload::from_request(&request);
        let commit_id = ServingCommitId::parse(commit_id_for_identity(&identity)?)
            .map_err(|_| ServingFactError::InvalidPayload)?;
        Ok(Self { commit_id, request })
    }

    pub fn decode_payload(payload: &[u8]) -> Result<Self, ServingFactError> {
        let payload = serde_json::from_slice::<ServingCommitPayload>(payload)
            .map_err(|_| ServingFactError::InvalidPayload)?;
        payload.try_into()
    }

    #[must_use]
    pub fn commit_id(&self) -> &ServingCommitId {
        &self.commit_id
    }

    #[must_use]
    pub fn request(&self) -> &ServingCommitRequest {
        &self.request
    }

    #[must_use]
    pub fn route(&self) -> &RouteId {
        self.request.route()
    }

    #[must_use]
    pub fn generation(&self) -> ServingGeneration {
        self.request.generation()
    }

    #[must_use]
    pub fn snapshot(&self) -> ServingCommittedSnapshot {
        ServingCommittedSnapshot::new(self.commit_id.clone(), self.request.commit_identity())
    }

    fn payload(&self) -> ServingCommitPayload {
        ServingCommitPayload {
            serving_commit_id: self.commit_id.as_str().to_owned(),
            route_id: self.request.route().as_str().to_owned(),
            hostname: self.request.hostname().as_str().to_owned(),
            target: self.request.target().as_str().to_owned(),
            generation: self.request.generation().value(),
        }
    }

    fn target(&self) -> Result<ProductFactTarget, ServingFactError> {
        Ok(ProductFactTarget::new(
            ProductFactResource::parse(format!("serving-route:{}", self.route().as_str()))
                .map_err(|_| ServingFactError::InvalidPayload)?,
            ProductFactKey::parse(format!("/facts/serving/{}", self.commit_id.as_str()))
                .map_err(|_| ServingFactError::InvalidPayload)?,
            ProductFactKind::parse(SERVING_COMMIT_KIND)
                .map_err(|_| ServingFactError::InvalidPayload)?,
        ))
    }
}

impl ProductFact for ServingCommitFact {
    type Error = ServingFactError;

    fn encode(&self) -> Result<ProductFactEnvelope, Self::Error> {
        let payload =
            serde_json::to_vec(&self.payload()).map_err(|_| ServingFactError::InvalidPayload)?;
        Ok(ProductFactEnvelope::new(
            self.target()?,
            ProductFactPayload::new(payload).map_err(|_| ServingFactError::InvalidPayload)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingGenerationSlotFact {
    commit: ServingCommitFact,
}

impl ServingGenerationSlotFact {
    pub fn from_request(request: ServingCommitRequest) -> Result<Self, ServingFactError> {
        Ok(Self {
            commit: ServingCommitFact::from_request(request)?,
        })
    }

    #[must_use]
    pub fn commit(&self) -> &ServingCommitFact {
        &self.commit
    }

    fn target(&self) -> Result<ProductFactTarget, ServingFactError> {
        Ok(ProductFactTarget::new(
            ProductFactResource::parse(format!(
                "serving-route-generation:{}:{}",
                self.commit.route().as_str(),
                self.commit.generation().value()
            ))
            .map_err(|_| ServingFactError::InvalidPayload)?,
            ProductFactKey::parse(format!(
                "/facts/serving/{}/generation/{}",
                self.commit.route().as_str(),
                self.commit.generation().value()
            ))
            .map_err(|_| ServingFactError::InvalidPayload)?,
            ProductFactKind::parse(SERVING_GENERATION_SLOT_KIND)
                .map_err(|_| ServingFactError::InvalidPayload)?,
        ))
    }
}

impl ProductFact for ServingGenerationSlotFact {
    type Error = ServingFactError;

    fn encode(&self) -> Result<ProductFactEnvelope, Self::Error> {
        let payload = serde_json::to_vec(&self.commit.payload())
            .map_err(|_| ServingFactError::InvalidPayload)?;
        Ok(ProductFactEnvelope::new(
            self.target()?,
            ProductFactPayload::new(payload).map_err(|_| ServingFactError::InvalidPayload)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingCommitReducer {
    route: RouteId,
}

impl ServingCommitReducer {
    #[must_use]
    pub fn new(route: RouteId) -> Self {
        Self { route }
    }

    pub fn reduce<I>(&self, facts: I) -> Result<ServingRouteState, ServingFactError>
    where
        I: IntoIterator<Item = ServingCommitFact>,
    {
        let mut commits = Vec::new();
        for fact in facts {
            if fact.route() != &self.route {
                return Err(ServingFactError::TargetMismatch);
            }
            commits.push(fact);
        }
        if commits.is_empty() {
            return Ok(ServingRouteState::empty(self.route.clone()));
        }

        commits.sort_by(|left, right| {
            (left.generation(), left.commit_id()).cmp(&(right.generation(), right.commit_id()))
        });
        commits.dedup_by(|left, right| left.commit_id() == right.commit_id());
        let highest_generation = commits.last().expect("non-empty commits").generation();
        if commits
            .iter()
            .filter(|fact| fact.generation() == highest_generation)
            .count()
            > 1
        {
            return Err(ServingFactError::TargetMismatch);
        }
        let superseded = commits.len() - 1;
        let current = commits.pop().expect("non-empty commits").snapshot();
        Ok(ServingRouteState::new(
            self.route.clone(),
            Some(current),
            superseded,
        ))
    }
}

fn commit_id_for_identity(
    identity: &ServingCommitIdentityPayload,
) -> Result<String, ServingFactError> {
    let bytes = serde_json::to_vec(identity).map_err(|_| ServingFactError::InvalidPayload)?;
    let digest = Sha256::digest(bytes);
    Ok(hex_prefix(&digest, 16))
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut value = String::with_capacity(len * 2);
    for byte in bytes.iter().take(len) {
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ServingCommitPayload {
    serving_commit_id: String,
    route_id: String,
    hostname: String,
    target: String,
    generation: u64,
}

impl TryFrom<ServingCommitPayload> for ServingCommitFact {
    type Error = ServingFactError;

    fn try_from(value: ServingCommitPayload) -> Result<Self, Self::Error> {
        let request = ServingCommitRequest::new(
            RouteId::parse(value.route_id).map_err(|_| ServingFactError::InvalidPayload)?,
            Hostname::parse(value.hostname).map_err(|_| ServingFactError::InvalidPayload)?,
            ServingTarget::parse(value.target).map_err(|_| ServingFactError::InvalidPayload)?,
            ServingGeneration::new(value.generation),
        );
        let identity = ServingCommitIdentityPayload::from_request(&request);
        if commit_id_for_identity(&identity)? != value.serving_commit_id {
            return Err(ServingFactError::TargetMismatch);
        }
        Ok(Self {
            commit_id: ServingCommitId::parse(value.serving_commit_id)
                .map_err(|_| ServingFactError::InvalidPayload)?,
            request,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ServingCommitIdentityPayload {
    route_id: String,
    hostname: String,
    target: String,
    generation: u64,
}

impl ServingCommitIdentityPayload {
    fn from_request(request: &ServingCommitRequest) -> Self {
        Self {
            route_id: request.route().as_str().to_owned(),
            hostname: request.hostname().as_str().to_owned(),
            target: request.target().as_str().to_owned(),
            generation: request.generation().value(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serving_commit_round_trips_through_product_payload() {
        let fact = ServingCommitFact::from_request(request(7)).expect("fact");

        let envelope = fact.encode().expect("encode");
        let decoded =
            ServingCommitFact::decode_payload(envelope.payload().as_bytes()).expect("decode");

        assert_eq!(decoded, fact);
        assert_eq!(
            envelope.target().resource().as_str(),
            "serving-route:route-app"
        );
        assert!(
            envelope
                .target()
                .key()
                .as_str()
                .starts_with("/facts/serving/")
        );
        assert_eq!(envelope.target().kind().as_str(), SERVING_COMMIT_KIND);
    }

    #[test]
    fn serving_generation_slot_uses_route_generation_uniqueness_target() {
        let slot = ServingGenerationSlotFact::from_request(request(7)).expect("slot");

        let envelope = slot.encode().expect("encode");
        let decoded =
            ServingCommitFact::decode_payload(envelope.payload().as_bytes()).expect("decode");

        assert_eq!(decoded, *slot.commit());
        assert_eq!(
            envelope.target().resource().as_str(),
            "serving-route-generation:route-app:7"
        );
        assert_eq!(
            envelope.target().key().as_str(),
            "/facts/serving/route-app/generation/7"
        );
        assert_eq!(
            envelope.target().kind().as_str(),
            SERVING_GENERATION_SLOT_KIND
        );
    }

    #[test]
    fn reducer_selects_highest_generation_for_route() {
        let reducer = ServingCommitReducer::new(route());

        let state = reducer
            .reduce([
                ServingCommitFact::from_request(request(7)).expect("old"),
                ServingCommitFact::from_request(request(9)).expect("new"),
            ])
            .expect("state");

        let current = state.current().expect("current");
        assert_eq!(current.identity().generation(), ServingGeneration::new(9));
        assert_eq!(state.superseded(), 1);
    }

    #[test]
    fn reducer_rejects_mismatched_route() {
        let reducer = ServingCommitReducer::new(route());
        let other = ServingCommitRequest::new(
            RouteId::parse("route-admin").expect("route"),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse("gateway-a").expect("target"),
            ServingGeneration::new(7),
        );

        let result = reducer.reduce([ServingCommitFact::from_request(other).expect("fact")]);

        assert_eq!(result, Err(ServingFactError::TargetMismatch));
    }

    #[test]
    fn reducer_rejects_same_generation_route_conflict() {
        let reducer = ServingCommitReducer::new(route());
        let alternate = ServingCommitRequest::new(
            route(),
            Hostname::parse("admin.example.com").expect("hostname"),
            ServingTarget::parse("gateway-b").expect("target"),
            ServingGeneration::new(7),
        );

        let result = reducer.reduce([
            ServingCommitFact::from_request(request(7)).expect("first"),
            ServingCommitFact::from_request(alternate).expect("alternate"),
        ]);

        assert_eq!(result, Err(ServingFactError::TargetMismatch));
    }

    fn request(generation: u64) -> ServingCommitRequest {
        ServingCommitRequest::new(
            route(),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse("gateway-a").expect("target"),
            ServingGeneration::new(generation),
        )
    }

    fn route() -> RouteId {
        RouteId::parse("route-app").expect("route")
    }
}
