use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use mvp_bus::{
    BusSession, FactAccess, FactAuthorizer, FactContentHash, FactKey, FactKeyPattern, FactPayload,
    IslandId, PrincipalId,
};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, FactSourceError, FactSourceResult,
    classify_fact_key,
};

use super::{IrohFactResult, backend_error};

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

    pub(super) fn upsert(&self, entry: LocalFactEntry) -> IrohFactResult<()> {
        self.entries
            .write()
            .map_err(|source| backend_error("lock local fact view", source))?
            .insert(LocalFactIdentity::from(&entry.metadata), entry);
        Ok(())
    }

    pub(super) fn record_rejected(&self, rejected: IrohRejectedFactEntry) -> IrohFactResult<()> {
        let mut rejected_entries = self
            .rejected_entries
            .write()
            .map_err(|source| backend_error("lock rejected fact entries", source))?;
        if !rejected_entries.contains(&rejected) {
            rejected_entries.push(rejected);
        }
        Ok(())
    }

    pub(super) fn contains_key(&self, island: &IslandId, key: &FactKey) -> IrohFactResult<bool> {
        Ok(self
            .entries
            .read()
            .map_err(|source| backend_error("lock local fact view", source))?
            .values()
            .any(|entry| entry.metadata.island == *island && entry.metadata.key == *key))
    }

    pub(super) fn contains_content_hash(
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

    pub(super) fn contains_payload(&self, metadata: &LocalFactMetadata) -> IrohFactResult<bool> {
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

    pub(super) fn authorized_entries_for_key(
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
    pub(super) fn invalid_entry_key_utf8(message: String) -> Self {
        Self {
            reason: IrohRejectedFactReason::InvalidEntryKeyUtf8 { message },
        }
    }

    pub(super) fn invalid_entry_key(key: String, message: String) -> Self {
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
pub(super) struct LocalFactEntry {
    pub(super) metadata: LocalFactMetadata,
    pub(super) payload: Option<FactPayload>,
}

#[derive(Clone)]
pub(super) struct LocalFactMetadata {
    pub(super) island: IslandId,
    pub(super) key: FactKey,
    pub(super) author: PrincipalId,
    pub(super) author_verified: bool,
    pub(super) content_hash: FactContentHash,
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

pub(super) fn metadata_has_write_authority(
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
