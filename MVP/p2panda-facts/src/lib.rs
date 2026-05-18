use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};
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
use p2panda_core::{
    Body, Hash, Header, Operation, PrivateKey, PublicKey, RawOperation, validate_operation,
};
use p2panda_store::sqlite::store::{
    SqliteStore, connection_pool, create_database, run_pending_migrations,
};
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
    #[error("p2panda persistent store path {path} is invalid: {message}")]
    InvalidStorePath { path: PathBuf, message: String },
    #[error("p2panda persistent operation for fact {key} is missing payload bytes")]
    MissingPayload { key: FactKey },
    #[error("p2panda operation extensions were invalid: {message}")]
    InvalidExtensions { message: String },
    #[error("cannot import {operation} operation through {session} island session")]
    ImportIslandMismatch {
        session: IslandId,
        operation: IslandId,
    },
    #[error("principal {principal} in island {island} has no trusted p2panda author key")]
    UntrustedAuthorKey {
        island: IslandId,
        principal: PrincipalId,
    },
    #[error(
        "principal {principal} in island {island} used a p2panda author key that is not trusted"
    )]
    AuthorKeyMismatch {
        island: IslandId,
        principal: PrincipalId,
    },
    #[error(
        "p2panda operation for fact {key} by {principal} in island {island} is missing {missing_operations} predecessor operations"
    )]
    OutOfOrderOperation {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
        missing_operations: u64,
    },
    #[error("p2panda operation for fact {key} by {principal} in island {island} is outdated")]
    OutdatedOperation {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
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

    #[must_use]
    pub fn author_key(&self) -> PandaFactAuthorKey {
        PandaFactAuthorKey(self.public_key())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PandaFactAuthorKey(PublicKey);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandaTrustedAuthorKey {
    island: IslandId,
    principal: PrincipalId,
    author_key: PandaFactAuthorKey,
}

impl PandaTrustedAuthorKey {
    #[must_use]
    pub fn new(island: IslandId, principal: PrincipalId, author_key: PandaFactAuthorKey) -> Self {
        Self {
            island,
            principal,
            author_key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandaSqliteOpenConfig {
    path: PathBuf,
    known_islands: Vec<IslandId>,
    trusted_author_keys: Vec<PandaTrustedAuthorKey>,
    max_connections: u32,
}

impl PandaSqliteOpenConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, known_islands: Vec<IslandId>) -> Self {
        Self {
            path: path.into(),
            known_islands,
            trusted_author_keys: Vec::new(),
            max_connections: 1,
        }
    }

    #[must_use]
    pub fn with_trusted_author_key(mut self, trusted: PandaTrustedAuthorKey) -> Self {
        self.trusted_author_keys.push(trusted);
        self
    }

    #[must_use]
    pub fn with_max_connections(mut self, max_connections: u32) -> Self {
        self.max_connections = max_connections.max(1);
        self
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
    backend: PandaFactBackend,
    authorizer: Arc<dyn FactAuthorizer>,
    fact_index: BTreeMap<(IslandId, FactKey), BTreeSet<FactContentHash>>,
    operations: Vec<PandaFactOperation>,
    operation_hashes: BTreeSet<Hash>,
    facts: Vec<StoredFactOperation>,
    facts_by_identity: BTreeMap<StoredFactIdentity, usize>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
    trusted_author_keys: BTreeMap<(IslandId, PrincipalId), PublicKey>,
}

enum PandaFactBackend {
    Memory(MemoryStore<IslandLog, PandaFactExtensions>),
    Sqlite(SqliteStore<IslandLog, PandaFactExtensions>),
}

impl PandaFactBackend {
    async fn ingest_operation(
        &mut self,
        header: Header<PandaFactExtensions>,
        body: Option<Body>,
        header_bytes: Vec<u8>,
        log_id: &IslandLog,
    ) -> Result<IngestResult<PandaFactExtensions>> {
        match self {
            Self::Memory(store) => {
                ingest_operation(store, header, body, header_bytes, log_id, false)
                    .await
                    .map_err(PandaFactError::Ingest)
            }
            Self::Sqlite(store) => {
                ingest_operation(store, header, body, header_bytes, log_id, false)
                    .await
                    .map_err(PandaFactError::Ingest)
            }
        }
    }

    async fn latest_operation(
        &self,
        public_key: &PublicKey,
        log_id: &IslandLog,
    ) -> Result<Option<(Header<PandaFactExtensions>, Option<Body>)>> {
        match self {
            Self::Memory(store) => store
                .latest_operation(public_key, log_id)
                .await
                .map_err(store_error),
            Self::Sqlite(store) => store
                .latest_operation(public_key, log_id)
                .await
                .map_err(store_error),
        }
    }

    async fn get_log_heights(&self, log_id: &IslandLog) -> Result<Vec<(PublicKey, u64)>> {
        match self {
            Self::Memory(store) => store.get_log_heights(log_id).await.map_err(store_error),
            Self::Sqlite(store) => store.get_log_heights(log_id).await.map_err(store_error),
        }
    }

    async fn get_raw_log(
        &self,
        public_key: &PublicKey,
        log_id: &IslandLog,
    ) -> Result<Option<Vec<RawOperation>>> {
        match self {
            Self::Memory(store) => store
                .get_raw_log(public_key, log_id, None)
                .await
                .map_err(store_error),
            Self::Sqlite(store) => store
                .get_raw_log(public_key, log_id, None)
                .await
                .map_err(store_error),
        }
    }
}

impl PandaFactStore {
    #[must_use]
    pub fn new(authorizer: Arc<dyn FactAuthorizer>) -> Self {
        Self {
            backend: PandaFactBackend::Memory(MemoryStore::new()),
            authorizer,
            fact_index: BTreeMap::new(),
            operations: Vec::new(),
            operation_hashes: BTreeSet::new(),
            facts: Vec::new(),
            facts_by_identity: BTreeMap::new(),
            payloads: BTreeMap::new(),
            trusted_author_keys: BTreeMap::new(),
        }
    }

    pub async fn open_sqlite(
        authorizer: Arc<dyn FactAuthorizer>,
        config: PandaSqliteOpenConfig,
    ) -> Result<Self> {
        prepare_sqlite_parent(&config.path)?;
        let url = sqlite_url(&config.path);
        create_database(&url).await.map_err(store_error)?;
        let pool = connection_pool(&url, config.max_connections)
            .await
            .map_err(store_error)?;
        run_pending_migrations(&pool).await.map_err(store_error)?;
        let mut store = Self {
            backend: PandaFactBackend::Sqlite(SqliteStore::new(pool)),
            authorizer,
            fact_index: BTreeMap::new(),
            operations: Vec::new(),
            operation_hashes: BTreeSet::new(),
            facts: Vec::new(),
            facts_by_identity: BTreeMap::new(),
            payloads: BTreeMap::new(),
            trusted_author_keys: BTreeMap::new(),
        };
        for trusted in config.trusted_author_keys {
            store.bind_author_key(&trusted.island, &trusted.principal, trusted.author_key.0)?;
        }
        store.rebuild_indexes(&config.known_islands).await?;
        Ok(store)
    }

    pub fn trust_author_key(
        &mut self,
        island: &IslandId,
        principal: PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> Result<()> {
        self.bind_author_key(island, &principal, author_key.0)
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
        self.bind_author_key(session.island(), author.principal(), author.public_key())?;

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
        let result = self
            .backend
            .ingest_operation(
                header,
                Some(body),
                header_bytes,
                &IslandLog::from(session.island()),
            )
            .await?;

        let IngestResult::Complete(operation) = result else {
            return Err(PandaFactError::InvalidOperation);
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let metadata = metadata_from_operation(&operation, content_hash)?;

        self.record_operation(operation.hash, raw_operation);
        Ok(self.record_fact_operation(metadata, payload, pre_ingest))
    }

    pub fn export_operations(&self) -> impl Iterator<Item = &PandaFactOperation> {
        self.operations.iter()
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
        let operation_hash = header.hash();
        let public_key = header.public_key;
        self.authorize_import(session, &metadata, public_key)?;
        if self.operation_hashes.contains(&operation_hash) {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let result = self
            .backend
            .ingest_operation(
                header,
                Some(body),
                operation.header_bytes(),
                &IslandLog::from(&metadata.island),
            )
            .await?;
        let imported = match result {
            IngestResult::Complete(imported) => imported,
            IngestResult::Retry(_, _, _, missing_operations) => {
                return Err(PandaFactError::OutOfOrderOperation {
                    island: metadata.island,
                    principal: metadata.author,
                    key: metadata.key,
                    missing_operations,
                });
            }
            IngestResult::Outdated(_) => {
                return Err(PandaFactError::OutdatedOperation {
                    island: metadata.island,
                    principal: metadata.author,
                    key: metadata.key,
                });
            }
        };
        validate_operation(&imported).map_err(|_| PandaFactError::InvalidOperation)?;

        let pre_ingest = self.pre_ingest_status(&metadata);
        self.record_operation(operation_hash, operation.clone());
        Ok(self.record_fact_operation(
            metadata,
            FactPayload::from(operation.body().to_vec()),
            pre_ingest,
        ))
    }

    fn authorize_import(
        &self,
        session: &BusSession,
        metadata: &PandaFactMetadata,
        public_key: PublicKey,
    ) -> Result<()> {
        if session.island() != &metadata.island {
            return Err(PandaFactError::ImportIslandMismatch {
                session: session.island().clone(),
                operation: metadata.island.clone(),
            });
        }
        self.require_author_key(&metadata.island, &metadata.author, public_key)?;
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

    fn bind_author_key(
        &mut self,
        island: &IslandId,
        principal: &PrincipalId,
        public_key: PublicKey,
    ) -> Result<()> {
        match self
            .trusted_author_keys
            .get(&(island.clone(), principal.clone()))
        {
            Some(existing) if *existing == public_key => Ok(()),
            Some(_) => Err(PandaFactError::AuthorKeyMismatch {
                island: island.clone(),
                principal: principal.clone(),
            }),
            None => {
                self.trusted_author_keys
                    .insert((island.clone(), principal.clone()), public_key);
                Ok(())
            }
        }
    }

    fn require_author_key(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        public_key: PublicKey,
    ) -> Result<()> {
        match self
            .trusted_author_keys
            .get(&(island.clone(), principal.clone()))
        {
            Some(existing) if *existing == public_key => Ok(()),
            Some(_) => Err(PandaFactError::AuthorKeyMismatch {
                island: island.clone(),
                principal: principal.clone(),
            }),
            None => Err(PandaFactError::UntrustedAuthorKey {
                island: island.clone(),
                principal: principal.clone(),
            }),
        }
    }

    async fn next_log_position(
        &self,
        island: &IslandId,
        author: &PandaFactAuthor,
    ) -> Result<LogPosition> {
        let latest = self
            .backend
            .latest_operation(&author.public_key(), &IslandLog::from(island))
            .await?;
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

    async fn rebuild_indexes(&mut self, islands: &[IslandId]) -> Result<()> {
        self.clear_derived_indexes();
        for island in islands {
            let log_id = IslandLog::from(island);
            for (public_key, _height) in self.backend.get_log_heights(&log_id).await? {
                let Some(raw_operations) = self.backend.get_raw_log(&public_key, &log_id).await?
                else {
                    continue;
                };
                for raw_operation in raw_operations {
                    self.record_rebuilt_operation(raw_operation)?;
                }
            }
        }
        Ok(())
    }

    fn clear_derived_indexes(&mut self) {
        self.fact_index.clear();
        self.operations.clear();
        self.operation_hashes.clear();
        self.facts.clear();
        self.facts_by_identity.clear();
        self.payloads.clear();
    }

    fn record_rebuilt_operation(&mut self, raw_operation: RawOperation) -> Result<()> {
        let (header_bytes, body_bytes) = raw_operation;
        let header: Header<PandaFactExtensions> =
            decode_cbor(header_bytes.as_slice()).map_err(|error| {
                PandaFactError::InvalidExtensions {
                    message: error.to_string(),
                }
            })?;
        let missing_payload_key =
            FactKey::parse(header.extensions.key.clone()).map_err(|error| {
                PandaFactError::InvalidExtensions {
                    message: error.to_string(),
                }
            })?;
        let body_bytes = body_bytes.ok_or(PandaFactError::MissingPayload {
            key: missing_payload_key,
        })?;
        let body = Body::new(&body_bytes);
        let content_hash = content_hash_from_body(&body);
        let metadata = metadata_from_header(&header, content_hash)?;
        self.require_author_key(&metadata.island, &metadata.author, header.public_key)?;
        let operation = Operation {
            hash: header.hash(),
            header,
            body: Some(body),
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let pre_ingest = self.pre_ingest_status(&metadata);
        self.record_operation(
            operation.hash,
            PandaFactOperation::new(header_bytes, body_bytes.clone()),
        );
        self.record_fact_operation(metadata, FactPayload::from(body_bytes), pre_ingest);
        Ok(())
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

    fn record_operation(&mut self, operation_hash: Hash, operation: PandaFactOperation) {
        if self.operation_hashes.insert(operation_hash) {
            self.operations.push(operation);
        }
    }

    fn record_fact_operation(
        &mut self,
        metadata: PandaFactMetadata,
        payload: FactPayload,
        status: FactPreIngestStatus,
    ) -> PandaFactWriteOutcome {
        if status == FactPreIngestStatus::AlreadyPresent {
            return PandaFactWriteOutcome::AlreadyPresent(metadata);
        }
        self.fact_index
            .entry((metadata.island.clone(), metadata.key.clone()))
            .or_default()
            .insert(metadata.content_hash.clone());
        self.payloads
            .entry(metadata.content_hash.clone())
            .or_insert(payload);
        let identity = StoredFactIdentity::from_metadata(&metadata);
        self.facts_by_identity.entry(identity).or_insert_with(|| {
            let index = self.facts.len();
            self.facts.push(StoredFactOperation::new(metadata.clone()));
            index
        });
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
            .facts
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
        let mut payloads = BTreeMap::new();
        for candidate in candidates {
            if candidate.island() != island {
                continue;
            }
            let identity = StoredFactIdentity::from_candidate(candidate);
            let Some(stored) = self
                .facts_by_identity
                .get(&identity)
                .and_then(|index| self.facts.get(*index))
            else {
                continue;
            };
            if !self.authorizer.can_session_access_fact(
                session,
                &stored.metadata.key,
                FactAccess::Read,
            ) || !self.authorizer.can_principal_access_fact(
                &stored.metadata.island,
                &stored.metadata.author,
                &stored.metadata.key,
                FactAccess::Write,
            ) {
                continue;
            }
            if let Some(payload) = self.payloads.get(&stored.metadata.content_hash) {
                payloads.insert(stored.metadata.content_hash.clone(), payload.clone());
            }
        }
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
        } else if !self.authorizer.can_principal_access_fact(
            &stored.metadata.island,
            &stored.metadata.author,
            &stored.metadata.key,
            FactAccess::Write,
        ) {
            CandidateStatus::Unverified
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
    fn new(metadata: PandaFactMetadata) -> Self {
        Self { metadata }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StoredFactIdentity {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    content_hash: FactContentHash,
}

impl StoredFactIdentity {
    fn from_metadata(metadata: &PandaFactMetadata) -> Self {
        Self {
            island: metadata.island.clone(),
            key: metadata.key.clone(),
            author: metadata.author.clone(),
            content_hash: metadata.content_hash.clone(),
        }
    }

    fn from_candidate(candidate: &FactCandidate) -> Self {
        Self {
            island: candidate.island().clone(),
            key: candidate.key().clone(),
            author: candidate.author().clone(),
            content_hash: candidate.content_hash().clone(),
        }
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

fn prepare_sqlite_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| PandaFactError::InvalidStorePath {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    Ok(())
}

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
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
    use tempfile::tempdir;

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

    fn sqlite_config(
        path: impl Into<PathBuf>,
        island: &IslandId,
        author: &PandaFactAuthor,
    ) -> PandaSqliteOpenConfig {
        PandaSqliteOpenConfig::new(path, vec![island.clone()]).with_trusted_author_key(
            PandaTrustedAuthorKey::new(
                island.clone(),
                author.principal().clone(),
                author.author_key(),
            ),
        )
    }

    fn trust_author(store: &mut PandaFactStore, session: &BusSession, author: &PandaFactAuthor) {
        store
            .trust_author_key(
                session.island(),
                author.principal().clone(),
                author.author_key(),
            )
            .expect("trust p2panda author key");
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
    async fn read_payloads_rejects_forged_candidate_metadata() {
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
                key("/facts/private/node-1"),
                FactPayload::from_static(b"private"),
            )
            .await
            .expect("write private fact");
        let private_candidate = store
            .list_candidates(writer.island(), &pattern("/facts/>"), &writer)
            .expect("list private candidate")
            .into_iter()
            .find(|candidate| candidate.key() == &key("/facts/private/node-1"))
            .expect("private candidate exists");
        let forged = FactCandidate::new(
            writer.island().clone(),
            key("/facts/public/forged"),
            principal("writer"),
            private_candidate.content_hash().clone(),
            mvp_projection::FactKind::Unsupported,
            0,
            CandidateStatus::Verified,
        );

        let payloads = store
            .read_payloads(reader.island(), &[forged], &reader)
            .expect("read payloads");
        assert!(payloads.is_empty());
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
        trust_author(&mut imported, &writer, &author);
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
    async fn sqlite_store_reopens_and_rebuilds_fact_indexes() {
        let directory = tempdir().expect("create tempdir");
        let path = directory.path().join("facts.sqlite");
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let fact_key = key("/facts/node/node-1/joined/1");
        let mut store = PandaFactStore::open_sqlite(
            Arc::new(bus.clone()),
            PandaSqliteOpenConfig::new(&path, vec![writer.island().clone()]),
        )
        .await
        .expect("open sqlite store");
        store
            .write_fact_payload(
                &writer,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write persistent fact");
        drop(store);

        let mut reopened = PandaFactStore::open_sqlite(
            Arc::new(bus),
            sqlite_config(&path, writer.island(), &author),
        )
        .await
        .expect("reopen sqlite store");
        let candidates = reopened
            .list_candidates(writer.island(), &pattern("/facts/node/>"), &writer)
            .expect("list reopened candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status(), CandidateStatus::Verified);
        let payloads = reopened
            .read_payloads(writer.island(), &candidates, &writer)
            .expect("read reopened payload");
        assert!(
            payloads
                .values()
                .any(|payload| payload.as_bytes() == b"payload")
        );

        let duplicate = reopened
            .write_fact_payload(
                &writer,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write duplicate after reopen");
        assert!(matches!(
            duplicate,
            PandaFactWriteOutcome::AlreadyPresent(_)
        ));

        let conflict = reopened
            .write_fact_payload(
                &writer,
                &author,
                fact_key,
                FactPayload::from_static(b"conflict"),
            )
            .await
            .expect("write conflict after reopen");
        assert!(matches!(conflict, PandaFactWriteOutcome::Conflict(_)));
    }

    #[tokio::test]
    async fn sqlite_reopen_requires_trusted_author_keys_for_stored_operations() {
        let directory = tempdir().expect("create tempdir");
        let path = directory.path().join("facts.sqlite");
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let mut store = PandaFactStore::open_sqlite(
            Arc::new(bus.clone()),
            PandaSqliteOpenConfig::new(&path, vec![writer.island().clone()]),
        )
        .await
        .expect("open sqlite store");
        store
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/node-1/joined/1"),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write persistent fact");
        drop(store);

        let error = match PandaFactStore::open_sqlite(
            Arc::new(bus),
            PandaSqliteOpenConfig::new(&path, vec![writer.island().clone()]),
        )
        .await
        {
            Ok(_) => panic!("reopen without trusted author key fails"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            PandaFactError::UntrustedAuthorKey { principal, .. } if principal == PrincipalId::new("writer")
        ));
    }

    #[tokio::test]
    async fn import_rejects_cross_island_untrusted_and_revoked_authors() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let mut source = store_from_bus(bus.clone());
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let laptop = authority.grant_in(
            island("laptop"),
            principal("projection"),
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let fact_key = key("/facts/node/node-1/joined/1");
        source
            .write_fact_payload(
                &writer,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write source fact");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();
        let [operation] = exported.as_slice() else {
            panic!("expected one exported operation");
        };

        let mut imported = store_from_bus(bus.clone());
        trust_author(&mut imported, &writer, &author);
        let cross_island = imported
            .import_operation(&laptop, operation)
            .await
            .expect_err("cross-island import fails");
        assert!(matches!(
            cross_island,
            PandaFactError::ImportIslandMismatch { .. }
        ));

        let mut untrusted = store_from_bus(bus.clone());
        let missing_key = untrusted
            .import_operation(&writer, operation)
            .await
            .expect_err("untrusted import fails");
        assert!(matches!(
            missing_key,
            PandaFactError::UntrustedAuthorKey { .. }
        ));

        authority.revoke(&writer);
        let mut revoked = store_from_bus(bus);
        trust_author(&mut revoked, &writer, &author);
        let revoked_author = revoked
            .import_operation(&writer, operation)
            .await
            .expect_err("revoked author import fails");
        assert!(matches!(
            revoked_author,
            PandaFactError::UnauthorizedWrite { key, .. } if key == fact_key
        ));
    }

    #[tokio::test]
    async fn import_rejects_operation_signed_by_untrusted_key_for_claimed_author() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let trusted_author = PandaFactAuthor::new(principal("writer"));
        let forged_author = PandaFactAuthor::new(principal("writer"));
        let mut source = store_from_bus(bus.clone());
        source
            .write_fact_payload(
                &writer,
                &forged_author,
                key("/facts/node/node-1/joined/1"),
                FactPayload::from_static(b"payload"),
            )
            .await
            .expect("write forged-key source fact");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();

        let mut imported = store_from_bus(bus);
        trust_author(&mut imported, &writer, &trusted_author);
        let error = imported
            .import_operation(&writer, &exported[0])
            .await
            .expect_err("mismatched author key fails");
        assert!(matches!(error, PandaFactError::AuthorKeyMismatch { .. }));
    }

    #[tokio::test]
    async fn import_reports_out_of_order_operations_without_calling_them_invalid() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let mut source = store_from_bus(bus.clone());
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        source
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/node-1/joined/1"),
                FactPayload::from_static(b"one"),
            )
            .await
            .expect("write first operation");
        source
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/node-2/joined/1"),
                FactPayload::from_static(b"two"),
            )
            .await
            .expect("write second operation");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();
        let [first, second] = exported.as_slice() else {
            panic!("expected two exported operations");
        };

        let mut imported = store_from_bus(bus);
        trust_author(&mut imported, &writer, &author);
        let retry = imported
            .import_operation(&writer, second)
            .await
            .expect_err("second operation is out of order");
        assert!(matches!(
            retry,
            PandaFactError::OutOfOrderOperation {
                missing_operations: 1,
                ..
            }
        ));
        assert!(matches!(
            imported
                .import_operation(&writer, first)
                .await
                .expect("import first operation"),
            PandaFactWriteOutcome::Inserted(_)
        ));
        assert!(matches!(
            imported
                .import_operation(&writer, second)
                .await
                .expect("retry second operation after predecessor"),
            PandaFactWriteOutcome::Inserted(_)
        ));
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
        trust_author(&mut imported, &writer, &author);
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
        trust_author(&mut imported, &session, &author);
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
