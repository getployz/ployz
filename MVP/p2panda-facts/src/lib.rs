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
use p2panda_core::cbor::decode_cbor;
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
    #[error("cannot import {operation} operation through {session} island session")]
    ImportIslandMismatch {
        session: IslandId,
        operation: IslandId,
    },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandaFactOperation {
    header: Arc<[u8]>,
    body: Arc<[u8]>,
}

impl PandaFactOperation {
    fn new(header: Vec<u8>, body: Vec<u8>) -> Self {
        Self {
            header: header.into(),
            body: body.into(),
        }
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn body(&self) -> &[u8] {
        &self.body
    }

    fn header_bytes(&self) -> Vec<u8> {
        self.header.to_vec()
    }
}

pub struct PandaFactStore {
    store: MemoryStore<IslandLog, PandaFactExtensions>,
    authorizer: Arc<dyn FactAuthorizer>,
    fact_index: BTreeMap<(IslandId, FactKey), BTreeSet<FactContentHash>>,
    operations: Vec<StoredFactOperation>,
    operation_by_hash: BTreeMap<FactContentHash, usize>,
}

impl PandaFactStore {
    #[must_use]
    pub fn new(authorizer: Arc<dyn FactAuthorizer>) -> Self {
        Self {
            store: MemoryStore::new(),
            authorizer,
            fact_index: BTreeMap::new(),
            operations: Vec::new(),
            operation_by_hash: BTreeMap::new(),
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
        let pre_ingest = self.pre_ingest_status(&metadata);
        if pre_ingest == FactPreIngestStatus::AlreadyPresent {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let log_position = self.next_log_position(session.island(), author).await?;
        let raw_body = payload.as_bytes().to_vec();

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
        let raw_operation = PandaFactOperation::new(header_bytes.clone(), raw_body);
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

        Ok(self.record_fact_operation(metadata, raw_operation, pre_ingest))
    }

    pub fn export_operations(&self) -> impl Iterator<Item = &PandaFactOperation> {
        self.operations.iter().map(|stored| &stored.raw)
    }

    pub async fn import_operation(
        &mut self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        let header: Header<PandaFactExtensions> =
            decode_cbor(operation.header()).map_err(|error| PandaFactError::InvalidExtensions {
                message: error.to_string(),
            })?;
        let body = Body::new(operation.body());
        let content_hash = content_hash_from_body(&body);
        let metadata = metadata_from_header(&header, content_hash)?;
        self.authorize_import(session, &metadata)?;

        let pre_ingest = self.pre_ingest_status(&metadata);
        if pre_ingest == FactPreIngestStatus::AlreadyPresent {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let result = ingest_operation(
            &mut self.store,
            header,
            Some(body),
            operation.header_bytes(),
            &IslandLog::from(&metadata.island),
            false,
        )
        .await?;
        let IngestResult::Complete(imported) = result else {
            return Err(PandaFactError::InvalidOperation);
        };
        validate_operation(&imported).map_err(|_| PandaFactError::InvalidOperation)?;

        Ok(self.record_fact_operation(metadata, operation.clone(), pre_ingest))
    }

    fn authorize_import(&self, session: &BusSession, metadata: &PandaFactMetadata) -> Result<()> {
        if session.island() != &metadata.island {
            return Err(PandaFactError::ImportIslandMismatch {
                session: session.island().clone(),
                operation: metadata.island.clone(),
            });
        }
        if !self.authorizer.can_principal_access_fact(
            &metadata.island,
            &metadata.author,
            &metadata.key,
            FactAccess::Write,
        ) {
            return Err(PandaFactError::UnauthorizedWrite {
                island: metadata.island.clone(),
                principal: metadata.author.clone(),
                key: metadata.key.clone(),
            });
        }
        Ok(())
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

    fn pre_ingest_status(&self, metadata: &PandaFactMetadata) -> FactPreIngestStatus {
        let Some(existing) = self
            .fact_index
            .get(&(metadata.island.clone(), metadata.key.clone()))
        else {
            return FactPreIngestStatus::Inserted;
        };
        if existing.contains(&metadata.content_hash) {
            FactPreIngestStatus::AlreadyPresent
        } else {
            FactPreIngestStatus::Conflict
        }
    }

    fn record_fact_operation(
        &mut self,
        metadata: PandaFactMetadata,
        operation: PandaFactOperation,
        status: FactPreIngestStatus,
    ) -> PandaFactWriteOutcome {
        self.fact_index
            .entry((metadata.island.clone(), metadata.key.clone()))
            .or_default()
            .insert(metadata.content_hash.clone());
        self.operation_by_hash
            .entry(metadata.content_hash.clone())
            .or_insert(self.operations.len());
        self.operations
            .push(StoredFactOperation::new(metadata.clone(), operation));
        match status {
            FactPreIngestStatus::Inserted => PandaFactWriteOutcome::Inserted(metadata),
            FactPreIngestStatus::Conflict => PandaFactWriteOutcome::Conflict(metadata),
            FactPreIngestStatus::AlreadyPresent => PandaFactWriteOutcome::AlreadyPresent(metadata),
        }
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
        let payloads = wanted
            .into_iter()
            .filter_map(|hash| {
                let operation = self
                    .operation_by_hash
                    .get(&hash)
                    .and_then(|index| self.operations.get(*index))?;
                Some((hash, FactPayload::from(operation.raw.body().to_vec())))
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
    raw: PandaFactOperation,
}

impl StoredFactOperation {
    fn new(metadata: PandaFactMetadata, raw: PandaFactOperation) -> Self {
        Self { metadata, raw }
    }
}

struct LogPosition {
    seq_num: u64,
    backlink: Option<Hash>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FactPreIngestStatus {
    AlreadyPresent,
    Inserted,
    Conflict,
}

fn metadata_from_operation(
    operation: &Operation<PandaFactExtensions>,
    content_hash: FactContentHash,
) -> Result<PandaFactMetadata> {
    metadata_from_header(&operation.header, content_hash)
}

fn metadata_from_header(
    header: &Header<PandaFactExtensions>,
    content_hash: FactContentHash,
) -> Result<PandaFactMetadata> {
    let key = FactKey::parse(header.extensions.key.clone()).map_err(|error| {
        PandaFactError::InvalidExtensions {
            message: error.to_string(),
        }
    })?;
    Ok(PandaFactMetadata::new(
        IslandId::new(header.extensions.island.clone()),
        key,
        PrincipalId::new(header.extensions.author.clone()),
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

    fn store_from_bus(bus: InMemoryBus) -> PandaFactStore {
        PandaFactStore::new(Arc::new(bus))
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

    #[tokio::test]
    async fn exported_operations_import_into_an_empty_store() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let mut source = store_from_bus(bus.clone());
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let projection = grant_prod(
            &authority,
            "projection",
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let fact_key = key("/facts/node/node-1/joined/1");
        source
            .write_fact_payload(
                &writer,
                &author,
                fact_key,
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write source fact");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();

        let mut imported = store_from_bus(bus);
        let outcome = imported
            .import_operation(&projection, &exported[0])
            .await
            .expect("import exported operation");
        assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));

        let candidates = imported
            .list_candidates(projection.island(), &pattern("/facts/node/>"), &projection)
            .expect("list imported candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status(), CandidateStatus::Verified);
        let payloads = imported
            .read_payloads(projection.island(), &candidates, &projection)
            .expect("read imported payloads");
        assert!(
            payloads
                .values()
                .any(|payload| payload.as_bytes() == b"payload")
        );
    }

    #[tokio::test]
    async fn imported_operations_still_enforce_reader_permissions() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let mut source = store_from_bus(bus.clone());
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let blind_reader = grant_prod(&authority, "blind-reader", Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        source
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/node-1/joined/1"),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write source fact");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();
        let [exported] = exported.as_slice() else {
            panic!("expected one exported operation");
        };

        let mut imported = store_from_bus(bus);
        let outcome = imported
            .import_operation(&blind_reader, exported)
            .await
            .expect("import operation through same-island session");
        assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));

        let candidates = imported
            .list_candidates(
                blind_reader.island(),
                &pattern("/facts/node/>"),
                &blind_reader,
            )
            .expect("list imported candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status(), CandidateStatus::Unauthorized);
        let payloads = imported
            .read_payloads(blind_reader.island(), &candidates, &blind_reader)
            .expect("read imported payloads");
        assert!(payloads.is_empty());
    }

    #[tokio::test]
    async fn importing_duplicates_and_conflicts_preserves_immutable_fact_semantics() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let mut source = store_from_bus(bus.clone());
        let session = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let fact_key = key("/facts/node/node-1/joined/1");
        source
            .write_fact_payload(
                &session,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"one"),
            )
            .await
            .expect("write first source fact");
        source
            .write_fact_payload(
                &session,
                &author,
                fact_key,
                FactPayload::from_static(b"two"),
            )
            .await
            .expect("write conflicting source fact");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();

        let mut imported = store_from_bus(bus);
        assert!(matches!(
            imported
                .import_operation(&session, &exported[0])
                .await
                .expect("import first operation"),
            PandaFactWriteOutcome::Inserted(_)
        ));
        assert!(matches!(
            imported
                .import_operation(&session, &exported[0])
                .await
                .expect("import duplicate operation"),
            PandaFactWriteOutcome::AlreadyPresent(_)
        ));
        assert!(matches!(
            imported
                .import_operation(&session, &exported[1])
                .await
                .expect("import conflict operation"),
            PandaFactWriteOutcome::Conflict(_)
        ));

        let candidates = imported
            .list_candidates(session.island(), &pattern("/facts/node/>"), &session)
            .expect("list imported conflict candidates");
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.status() == CandidateStatus::Conflict)
        );
    }
}
