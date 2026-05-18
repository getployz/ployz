use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::sync::Arc;

use mvp_bus::{
    BusSession, FactAccess, FactAuthorizer, FactContentHash, FactKey, FactKeyPattern, FactPayload,
    IslandId, PrincipalId,
};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, FactSourceError, FactSourceResult,
    classify_fact_key,
};
use p2panda_core::{Body, Hash, Header, Operation, PrivateKey, validate_operation};
use p2panda_store::{LogStore, MemoryStore};
use p2panda_stream::operation::{IngestError, IngestResult, ingest_operation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PandaFactError {
    #[error("p2panda ingest failed: {0}")]
    Ingest(#[from] IngestError),
    #[error("p2panda operation failed validation")]
    InvalidOperation,
    #[error("p2panda store failed: {message}")]
    Store { message: String },
    #[error("p2panda operation extensions were invalid: {message}")]
    InvalidExtensions { message: String },
    #[error("author principal {author} does not match session principal {session}")]
    PrincipalMismatch {
        session: PrincipalId,
        author: PrincipalId,
    },
    #[error("principal {principal} is not allowed to write fact {key} in island {island}")]
    UnauthorizedWrite {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
    },
}

pub type Result<T> = std::result::Result<T, PandaFactError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IslandLog(String);

impl From<&IslandId> for IslandLog {
    fn from(value: &IslandId) -> Self {
        Self(value.as_str().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PandaFactExtensions {
    island: String,
    key: String,
    author: String,
}

impl PandaFactExtensions {
    fn new(island: &IslandId, key: &FactKey, author: &PrincipalId) -> Self {
        Self {
            island: island.as_str().to_owned(),
            key: key.as_str().to_owned(),
            author: author.as_str().to_owned(),
        }
    }
}

pub struct PandaFactAuthor {
    principal: PrincipalId,
    key: PrivateKey,
}

impl PandaFactAuthor {
    #[must_use]
    pub fn new(principal: PrincipalId) -> Self {
        Self {
            principal,
            key: PrivateKey::new(),
        }
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    fn public_key(&self) -> p2panda_core::PublicKey {
        self.key.public_key()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandaFactMetadata {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    content_hash: FactContentHash,
}

impl PandaFactMetadata {
    fn new(
        island: IslandId,
        key: FactKey,
        author: PrincipalId,
        content_hash: FactContentHash,
    ) -> Self {
        Self {
            island,
            key,
            author,
            content_hash,
        }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PandaFactWriteOutcome {
    Inserted(PandaFactMetadata),
    AlreadyPresent(PandaFactMetadata),
    Conflict(PandaFactMetadata),
}

pub struct PandaFactStore {
    store: MemoryStore<IslandLog, PandaFactExtensions>,
    authorizer: Arc<dyn FactAuthorizer>,
    fact_index: BTreeMap<(IslandId, FactKey), BTreeSet<FactContentHash>>,
    operations: Vec<StoredFactOperation>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
}

impl PandaFactStore {
    #[must_use]
    pub fn new(authorizer: Arc<dyn FactAuthorizer>) -> Self {
        Self {
            store: MemoryStore::new(),
            authorizer,
            fact_index: BTreeMap::new(),
            operations: Vec::new(),
            payloads: BTreeMap::new(),
        }
    }

    pub async fn write_fact_payload(
        &mut self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWriteOutcome> {
        if session.principal() != author.principal() {
            return Err(PandaFactError::PrincipalMismatch {
                session: session.principal().clone(),
                author: author.principal().clone(),
            });
        }
        if !self
            .authorizer
            .can_session_access_fact(session, &key, FactAccess::Write)
        {
            return Err(PandaFactError::UnauthorizedWrite {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key,
            });
        }

        let body = Body::new(payload.as_bytes());
        let content_hash = content_hash_from_body(&body);
        let metadata = PandaFactMetadata::new(
            session.island().clone(),
            key.clone(),
            author.principal().clone(),
            content_hash.clone(),
        );
        let fact_index_key = (session.island().clone(), key.clone());
        let existing = self.fact_index.get(&fact_index_key);
        if existing.is_some_and(|hashes| hashes.contains(&content_hash)) {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let conflict = existing.is_some_and(|hashes| !hashes.is_empty());
        let log_position = self.next_log_position(session.island(), author).await?;

        let mut header = Header {
            version: 1,
            public_key: author.public_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: 0,
            seq_num: log_position.seq_num,
            backlink: log_position.backlink,
            previous: Vec::new(),
            extensions: PandaFactExtensions::new(session.island(), &key, author.principal()),
        };
        header.sign(&author.key);
        let header_bytes = header.to_bytes();
        let result = ingest_operation(
            &mut self.store,
            header,
            Some(body),
            header_bytes,
            &IslandLog::from(session.island()),
            false,
        )
        .await?;

        let IngestResult::Complete(operation) = result else {
            return Err(PandaFactError::InvalidOperation);
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let metadata = metadata_from_operation(&operation, content_hash)?;

        self.fact_index
            .entry((metadata.island.clone(), metadata.key.clone()))
            .or_default()
            .insert(metadata.content_hash.clone());
        self.payloads
            .entry(metadata.content_hash.clone())
            .or_insert(payload);
        self.operations
            .push(StoredFactOperation::from_metadata(metadata.clone()));
        if conflict {
            Ok(PandaFactWriteOutcome::Conflict(metadata))
        } else {
            Ok(PandaFactWriteOutcome::Inserted(metadata))
        }
    }

    async fn next_log_position(
        &self,
        island: &IslandId,
        author: &PandaFactAuthor,
    ) -> Result<LogPosition> {
        let latest = self
            .store
            .latest_operation(&author.public_key(), &IslandLog::from(island))
            .await
            .map_err(store_error)?;
        Ok(match latest {
            Some((header, _)) => LogPosition {
                seq_num: header.seq_num + 1,
                backlink: Some(header.hash()),
            },
            None => LogPosition {
                seq_num: 0,
                backlink: None,
            },
        })
    }
}

impl FactSource for PandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        let candidates = self
            .operations
            .iter()
            .filter_map(|stored| self.candidate_for(stored, island, pattern, session))
            .collect::<Vec<_>>();
        Ok(candidates)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        let wanted = candidates
            .iter()
            .filter(|candidate| candidate.island() == island)
            .filter(|candidate| {
                self.authorizer
                    .can_session_access_fact(session, candidate.key(), FactAccess::Read)
            })
            .map(FactCandidate::content_hash)
            .cloned()
            .collect::<BTreeSet<_>>();
        let payloads = self
            .operations
            .iter()
            .filter(|stored| stored.metadata.island == *island)
            .filter(|stored| wanted.contains(&stored.metadata.content_hash))
            .filter_map(|stored| {
                self.payloads
                    .get(&stored.metadata.content_hash)
                    .cloned()
                    .map(|payload| (stored.metadata.content_hash.clone(), payload))
            })
            .collect();
        Ok(payloads)
    }
}

impl PandaFactStore {
    fn candidate_for(
        &self,
        stored: &StoredFactOperation,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> Option<FactCandidate> {
        if !pattern.matches(&stored.metadata.key) {
            return None;
        }
        let status = if stored.metadata.island != *island {
            CandidateStatus::CrossIsland
        } else if !self.authorizer.can_session_access_fact(
            session,
            &stored.metadata.key,
            FactAccess::Read,
        ) {
            CandidateStatus::Unauthorized
        } else if self
            .fact_index
            .get(&(stored.metadata.island.clone(), stored.metadata.key.clone()))
            .is_some_and(|hashes| hashes.len() > 1)
        {
            CandidateStatus::Conflict
        } else {
            CandidateStatus::Verified
        };
        let classification = classify_fact_key(&stored.metadata.key);
        Some(FactCandidate::new(
            stored.metadata.island.clone(),
            stored.metadata.key.clone(),
            stored.metadata.author.clone(),
            stored.metadata.content_hash.clone(),
            classification.kind(),
            classification.epoch(),
            status,
        ))
    }
}

struct StoredFactOperation {
    metadata: PandaFactMetadata,
}

impl StoredFactOperation {
    fn from_metadata(metadata: PandaFactMetadata) -> Self {
        Self { metadata }
    }
}

struct LogPosition {
    seq_num: u64,
    backlink: Option<Hash>,
}

fn metadata_from_operation(
    operation: &Operation<PandaFactExtensions>,
    content_hash: FactContentHash,
) -> Result<PandaFactMetadata> {
    let key = FactKey::parse(operation.header.extensions.key.clone()).map_err(|error| {
        PandaFactError::InvalidExtensions {
            message: error.to_string(),
        }
    })?;
    Ok(PandaFactMetadata::new(
        IslandId::new(operation.header.extensions.island.clone()),
        key,
        PrincipalId::new(operation.header.extensions.author.clone()),
        content_hash,
    ))
}

fn content_hash_from_body(body: &Body) -> FactContentHash {
    FactContentHash::new(format!("b3:{}", body.hash().to_hex()))
}

fn store_error(error: impl Display) -> PandaFactError {
    PandaFactError::Store {
        message: error.to_string(),
    }
}

impl From<PandaFactError> for FactSourceError {
    fn from(error: PandaFactError) -> Self {
        Self::Unavailable {
            name: format!("p2panda fact store: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mvp_bus::{BusAuthority, Grant, harness::InMemoryBus};

    use super::*;

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact pattern parses")
    }

    fn principal(value: &str) -> PrincipalId {
        PrincipalId::new(value)
    }

    fn island(value: &str) -> IslandId {
        IslandId::new(value)
    }

    fn grant_prod(authority: &BusAuthority, principal: &str, grant: Grant) -> BusSession {
        authority.grant_in(island("prod"), PrincipalId::new(principal), grant)
    }

    fn store_with_authority() -> (PandaFactStore, BusAuthority) {
        let (bus, authority) = InMemoryBus::new_with_authority();
        (PandaFactStore::new(Arc::new(bus)), authority)
    }

    #[tokio::test]
    async fn authorized_write_returns_insert_duplicate_and_conflict_outcomes() {
        let (mut store, authority) = store_with_authority();
        let session = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let fact_key = key("/facts/node/node-1/joined/1");

        let inserted = store
            .write_fact_payload(
                &session,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"one"),
            )
            .await
            .expect("write first fact");
        assert!(matches!(inserted, PandaFactWriteOutcome::Inserted(_)));

        let duplicate = store
            .write_fact_payload(
                &session,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"one"),
            )
            .await
            .expect("write duplicate fact");
        assert!(matches!(
            duplicate,
            PandaFactWriteOutcome::AlreadyPresent(_)
        ));

        let conflict = store
            .write_fact_payload(
                &session,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"two"),
            )
            .await
            .expect("write conflicting fact");
        assert!(matches!(conflict, PandaFactWriteOutcome::Conflict(_)));

        let candidates = store
            .list_candidates(session.island(), &pattern("/facts/node/>"), &session)
            .expect("list candidates");
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.status() == CandidateStatus::Conflict)
        );
    }

    #[tokio::test]
    async fn unauthorized_write_is_rejected_before_operation_ingest() {
        let (mut store, authority) = store_with_authority();
        let session = grant_prod(&authority, "intruder", Grant::empty());
        let author = PandaFactAuthor::new(principal("intruder"));
        let fact_key = key("/facts/node/evil/joined/1");

        let error = store
            .write_fact_payload(
                &session,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect_err("unauthorized write fails");

        assert!(matches!(
            error,
            PandaFactError::UnauthorizedWrite { key, .. } if key == fact_key
        ));
        assert!(
            store
                .list_candidates(session.island(), &pattern("/facts/>"), &session)
                .expect("list candidates")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn author_principal_must_match_session_principal() {
        let (mut store, authority) = store_with_authority();
        let session = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let forged_author = PandaFactAuthor::new(principal("admin"));
        let fact_key = key("/facts/node/node-1/joined/1");

        let error = store
            .write_fact_payload(
                &session,
                &forged_author,
                fact_key,
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect_err("principal mismatch fails before authorization by name");

        assert!(matches!(
            error,
            PandaFactError::PrincipalMismatch { session, author }
                if session == principal("writer") && author == principal("admin")
        ));
    }

    #[tokio::test]
    async fn read_payloads_respects_session_read_grants() {
        let (mut store, authority) = store_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/public/>")),
        );
        let reader = grant_prod(
            &authority,
            "reader",
            Grant::empty().with_fact_read(pattern("/facts/public/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        store
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/public/node-1"),
                FactPayload::from_static(b"public"),
            )
            .await
            .expect("write public fact");
        store
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/private/node-1"),
                FactPayload::from_static(b"private"),
            )
            .await
            .expect("write private fact");

        let candidates = store
            .list_candidates(writer.island(), &pattern("/facts/>"), &reader)
            .expect("list candidates");
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.status() == CandidateStatus::Unauthorized)
                .count(),
            1
        );
        let payloads = store
            .read_payloads(writer.island(), &candidates, &reader)
            .expect("read payloads");
        assert_eq!(payloads.len(), 1);
        assert!(
            payloads
                .values()
                .any(|payload| payload.as_bytes() == b"public")
        );
    }
}
