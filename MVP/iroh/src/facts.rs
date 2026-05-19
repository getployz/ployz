use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use iroh::{Endpoint, endpoint::presets, protocol::Router};
use iroh_blobs::{
    ALPN as BLOBS_ALPN, BlobsProtocol, Hash as IrohHash, api::Store as BlobStore,
    store::mem::MemStore,
};
use iroh_docs::{
    ALPN as DOCS_ALPN, AuthorId, DocTicket, Entry,
    api::{
        Doc, DocsApi,
        protocol::{AddrInfoOptions, ShareMode},
    },
    protocol::Docs,
    store::Query,
};
use iroh_gossip::{ALPN as GOSSIP_ALPN, net::Gossip};
use mvp_bus::{
    BusSession, FactAccess, FactAuthorizer, FactContentHash, FactKey, FactKeyPattern, FactPayload,
    IslandId, PrincipalId,
};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, FactSourceError, FactSourceResult,
    classify_fact_key,
};
use n0_future::TryStreamExt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IrohFactError {
    #[error("iroh fact operation {operation} failed: {message}")]
    Backend {
        operation: &'static str,
        message: String,
    },
    #[error("iroh fact operation {operation} is unavailable")]
    Unavailable { operation: &'static str },
    #[error("iroh fact sync timed out during {operation} after {timeout:?}")]
    Timeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("principal {principal} cannot write fact {key} in island {island}")]
    UnauthorizedWrite {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
    },
    #[error("iroh docs entry key is not valid UTF-8: {message}")]
    InvalidEntryKeyUtf8 { message: String },
    #[error("iroh docs entry key {key} is not a valid fact key: {message}")]
    InvalidEntryKey { key: String, message: String },
}

pub type IrohFactResult<T> = std::result::Result<T, IrohFactError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrohFactMetadata {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    content_hash: FactContentHash,
}

impl IrohFactMetadata {
    fn from_local(metadata: &LocalFactMetadata) -> Self {
        Self {
            island: metadata.island.clone(),
            key: metadata.key.clone(),
            author: metadata.author.clone(),
            content_hash: metadata.content_hash.clone(),
        }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrohImmutableWriteOutcome {
    Inserted(IrohFactMetadata),
    AlreadyPresent(IrohFactMetadata),
    Conflict(IrohFactMetadata),
}

pub struct IrohFactNode {
    router: Router,
    docs: DocsApi,
    blobs: BlobStore,
    local_view: IrohFactLocalView,
}

impl IrohFactNode {
    pub async fn memory() -> IrohFactResult<Self> {
        let endpoint = Endpoint::bind(presets::Minimal)
            .await
            .map_err(|source| backend_error("bind endpoint", source))?;
        let blobs = MemStore::default();
        let gossip = Gossip::builder().spawn(endpoint.clone());
        let docs_protocol = Docs::memory()
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await
            .map_err(|source| backend_error("spawn docs", source))?;
        let docs = docs_protocol.api().clone();
        let router = Router::builder(endpoint.clone())
            .accept(BLOBS_ALPN, BlobsProtocol::new(&blobs, None))
            .accept(GOSSIP_ALPN, gossip)
            .accept(DOCS_ALPN, docs_protocol)
            .spawn();

        Ok(Self {
            router,
            docs,
            blobs: (*blobs).clone(),
            local_view: IrohFactLocalView::default(),
        })
    }

    pub async fn create_author(&self, principal: PrincipalId) -> IrohFactResult<IrohFactAuthor> {
        let raw = self
            .docs
            .author_create()
            .await
            .map_err(|source| backend_error("create author", source))?;
        Ok(IrohFactAuthor { raw, principal })
    }

    pub async fn create_fact_doc(&self, island: IslandId) -> IrohFactResult<IrohFactDoc> {
        let doc = self
            .docs
            .create()
            .await
            .map_err(|source| backend_error("create doc", source))?;
        Ok(IrohFactDoc::new(
            island,
            doc,
            self.blobs.clone(),
            self.local_view.clone(),
            BTreeMap::new(),
        ))
    }

    pub async fn import_fact_doc(&self, ticket: IrohFactDocTicket) -> IrohFactResult<IrohFactDoc> {
        let doc = self
            .docs
            .import(ticket.raw)
            .await
            .map_err(|source| backend_error("import doc", source))?;
        Ok(IrohFactDoc::new(
            ticket.island,
            doc,
            self.blobs.clone(),
            self.local_view.clone(),
            ticket.author_bindings,
        ))
    }

    #[must_use]
    pub fn local_view(&self) -> IrohFactLocalView {
        self.local_view.clone()
    }

    pub async fn shutdown(self) -> IrohFactResult<()> {
        self.router
            .shutdown()
            .await
            .map_err(|source| backend_error("shutdown router", source))
    }
}

#[derive(Debug, Clone)]
pub struct IrohFactAuthor {
    raw: AuthorId,
    principal: PrincipalId,
}

impl IrohFactAuthor {
    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }
}

#[derive(Clone)]
pub struct IrohFactDoc {
    island: IslandId,
    doc: Doc,
    blobs: BlobStore,
    local_view: IrohFactLocalView,
    author_bindings: Arc<RwLock<BTreeMap<AuthorId, PrincipalId>>>,
}

impl IrohFactDoc {
    fn new(
        island: IslandId,
        doc: Doc,
        blobs: BlobStore,
        local_view: IrohFactLocalView,
        author_bindings: BTreeMap<AuthorId, PrincipalId>,
    ) -> Self {
        Self {
            island,
            doc,
            blobs,
            local_view,
            author_bindings: Arc::new(RwLock::new(author_bindings)),
        }
    }

    pub fn bind_author(&self, author: &IrohFactAuthor) -> IrohFactResult<()> {
        self.author_bindings
            .write()
            .map_err(|source| backend_error("lock author bindings", source))?
            .insert(author.raw, author.principal.clone());
        Ok(())
    }

    fn authorize_and_bind_write(
        &self,
        author: &IrohFactAuthor,
        key: &FactKey,
        authorizer: &dyn FactAuthorizer,
    ) -> IrohFactResult<()> {
        if !authorizer.can_principal_access_fact(
            &self.island,
            author.principal(),
            key,
            FactAccess::Write,
        ) {
            return Err(IrohFactError::UnauthorizedWrite {
                island: self.island.clone(),
                principal: author.principal().clone(),
                key: key.clone(),
            });
        }

        self.bind_author(author)
    }

    pub async fn write_fact_payload(
        &self,
        author: &IrohFactAuthor,
        key: FactKey,
        payload: FactPayload,
        authorizer: &dyn FactAuthorizer,
    ) -> IrohFactResult<FactContentHash> {
        self.authorize_and_bind_write(author, &key, authorizer)?;
        let content_hash = self
            .doc
            .set_bytes(
                author.raw,
                key.as_str().as_bytes().to_vec(),
                payload.as_bytes().to_vec(),
            )
            .await
            .map_err(|source| backend_error("write fact payload", source))?;
        let fact_hash = iroh_hash_to_fact_hash(content_hash);
        self.local_view.upsert(LocalFactEntry {
            metadata: LocalFactMetadata {
                island: self.island.clone(),
                key,
                author: author.principal().clone(),
                author_verified: true,
                content_hash: fact_hash.clone(),
            },
            payload: Some(payload),
        })?;
        Ok(fact_hash)
    }

    pub async fn write_immutable_fact_payload(
        &self,
        author: &IrohFactAuthor,
        key: FactKey,
        payload: FactPayload,
        authorizer: &dyn FactAuthorizer,
    ) -> IrohFactResult<IrohImmutableWriteOutcome> {
        self.authorize_and_bind_write(author, &key, authorizer)?;
        self.refresh_key(&key).await?;
        let content_hash = FactContentHash::for_payload(&payload);
        let authorized =
            self.local_view
                .authorized_entries_for_key(&self.island, &key, authorizer)?;
        if let Some(existing) = authorized
            .iter()
            .find(|metadata| metadata.content_hash == content_hash)
        {
            return Ok(IrohImmutableWriteOutcome::AlreadyPresent(
                IrohFactMetadata::from_local(existing),
            ));
        }
        if let Some(existing) = authorized.first() {
            return Ok(IrohImmutableWriteOutcome::Conflict(
                IrohFactMetadata::from_local(existing),
            ));
        }

        let iroh_hash = self
            .doc
            .set_bytes(
                author.raw,
                key.as_str().as_bytes().to_vec(),
                payload.as_bytes().to_vec(),
            )
            .await
            .map_err(|source| backend_error("write immutable fact payload", source))?;
        let written = LocalFactMetadata {
            island: self.island.clone(),
            key,
            author: author.principal().clone(),
            author_verified: true,
            content_hash: iroh_hash_to_fact_hash(iroh_hash),
        };
        self.local_view.upsert(LocalFactEntry {
            metadata: written.clone(),
            payload: Some(payload),
        })?;
        Ok(IrohImmutableWriteOutcome::Inserted(
            IrohFactMetadata::from_local(&written),
        ))
    }

    pub async fn share(&self) -> IrohFactResult<IrohFactDocTicket> {
        let raw = self
            .doc
            .share(ShareMode::Write, AddrInfoOptions::RelayAndAddresses)
            .await
            .map_err(|source| backend_error("share doc", source))?;
        let author_bindings = self
            .author_bindings
            .read()
            .map_err(|source| backend_error("lock author bindings", source))?
            .clone();
        Ok(IrohFactDocTicket {
            island: self.island.clone(),
            raw,
            author_bindings,
        })
    }

    pub async fn refresh_local_view(&self) -> IrohFactResult<usize> {
        self.refresh_query(Query::all()).await
    }

    pub async fn refresh_fact_key(&self, key: &FactKey) -> IrohFactResult<usize> {
        self.refresh_key(key).await
    }

    async fn refresh_key(&self, key: &FactKey) -> IrohFactResult<usize> {
        self.refresh_query(Query::key_exact(key.as_str().as_bytes()))
            .await
    }

    async fn refresh_query(&self, query: impl Into<Query>) -> IrohFactResult<usize> {
        let entries = self
            .doc
            .get_many(query)
            .await
            .map_err(|source| backend_error("query doc entries", source))?;
        tokio::pin!(entries);
        let mut applied = 0usize;

        while let Some(entry) = entries
            .as_mut()
            .try_next()
            .await
            .map_err(|source| backend_error("read doc entry", source))?
        {
            let local_entry = match self.entry_to_local(entry).await {
                Ok(local_entry) => local_entry,
                Err(IrohFactError::InvalidEntryKeyUtf8 { message }) => {
                    self.local_view
                        .record_rejected(IrohRejectedFactEntry::invalid_entry_key_utf8(message))?;
                    continue;
                }
                Err(IrohFactError::InvalidEntryKey { key, message }) => {
                    self.local_view
                        .record_rejected(IrohRejectedFactEntry::invalid_entry_key(key, message))?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let Some(local_entry) = local_entry else {
                continue;
            };
            self.local_view.upsert(local_entry)?;
            applied += 1;
        }

        Ok(applied)
    }

    pub async fn wait_for_key(&self, key: &FactKey, timeout: Duration) -> IrohFactResult<()> {
        self.wait_for_refreshed_key(key, timeout, "wait for fact key", |view, island, key| {
            view.contains_key(island, key)
        })
        .await
    }

    pub async fn wait_for_content_hash(
        &self,
        key: &FactKey,
        content_hash: &FactContentHash,
        timeout: Duration,
    ) -> IrohFactResult<()> {
        self.wait_for_refreshed_key(
            key,
            timeout,
            "wait for fact content hash",
            |view, island, key| view.contains_content_hash(island, key, content_hash),
        )
        .await
    }

    async fn wait_for_refreshed_key(
        &self,
        key: &FactKey,
        timeout: Duration,
        operation: &'static str,
        found: impl Fn(&IrohFactLocalView, &IslandId, &FactKey) -> IrohFactResult<bool>,
    ) -> IrohFactResult<()> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.refresh_key(key)).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err(IrohFactError::Timeout { operation, timeout });
                }
            };
            if found(&self.local_view, &self.island, key)? {
                return Ok(());
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(25))).await;
        }

        Err(IrohFactError::Timeout { operation, timeout })
    }

    async fn entry_to_local(&self, entry: Entry) -> IrohFactResult<Option<LocalFactEntry>> {
        let metadata = self.entry_metadata(&entry)?;
        if self.local_view.contains_payload(&metadata)? {
            return Ok(None);
        }
        let payload = self
            .blobs
            .get_bytes(entry.content_hash())
            .await
            .ok()
            .map(|bytes| bytes.to_vec().into());

        Ok(Some(LocalFactEntry { metadata, payload }))
    }

    fn entry_metadata(&self, entry: &Entry) -> IrohFactResult<LocalFactMetadata> {
        let raw_key = std::str::from_utf8(entry.key()).map_err(|source| {
            IrohFactError::InvalidEntryKeyUtf8 {
                message: source.to_string(),
            }
        })?;
        let key = FactKey::parse(raw_key.to_string()).map_err(|source| {
            IrohFactError::InvalidEntryKey {
                key: raw_key.to_string(),
                message: source.to_string(),
            }
        })?;
        let (author, author_verified) = self.principal_for_author(entry.author())?;

        Ok(LocalFactMetadata {
            island: self.island.clone(),
            key,
            author,
            author_verified,
            content_hash: iroh_hash_to_fact_hash(entry.content_hash()),
        })
    }

    fn principal_for_author(&self, author: AuthorId) -> IrohFactResult<(PrincipalId, bool)> {
        let bindings = self
            .author_bindings
            .read()
            .map_err(|source| backend_error("lock author bindings", source))?;
        if let Some(principal) = bindings.get(&author) {
            return Ok((principal.clone(), true));
        }

        Ok((
            PrincipalId::new(format!("iroh-author:{}", bytes_to_hex(author.as_bytes()))),
            false,
        ))
    }
}

#[derive(Clone)]
pub struct IrohFactDocTicket {
    island: IslandId,
    raw: DocTicket,
    author_bindings: BTreeMap<AuthorId, PrincipalId>,
}

#[derive(Clone, Default)]
pub struct IrohFactLocalView {
    entries: Arc<RwLock<BTreeMap<LocalFactIdentity, LocalFactEntry>>>,
    rejected_entries: Arc<RwLock<Vec<IrohRejectedFactEntry>>>,
}

impl IrohFactLocalView {
    pub fn rejected_entries(&self) -> IrohFactResult<Vec<IrohRejectedFactEntry>> {
        self.rejected_entries
            .read()
            .map_err(|source| backend_error("lock rejected fact entries", source))
            .map(|entries| entries.clone())
    }

    fn upsert(&self, entry: LocalFactEntry) -> IrohFactResult<()> {
        self.entries
            .write()
            .map_err(|source| backend_error("lock local fact view", source))?
            .insert(LocalFactIdentity::from(&entry.metadata), entry);
        Ok(())
    }

    fn record_rejected(&self, rejected: IrohRejectedFactEntry) -> IrohFactResult<()> {
        let mut rejected_entries = self
            .rejected_entries
            .write()
            .map_err(|source| backend_error("lock rejected fact entries", source))?;
        if !rejected_entries.contains(&rejected) {
            rejected_entries.push(rejected);
        }
        Ok(())
    }

    fn contains_key(&self, island: &IslandId, key: &FactKey) -> IrohFactResult<bool> {
        Ok(self
            .entries
            .read()
            .map_err(|source| backend_error("lock local fact view", source))?
            .values()
            .any(|entry| entry.metadata.island == *island && entry.metadata.key == *key))
    }

    fn contains_content_hash(
        &self,
        island: &IslandId,
        key: &FactKey,
        content_hash: &FactContentHash,
    ) -> IrohFactResult<bool> {
        Ok(self
            .entries
            .read()
            .map_err(|source| backend_error("lock local fact view", source))?
            .values()
            .any(|entry| {
                entry.metadata.island == *island
                    && entry.metadata.key == *key
                    && entry.metadata.content_hash == *content_hash
                    && entry.payload.is_some()
            }))
    }

    fn contains_payload(&self, metadata: &LocalFactMetadata) -> IrohFactResult<bool> {
        let identity = LocalFactIdentity::from(metadata);
        Ok(self
            .entries
            .read()
            .map_err(|source| backend_error("lock local fact view", source))?
            .get(&identity)
            .is_some_and(|entry| {
                entry.metadata.content_hash == metadata.content_hash && entry.payload.is_some()
            }))
    }

    fn list(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
    ) -> FactSourceResult<Vec<LocalFactMetadata>> {
        Ok(self
            .entries
            .read()
            .map_err(|source| FactSourceError::Unavailable {
                name: format!("iroh local view lock: {source}"),
            })?
            .values()
            .map(|entry| &entry.metadata)
            .filter(|metadata| metadata.island == *island && pattern.matches(&metadata.key))
            .cloned()
            .collect())
    }

    fn read_authorized_payloads(
        &self,
        candidates: &[FactCandidate],
        authorizer: &dyn FactAuthorizer,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        let entries = self
            .entries
            .read()
            .map_err(|source| FactSourceError::Unavailable {
                name: format!("iroh local view lock: {source}"),
            })?;
        let mut payloads = BTreeMap::new();
        for candidate in candidates {
            let identity = LocalFactIdentity {
                island: candidate.island().clone(),
                key: candidate.key().clone(),
                author: candidate.author().clone(),
            };
            let Some(entry) = entries.get(&identity) else {
                continue;
            };
            if entry.metadata.content_hash != *candidate.content_hash() {
                continue;
            }
            if !metadata_has_write_authority(&entry.metadata, authorizer) {
                continue;
            }
            let Some(payload) = &entry.payload else {
                continue;
            };
            payloads.insert(candidate.content_hash().clone(), payload.clone());
        }
        Ok(payloads)
    }

    fn authorized_entries_for_key(
        &self,
        island: &IslandId,
        key: &FactKey,
        authorizer: &dyn FactAuthorizer,
    ) -> IrohFactResult<Vec<LocalFactMetadata>> {
        Ok(self
            .entries
            .read()
            .map_err(|source| backend_error("lock local fact view", source))?
            .values()
            .map(|entry| &entry.metadata)
            .filter(|metadata| {
                metadata.island == *island
                    && metadata.key == *key
                    && metadata_has_write_authority(metadata, authorizer)
            })
            .cloned()
            .collect())
    }
}

#[derive(Clone)]
pub struct IrohDocsFactSource {
    local_view: IrohFactLocalView,
    authorizer: Arc<dyn FactAuthorizer>,
}

impl IrohDocsFactSource {
    #[must_use]
    pub fn new(local_view: IrohFactLocalView, authorizer: Arc<dyn FactAuthorizer>) -> Self {
        Self {
            local_view,
            authorizer,
        }
    }
}

impl FactSource for IrohDocsFactSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }

        let entries = self
            .local_view
            .list(island, pattern)?
            .into_iter()
            .filter(|entry| {
                self.authorizer
                    .can_session_access_fact(session, &entry.key, FactAccess::Read)
            })
            .collect::<Vec<_>>();
        let mut authorized_counts = BTreeMap::new();
        for entry in &entries {
            if metadata_has_write_authority(entry, self.authorizer.as_ref()) {
                *authorized_counts
                    .entry((entry.island.clone(), entry.key.clone()))
                    .or_insert(0usize) += 1;
            }
        }

        Ok(entries
            .into_iter()
            .map(|entry| {
                let conflict = metadata_has_write_authority(&entry, self.authorizer.as_ref())
                    && authorized_counts
                        .get(&(entry.island.clone(), entry.key.clone()))
                        .copied()
                        .unwrap_or(0)
                        > 1;
                local_entry_to_candidate(entry, conflict, self.authorizer.as_ref())
            })
            .collect())
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

        let authorized_candidates = candidates
            .iter()
            .filter(|candidate| {
                candidate.island() == island
                    && self.authorizer.can_session_access_fact(
                        session,
                        candidate.key(),
                        FactAccess::Read,
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        self.local_view
            .read_authorized_payloads(&authorized_candidates, self.authorizer.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrohRejectedFactEntry {
    reason: IrohRejectedFactReason,
}

impl IrohRejectedFactEntry {
    fn invalid_entry_key_utf8(message: String) -> Self {
        Self {
            reason: IrohRejectedFactReason::InvalidEntryKeyUtf8 { message },
        }
    }

    fn invalid_entry_key(key: String, message: String) -> Self {
        Self {
            reason: IrohRejectedFactReason::InvalidEntryKey { key, message },
        }
    }

    #[must_use]
    pub fn reason(&self) -> &IrohRejectedFactReason {
        &self.reason
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrohRejectedFactReason {
    InvalidEntryKeyUtf8 { message: String },
    InvalidEntryKey { key: String, message: String },
}

#[derive(Clone)]
struct LocalFactEntry {
    metadata: LocalFactMetadata,
    payload: Option<FactPayload>,
}

#[derive(Clone)]
struct LocalFactMetadata {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    author_verified: bool,
    content_hash: FactContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LocalFactIdentity {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
}

impl From<&LocalFactMetadata> for LocalFactIdentity {
    fn from(metadata: &LocalFactMetadata) -> Self {
        Self {
            island: metadata.island.clone(),
            key: metadata.key.clone(),
            author: metadata.author.clone(),
        }
    }
}

fn metadata_has_write_authority(
    metadata: &LocalFactMetadata,
    authorizer: &dyn FactAuthorizer,
) -> bool {
    metadata.author_verified
        && authorizer.can_principal_access_fact(
            &metadata.island,
            &metadata.author,
            &metadata.key,
            FactAccess::Write,
        )
}

fn local_entry_to_candidate(
    metadata: LocalFactMetadata,
    conflict: bool,
    authorizer: &dyn FactAuthorizer,
) -> FactCandidate {
    let status = if !metadata.author_verified {
        CandidateStatus::Unverified
    } else if !metadata_has_write_authority(&metadata, authorizer) {
        CandidateStatus::Unauthorized
    } else if conflict {
        CandidateStatus::Conflict
    } else {
        CandidateStatus::Verified
    };
    let classification = classify_fact_key(&metadata.key);

    FactCandidate::new(
        metadata.island.clone(),
        metadata.key.clone(),
        metadata.author.clone(),
        metadata.content_hash.clone(),
        classification.kind(),
        classification.epoch(),
        status,
    )
}

fn iroh_hash_to_fact_hash(hash: IrohHash) -> FactContentHash {
    FactContentHash::new(format!("b3:{}", hash.to_hex()))
}

fn backend_error(operation: &'static str, source: impl std::fmt::Display) -> IrohFactError {
    IrohFactError::Backend {
        operation,
        message: source.to_string(),
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests;
