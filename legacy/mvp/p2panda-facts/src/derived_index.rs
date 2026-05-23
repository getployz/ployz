use super::*;

pub(super) struct DerivedFactIndex {
    facts_by_key: BTreeMap<(IslandId, FactKey), BTreeSet<FactContentHash>>,
    facts: Vec<StoredFactOperation>,
    facts_by_identity: BTreeMap<StoredFactIdentity, usize>,
    facts_by_key_hash: BTreeMap<StoredFactKeyHash, usize>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
}

impl DerivedFactIndex {
    #[must_use]
    pub(super) fn empty() -> Self {
        Self {
            facts_by_key: BTreeMap::new(),
            facts: Vec::new(),
            facts_by_identity: BTreeMap::new(),
            facts_by_key_hash: BTreeMap::new(),
            payloads: BTreeMap::new(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.facts_by_key.clear();
        self.facts.clear();
        self.facts_by_identity.clear();
        self.facts_by_key_hash.clear();
        self.payloads.clear();
    }

    pub(super) fn pre_ingest_status(&self, metadata: &PandaFactMetadata) -> FactPreIngestStatus {
        let Some(existing) = self
            .facts_by_key
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

    pub(super) fn record_fact_operation(
        &mut self,
        metadata: PandaFactMetadata,
        payload: FactPayload,
        status: FactPreIngestStatus,
    ) -> PandaFactWriteOutcome {
        if status == FactPreIngestStatus::AlreadyPresent {
            return PandaFactWriteOutcome::AlreadyPresent(metadata);
        }
        self.facts_by_key
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

    pub(super) fn list_candidates(
        &self,
        authorizer: &dyn FactAuthorizer,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> Vec<FactCandidate> {
        if let Ok(exact_key) = FactKey::parse(pattern.as_str()) {
            return self.exact_candidates(authorizer, island, &exact_key, pattern, session);
        }
        self.facts
            .iter()
            .filter(|stored| stored.metadata.island == *island)
            .filter_map(|stored| self.candidate_for(authorizer, stored, island, pattern, session))
            .collect()
    }

    pub(super) fn read_payloads(
        &self,
        authorizer: &dyn FactAuthorizer,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> BTreeMap<FactContentHash, FactPayload> {
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
            let Some(current) = self.candidate_for(
                authorizer,
                stored,
                island,
                &exact_pattern(candidate),
                session,
            ) else {
                continue;
            };
            if current != *candidate || !candidate_payload_is_readable(current.status()) {
                continue;
            }
            if let Some(payload) = self.payloads.get(&stored.metadata.content_hash) {
                payloads.insert(stored.metadata.content_hash.clone(), payload.clone());
            }
        }
        payloads
    }

    fn exact_candidates(
        &self,
        authorizer: &dyn FactAuthorizer,
        island: &IslandId,
        key: &FactKey,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> Vec<FactCandidate> {
        let Some(content_hashes) = self.facts_by_key.get(&(island.clone(), key.clone())) else {
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
                    .and_then(|stored| {
                        self.candidate_for(authorizer, stored, island, pattern, session)
                    })
            })
            .collect()
    }

    fn candidate_for(
        &self,
        authorizer: &dyn FactAuthorizer,
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
        } else if !authorizer.can_session_access_fact(
            session,
            &stored.metadata.key,
            FactAccess::Read,
        ) {
            CandidateStatus::Unauthorized
        } else if !authorizer.can_principal_access_fact(
            &stored.metadata.island,
            &stored.metadata.author,
            &stored.metadata.key,
            FactAccess::Write,
        ) {
            CandidateStatus::Unverified
        } else if self
            .facts_by_key
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FactPreIngestStatus {
    AlreadyPresent,
    Inserted,
    Conflict,
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
