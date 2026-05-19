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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedFactKey {
    NodeJoined {
        node_id: String,
        epoch: String,
    },
    NodeRemovalStarted {
        node_id: String,
        epoch: String,
    },
    NodeTombstoned {
        node_id: String,
        epoch: String,
    },
    PeerAdmitted {
        node_id: String,
        epoch: String,
    },
    ServiceRegistered {
        service: String,
        node_id: String,
        epoch: String,
    },
    ServingCommit {
        serving_commit_id: String,
    },
    RouteCommit {
        route_commit_id: String,
    },
    GatewayCommit {
        gateway_commit_id: String,
    },
    DnsCommit {
        dns_commit_id: String,
    },
    LeaseClaimed {
        resource: String,
        epoch: String,
    },
    LeaseRenewed {
        resource: String,
        epoch: String,
        claim_hash: String,
        renewed_at: String,
    },
    LeaseReleased {
        resource: String,
        epoch: String,
        claim_hash: String,
        release: String,
    },
    AcmeHttp01Presented {
        hostname: String,
        token: String,
        epoch: String,
    },
    AcmeHttp01Cleared {
        hostname: String,
        token: String,
        epoch: String,
        claim_hash: String,
    },
    Unsupported,
}

impl ParsedFactKey {
    #[must_use]
    pub(crate) fn classification(&self) -> FactKeyClassification {
        match self {
            Self::NodeJoined { epoch, .. } => classify_epoch(FactKind::NodeJoined, epoch),
            Self::NodeRemovalStarted { epoch, .. } => {
                classify_epoch(FactKind::NodeRemovalStarted, epoch)
            }
            Self::NodeTombstoned { epoch, .. } => classify_epoch(FactKind::NodeTombstoned, epoch),
            Self::PeerAdmitted { epoch, .. } => classify_epoch(FactKind::PeerAdmitted, epoch),
            Self::ServiceRegistered { epoch, .. } => {
                classify_epoch(FactKind::ServiceRegistered, epoch)
            }
            Self::ServingCommit { .. } => FactKeyClassification::new(FactKind::ServingCommit, 0),
            Self::RouteCommit { .. } => FactKeyClassification::new(FactKind::RouteCommit, 0),
            Self::GatewayCommit { .. } => FactKeyClassification::new(FactKind::GatewayCommit, 0),
            Self::DnsCommit { .. } => FactKeyClassification::new(FactKind::DnsCommit, 0),
            Self::LeaseClaimed { epoch, .. } => classify_epoch(FactKind::LeaseClaimed, epoch),
            Self::LeaseRenewed { epoch, .. } => classify_epoch(FactKind::LeaseRenewed, epoch),
            Self::LeaseReleased { epoch, .. } => classify_epoch(FactKind::LeaseReleased, epoch),
            Self::AcmeHttp01Presented { epoch, .. } => {
                classify_epoch(FactKind::AcmeHttp01Presented, epoch)
            }
            Self::AcmeHttp01Cleared { epoch, .. } => {
                classify_epoch(FactKind::AcmeHttp01Cleared, epoch)
            }
            Self::Unsupported => FactKeyClassification::new(FactKind::Unsupported, 0),
        }
    }
}

#[must_use]
pub fn classify_fact_key(key: &FactKey) -> FactKeyClassification {
    parse_fact_key(key).classification()
}

pub(crate) fn parse_fact_key(key: &FactKey) -> ParsedFactKey {
    let segments = key.segments().collect::<Vec<_>>();
    match segments.as_slice() {
        ["facts", "node", _node_id, "joined", epoch]
        | ["facts", "node", _node_id, "joined", epoch, _] => ParsedFactKey::NodeJoined {
            node_id: (*_node_id).to_string(),
            epoch: (*epoch).to_string(),
        },
        ["facts", "node", _node_id, "removal_started", epoch]
        | ["facts", "node", _node_id, "removal_started", epoch, _] => {
            ParsedFactKey::NodeRemovalStarted {
                node_id: (*_node_id).to_string(),
                epoch: (*epoch).to_string(),
            }
        }
        ["facts", "node", _node_id, "tombstoned", epoch]
        | ["facts", "node", _node_id, "tombstoned", epoch, _] => ParsedFactKey::NodeTombstoned {
            node_id: (*_node_id).to_string(),
            epoch: (*epoch).to_string(),
        },
        ["facts", "peer", _node_id, "admitted", epoch]
        | ["facts", "peer", _node_id, "admitted", epoch, _] => ParsedFactKey::PeerAdmitted {
            node_id: (*_node_id).to_string(),
            epoch: (*epoch).to_string(),
        },
        ["facts", "service", _service, _node_id, "registered", epoch]
        | [
            "facts",
            "service",
            _service,
            _node_id,
            "registered",
            epoch,
            _,
        ] => ParsedFactKey::ServiceRegistered {
            service: (*_service).to_string(),
            node_id: (*_node_id).to_string(),
            epoch: (*epoch).to_string(),
        },
        ["facts", "serving", _serving_commit] => ParsedFactKey::ServingCommit {
            serving_commit_id: (*_serving_commit).to_string(),
        },
        ["facts", "routes", _route_commit] => ParsedFactKey::RouteCommit {
            route_commit_id: (*_route_commit).to_string(),
        },
        ["facts", "gateway", _gateway_commit] => ParsedFactKey::GatewayCommit {
            gateway_commit_id: (*_gateway_commit).to_string(),
        },
        ["facts", "dns", _dns_commit] => ParsedFactKey::DnsCommit {
            dns_commit_id: (*_dns_commit).to_string(),
        },
        ["facts", "lease", _resource, "claimed", epoch]
        | ["facts", "lease", _resource, "claimed", epoch, _] => ParsedFactKey::LeaseClaimed {
            resource: (*_resource).to_string(),
            epoch: (*epoch).to_string(),
        },
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
        ] => ParsedFactKey::LeaseRenewed {
            resource: (*_resource).to_string(),
            epoch: (*epoch).to_string(),
            claim_hash: (*_claim_hash).to_string(),
            renewed_at: (*_renewed_at).to_string(),
        },
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
        ] => ParsedFactKey::LeaseReleased {
            resource: (*_resource).to_string(),
            epoch: (*epoch).to_string(),
            claim_hash: (*_claim_hash).to_string(),
            release: (*_released_at).to_string(),
        },
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
        ] => ParsedFactKey::AcmeHttp01Presented {
            hostname: (*_hostname).to_string(),
            token: (*_token).to_string(),
            epoch: (*epoch).to_string(),
        },
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
        ] => ParsedFactKey::AcmeHttp01Cleared {
            hostname: (*_hostname).to_string(),
            token: (*_token).to_string(),
            epoch: (*epoch).to_string(),
            claim_hash: (*_claim_hash).to_string(),
        },
        _ => ParsedFactKey::Unsupported,
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
