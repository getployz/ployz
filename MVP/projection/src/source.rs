use std::collections::BTreeMap;

use mvp_bus::{
    BusError, BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId,
    PrincipalId,
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactKind {
    NodeJoined,
    NodeRemovalStarted,
    NodeTombstoned,
    PeerAdmitted,
    ServiceRegistered,
    ServingCommit,
    RouteCommit,
    GatewayCommit,
    DnsCommit,
    LeaseClaimed,
    LeaseRenewed,
    LeaseReleased,
    AcmeHttp01Presented,
    AcmeHttp01Cleared,
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
        ["facts", "node", _node_id, "removal_started", epoch]
        | ["facts", "node", _node_id, "removal_started", epoch, _] => {
            classify_epoch(FactKind::NodeRemovalStarted, epoch)
        }
        ["facts", "node", _node_id, "tombstoned", epoch]
        | ["facts", "node", _node_id, "tombstoned", epoch, _] => {
            classify_epoch(FactKind::NodeTombstoned, epoch)
        }
        ["facts", "peer", _node_id, "admitted", epoch]
        | ["facts", "peer", _node_id, "admitted", epoch, _] => {
            classify_epoch(FactKind::PeerAdmitted, epoch)
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
        ["facts", "lease", _resource, "claimed", epoch]
        | ["facts", "lease", _resource, "claimed", epoch, _] => {
            classify_epoch(FactKind::LeaseClaimed, epoch)
        }
        [
            "facts",
            "lease",
            _resource,
            "renewed",
            epoch,
            _claim_hash,
            _renewed_at,
        ]
        | [
            "facts",
            "lease",
            _resource,
            "renewed",
            epoch,
            _claim_hash,
            _renewed_at,
            _,
        ] => classify_epoch(FactKind::LeaseRenewed, epoch),
        [
            "facts",
            "lease",
            _resource,
            "released",
            epoch,
            _claim_hash,
            _released_at,
        ]
        | [
            "facts",
            "lease",
            _resource,
            "released",
            epoch,
            _claim_hash,
            _released_at,
            _,
        ] => classify_epoch(FactKind::LeaseReleased, epoch),
        [
            "facts",
            "acme",
            "http01",
            _hostname,
            _token,
            "presented",
            epoch,
        ]
        | [
            "facts",
            "acme",
            "http01",
            _hostname,
            _token,
            "presented",
            epoch,
            _,
        ] => classify_epoch(FactKind::AcmeHttp01Presented, epoch),
        [
            "facts",
            "acme",
            "http01",
            _hostname,
            _token,
            "cleared",
            epoch,
            _claim_hash,
        ]
        | [
            "facts",
            "acme",
            "http01",
            _hostname,
            _token,
            "cleared",
            epoch,
            _claim_hash,
            _,
        ] => classify_epoch(FactKind::AcmeHttp01Cleared, epoch),
        _ => FactKeyClassification::new(FactKind::Unsupported, 0),
    }
}

#[must_use]
pub(crate) fn is_reducible_conflict_kind(kind: FactKind) -> bool {
    matches!(
        kind,
        FactKind::NodeRemovalStarted
            | FactKind::LeaseClaimed
            | FactKind::LeaseRenewed
            | FactKind::LeaseReleased
            | FactKind::AcmeHttp01Presented
            | FactKind::AcmeHttp01Cleared
    )
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
    fn classifies_tombstone_epochs() {
        let classification = classify_fact_key(&key("/facts/node/node-1/tombstoned/13"));

        assert_eq!(classification.kind(), FactKind::NodeTombstoned);
        assert_eq!(classification.epoch(), 13);
    }

    #[test]
    fn classifies_removal_started_epochs() {
        let classification = classify_fact_key(&key("/facts/node/node-1/removal_started/14"));

        assert_eq!(classification.kind(), FactKind::NodeRemovalStarted);
        assert_eq!(classification.epoch(), 14);
    }

    #[test]
    fn classifies_lease_and_acme_epochs() {
        let claim = classify_fact_key(&key("/facts/lease/acme.http01.example.token/claimed/7"));
        let presented = classify_fact_key(&key("/facts/acme/http01/example.com/token/presented/8"));
        let cleared = classify_fact_key(&key("/facts/acme/http01/example.com/token/cleared/9/abc"));

        assert_eq!(claim.kind(), FactKind::LeaseClaimed);
        assert_eq!(claim.epoch(), 7);
        assert_eq!(presented.kind(), FactKind::AcmeHttp01Presented);
        assert_eq!(presented.epoch(), 8);
        assert_eq!(cleared.kind(), FactKind::AcmeHttp01Cleared);
        assert_eq!(cleared.epoch(), 9);
    }

    #[test]
    fn invalid_epoch_is_unsupported_instead_of_epoch_zero() {
        let classification = classify_fact_key(&key("/facts/service/web/node-1/registered/nope"));

        assert_eq!(classification.kind(), FactKind::Unsupported);
        assert_eq!(classification.epoch(), 0);
    }
}
