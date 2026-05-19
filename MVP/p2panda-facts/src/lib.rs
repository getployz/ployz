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
use mvp_p2panda_authz::{
    IslandAuthoritySnapshot, IslandAuthzError, IslandAuthzStore, IslandMemberAuthorKey,
    IslandMemberEpoch,
};
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
pub struct PandaFactAuthoritySource {
    snapshots: Vec<IslandAuthoritySnapshot>,
}

impl PandaFactAuthoritySource {
    #[must_use]
    pub fn from_snapshots(snapshots: Vec<IslandAuthoritySnapshot>) -> Self {
        Self { snapshots }
    }

    pub async fn from_membership_store(
        store: &IslandAuthzStore,
    ) -> std::result::Result<Self, PandaAuthoritySourceError> {
        Ok(Self {
            snapshots: vec![store.authority_snapshot().await?],
        })
    }

    fn into_snapshots(self) -> Vec<IslandAuthoritySnapshot> {
        self.snapshots
    }
}

#[derive(Debug, Error)]
pub enum PandaAuthoritySourceError {
    #[error(transparent)]
    Authz(#[from] IslandAuthzError),
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
    pub fn with_authority_source(mut self, authority: PandaFactAuthoritySource) -> Self {
        self.authority_snapshots.extend(authority.into_snapshots());
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

mod store_runtime;

use store_runtime::PandaBackendIngest;
pub use store_runtime::{SharedPandaFactStore, sync_panda_fact_stores};
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
mod tests;
