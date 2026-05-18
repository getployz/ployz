use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt::Display;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use futures::{StreamExt, channel::mpsc};
use mvp_bus::{
    BusSession, FactAccess, FactAuthorizer, FactContentHash, FactKey, FactKeyPattern, FactPayload,
    IslandId, PrincipalId,
};
use mvp_p2panda_authz::{IslandAuthoritySnapshot, IslandMemberAuthorKey, IslandMemberEpoch};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, FactSourceError, FactSourceResult,
    classify_fact_key,
};
use p2panda_core::cbor::{decode_cbor, encode_cbor};
use p2panda_core::{
    Body, Hash, Header, Operation, OperationError, RawOperation, SigningKey, Topic, VerifyingKey,
    validate_operation,
};
use p2panda_store::logs::LogStore;
use p2panda_store::topics::TopicStore;
use p2panda_store::{SqliteStore, SqliteStoreBuilder, Transaction};
use p2panda_stream::ingest::{IngestError, ingest_operation};
use p2panda_sync::protocols::{
    LogSync, LogSyncError, LogSyncEvent, LogSyncMessage, LogSyncMetrics, Logs,
};
use p2panda_sync::traits::Protocol;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, broadcast};

const LOG_SYNC_MESSAGE_CAPACITY: usize = 128;
const LOG_SYNC_EVENT_CAPACITY: usize = 1_024;

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
    #[error("p2panda author key is invalid: {message}")]
    InvalidAuthorKey { message: String },
    #[error("p2panda author private key is invalid: {message}")]
    InvalidAuthorPrivateKey { message: String },
    #[error("p2panda operation extensions were invalid: {message}")]
    InvalidExtensions { message: String },
    #[error("cannot import {operation} operation through {session} island session")]
    ImportIslandMismatch {
        session: IslandId,
        operation: IslandId,
    },
    #[error("principal {principal} is not a trusted replica importer for island {island}")]
    UnauthorizedReplicaImport {
        island: IslandId,
        principal: PrincipalId,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PandaFactSyncSide {
    Left,
    Right,
}

impl Display for PandaFactSyncSide {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Left => formatter.write_str("left"),
            Self::Right => formatter.write_str("right"),
        }
    }
}

#[derive(Debug, Error)]
pub enum PandaFactSyncError {
    #[error("sync {side} replica session is for island {session}, but scope is for {scope}")]
    ReplicaIslandMismatch {
        side: PandaFactSyncSide,
        session: IslandId,
        scope: IslandId,
    },
    #[error("sync {side} principal {principal} is not a trusted replica for island {island}")]
    UnauthorizedReplica {
        side: PandaFactSyncSide,
        island: IslandId,
        principal: PrincipalId,
    },
    #[error("sync {side} scope principal {principal} has no trusted author key in island {island}")]
    ScopeAuthorKeyMissing {
        side: PandaFactSyncSide,
        island: IslandId,
        principal: PrincipalId,
    },
    #[error(
        "sync {side} scope principal {principal} has a mismatched author key in island {island}"
    )]
    ScopeAuthorKeyMismatch {
        side: PandaFactSyncSide,
        island: IslandId,
        principal: PrincipalId,
    },
    #[error("sync {side} protocol failed: {source}")]
    Protocol {
        side: PandaFactSyncSide,
        #[source]
        source: LogSyncError,
    },
    #[error("sync {side} event receiver lagged by {skipped} messages")]
    EventReceiverLagged {
        side: PandaFactSyncSide,
        skipped: u64,
    },
    #[error("sync {side} received an operation without payload bytes")]
    MissingOperationBody { side: PandaFactSyncSide },
    #[error("sync {side} import failed: {source}")]
    Import {
        side: PandaFactSyncSide,
        #[source]
        source: PandaFactError,
    },
}

pub type SyncResult<T> = std::result::Result<T, PandaFactSyncError>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PandaFactLogId(String);

type IslandLog = PandaFactLogId;

impl From<&IslandId> for PandaFactLogId {
    fn from(value: &IslandId) -> Self {
        Self(value.as_str().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PandaFactExtensions {
    island: String,
    key: String,
    author: String,
    #[serde(default)]
    authority_epoch: Option<u64>,
}

impl PandaFactExtensions {
    fn new(
        island: &IslandId,
        key: &FactKey,
        author: &PrincipalId,
        authority_epoch: Option<IslandMemberEpoch>,
    ) -> Self {
        Self {
            island: island.as_str().to_owned(),
            key: key.as_str().to_owned(),
            author: author.as_str().to_owned(),
            authority_epoch: authority_epoch.map(IslandMemberEpoch::get),
        }
    }
}

pub struct PandaFactAuthor {
    principal: PrincipalId,
    key: SigningKey,
}

impl PandaFactAuthor {
    #[must_use]
    pub fn new(principal: PrincipalId) -> Self {
        Self {
            principal,
            key: SigningKey::generate(),
        }
    }

    #[must_use]
    pub fn from_seed(principal: PrincipalId, seed: [u8; 32]) -> Self {
        Self {
            principal,
            key: SigningKey::from_bytes(&seed),
        }
    }

    #[must_use]
    pub fn from_private_key_bytes(principal: PrincipalId, bytes: [u8; 32]) -> Self {
        Self {
            principal,
            key: SigningKey::from_bytes(&bytes),
        }
    }

    pub fn from_private_key_hex(principal: PrincipalId, value: &str) -> Result<Self> {
        let bytes = parse_author_private_key_hex(value)?;
        Ok(Self::from_private_key_bytes(principal, bytes))
    }

    #[must_use]
    pub fn private_key_hex(&self) -> String {
        self.key.to_hex()
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    fn public_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    #[must_use]
    pub fn author_key(&self) -> PandaFactAuthorKey {
        PandaFactAuthorKey(self.public_key())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PandaFactAuthorKey(VerifyingKey);

impl PandaFactAuthorKey {
    pub fn parse_hex(value: &str) -> Result<Self> {
        value
            .parse::<VerifyingKey>()
            .map(Self)
            .map_err(|error| PandaFactError::InvalidAuthorKey {
                message: error.to_string(),
            })
    }

    #[must_use]
    pub fn as_hex(self) -> String {
        self.0.to_hex()
    }

    #[must_use]
    fn public_key(self) -> VerifyingKey {
        self.0
    }
}

impl From<PandaFactAuthorKey> for IslandMemberAuthorKey {
    fn from(value: PandaFactAuthorKey) -> Self {
        Self::from_public_key(value.public_key())
    }
}

fn parse_author_private_key_hex(value: &str) -> Result<[u8; 32]> {
    let trimmed = value.trim();
    if trimmed.len() != 64 {
        return Err(PandaFactError::InvalidAuthorPrivateKey {
            message: format!(
                "private key must be 64 lowercase hex characters, got {}",
                trimmed.len()
            ),
        });
    }
    let mut bytes = [0_u8; 32];
    for (index, chunk) in trimmed.as_bytes().chunks_exact(2).enumerate() {
        let high =
            decode_hex_nibble(chunk[0]).ok_or_else(|| PandaFactError::InvalidAuthorPrivateKey {
                message: "private key contains invalid hex".to_string(),
            })?;
        let low =
            decode_hex_nibble(chunk[1]).ok_or_else(|| PandaFactError::InvalidAuthorPrivateKey {
                message: "private key contains invalid hex".to_string(),
            })?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandaFactSyncScope {
    island: IslandId,
    trusted_authors: BTreeMap<PrincipalId, PandaFactAuthorKey>,
}

impl PandaFactSyncScope {
    #[must_use]
    pub fn new(island: IslandId) -> Self {
        Self {
            island,
            trusted_authors: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_trusted_author(
        mut self,
        principal: PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> Self {
        self.trusted_authors.insert(principal, author_key);
        self
    }

    #[must_use]
    pub fn from_trusted_authors(
        island: IslandId,
        authors: impl IntoIterator<Item = (PrincipalId, PandaFactAuthorKey)>,
    ) -> Self {
        authors
            .into_iter()
            .fold(Self::new(island), |scope, (principal, author_key)| {
                scope.with_trusted_author(principal, author_key)
            })
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    fn logs(&self) -> Logs<IslandLog> {
        self.trusted_authors
            .values()
            .map(|key| (key.public_key(), vec![IslandLog::from(&self.island)]))
            .collect()
    }

    #[must_use]
    pub fn from_authority(authority: &IslandAuthoritySnapshot) -> Self {
        Self::from_trusted_authors(
            authority.island().clone(),
            authority.active_writers().map(|member| {
                (
                    member.principal().clone(),
                    PandaFactAuthorKey(member.author_key().public_key()),
                )
            }),
        )
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PandaFactSyncPeerReport {
    pub received: u64,
    pub imported: u64,
    pub duplicate: u64,
    pub conflict: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PandaFactSyncReport {
    pub left: PandaFactSyncPeerReport,
    pub right: PandaFactSyncPeerReport,
}

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
    authority_snapshots: Vec<IslandAuthoritySnapshot>,
    max_connections: u32,
}

impl PandaSqliteOpenConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, known_islands: Vec<IslandId>) -> Self {
        Self {
            path: path.into(),
            known_islands,
            trusted_author_keys: Vec::new(),
            authority_snapshots: Vec::new(),
            max_connections: 1,
        }
    }

    #[must_use]
    pub fn with_trusted_author_key(mut self, trusted: PandaTrustedAuthorKey) -> Self {
        self.trusted_author_keys.push(trusted);
        self
    }

    #[must_use]
    pub fn with_authority_snapshot(mut self, authority: IslandAuthoritySnapshot) -> Self {
        self.authority_snapshots.push(authority);
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
    authority_epoch: Option<IslandMemberEpoch>,
    content_hash: FactContentHash,
}

impl PandaFactMetadata {
    fn new(
        island: IslandId,
        key: FactKey,
        author: PrincipalId,
        authority_epoch: Option<IslandMemberEpoch>,
        content_hash: FactContentHash,
    ) -> Self {
        Self {
            island,
            key,
            author,
            authority_epoch,
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
    pub fn authority_epoch(&self) -> Option<IslandMemberEpoch> {
        self.authority_epoch
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
pub struct PandaFactWrite {
    outcome: PandaFactWriteOutcome,
    operation: Option<PandaFactOperation>,
}

impl PandaFactWrite {
    fn new(outcome: PandaFactWriteOutcome, operation: Option<PandaFactOperation>) -> Self {
        Self { outcome, operation }
    }

    #[must_use]
    pub fn outcome(&self) -> &PandaFactWriteOutcome {
        &self.outcome
    }

    #[must_use]
    pub fn operation(&self) -> Option<&PandaFactOperation> {
        self.operation.as_ref()
    }

    #[must_use]
    pub fn into_outcome(self) -> PandaFactWriteOutcome {
        self.outcome
    }
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

    pub fn to_p2panda_operation(&self) -> Result<Operation<PandaFactExtensions>> {
        let header: Header<PandaFactExtensions> =
            decode_cbor(self.header()).map_err(|error| PandaFactError::InvalidExtensions {
                message: error.to_string(),
            })?;
        let body = Body::new(self.body());
        let operation = Operation {
            hash: header.hash(),
            header,
            body: Some(body),
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        Ok(operation)
    }

    pub fn from_p2panda_operation(operation: Operation<PandaFactExtensions>) -> Result<Self> {
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let header =
            encode_cbor(operation.header()).map_err(|error| PandaFactError::InvalidExtensions {
                message: error.to_string(),
            })?;
        let key = FactKey::parse(operation.header.extensions.key.clone()).map_err(|error| {
            PandaFactError::InvalidExtensions {
                message: error.to_string(),
            }
        })?;
        let body = operation
            .body
            .ok_or(PandaFactError::MissingPayload { key })?;
        Ok(Self::new(header, body.to_bytes()))
    }

    pub fn encoded_size(&self) -> usize {
        self.header.len() + self.body.len()
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
    facts_by_key_hash: BTreeMap<StoredFactKeyHash, usize>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
    authority_snapshots: BTreeMap<IslandId, IslandAuthoritySnapshot>,
    trusted_author_keys: BTreeMap<(IslandId, PrincipalId), VerifyingKey>,
    trusted_replica_peers: BTreeSet<(IslandId, PrincipalId)>,
}

enum FactAuthority<'a> {
    Snapshot(&'a IslandAuthoritySnapshot),
    Manual {
        island: &'a IslandId,
        trusted_author_keys: &'a BTreeMap<(IslandId, PrincipalId), VerifyingKey>,
        trusted_replica_peers: &'a BTreeSet<(IslandId, PrincipalId)>,
    },
}

impl FactAuthority<'_> {
    fn require_replica_importer(&self, principal: &PrincipalId) -> Result<()> {
        match self {
            Self::Snapshot(authority) => {
                if authority.active_replica_importer(principal).is_some() {
                    return Ok(());
                }
                Err(PandaFactError::UnauthorizedReplicaImport {
                    island: authority.island().clone(),
                    principal: principal.clone(),
                })
            }
            Self::Manual {
                island,
                trusted_replica_peers,
                ..
            } => {
                if trusted_replica_peers.contains(&((*island).clone(), principal.clone())) {
                    return Ok(());
                }
                Err(PandaFactError::UnauthorizedReplicaImport {
                    island: (*island).clone(),
                    principal: principal.clone(),
                })
            }
        }
    }

    fn require_active_writer(
        &self,
        principal: &PrincipalId,
        public_key: VerifyingKey,
    ) -> Result<()> {
        self.active_writer_epoch(principal, public_key).map(|_| ())
    }

    fn active_writer_epoch(
        &self,
        principal: &PrincipalId,
        public_key: VerifyingKey,
    ) -> Result<Option<IslandMemberEpoch>> {
        match self {
            Self::Snapshot(authority) => {
                let Some(member) = authority.active_writer(principal) else {
                    return Err(PandaFactError::UntrustedAuthorKey {
                        island: authority.island().clone(),
                        principal: principal.clone(),
                    });
                };
                if member.author_key().public_key() != public_key {
                    return Err(PandaFactError::AuthorKeyMismatch {
                        island: authority.island().clone(),
                        principal: principal.clone(),
                    });
                }
                Ok(Some(member.epoch()))
            }
            Self::Manual {
                island,
                trusted_author_keys,
                ..
            } => {
                require_manual_author_key(trusted_author_keys, island, principal, public_key)?;
                Ok(None)
            }
        }
    }
}

fn require_manual_author_key(
    trusted_author_keys: &BTreeMap<(IslandId, PrincipalId), VerifyingKey>,
    island: &IslandId,
    principal: &PrincipalId,
    public_key: VerifyingKey,
) -> Result<()> {
    match trusted_author_keys.get(&(island.clone(), principal.clone())) {
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

#[derive(Clone)]
enum PandaFactBackend {
    Memory(PandaMemoryStore),
    Sqlite(SqliteStore),
}

#[derive(Clone, Default)]
struct PandaMemoryStore {
    inner: Arc<StdMutex<PandaMemoryInner>>,
}

#[derive(Default)]
struct PandaMemoryInner {
    operations: BTreeMap<Hash, Operation<PandaFactExtensions>>,
    logs: BTreeMap<(VerifyingKey, IslandLog), Vec<Hash>>,
    topics: BTreeMap<Topic, BTreeMap<VerifyingKey, Vec<IslandLog>>>,
}

impl PandaMemoryStore {
    async fn ingest_operation(
        &self,
        operation: Operation<PandaFactExtensions>,
        log_id: &IslandLog,
    ) -> Result<PandaBackendIngest> {
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let mut inner = self.inner.lock().expect("memory panda store");
        if inner.operations.contains_key(&operation.hash) {
            return Ok(PandaBackendIngest::AlreadyPresent(operation));
        }
        let log_key = (operation.header.verifying_key, log_id.clone());
        let latest_operation = inner
            .logs
            .get(&log_key)
            .and_then(|hashes| hashes.last())
            .and_then(|hash| inner.operations.get(hash));
        match (operation.header.backlink, latest_operation) {
            (Some(backlink), Some(latest_operation))
                if backlink == latest_operation.hash
                    && operation.header.seq_num == latest_operation.header.seq_num + 1 => {}
            (None, None) if operation.header.seq_num == 0 => {}
            _ => {
                return Err(PandaFactError::OutOfOrderOperation {
                    island: IslandId::new(operation.header.extensions.island.clone()),
                    principal: PrincipalId::new(operation.header.extensions.author.clone()),
                    key: FactKey::parse(operation.header.extensions.key.clone()).map_err(
                        |error| PandaFactError::InvalidExtensions {
                            message: error.to_string(),
                        },
                    )?,
                    missing_operations: 1,
                });
            }
        }
        inner.logs.entry(log_key).or_default().push(operation.hash);
        inner.operations.insert(operation.hash, operation.clone());
        Ok(PandaBackendIngest::Inserted(operation))
    }

    async fn latest_operation(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Operation<PandaFactExtensions>>> {
        let inner = self.inner.lock().expect("memory panda store");
        Ok(inner
            .logs
            .get(&(*public_key, log_id.clone()))
            .and_then(|hashes| hashes.last())
            .and_then(|hash| inner.operations.get(hash))
            .cloned())
    }

    async fn get_log_heights_for_log(
        &self,
        log_id: &IslandLog,
    ) -> Result<Vec<(VerifyingKey, u64)>> {
        let inner = self.inner.lock().expect("memory panda store");
        Ok(inner
            .logs
            .iter()
            .filter_map(|((author, current_log), hashes)| {
                (current_log == log_id).then_some((*author, hashes.len().saturating_sub(1) as u64))
            })
            .collect())
    }

    async fn raw_log(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Vec<RawOperation>>> {
        let inner = self.inner.lock().expect("memory panda store");
        let Some(hashes) = inner.logs.get(&(*public_key, log_id.clone())) else {
            return Ok(None);
        };
        let mut raw = Vec::new();
        for hash in hashes {
            let Some(operation) = inner.operations.get(hash) else {
                continue;
            };
            raw.push((
                encode_cbor(operation.header()).map_err(|error| {
                    PandaFactError::InvalidExtensions {
                        message: error.to_string(),
                    }
                })?,
                operation.body().map(Body::to_bytes),
            ));
        }
        Ok(Some(raw))
    }

    async fn associate_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().expect("memory panda store");
        let logs = inner.topics.entry(*topic).or_default();
        let log_ids = logs.entry(*author).or_default();
        if log_ids.contains(log_id) {
            return Ok(false);
        }
        log_ids.push(log_id.clone());
        Ok(true)
    }

    async fn remove_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().expect("memory panda store");
        let Some(logs) = inner.topics.get_mut(topic) else {
            return Ok(false);
        };
        let Some(log_ids) = logs.get_mut(author) else {
            return Ok(false);
        };
        let Some(index) = log_ids.iter().position(|current| current == log_id) else {
            return Ok(false);
        };
        log_ids.remove(index);
        Ok(true)
    }

    async fn resolve_topic(&self, topic: &Topic) -> Result<BTreeMap<VerifyingKey, Vec<IslandLog>>> {
        let inner = self.inner.lock().expect("memory panda store");
        Ok(inner.topics.get(topic).cloned().unwrap_or_default())
    }
}

impl LogStore<Operation<PandaFactExtensions>, VerifyingKey, IslandLog, u64, Hash>
    for PandaMemoryStore
{
    type Error = PandaMemoryStoreError;

    async fn get_latest_entry(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        Ok(self
            .latest_operation(author, log_id)
            .await
            .expect("memory latest"))
    }

    async fn get_latest_entry_tx(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        self.get_latest_entry(author, log_id).await
    }

    async fn get_log_heights(
        &self,
        author: &VerifyingKey,
        logs: &[IslandLog],
    ) -> std::result::Result<Option<BTreeMap<IslandLog, u64>>, Self::Error> {
        let inner = self.inner.lock().expect("memory panda store");
        let mut heights = BTreeMap::new();
        for log_id in logs {
            if let Some(hashes) = inner.logs.get(&(*author, log_id.clone())) {
                heights.insert(log_id.clone(), hashes.len().saturating_sub(1) as u64);
            }
        }
        Ok((!heights.is_empty()).then_some(heights))
    }

    async fn get_log_size(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<(u64, u64)>, Self::Error> {
        let inner = self.inner.lock().expect("memory panda store");
        let Some(hashes) = inner.logs.get(&(*author, log_id.clone())) else {
            return Ok(None);
        };
        let mut operations = 0_u64;
        let mut bytes = 0_u64;
        for hash in hashes {
            let Some(operation) = inner.operations.get(hash) else {
                continue;
            };
            if after.is_some_and(|after| operation.header.seq_num <= after)
                || until.is_some_and(|until| operation.header.seq_num > until)
            {
                continue;
            }
            let header = encode_cbor(operation.header()).map_err(|_| PandaMemoryStoreError)?;
            operations += 1;
            bytes += header.len() as u64 + operation.header.payload_size;
        }
        Ok(Some((operations, bytes)))
    }

    async fn get_log_entries(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<Vec<(Operation<PandaFactExtensions>, Vec<u8>)>>, Self::Error>
    {
        let inner = self.inner.lock().expect("memory panda store");
        let Some(hashes) = inner.logs.get(&(*author, log_id.clone())) else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        for hash in hashes {
            let Some(operation) = inner.operations.get(hash) else {
                continue;
            };
            if after.is_some_and(|after| operation.header.seq_num <= after) {
                continue;
            }
            if until.is_some_and(|until| operation.header.seq_num > until) {
                continue;
            }
            let header = encode_cbor(operation.header()).map_err(|_| PandaMemoryStoreError)?;
            entries.push((operation.clone(), header));
        }
        Ok(Some(entries))
    }

    async fn prune_entries(
        &self,
        _author: &VerifyingKey,
        _log_id: &IslandLog,
        _until: &u64,
    ) -> std::result::Result<u64, Self::Error> {
        Ok(0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("memory p2panda store failed")]
pub struct PandaMemoryStoreError;

#[derive(Debug, thiserror::Error)]
#[error("p2panda fact store adapter failed: {message}")]
pub struct PandaFactStoreAdapterError {
    message: String,
}

impl PandaFactStoreAdapterError {
    fn new(error: impl ToString) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl PandaFactBackend {
    async fn ingest_operation(
        &mut self,
        header: Header<PandaFactExtensions>,
        body: Option<Body>,
        log_id: &IslandLog,
    ) -> Result<PandaBackendIngest> {
        let operation = Operation {
            hash: header.hash(),
            header,
            body,
        };
        match self {
            Self::Memory(store) => store.ingest_operation(operation, log_id).await,
            Self::Sqlite(store) => {
                let inserted = ingest_operation(store, &operation, log_id, log_id, false)
                    .await
                    .map_err(|error| classify_p2panda_ingest_error(&operation, error))?;
                Ok(if inserted {
                    PandaBackendIngest::Inserted(operation)
                } else {
                    PandaBackendIngest::AlreadyPresent(operation)
                })
            }
        }
    }

    async fn latest_operation(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Operation<PandaFactExtensions>>> {
        match self {
            Self::Memory(store) => store.latest_operation(public_key, log_id).await,
            Self::Sqlite(store) => store
                .get_latest_entry(public_key, log_id)
                .await
                .map_err(store_error),
        }
    }

    async fn get_log_heights(&self, log_id: &IslandLog) -> Result<Vec<(VerifyingKey, u64)>> {
        match self {
            Self::Memory(store) => store.get_log_heights_for_log(log_id).await,
            Self::Sqlite(store) => {
                let associations =
                    <SqliteStore as TopicStore<IslandLog, VerifyingKey, IslandLog>>::resolve(
                        store, log_id,
                    )
                    .await
                    .map_err(store_error)?;
                let mut heights = Vec::new();
                for (author, logs) in associations {
                    let Some(log_heights) =
                        <SqliteStore as LogStore<
                            Operation<PandaFactExtensions>,
                            VerifyingKey,
                            IslandLog,
                            u64,
                            Hash,
                        >>::get_log_heights(store, &author, &logs)
                        .await
                        .map_err(store_error)?
                    else {
                        continue;
                    };
                    if let Some(height) = log_heights.get(log_id) {
                        heights.push((author, *height));
                    }
                }
                Ok(heights)
            }
        }
    }

    async fn get_raw_log(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Vec<RawOperation>>> {
        match self {
            Self::Memory(store) => store.raw_log(public_key, log_id).await,
            Self::Sqlite(store) => {
                let entries: Option<Vec<(Operation<PandaFactExtensions>, Vec<u8>)>> = store
                    .get_log_entries(public_key, log_id, None, None)
                    .await
                    .map_err(store_error)?;
                let Some(entries) = entries else {
                    return Ok(None);
                };
                let mut raw = Vec::new();
                for (operation, header_bytes) in entries {
                    raw.push((header_bytes, operation.body().map(Body::to_bytes)));
                }
                Ok(Some(raw))
            }
        }
    }

    async fn associate_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        match self {
            Self::Memory(store) => store.associate_topic(topic, author, log_id).await,
            Self::Sqlite(store) => {
                let permit = store
                    .begin()
                    .await
                    .map_err(|error| store_error_with("begin topic association", error))?;
                let result =
                    <SqliteStore as TopicStore<Topic, VerifyingKey, IslandLog>>::associate(
                        store, topic, author, log_id,
                    )
                    .await;
                match result {
                    Ok(associated) => {
                        store
                            .commit(permit)
                            .await
                            .map_err(|error| store_error_with("commit topic association", error))?;
                        Ok(associated)
                    }
                    Err(error) => {
                        let _ = store.rollback(permit).await;
                        Err(store_error_with("associate topic", error))
                    }
                }
            }
        }
    }

    async fn remove_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        match self {
            Self::Memory(store) => store.remove_topic(topic, author, log_id).await,
            Self::Sqlite(store) => {
                let permit = store
                    .begin()
                    .await
                    .map_err(|error| store_error_with("begin topic removal", error))?;
                let result = <SqliteStore as TopicStore<Topic, VerifyingKey, IslandLog>>::remove(
                    store, topic, author, log_id,
                )
                .await;
                match result {
                    Ok(removed) => {
                        store
                            .commit(permit)
                            .await
                            .map_err(|error| store_error_with("commit topic removal", error))?;
                        Ok(removed)
                    }
                    Err(error) => {
                        let _ = store.rollback(permit).await;
                        Err(store_error_with("remove topic", error))
                    }
                }
            }
        }
    }

    async fn resolve_topic(&self, topic: &Topic) -> Result<BTreeMap<VerifyingKey, Vec<IslandLog>>> {
        match self {
            Self::Memory(store) => store.resolve_topic(topic).await,
            Self::Sqlite(store) => {
                <SqliteStore as TopicStore<Topic, VerifyingKey, IslandLog>>::resolve(store, topic)
                    .await
                    .map_err(store_error)
            }
        }
    }
}

fn classify_p2panda_ingest_error(
    operation: &Operation<PandaFactExtensions>,
    error: IngestError,
) -> PandaFactError {
    match error {
        IngestError::InvalidOperation(
            OperationError::BacklinkMissing
            | OperationError::BacklinkMismatch
            | OperationError::SeqNumNonIncremental(_, _),
        ) => operation_out_of_order_error(operation),
        other => PandaFactError::Ingest(other),
    }
}

fn operation_out_of_order_error(operation: &Operation<PandaFactExtensions>) -> PandaFactError {
    PandaFactError::OutOfOrderOperation {
        island: IslandId::new(operation.header.extensions.island.clone()),
        principal: PrincipalId::new(operation.header.extensions.author.clone()),
        key: FactKey::parse(operation.header.extensions.key.clone()).unwrap_or_else(|_| {
            FactKey::parse("/facts/invalid/out-of-order").expect("fallback fact key parses")
        }),
        missing_operations: 1,
    }
}

impl PandaFactStore {
    async fn ensure_transport_topic_association(
        &mut self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        self.backend.associate_topic(topic, author, log_id).await
    }
}

enum PandaBackendIngest {
    Inserted(Operation<PandaFactExtensions>),
    AlreadyPresent(Operation<PandaFactExtensions>),
}

impl PandaFactStore {
    #[must_use]
    pub fn new(authorizer: Arc<dyn FactAuthorizer>) -> Self {
        Self::from_backend(
            authorizer,
            PandaFactBackend::Memory(PandaMemoryStore::default()),
        )
    }

    pub async fn open_sqlite(
        authorizer: Arc<dyn FactAuthorizer>,
        config: PandaSqliteOpenConfig,
    ) -> Result<Self> {
        prepare_sqlite_parent(&config.path)?;
        let url = sqlite_url(&config.path);
        let sqlite = SqliteStoreBuilder::new()
            .database_url(&url)
            .max_connections(config.max_connections)
            .build()
            .await
            .map_err(store_error)?;
        let mut store = Self::from_backend(authorizer, PandaFactBackend::Sqlite(sqlite));
        for trusted in config.trusted_author_keys {
            store.bind_author_key(&trusted.island, &trusted.principal, trusted.author_key.0)?;
        }
        for authority in config.authority_snapshots {
            store.install_authority_snapshot(authority);
        }
        store.rebuild_indexes(&config.known_islands).await?;
        Ok(store)
    }

    fn from_backend(authorizer: Arc<dyn FactAuthorizer>, backend: PandaFactBackend) -> Self {
        Self {
            backend,
            authorizer,
            fact_index: BTreeMap::new(),
            operations: Vec::new(),
            operation_hashes: BTreeSet::new(),
            facts: Vec::new(),
            facts_by_identity: BTreeMap::new(),
            facts_by_key_hash: BTreeMap::new(),
            payloads: BTreeMap::new(),
            authority_snapshots: BTreeMap::new(),
            trusted_author_keys: BTreeMap::new(),
            trusted_replica_peers: BTreeSet::new(),
        }
    }

    pub fn install_authority_snapshot(&mut self, authority: IslandAuthoritySnapshot) {
        self.authority_snapshots
            .insert(authority.island().clone(), authority);
    }

    pub fn trust_author_key(
        &mut self,
        island: &IslandId,
        principal: PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> Result<()> {
        self.bind_author_key(island, &principal, author_key.0)
    }

    pub fn trust_replica_peer(&mut self, island: &IslandId, principal: PrincipalId) {
        self.trusted_replica_peers
            .insert((island.clone(), principal));
    }

    fn fact_authority_for<'a>(&'a self, island: &'a IslandId) -> FactAuthority<'a> {
        match self.authority_snapshots.get(island) {
            Some(authority) => FactAuthority::Snapshot(authority),
            None => FactAuthority::Manual {
                island,
                trusted_author_keys: &self.trusted_author_keys,
                trusted_replica_peers: &self.trusted_replica_peers,
            },
        }
    }

    #[must_use]
    pub fn can_write_fact(&self, session: &BusSession, key: &FactKey) -> bool {
        self.authorizer
            .can_session_access_fact(session, key, FactAccess::Write)
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
        let authority_epoch = self.authorize_local_author(session, author)?;

        let body = Body::new(payload.as_bytes());
        let content_hash = FactContentHash::for_payload(&payload);
        let metadata = PandaFactMetadata::new(
            session.island().clone(),
            key.clone(),
            author.principal().clone(),
            authority_epoch,
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
            verifying_key: author.public_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: p2panda_core::Timestamp::now(),
            seq_num: log_position.seq_num,
            backlink: log_position.backlink,
            extensions: PandaFactExtensions::new(
                session.island(),
                &key,
                author.principal(),
                authority_epoch,
            ),
        };
        header.sign(&author.key);
        let header_bytes =
            encode_cbor(&header).map_err(|error| PandaFactError::InvalidExtensions {
                message: error.to_string(),
            })?;
        let raw_operation = PandaFactOperation::new(header_bytes.clone(), raw_body);
        let result = self
            .backend
            .ingest_operation(header, Some(body), &IslandLog::from(session.island()))
            .await?;

        let operation = match result {
            PandaBackendIngest::Inserted(operation)
            | PandaBackendIngest::AlreadyPresent(operation) => operation,
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let metadata = metadata_from_operation(&operation, content_hash)?;

        self.record_operation(operation.hash, raw_operation);
        Ok(self.record_fact_operation(metadata, payload, pre_ingest))
    }

    /// Export stored operations for deterministic harnesses and debug tooling.
    ///
    /// Product replication should use [`sync_panda_fact_stores`] or a network
    /// transport that carries these opaque operation envelopes back through
    /// [`PandaFactStore::import_replica_operation`].
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
        let body_bytes = operation.body().to_vec();
        let body = Body::new(&body_bytes);
        self.import_decoded_operation(session, header, body, operation.header_bytes(), body_bytes)
            .await
    }

    pub async fn import_replica_operation(
        &mut self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        self.require_trusted_replica_importer(session)?;
        self.import_operation(session, operation).await
    }

    async fn import_decoded_operation(
        &mut self,
        session: &BusSession,
        header: Header<PandaFactExtensions>,
        body: Body,
        header_bytes: Vec<u8>,
        body_bytes: Vec<u8>,
    ) -> Result<PandaFactWriteOutcome> {
        let payload = FactPayload::from(body_bytes.clone());
        let content_hash = FactContentHash::for_payload(&payload);
        let metadata = metadata_from_header(&header, content_hash)?;
        let operation_hash = header.hash();
        let public_key = header.verifying_key;
        self.authorize_import(session, &metadata, public_key)?;
        let candidate = Operation {
            hash: operation_hash,
            header: header.clone(),
            body: Some(body.clone()),
        };
        validate_operation(&candidate).map_err(|_| PandaFactError::InvalidOperation)?;
        if self.operation_hashes.contains(&operation_hash) {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let result = self
            .backend
            .ingest_operation(header, Some(body), &IslandLog::from(&metadata.island))
            .await?;
        let imported = match result {
            PandaBackendIngest::Inserted(imported) => imported,
            PandaBackendIngest::AlreadyPresent(_) => {
                return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
            }
        };
        validate_operation(&imported).map_err(|_| PandaFactError::InvalidOperation)?;

        let pre_ingest = self.pre_ingest_status(&metadata);
        self.record_operation(
            operation_hash,
            PandaFactOperation::new(header_bytes, body_bytes),
        );
        Ok(self.record_fact_operation(metadata, payload, pre_ingest))
    }

    fn authorize_import(
        &self,
        session: &BusSession,
        metadata: &PandaFactMetadata,
        public_key: VerifyingKey,
    ) -> Result<()> {
        if session.island() != &metadata.island {
            return Err(PandaFactError::ImportIslandMismatch {
                session: session.island().clone(),
                operation: metadata.island.clone(),
            });
        }
        self.fact_authority_for(&metadata.island)
            .require_active_writer(&metadata.author, public_key)?;
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

    fn require_trusted_replica_importer(&self, session: &BusSession) -> Result<()> {
        self.fact_authority_for(session.island())
            .require_replica_importer(session.principal())
    }

    fn authorize_local_author(
        &mut self,
        session: &BusSession,
        author: &PandaFactAuthor,
    ) -> Result<Option<IslandMemberEpoch>> {
        match self.fact_authority_for(session.island()) {
            authority @ FactAuthority::Snapshot(_) => {
                authority.active_writer_epoch(author.principal(), author.public_key())
            }
            FactAuthority::Manual { .. } => {
                self.bind_author_key(session.island(), author.principal(), author.public_key())?;
                Ok(None)
            }
        }
    }

    fn bind_author_key(
        &mut self,
        island: &IslandId,
        principal: &PrincipalId,
        public_key: VerifyingKey,
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
        metadata: &PandaFactMetadata,
        public_key: VerifyingKey,
    ) -> Result<()> {
        self.fact_authority_for(&metadata.island)
            .require_active_writer(&metadata.author, public_key)
    }

    fn validate_sync_scope(
        &self,
        side: PandaFactSyncSide,
        session: &BusSession,
        scope: &PandaFactSyncScope,
    ) -> SyncResult<()> {
        if session.island() != scope.island() {
            return Err(PandaFactSyncError::ReplicaIslandMismatch {
                side,
                session: session.island().clone(),
                scope: scope.island().clone(),
            });
        }
        if self
            .fact_authority_for(scope.island())
            .require_replica_importer(session.principal())
            .is_err()
        {
            return Err(PandaFactSyncError::UnauthorizedReplica {
                side,
                island: scope.island().clone(),
                principal: session.principal().clone(),
            });
        }
        for (principal, author_key) in &scope.trusted_authors {
            if let Err(error) = self.require_sync_scope_author_key(
                scope.island(),
                principal,
                author_key.public_key(),
            ) {
                return Err(match error {
                    PandaFactError::AuthorKeyMismatch { .. } => {
                        PandaFactSyncError::ScopeAuthorKeyMismatch {
                            side,
                            island: scope.island().clone(),
                            principal: principal.clone(),
                        }
                    }
                    PandaFactError::UntrustedAuthorKey { .. } => {
                        PandaFactSyncError::ScopeAuthorKeyMissing {
                            side,
                            island: scope.island().clone(),
                            principal: principal.clone(),
                        }
                    }
                    source @ (PandaFactError::Ingest(_)
                    | PandaFactError::InvalidOperation
                    | PandaFactError::Store { .. }
                    | PandaFactError::InvalidStorePath { .. }
                    | PandaFactError::MissingPayload { .. }
                    | PandaFactError::InvalidAuthorKey { .. }
                    | PandaFactError::InvalidAuthorPrivateKey { .. }
                    | PandaFactError::InvalidExtensions { .. }
                    | PandaFactError::ImportIslandMismatch { .. }
                    | PandaFactError::UnauthorizedReplicaImport { .. }
                    | PandaFactError::OutOfOrderOperation { .. }
                    | PandaFactError::PrincipalMismatch { .. }
                    | PandaFactError::UnauthorizedWrite { .. }) => {
                        PandaFactSyncError::Import { side, source }
                    }
                });
            }
        }
        Ok(())
    }

    fn require_sync_scope_author_key(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        public_key: VerifyingKey,
    ) -> Result<()> {
        self.fact_authority_for(island)
            .require_active_writer(principal, public_key)
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
            Some(operation) => LogPosition {
                seq_num: operation.header.seq_num + 1,
                backlink: Some(operation.hash),
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
        self.facts_by_key_hash.clear();
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
        let payload = FactPayload::from(body_bytes.clone());
        let content_hash = FactContentHash::for_payload(&payload);
        let metadata = metadata_from_header(&header, content_hash)?;
        self.require_author_key(&metadata, header.verifying_key)?;
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
        self.record_fact_operation(metadata, payload, pre_ingest);
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
        let key_hash = StoredFactKeyHash::from_metadata(&metadata);
        self.facts_by_identity.entry(identity).or_insert_with(|| {
            let index = self.facts.len();
            self.facts.push(StoredFactOperation::new(metadata.clone()));
            self.facts_by_key_hash.insert(key_hash, index);
            index
        });
        match status {
            FactPreIngestStatus::Inserted => PandaFactWriteOutcome::Inserted(metadata),
            FactPreIngestStatus::Conflict => PandaFactWriteOutcome::Conflict(metadata),
            FactPreIngestStatus::AlreadyPresent => PandaFactWriteOutcome::AlreadyPresent(metadata),
        }
    }
}

type SyncEvent = LogSyncEvent<PandaFactExtensions>;
type SyncMessage = LogSyncMessage<IslandLog>;

/// Synchronize two canonical fact stores while preserving Ployz import checks.
///
/// This remains the deterministic same-process proof path. Network transport
/// may replace the message carrier, but received operations must still enter
/// through the canonical import path before becoming projection-visible truth.
pub async fn sync_panda_fact_stores(
    left: &mut PandaFactStore,
    left_session: &BusSession,
    right: &mut PandaFactStore,
    right_session: &BusSession,
    scope: &PandaFactSyncScope,
) -> SyncResult<PandaFactSyncReport> {
    left.validate_sync_scope(PandaFactSyncSide::Left, left_session, scope)?;
    right.validate_sync_scope(PandaFactSyncSide::Right, right_session, scope)?;

    let logs = scope.logs();
    match (left.backend.clone(), right.backend.clone()) {
        (PandaFactBackend::Memory(left_backend), PandaFactBackend::Memory(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
        (PandaFactBackend::Memory(left_backend), PandaFactBackend::Sqlite(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
        (PandaFactBackend::Sqlite(left_backend), PandaFactBackend::Memory(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
        (PandaFactBackend::Sqlite(left_backend), PandaFactBackend::Sqlite(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
    }
}

async fn run_log_sync_pair<LeftStore, RightStore>(
    left_store: LeftStore,
    right_store: RightStore,
    logs: Logs<IslandLog>,
    left_replica: (&mut PandaFactStore, &BusSession),
    right_replica: (&mut PandaFactStore, &BusSession),
) -> SyncResult<PandaFactSyncReport>
where
    LeftStore: LogStore<Operation<PandaFactExtensions>, VerifyingKey, IslandLog, u64, Hash>
        + Clone
        + Send
        + 'static,
    RightStore: LogStore<Operation<PandaFactExtensions>, VerifyingKey, IslandLog, u64, Hash>
        + Clone
        + Send
        + 'static,
{
    let (left_tx, right_rx) = mpsc::channel::<SyncMessage>(LOG_SYNC_MESSAGE_CAPACITY);
    let (right_tx, left_rx) = mpsc::channel::<SyncMessage>(LOG_SYNC_MESSAGE_CAPACITY);
    let (left_event_tx, left_event_rx) = broadcast::channel(LOG_SYNC_EVENT_CAPACITY);
    let (right_event_tx, right_event_rx) = broadcast::channel(LOG_SYNC_EVENT_CAPACITY);

    let (left, left_session) = left_replica;
    let (right, right_session) = right_replica;
    let left_events =
        collect_and_import_sync_events(PandaFactSyncSide::Left, left_event_rx, left, left_session);
    let right_events = collect_and_import_sync_events(
        PandaFactSyncSide::Right,
        right_event_rx,
        right,
        right_session,
    );

    let mut left_sink = left_tx;
    let mut right_sink = right_tx;
    let mut left_stream = left_rx.map(Ok::<_, Infallible>);
    let mut right_stream = right_rx.map(Ok::<_, Infallible>);
    let left_sync = LogSync::new(left_store, logs.clone(), left_event_tx);
    let right_sync = LogSync::new(right_store, logs, right_event_tx);

    let (left_result, right_result, left_report, right_report) = tokio::join!(
        left_sync.run(&mut left_sink, &mut left_stream),
        right_sync.run(&mut right_sink, &mut right_stream),
        left_events,
        right_events,
    );
    let mut left_report = left_report?;
    let mut right_report = right_report?;
    let (_, left_metrics) = left_result.map_err(|source| PandaFactSyncError::Protocol {
        side: PandaFactSyncSide::Left,
        source,
    })?;
    let (_, right_metrics) = right_result.map_err(|source| PandaFactSyncError::Protocol {
        side: PandaFactSyncSide::Right,
        source,
    })?;

    apply_metrics(&mut left_report, &left_metrics);
    apply_metrics(&mut right_report, &right_metrics);
    Ok(PandaFactSyncReport {
        left: left_report,
        right: right_report,
    })
}

async fn collect_and_import_sync_events(
    side: PandaFactSyncSide,
    mut events: broadcast::Receiver<SyncEvent>,
    store: &mut PandaFactStore,
    session: &BusSession,
) -> SyncResult<PandaFactSyncPeerReport> {
    let mut report = PandaFactSyncPeerReport::default();
    loop {
        match events.recv().await {
            Ok(LogSyncEvent::OperationReceived { operation, .. }) => {
                import_synced_operation(side, store, session, *operation, &mut report).await?;
            }
            Ok(LogSyncEvent::MetricsExchanged { .. }) => {}
            Err(broadcast::error::RecvError::Closed) => return Ok(report),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                return Err(PandaFactSyncError::EventReceiverLagged { side, skipped });
            }
        }
    }
}

async fn import_synced_operation(
    side: PandaFactSyncSide,
    store: &mut PandaFactStore,
    session: &BusSession,
    operation: Operation<PandaFactExtensions>,
    report: &mut PandaFactSyncPeerReport,
) -> SyncResult<()> {
    let Operation { header, body, .. } = operation;
    let body = body.ok_or(PandaFactSyncError::MissingOperationBody { side })?;
    report.received += 1;
    let header_bytes = encode_cbor(&header).map_err(|error| PandaFactSyncError::Import {
        side,
        source: PandaFactError::InvalidExtensions {
            message: error.to_string(),
        },
    })?;
    let body_bytes = body.to_bytes();
    match store
        .import_decoded_operation(session, header, body, header_bytes, body_bytes)
        .await
    {
        Ok(PandaFactWriteOutcome::Inserted(_)) => report.imported += 1,
        Ok(PandaFactWriteOutcome::AlreadyPresent(_)) => report.duplicate += 1,
        Ok(PandaFactWriteOutcome::Conflict(_)) => report.conflict += 1,
        Err(source) => {
            return Err(PandaFactSyncError::Import { side, source });
        }
    }
    Ok(())
}

fn apply_metrics(report: &mut PandaFactSyncPeerReport, metrics: &LogSyncMetrics) {
    report.bytes_received = metrics.received_bytes;
    report.bytes_sent = metrics.sent_bytes;
}

impl FactSource for PandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }
        if let Ok(exact_key) = FactKey::parse(pattern.as_str()) {
            return Ok(self.exact_candidates(island, &exact_key, pattern, session));
        }
        let candidates = self
            .facts
            .iter()
            .filter(|stored| stored.metadata.island == *island)
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
        if island != session.island() {
            return Ok(BTreeMap::new());
        }
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
            let Some(current) =
                self.candidate_for(stored, island, &exact_pattern(candidate), session)
            else {
                continue;
            };
            if current != *candidate || !candidate_payload_is_readable(current.status()) {
                continue;
            }
            if let Some(payload) = self.payloads.get(&stored.metadata.content_hash) {
                payloads.insert(stored.metadata.content_hash.clone(), payload.clone());
            }
        }
        Ok(payloads)
    }
}

#[derive(Clone)]
pub struct SharedPandaFactStore {
    store: Arc<Mutex<PandaFactStore>>,
}

impl SharedPandaFactStore {
    #[must_use]
    pub fn new(store: PandaFactStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    pub async fn write_fact_payload(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .write_fact_payload(session, author, key, payload)
            .await
    }

    pub async fn write_fact_payload_with_operation(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWrite> {
        let mut store = self.store.lock().await;
        let operation_index = store.operations.len();
        let outcome = store
            .write_fact_payload(session, author, key, payload)
            .await?;
        let operation = match outcome {
            PandaFactWriteOutcome::Inserted(_) | PandaFactWriteOutcome::Conflict(_) => {
                store.operations.get(operation_index).cloned()
            }
            PandaFactWriteOutcome::AlreadyPresent(ref metadata) => {
                store.operation_for_metadata(metadata)?
            }
        };
        Ok(PandaFactWrite::new(outcome, operation))
    }

    pub async fn trust_author_key(
        &self,
        island: &IslandId,
        principal: PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> Result<()> {
        self.store
            .lock()
            .await
            .trust_author_key(island, principal, author_key)
    }

    pub async fn trust_replica_peer(&self, island: &IslandId, principal: PrincipalId) {
        self.store
            .lock()
            .await
            .trust_replica_peer(island, principal);
    }

    pub async fn import_operation(
        &self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .import_operation(session, operation)
            .await
    }

    pub async fn import_replica_operation(
        &self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .import_replica_operation(session, operation)
            .await
    }

    pub async fn export_operations(&self) -> Vec<PandaFactOperation> {
        self.store
            .lock()
            .await
            .export_operations()
            .cloned()
            .collect()
    }

    pub async fn associate_transport_topic(
        &self,
        topic: Topic,
        operation: &PandaFactOperation,
    ) -> Result<bool> {
        let operation = operation.to_p2panda_operation()?;
        let log_id = IslandLog::from(&IslandId::new(operation.header.extensions.island.clone()));
        self.store
            .lock()
            .await
            .ensure_transport_topic_association(&topic, &operation.header.verifying_key, &log_id)
            .await
    }

    pub async fn import_replica_p2panda_operation(
        &self,
        session: &BusSession,
        topic: Topic,
        operation: Operation<PandaFactExtensions>,
    ) -> Result<PandaFactWriteOutcome> {
        let fact_operation = PandaFactOperation::from_p2panda_operation(operation.clone())?;
        let outcome = self
            .import_replica_operation(session, &fact_operation)
            .await?;
        let log_id = IslandLog::from(&IslandId::new(operation.header.extensions.island.clone()));
        self.store
            .lock()
            .await
            .ensure_transport_topic_association(&topic, &operation.header.verifying_key, &log_id)
            .await?;
        Ok(outcome)
    }

    pub fn try_can_write_fact(
        &self,
        session: &BusSession,
        key: &FactKey,
    ) -> FactSourceResult<bool> {
        Ok(self
            .store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .can_write_fact(session, key))
    }

    fn unavailable(&self) -> FactSourceError {
        FactSourceError::Unavailable {
            name: "p2panda fact store".to_string(),
        }
    }
}

impl LogStore<Operation<PandaFactExtensions>, VerifyingKey, PandaFactLogId, u64, Hash>
    for SharedPandaFactStore
{
    type Error = PandaFactStoreAdapterError;

    async fn get_latest_entry(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        self.store
            .lock()
            .await
            .backend
            .latest_operation(author, log_id)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }

    async fn get_latest_entry_tx(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        self.get_latest_entry(author, log_id).await
    }

    async fn get_log_heights(
        &self,
        author: &VerifyingKey,
        logs: &[PandaFactLogId],
    ) -> std::result::Result<Option<BTreeMap<PandaFactLogId, u64>>, Self::Error> {
        let store = self.store.lock().await;
        let mut heights = BTreeMap::new();
        for log_id in logs {
            let Some(operation) = store
                .backend
                .latest_operation(author, log_id)
                .await
                .map_err(PandaFactStoreAdapterError::new)?
            else {
                continue;
            };
            heights.insert(log_id.clone(), operation.header.seq_num);
        }
        Ok((!heights.is_empty()).then_some(heights))
    }

    async fn get_log_size(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<(u64, u64)>, Self::Error> {
        let entries = self.get_log_entries(author, log_id, after, until).await?;
        Ok(entries.map(|entries| {
            let bytes = entries
                .iter()
                .map(|(operation, header)| header.len() as u64 + operation.header.payload_size)
                .sum();
            (entries.len() as u64, bytes)
        }))
    }

    async fn get_log_entries(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<Vec<(Operation<PandaFactExtensions>, Vec<u8>)>>, Self::Error>
    {
        let store = self.store.lock().await;
        match &store.backend {
            PandaFactBackend::Memory(memory) => memory
                .get_log_entries(author, log_id, after, until)
                .await
                .map_err(PandaFactStoreAdapterError::new),
            PandaFactBackend::Sqlite(sqlite) => {
                <SqliteStore as LogStore<
                    Operation<PandaFactExtensions>,
                    VerifyingKey,
                    IslandLog,
                    u64,
                    Hash,
                >>::get_log_entries(sqlite, author, log_id, after, until)
                .await
                .map_err(PandaFactStoreAdapterError::new)
            }
        }
    }

    async fn prune_entries(
        &self,
        _author: &VerifyingKey,
        _log_id: &PandaFactLogId,
        _until: &u64,
    ) -> std::result::Result<u64, Self::Error> {
        Ok(0)
    }
}

impl TopicStore<Topic, VerifyingKey, PandaFactLogId> for SharedPandaFactStore {
    type Error = PandaFactStoreAdapterError;

    async fn associate(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        data_id: &PandaFactLogId,
    ) -> std::result::Result<bool, Self::Error> {
        self.store
            .lock()
            .await
            .ensure_transport_topic_association(topic, author, data_id)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }

    async fn remove(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        data_id: &PandaFactLogId,
    ) -> std::result::Result<bool, Self::Error> {
        self.store
            .lock()
            .await
            .backend
            .remove_topic(topic, author, data_id)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }

    async fn resolve(
        &self,
        topic: &Topic,
    ) -> std::result::Result<BTreeMap<VerifyingKey, Vec<PandaFactLogId>>, Self::Error> {
        self.store
            .lock()
            .await
            .backend
            .resolve_topic(topic)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }
}

impl FactSource for SharedPandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .read_payloads(island, candidates, session)
    }
}

fn exact_pattern(candidate: &FactCandidate) -> FactKeyPattern {
    FactKeyPattern::parse(candidate.key().as_str())
        .expect("stored fact candidate key is always a valid exact fact pattern")
}

fn candidate_payload_is_readable(status: CandidateStatus) -> bool {
    matches!(
        status,
        CandidateStatus::Verified | CandidateStatus::Conflict
    )
}

impl PandaFactStore {
    fn operation_for_metadata(
        &self,
        metadata: &PandaFactMetadata,
    ) -> Result<Option<PandaFactOperation>> {
        for operation in self.operations.iter().rev() {
            let header: Header<PandaFactExtensions> =
                decode_cbor(operation.header()).map_err(|error| {
                    PandaFactError::InvalidExtensions {
                        message: error.to_string(),
                    }
                })?;
            let content_hash =
                FactContentHash::for_payload(&FactPayload::from(operation.body().to_vec()));
            if metadata_from_header(&header, content_hash)? == *metadata {
                return Ok(Some(operation.clone()));
            }
        }
        Ok(None)
    }

    fn exact_candidates(
        &self,
        island: &IslandId,
        key: &FactKey,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> Vec<FactCandidate> {
        let Some(content_hashes) = self.fact_index.get(&(island.clone(), key.clone())) else {
            return Vec::new();
        };
        content_hashes
            .iter()
            .filter_map(|content_hash| {
                let identity =
                    StoredFactKeyHash::new(island.clone(), key.clone(), content_hash.clone());
                self.facts_by_key_hash
                    .get(&identity)
                    .and_then(|index| self.facts.get(*index))
                    .and_then(|stored| self.candidate_for(stored, island, pattern, session))
            })
            .collect()
    }

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StoredFactKeyHash {
    island: IslandId,
    key: FactKey,
    content_hash: FactContentHash,
}

impl StoredFactKeyHash {
    fn new(island: IslandId, key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            island,
            key,
            content_hash,
        }
    }

    fn from_metadata(metadata: &PandaFactMetadata) -> Self {
        Self {
            island: metadata.island.clone(),
            key: metadata.key.clone(),
            content_hash: metadata.content_hash.clone(),
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
    let authority_epoch = match header.extensions.authority_epoch {
        Some(epoch) => Some(IslandMemberEpoch::new(NonZeroU64::new(epoch).ok_or_else(
            || PandaFactError::InvalidExtensions {
                message: "authority epoch must be non-zero".to_string(),
            },
        )?)),
        None => None,
    };
    Ok(PandaFactMetadata::new(
        IslandId::new(header.extensions.island.clone()),
        key,
        PrincipalId::new(header.extensions.author.clone()),
        authority_epoch,
        content_hash,
    ))
}

fn store_error(error: impl Display) -> PandaFactError {
    PandaFactError::Store {
        message: error.to_string(),
    }
}

fn store_error_with(context: &str, error: impl Display) -> PandaFactError {
    PandaFactError::Store {
        message: format!("{context}: {error}"),
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
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use mvp_bus::{BusAuthority, Grant, harness::InMemoryBus};
    use mvp_p2panda_authz::{
        IslandAuthz, IslandAuthzMemoryLog, IslandMemberAuthorKey, IslandMemberEpoch,
        IslandMemberKeyBinding, ReplicaImportAccess,
    };
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

    #[test]
    fn author_private_key_hex_round_trips_for_process_roles() {
        let seed = "12".repeat(32);
        let author = PandaFactAuthor::from_private_key_hex(principal("writer"), &seed)
            .expect("private key parses");
        let same_author =
            PandaFactAuthor::from_private_key_hex(principal("writer"), &author.private_key_hex())
                .expect("private key round trips");

        assert_eq!(author.private_key_hex(), seed);
        assert_eq!(same_author.author_key(), author.author_key());
        assert!(matches!(
            PandaFactAuthor::from_private_key_hex(principal("writer"), "123"),
            Err(PandaFactError::InvalidAuthorPrivateKey { .. })
        ));
        assert!(matches!(
            PandaFactAuthor::from_private_key_hex(principal("writer"), &"GG".repeat(32)),
            Err(PandaFactError::InvalidAuthorPrivateKey { .. })
        ));
    }

    fn store_from_bus(bus: InMemoryBus) -> PandaFactStore {
        PandaFactStore::new(Arc::new(bus))
    }

    fn authz_binding(
        island: &IslandId,
        author: &PandaFactAuthor,
        epoch: u64,
    ) -> IslandMemberKeyBinding {
        IslandMemberKeyBinding::new(
            island.clone(),
            author.principal().clone(),
            IslandMemberEpoch::new(NonZeroU64::new(epoch).expect("test epoch is non-zero")),
            author.author_key().into(),
        )
    }

    struct AuthorityFixture {
        log: IslandAuthzMemoryLog,
        authz: IslandAuthz,
        root: IslandMemberKeyBinding,
        root_private_key: SigningKey,
    }

    impl AuthorityFixture {
        fn snapshot(&self) -> IslandAuthoritySnapshot {
            self.authz.authority_snapshot()
        }
    }

    async fn authority_fixture_for_writer_and_replica(
        island: &IslandId,
        writer: &PandaFactAuthor,
        replica: &PandaFactAuthor,
    ) -> AuthorityFixture {
        let root_private_key = SigningKey::from_bytes(&[9; 32]);
        let root = IslandMemberKeyBinding::new(
            island.clone(),
            PrincipalId::new("root"),
            IslandMemberEpoch::new(NonZeroU64::new(1).expect("test epoch is non-zero")),
            IslandMemberAuthorKey::from_public_key(root_private_key.verifying_key()),
        );
        let mut log = IslandAuthzMemoryLog::new(island.clone());
        let mut authz = log
            .create_root(root.clone(), &root_private_key)
            .await
            .expect("root membership operation persists");
        log.add_writer(
            &mut authz,
            &root,
            &root_private_key,
            authz_binding(island, writer, 1),
        )
        .await
        .expect("writer membership operation persists");
        log.add_replica_importer(
            &mut authz,
            &root,
            &root_private_key,
            authz_binding(island, replica, 1),
            ReplicaImportAccess::Read,
        )
        .await
        .expect("replica importer membership operation persists");
        AuthorityFixture {
            log,
            authz,
            root,
            root_private_key,
        }
    }

    async fn authority_for_writer_and_replica(
        island: &IslandId,
        writer: &PandaFactAuthor,
        replica: &PandaFactAuthor,
    ) -> IslandAuthoritySnapshot {
        authority_fixture_for_writer_and_replica(island, writer, replica)
            .await
            .snapshot()
    }

    #[tokio::test]
    async fn authority_snapshot_authorizes_local_write_without_manual_trust() {
        let (mut store, authority) = store_with_authority();
        let writer_session = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
        let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
        let snapshot =
            authority_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
        store.install_authority_snapshot(snapshot);

        let outcome = store
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/writer"),
                FactPayload::from_static(b"authorized"),
            )
            .await
            .expect("authz writer should write without manual trust");

        assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));
    }

    #[tokio::test]
    async fn authority_snapshot_authorizes_replica_import_without_manual_trust() {
        let (mut source, source_authority) = store_with_authority();
        let writer_session = grant_prod(
            &source_authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
        let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
        let snapshot =
            authority_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
        source.install_authority_snapshot(snapshot.clone());
        source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/import"),
                FactPayload::from_static(b"authorized"),
            )
            .await
            .expect("authz writer should write");
        let operation = source
            .export_operations()
            .next()
            .cloned()
            .expect("write exports one operation");

        let (mut target, target_authority) = store_with_authority();
        let _writer_grant = grant_prod(
            &target_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let replica_session = grant_prod(&target_authority, "replica", Grant::empty());
        target.install_authority_snapshot(snapshot);

        let outcome = target
            .import_replica_operation(&replica_session, &operation)
            .await
            .expect("authz replica importer should import without manual trust");

        assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));
    }

    #[tokio::test]
    async fn authority_snapshot_rejects_read_only_replica_import() {
        let (mut source, source_authority) = store_with_authority();
        let writer_session = grant_prod(
            &source_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
        let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
        let snapshot =
            authority_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
        source.install_authority_snapshot(snapshot.clone());
        source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/reject-import"),
                FactPayload::from_static(b"authorized"),
            )
            .await
            .expect("authz writer should write");
        let operation = source
            .export_operations()
            .next()
            .cloned()
            .expect("write exports one operation");

        let (mut target, target_authority) = store_with_authority();
        let _writer_grant = grant_prod(
            &target_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let readonly_session = grant_prod(&target_authority, "readonly", Grant::empty());
        target.install_authority_snapshot(snapshot);

        let error = target
            .import_replica_operation(&readonly_session, &operation)
            .await
            .expect_err("non-replica member cannot import");

        assert!(matches!(
            error,
            PandaFactError::UnauthorizedReplicaImport { .. }
        ));
    }

    #[tokio::test]
    async fn authority_snapshot_rejects_removed_writer_imports_and_future_writes() {
        let (mut source, source_authority) = store_with_authority();
        let writer_session = grant_prod(
            &source_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
        let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
        let mut fixture =
            authority_fixture_for_writer_and_replica(writer_session.island(), &writer, &replica)
                .await;
        source.install_authority_snapshot(fixture.snapshot());
        source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/pre-remove"),
                FactPayload::from_static(b"before-remove"),
            )
            .await
            .expect("writer should write before removal");
        let operation = source
            .export_operations()
            .next()
            .cloned()
            .expect("write exports one operation");

        fixture
            .log
            .remove_member(
                &mut fixture.authz,
                &fixture.root,
                &fixture.root_private_key,
                authz_binding(writer_session.island(), &writer, 1).member_id(),
            )
            .await
            .expect("writer removal persists");
        let removed_snapshot = fixture.snapshot();

        let forged_after_remove = source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/stale-post-remove"),
                FactPayload::from_static(b"stale-after-remove"),
            )
            .await
            .expect("stale local authority can still sign a partitioned operation");
        assert!(matches!(
            forged_after_remove,
            PandaFactWriteOutcome::Inserted(_)
        ));
        let forged_operation = source
            .export_operations()
            .last()
            .cloned()
            .expect("forged write exports an operation");

        let (mut target, target_authority) = store_with_authority();
        let _writer_grant = grant_prod(
            &target_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let replica_session = grant_prod(&target_authority, "replica", Grant::empty());
        target.install_authority_snapshot(removed_snapshot.clone());
        let import_error = target
            .import_replica_operation(&replica_session, &operation)
            .await
            .expect_err("removed writer imports are not accepted without fact-log frontier proof");
        assert!(matches!(
            import_error,
            PandaFactError::UntrustedAuthorKey { .. }
        ));
        let forged_error = target
            .import_replica_operation(&replica_session, &forged_operation)
            .await
            .expect_err("removed writer cannot forge a fresh operation with the old epoch");
        assert!(matches!(
            forged_error,
            PandaFactError::UntrustedAuthorKey { .. }
        ));

        source.install_authority_snapshot(removed_snapshot.clone());
        assert_eq!(
            source
                .list_candidates(
                    writer_session.island(),
                    &pattern("/facts/authz/pre-remove"),
                    &writer_session,
                )
                .expect("local pre-remove fact remains queryable")
                .len(),
            1
        );
        let error = source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/post-remove"),
                FactPayload::from_static(b"after-remove"),
            )
            .await
            .expect_err("removed writer cannot write future facts");
        assert!(matches!(error, PandaFactError::UntrustedAuthorKey { .. }));

        let scope = PandaFactSyncScope::from_authority(&removed_snapshot);
        assert!(!scope.trusted_authors.contains_key(writer.principal()));
    }

    #[tokio::test]
    async fn demoted_writer_becomes_replica_importer_without_write_authority() {
        let (mut source, source_authority) = store_with_authority();
        let writer_session = grant_prod(
            &source_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let root_session = grant_prod(&source_authority, "root", Grant::allow_all());
        let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
        let root_author = PandaFactAuthor::from_private_key_bytes(principal("root"), [9; 32]);
        let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
        let mut fixture =
            authority_fixture_for_writer_and_replica(writer_session.island(), &writer, &replica)
                .await;
        source.install_authority_snapshot(fixture.snapshot());
        source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/pre-demote"),
                FactPayload::from_static(b"before-demote"),
            )
            .await
            .expect("writer should write before demotion");
        let operation = source
            .export_operations()
            .next()
            .cloned()
            .expect("write exports one operation");

        fixture
            .log
            .demote_to_replica_importer(
                &mut fixture.authz,
                &fixture.root,
                &fixture.root_private_key,
                authz_binding(writer_session.island(), &writer, 1).member_id(),
                ReplicaImportAccess::Read,
            )
            .await
            .expect("writer demotion persists");
        let demoted_snapshot = fixture.snapshot();

        source.install_authority_snapshot(demoted_snapshot.clone());
        let write_error = source
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/post-demote"),
                FactPayload::from_static(b"after-demote"),
            )
            .await
            .expect_err("demoted writer cannot write future facts");
        assert!(matches!(
            write_error,
            PandaFactError::UntrustedAuthorKey { .. }
        ));
        source
            .write_fact_payload(
                &root_session,
                &root_author,
                key("/facts/authz/root-after-demote"),
                FactPayload::from_static(b"root-after-demote"),
            )
            .await
            .expect("root remains an active writer");
        let root_operation = source
            .export_operations()
            .last()
            .cloned()
            .expect("root write exports one operation");

        let (mut target, target_authority) = store_with_authority();
        let _target_root_grant = grant_prod(
            &target_authority,
            "root",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let demoted_replica_session = grant_prod(
            &target_authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        target.install_authority_snapshot(demoted_snapshot.clone());
        let old_writer_error = target
            .import_replica_operation(&demoted_replica_session, &operation)
            .await
            .expect_err("demoted writer's own old operation needs fact-log frontier proof");
        assert!(matches!(
            old_writer_error,
            PandaFactError::UntrustedAuthorKey { .. }
        ));
        let imported = target
            .import_replica_operation(&demoted_replica_session, &root_operation)
            .await
            .expect("demoted writer can import active writers as replica");
        assert!(matches!(imported, PandaFactWriteOutcome::Inserted(_)));

        let scope = PandaFactSyncScope::from_authority(&demoted_snapshot);
        assert!(!scope.trusted_authors.contains_key(writer.principal()));
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

    fn authority_sqlite_config(
        path: impl Into<PathBuf>,
        island: &IslandId,
        authority: IslandAuthoritySnapshot,
    ) -> PandaSqliteOpenConfig {
        PandaSqliteOpenConfig::new(path, vec![island.clone()]).with_authority_snapshot(authority)
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

    fn trust_replica(store: &mut PandaFactStore, session: &BusSession) {
        store.trust_replica_peer(session.island(), session.principal().clone());
    }

    fn sync_scope(session: &BusSession, authors: &[&PandaFactAuthor]) -> PandaFactSyncScope {
        PandaFactSyncScope::from_trusted_authors(
            session.island().clone(),
            authors
                .iter()
                .map(|author| (author.principal().clone(), author.author_key())),
        )
    }

    #[derive(Clone, Copy)]
    enum TestSyncBackend {
        Memory,
        Sqlite,
    }

    impl TestSyncBackend {
        fn name(self) -> &'static str {
            match self {
                Self::Memory => "memory",
                Self::Sqlite => "sqlite",
            }
        }
    }

    async fn test_sync_store(
        backend: TestSyncBackend,
        bus: InMemoryBus,
        path: PathBuf,
        island: &IslandId,
        authors: &[&PandaFactAuthor],
    ) -> PandaFactStore {
        let mut store = match backend {
            TestSyncBackend::Memory => store_from_bus(bus),
            TestSyncBackend::Sqlite => PandaFactStore::open_sqlite(
                Arc::new(bus),
                PandaSqliteOpenConfig::new(path, vec![island.clone()]),
            )
            .await
            .expect("open sqlite sync test store"),
        };
        for author in authors {
            store
                .trust_author_key(island, author.principal().clone(), author.author_key())
                .expect("trust sync test author");
        }
        store
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
    async fn fact_source_does_not_authorize_cross_island_reads_with_session_grants() {
        let (mut store, authority) = store_with_authority();
        let laptop_writer = authority.grant_in(
            island("laptop"),
            principal("writer"),
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let prod_reader = grant_prod(
            &authority,
            "reader",
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        store
            .write_fact_payload(
                &laptop_writer,
                &author,
                key("/facts/node/laptop-only/joined/1"),
                FactPayload::from_static(b"laptop"),
            )
            .await
            .expect("write laptop fact");

        let laptop_candidates = store
            .list_candidates(
                laptop_writer.island(),
                &pattern("/facts/node/>"),
                &laptop_writer,
            )
            .expect("list laptop candidates through laptop session");
        assert_eq!(laptop_candidates.len(), 1);

        let prod_candidates = store
            .list_candidates(
                laptop_writer.island(),
                &pattern("/facts/node/>"),
                &prod_reader,
            )
            .expect("list laptop candidates through prod session");
        assert!(prod_candidates.is_empty());
        let payloads = store
            .read_payloads(laptop_writer.island(), &laptop_candidates, &prod_reader)
            .expect("read laptop payloads through prod session");
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
    async fn shared_store_writes_reads_and_checks_preflight() {
        let (store, authority) = store_with_authority();
        let shared = SharedPandaFactStore::new(store);
        let session = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let readonly = grant_prod(
            &authority,
            "reader",
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let fact_key = key("/facts/node/node-1/joined/1");

        assert!(
            shared
                .try_can_write_fact(&session, &fact_key)
                .expect("preflight")
        );
        assert!(
            !shared
                .try_can_write_fact(&readonly, &fact_key)
                .expect("readonly preflight")
        );

        let inserted = shared
            .write_fact_payload(
                &session,
                &author,
                fact_key.clone(),
                FactPayload::from_static(b"joined"),
            )
            .await
            .expect("write shared fact");
        let repeated = shared
            .write_fact_payload(
                &session,
                &author,
                fact_key,
                FactPayload::from_static(b"joined"),
            )
            .await
            .expect("repeat shared fact");
        assert!(matches!(inserted, PandaFactWriteOutcome::Inserted(_)));
        assert!(matches!(repeated, PandaFactWriteOutcome::AlreadyPresent(_)));

        let candidates = shared
            .list_candidates(session.island(), &pattern("/facts/node/>"), &session)
            .expect("list candidates");
        assert_eq!(candidates.len(), 1);
        let payloads = shared
            .read_payloads(session.island(), &candidates, &session)
            .expect("read payloads");
        assert_eq!(payloads.len(), 1);
    }

    #[tokio::test]
    async fn shared_store_preserves_author_and_replica_import_modes() {
        let (source_store, authority) = store_with_authority();
        let source = SharedPandaFactStore::new(source_store);
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
                FactPayload::from_static(b"joined"),
            )
            .await
            .expect("write source fact");
        let operations = source.export_operations().await;

        let (author_import_bus, author_import_authority) = InMemoryBus::new_with_authority();
        let author_import_writer = grant_prod(
            &author_import_authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let author_import = SharedPandaFactStore::new(store_from_bus(author_import_bus));
        author_import
            .trust_author_key(
                author_import_writer.island(),
                author_import_writer.principal().clone(),
                author.author_key(),
            )
            .await
            .expect("trust author");
        let imported = author_import
            .import_operation(&author_import_writer, &operations[0])
            .await
            .expect("direct author import");
        assert!(matches!(imported, PandaFactWriteOutcome::Inserted(_)));

        let (replica_import_bus, replica_import_authority) = InMemoryBus::new_with_authority();
        let replica_import_writer = grant_prod(
            &replica_import_authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let replica_importer = grant_prod(
            &replica_import_authority,
            "replica",
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let replica_import = SharedPandaFactStore::new(store_from_bus(replica_import_bus));
        replica_import
            .trust_author_key(
                replica_import_writer.island(),
                replica_import_writer.principal().clone(),
                author.author_key(),
            )
            .await
            .expect("trust replica author");
        replica_import
            .trust_replica_peer(
                replica_importer.island(),
                replica_importer.principal().clone(),
            )
            .await;
        let imported = replica_import
            .import_replica_operation(&replica_importer, &operations[0])
            .await
            .expect("trusted replica import");
        assert!(matches!(imported, PandaFactWriteOutcome::Inserted(_)));
    }

    #[tokio::test]
    async fn shared_store_keeps_original_p2panda_write_errors() {
        let (store, authority) = store_with_authority();
        let shared = SharedPandaFactStore::new(store);
        let session = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let error = shared
            .write_fact_payload(
                &session,
                &author,
                key("/facts/node/node-1/joined/1"),
                FactPayload::from_static(b"joined"),
            )
            .await
            .expect_err("unauthorized write");
        assert!(matches!(error, PandaFactError::UnauthorizedWrite { .. }));
    }

    #[tokio::test]
    async fn shared_store_fact_source_reports_unavailable_while_write_locked() {
        let (store, authority) = store_with_authority();
        let shared = SharedPandaFactStore::new(store);
        let session = grant_prod(
            &authority,
            "reader",
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let _guard = shared.store.lock().await;

        let error = shared
            .list_candidates(session.island(), &pattern("/facts/>"), &session)
            .expect_err("locked store");
        assert!(matches!(
            error,
            FactSourceError::Unavailable { name } if name == "p2panda fact store"
        ));
        let error = shared
            .try_can_write_fact(&session, &key("/facts/node/node-1/joined/1"))
            .expect_err("locked preflight");
        assert!(matches!(
            error,
            FactSourceError::Unavailable { name } if name == "p2panda fact store"
        ));
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
    async fn sqlite_reopen_with_new_authority_fails_closed_for_removed_writer_history() {
        let directory = tempdir().expect("create tempdir");
        let path = directory.path().join("facts.sqlite");
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer_session = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
        let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
        let mut fixture =
            authority_fixture_for_writer_and_replica(writer_session.island(), &writer, &replica)
                .await;

        let mut store = PandaFactStore::open_sqlite(
            Arc::new(bus.clone()),
            authority_sqlite_config(&path, writer_session.island(), fixture.snapshot()),
        )
        .await
        .expect("open sqlite store with initial authority");
        store
            .write_fact_payload(
                &writer_session,
                &writer,
                key("/facts/authz/stale-before-removal"),
                FactPayload::from_static(b"stale-before-removal"),
            )
            .await
            .expect("writer can write before removal");
        drop(store);

        fixture
            .log
            .remove_member(
                &mut fixture.authz,
                &fixture.root,
                &fixture.root_private_key,
                authz_binding(writer_session.island(), &writer, 1).member_id(),
            )
            .await
            .expect("writer removal persists");

        let error = match PandaFactStore::open_sqlite(
            Arc::new(bus),
            authority_sqlite_config(&path, writer_session.island(), fixture.snapshot()),
        )
        .await
        {
            Ok(_) => panic!("reopen with removed writer authority should fail closed"),
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
    async fn sqlite_import_reports_out_of_order_operations_as_deferred() {
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
                key("/facts/node/sqlite-1/joined/1"),
                FactPayload::from_static(b"one"),
            )
            .await
            .expect("write first operation");
        source
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/sqlite-2/joined/1"),
                FactPayload::from_static(b"two"),
            )
            .await
            .expect("write second operation");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();
        let [_first, second] = exported.as_slice() else {
            panic!("expected two exported operations");
        };

        let directory = tempdir().expect("create tempdir");
        let mut imported = PandaFactStore::open_sqlite(
            Arc::new(bus),
            PandaSqliteOpenConfig::new(directory.path().join("facts.sqlite"), vec![island("prod")]),
        )
        .await
        .expect("open sqlite fact store");
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
    }

    #[tokio::test]
    async fn memory_import_rejects_non_incremental_sequence_numbers() {
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
                key("/facts/node/seq-1/joined/1"),
                FactPayload::from_static(b"one"),
            )
            .await
            .expect("write first operation");
        source
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/seq-2/joined/1"),
                FactPayload::from_static(b"two"),
            )
            .await
            .expect("write second operation");
        let exported = source.export_operations().cloned().collect::<Vec<_>>();
        let [first, second] = exported.as_slice() else {
            panic!("expected two exported operations");
        };
        let mut non_incremental = second.to_p2panda_operation().expect("operation decodes");
        non_incremental.header.seq_num = 99;
        non_incremental.header.signature = None;
        non_incremental.header.sign(&author.key);
        non_incremental.hash = non_incremental.header.hash();
        let non_incremental = PandaFactOperation::from_p2panda_operation(non_incremental)
            .expect("operation re-encodes");

        let mut imported = store_from_bus(bus);
        trust_author(&mut imported, &writer, &author);
        imported
            .import_operation(&writer, first)
            .await
            .expect("import first operation");
        let error = imported
            .import_operation(&writer, &non_incremental)
            .await
            .expect_err("non-incremental sequence fails");
        assert!(matches!(error, PandaFactError::OutOfOrderOperation { .. }));
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

    #[tokio::test]
    async fn p2panda_sync_imports_missing_operations_and_repeated_sync_is_noop() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        let mut left = store_from_bus(bus.clone());
        let mut right = store_from_bus(bus);
        trust_author(&mut left, &writer, &author);
        trust_author(&mut right, &writer, &author);
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        left.write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"joined"),
        )
        .await
        .expect("write source fact");
        let scope = sync_scope(&writer, &[&author]);

        let report =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect("sync stores");
        assert_eq!(report.right.received, 1);
        assert_eq!(report.right.imported, 1);
        let candidates = right
            .list_candidates(writer.island(), &pattern("/facts/node/>"), &writer)
            .expect("list synced candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status(), CandidateStatus::Verified);

        let no_op =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect("repeat sync stores");
        assert_eq!(no_op.left.received + no_op.right.received, 0);
    }

    #[tokio::test]
    async fn p2panda_sync_supports_mixed_memory_and_sqlite_backends() {
        for (left_backend, right_backend) in [
            (TestSyncBackend::Memory, TestSyncBackend::Sqlite),
            (TestSyncBackend::Sqlite, TestSyncBackend::Memory),
        ] {
            run_mixed_backend_sync_case(left_backend, right_backend).await;
        }
    }

    async fn run_mixed_backend_sync_case(
        left_backend: TestSyncBackend,
        right_backend: TestSyncBackend,
    ) {
        let directory = tempdir().expect("create tempdir");
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        let mut left = test_sync_store(
            left_backend,
            bus.clone(),
            directory
                .path()
                .join(format!("left-{}.sqlite", left_backend.name())),
            writer.island(),
            &[&author],
        )
        .await;
        let mut right = test_sync_store(
            right_backend,
            bus,
            directory
                .path()
                .join(format!("right-{}.sqlite", right_backend.name())),
            writer.island(),
            &[&author],
        )
        .await;
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        left.write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"joined"),
        )
        .await
        .expect("write mixed-backend source fact");
        let scope = sync_scope(&writer, &[&author]);

        let report =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect("sync mixed backends");
        assert_eq!(report.right.received, 1);
        assert_eq!(report.right.imported, 1);
        let candidates = right
            .list_candidates(writer.island(), &pattern("/facts/node/>"), &writer)
            .expect("list mixed-backend synced candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status(), CandidateStatus::Verified);

        let no_op =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect("repeat mixed-backend sync");
        assert_eq!(no_op.left.received + no_op.right.received, 0);
    }

    #[tokio::test]
    async fn p2panda_sync_preserves_bidirectional_conflict_candidates() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer_a = grant_prod(
            &authority,
            "writer-a",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let writer_b = grant_prod(
            &authority,
            "writer-b",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let author_a = PandaFactAuthor::new(principal("writer-a"));
        let author_b = PandaFactAuthor::new(principal("writer-b"));
        let mut left = store_from_bus(bus.clone());
        let mut right = store_from_bus(bus);
        for store in [&mut left, &mut right] {
            trust_author(store, &writer_a, &author_a);
            trust_author(store, &writer_b, &author_b);
        }
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        let fact_key = key("/facts/node/node-1/joined/1");
        left.write_fact_payload(
            &writer_a,
            &author_a,
            fact_key.clone(),
            FactPayload::from_static(b"left"),
        )
        .await
        .expect("write left fact");
        right
            .write_fact_payload(
                &writer_b,
                &author_b,
                fact_key,
                FactPayload::from_static(b"right"),
            )
            .await
            .expect("write right fact");

        let scope = sync_scope(&writer_a, &[&author_a, &author_b]);
        let report =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect("sync bidirectional stores");
        assert_eq!(report.left.conflict, 1);
        assert_eq!(report.right.conflict, 1);

        for store in [&left, &right] {
            let candidates = store
                .list_candidates(writer_a.island(), &pattern("/facts/node/>"), &writer_a)
                .expect("list conflict candidates");
            assert_eq!(candidates.len(), 2);
            assert!(
                candidates
                    .iter()
                    .all(|candidate| candidate.status() == CandidateStatus::Conflict)
            );
        }
    }

    #[tokio::test]
    async fn p2panda_sync_rejects_untrusted_replica_and_scope_key_substitution() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let untrusted_replica = grant_prod(&authority, "untrusted-replica", Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        let imposter = PandaFactAuthor::new(principal("writer"));
        let mut left = store_from_bus(bus.clone());
        let mut right = store_from_bus(bus);
        trust_author(&mut left, &writer, &author);
        trust_author(&mut right, &writer, &author);
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        let scope = sync_scope(&writer, &[&author]);
        let error = sync_panda_fact_stores(
            &mut left,
            &untrusted_replica,
            &mut right,
            &right_replica,
            &scope,
        )
        .await
        .expect_err("untrusted replica cannot start sync");
        assert!(matches!(
            error,
            PandaFactSyncError::UnauthorizedReplica {
                side: PandaFactSyncSide::Left,
                ..
            }
        ));

        let substituted = sync_scope(&writer, &[&imposter]);
        let error = sync_panda_fact_stores(
            &mut left,
            &left_replica,
            &mut right,
            &right_replica,
            &substituted,
        )
        .await
        .expect_err("scope key substitution is rejected");
        assert!(matches!(
            error,
            PandaFactSyncError::ScopeAuthorKeyMismatch {
                side: PandaFactSyncSide::Left,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn p2panda_sync_rejects_replica_island_mismatch_and_missing_scope_author() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let laptop_replica =
            authority.grant_in(island("laptop"), principal("left-replica"), Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        let mut left = store_from_bus(bus.clone());
        let mut right = store_from_bus(bus);
        trust_author(&mut left, &writer, &author);
        trust_author(&mut right, &writer, &author);
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        let scope = sync_scope(&writer, &[&author]);
        let error = sync_panda_fact_stores(
            &mut left,
            &laptop_replica,
            &mut right,
            &right_replica,
            &scope,
        )
        .await
        .expect_err("replica island mismatch is rejected");
        assert!(matches!(
            error,
            PandaFactSyncError::ReplicaIslandMismatch {
                side: PandaFactSyncSide::Left,
                ..
            }
        ));

        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        let mut left = store_from_bus(bus.clone());
        let mut right = store_from_bus(bus);
        trust_author(&mut right, &writer, &author);
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        let scope = sync_scope(&writer, &[&author]);
        let error =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect_err("missing scope author key is rejected");
        assert!(matches!(
            error,
            PandaFactSyncError::ScopeAuthorKeyMissing {
                side: PandaFactSyncSide::Left,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn p2panda_sync_rejects_received_operation_without_writer_grant() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
        let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
        let author = PandaFactAuthor::new(principal("writer"));
        let mut left = store_from_bus(bus.clone());
        let mut right = store_from_bus(bus);
        for store in [&mut left, &mut right] {
            trust_author(store, &writer, &author);
        }
        trust_replica(&mut left, &left_replica);
        trust_replica(&mut right, &right_replica);

        left.write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write source fact before grant revocation");
        authority.revoke(&writer);

        let scope = sync_scope(&writer, &[&author]);
        let error =
            sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
                .await
                .expect_err("received operation without writer grant is rejected");
        assert!(matches!(
            error,
            PandaFactSyncError::Import {
                side: PandaFactSyncSide::Right,
                source: PandaFactError::UnauthorizedWrite { .. },
            }
        ));
        assert!(
            right
                .list_candidates(writer.island(), &pattern("/facts/>"), &writer)
                .expect("list destination candidates")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn duplicate_import_rejects_same_header_with_corrupted_body() {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = grant_prod(
            &authority,
            "writer",
            Grant::empty()
                .with_fact_write(pattern("/facts/>"))
                .with_fact_read(pattern("/facts/>")),
        );
        let author = PandaFactAuthor::new(principal("writer"));
        let mut source = store_from_bus(bus.clone());
        source
            .write_fact_payload(
                &writer,
                &author,
                key("/facts/node/node-corrupt/joined/1"),
                FactPayload::from_static(b"valid-payload"),
            )
            .await
            .expect("write source operation");
        let operation = source
            .export_operations()
            .next()
            .expect("operation was recorded")
            .clone();
        let mut imported = store_from_bus(bus);
        trust_author(&mut imported, &writer, &author);
        imported
            .import_operation(&writer, &operation)
            .await
            .expect("import valid operation once");

        let corrupted =
            PandaFactOperation::new(operation.header_bytes(), b"corrupted-body".to_vec());
        let error = imported
            .import_operation(&writer, &corrupted)
            .await
            .expect_err("same signed header with changed body is rejected");
        assert!(matches!(error, PandaFactError::InvalidOperation));
    }
}
