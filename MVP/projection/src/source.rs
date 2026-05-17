use std::collections::BTreeMap;

use mvp_bus::{
    BusError, BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId,
    PrincipalId,
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactKind {
    NodeJoined,
    ServiceRegistered,
    ServingCommit,
    RouteCommit,
    GatewayCommit,
    DnsCommit,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactKeyClassification {
    kind: FactKind,
    epoch: u64,
}

impl FactKeyClassification {
    #[must_use]
    pub fn new(kind: FactKind, epoch: u64) -> Self {
        Self { kind, epoch }
    }

    #[must_use]
    pub fn kind(self) -> FactKind {
        self.kind
    }

    #[must_use]
    pub fn epoch(self) -> u64 {
        self.epoch
    }
}

#[must_use]
pub fn classify_fact_key(key: &FactKey) -> FactKeyClassification {
    let segments = key.segments().collect::<Vec<_>>();
    match segments.as_slice() {
        ["facts", "node", _node_id, "joined", epoch]
        | ["facts", "node", _node_id, "joined", epoch, _] => {
            classify_epoch(FactKind::NodeJoined, epoch)
        }
        ["facts", "service", _service, _node_id, "registered", epoch]
        | [
            "facts",
            "service",
            _service,
            _node_id,
            "registered",
            epoch,
            _,
        ] => classify_epoch(FactKind::ServiceRegistered, epoch),
        ["facts", "serving", _serving_commit] => {
            FactKeyClassification::new(FactKind::ServingCommit, 0)
        }
        ["facts", "routes", _route_commit] => FactKeyClassification::new(FactKind::RouteCommit, 0),
        ["facts", "gateway", _gateway_commit] => {
            FactKeyClassification::new(FactKind::GatewayCommit, 0)
        }
        ["facts", "dns", _dns_commit] => FactKeyClassification::new(FactKind::DnsCommit, 0),
        _ => FactKeyClassification::new(FactKind::Unsupported, 0),
    }
}

fn classify_epoch(kind: FactKind, epoch: &str) -> FactKeyClassification {
    match epoch.parse() {
        Ok(epoch) => FactKeyClassification::new(kind, epoch),
        Err(_) => FactKeyClassification::new(FactKind::Unsupported, 0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateStatus {
    Verified,
    Unverified,
    Unauthorized,
    CrossIsland,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCandidate {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    content_hash: FactContentHash,
    kind: FactKind,
    epoch: u64,
    status: CandidateStatus,
}

impl FactCandidate {
    #[must_use]
    pub fn new(
        island: IslandId,
        key: FactKey,
        author: PrincipalId,
        content_hash: FactContentHash,
        kind: FactKind,
        epoch: u64,
        status: CandidateStatus,
    ) -> Self {
        Self {
            island,
            key,
            author,
            content_hash,
            kind,
            epoch,
            status,
        }
    }

    #[must_use]
    pub fn verified(
        island: IslandId,
        key: FactKey,
        author: PrincipalId,
        content_hash: FactContentHash,
        kind: FactKind,
        epoch: u64,
    ) -> Self {
        Self::new(
            island,
            key,
            author,
            content_hash,
            kind,
            epoch,
            CandidateStatus::Verified,
        )
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        &self.key
    }

    #[must_use]
    pub fn author(&self) -> &PrincipalId {
        &self.author
    }

    #[must_use]
    pub fn content_hash(&self) -> &FactContentHash {
        &self.content_hash
    }

    #[must_use]
    pub fn kind(&self) -> FactKind {
        self.kind
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    #[must_use]
    pub fn status(&self) -> CandidateStatus {
        self.status
    }
}

pub type FactSourceResult<T> = std::result::Result<T, FactSourceError>;

#[derive(Debug, Error)]
pub enum FactSourceError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error("fact source unavailable: {name}")]
    Unavailable { name: String },
}

pub trait FactSource: Send + Sync {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>>;

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>>;
}

#[cfg(test)]
mod tests {
    use super::{FactKind, classify_fact_key};
    use mvp_bus::FactKey;

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    #[test]
    fn classifies_supported_fact_key_epochs() {
        let classification = classify_fact_key(&key("/facts/node/node-1/joined/12"));

        assert_eq!(classification.kind(), FactKind::NodeJoined);
        assert_eq!(classification.epoch(), 12);
    }

    #[test]
    fn invalid_epoch_is_unsupported_instead_of_epoch_zero() {
        let classification = classify_fact_key(&key("/facts/service/web/node-1/registered/nope"));

        assert_eq!(classification.kind(), FactKind::Unsupported);
        assert_eq!(classification.epoch(), 0);
    }
}
