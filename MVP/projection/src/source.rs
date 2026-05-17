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
    RouteCommit,
    GatewayCommit,
    DnsCommit,
    Unsupported,
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
