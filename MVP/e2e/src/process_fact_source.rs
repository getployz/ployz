use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use mvp_bus::{
    BusError, BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId, Payload,
    PrincipalId,
};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, FactSourceError, FactSourceResult,
    classify_fact_key,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub(crate) struct ProcessFactSource {
    root: PathBuf,
    read_policy: ProcessFactReadPolicy,
}

impl ProcessFactSource {
    pub(crate) fn new(
        root: impl Into<PathBuf>,
        read_policy: ProcessFactReadPolicy,
    ) -> Result<Self, String> {
        let source = Self {
            root: root.into(),
            read_policy,
        };
        source
            .ensure_dirs()
            .map_err(|error| format!("prepare process fact source: {error}"))?;
        Ok(source)
    }

    pub(crate) fn write_payload(
        &self,
        island: &IslandId,
        author: &PrincipalId,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<WrittenProcessFact, String> {
        let content_hash = FactContentHash::for_payload(&payload);
        let payload_path = self.payload_path(&content_hash);
        durably_replace(&payload_path, payload.as_bytes())?;

        let entry = StoredFactEntry {
            island: island.as_str().to_string(),
            key: key.as_str().to_string(),
            author: author.as_str().to_string(),
            content_hash: content_hash.as_str().to_string(),
        };
        let entry_bytes = serde_json::to_vec(&entry)
            .map_err(|error| format!("serialize process fact entry: {error}"))?;
        durably_replace(&self.entry_path(island, &key, &content_hash), &entry_bytes)?;

        Ok(WrittenProcessFact { key, content_hash })
    }

    fn ensure_dirs(&self) -> Result<(), String> {
        fs::create_dir_all(self.payloads_dir()).map_err(|error| {
            format!(
                "create payload dir '{}': {error}",
                self.payloads_dir().display()
            )
        })?;
        fs::create_dir_all(self.entries_dir()).map_err(|error| {
            format!(
                "create entry dir '{}': {error}",
                self.entries_dir().display()
            )
        })
    }

    fn payloads_dir(&self) -> PathBuf {
        self.root.join("payloads")
    }

    fn entries_dir(&self) -> PathBuf {
        self.root.join("entries")
    }

    fn payload_path(&self, content_hash: &FactContentHash) -> PathBuf {
        self.payloads_dir().join(format!(
            "{}.blob",
            encode_file_component(content_hash.as_str())
        ))
    }

    fn entry_path(
        &self,
        island: &IslandId,
        key: &FactKey,
        content_hash: &FactContentHash,
    ) -> PathBuf {
        self.entries_dir().join(format!(
            "{}__{}__{}.json",
            encode_file_component(island.as_str()),
            encode_file_component(key.as_str()),
            encode_file_component(content_hash.as_str())
        ))
    }

    fn read_entry_files(&self) -> FactSourceResult<Vec<StoredFactEntry>> {
        let mut paths = fs::read_dir(self.entries_dir())
            .map_err(|error| unavailable(format!("read process fact entries: {error}")))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| unavailable(format!("read process fact entry path: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();

        let mut entries = Vec::new();
        for path in paths {
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|error| {
                unavailable(format!(
                    "read process fact entry '{}': {error}",
                    path.display()
                ))
            })?;
            let entry = serde_json::from_slice::<StoredFactEntry>(&bytes).map_err(|error| {
                unavailable(format!(
                    "parse process fact entry '{}': {error}",
                    path.display()
                ))
            })?;
            entries.push(entry);
        }
        Ok(entries)
    }

    fn entry_payload_is_verified(&self, entry: &StoredFactEntry) -> FactSourceResult<bool> {
        let content_hash = FactContentHash::new(entry.content_hash.clone());
        let path = self.payload_path(&content_hash);
        let payload = match fs::read(&path) {
            Ok(payload) => payload,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(unavailable(format!(
                    "read process fact payload '{}': {error}",
                    path.display()
                )));
            }
        };
        Ok(FactContentHash::for_payload(&payload) == content_hash)
    }
}

impl FactSource for ProcessFactSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for entry in self.read_entry_files()? {
            if entry.island != island.as_str() {
                continue;
            }
            let key = parse_key(&entry)?;
            if pattern.matches(&key)
                && self.read_policy.can_read(session, &key)
                && self.entry_payload_is_verified(&entry)?
            {
                entries.push((entry, key));
            }
        }

        let mut counts = BTreeMap::new();
        for (_entry, key) in &entries {
            *counts.entry(key.clone()).or_insert(0usize) += 1;
        }

        let candidates = entries
            .into_iter()
            .map(|(entry, key)| {
                let status = if counts[&key] > 1 {
                    CandidateStatus::Conflict
                } else {
                    CandidateStatus::Verified
                };
                let classification = classify_fact_key(&key);
                FactCandidate::new(
                    IslandId::new(entry.island),
                    key,
                    PrincipalId::new(entry.author),
                    FactContentHash::new(entry.content_hash),
                    classification.kind(),
                    classification.epoch(),
                    status,
                )
            })
            .collect();
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
            if !self.read_policy.can_read(session, candidate.key()) {
                return Err(FactSourceError::Bus(BusError::UnauthorizedFactRead {
                    island: session.island().clone(),
                    principal: session.principal().clone(),
                    key: Box::new(candidate.key().clone()),
                }));
            }
            let path = self.payload_path(candidate.content_hash());
            let payload = match fs::read(&path) {
                Ok(payload) => payload,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(unavailable(format!(
                        "read process fact payload '{}': {error}",
                        path.display()
                    )));
                }
            };
            if FactContentHash::for_payload(&payload) != *candidate.content_hash() {
                continue;
            }
            payloads.insert(candidate.content_hash().clone(), Payload::from(payload));
        }
        Ok(payloads)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessFactReadPolicy {
    island: IslandId,
    principal: PrincipalId,
    allowed_patterns: Vec<FactKeyPattern>,
}

impl ProcessFactReadPolicy {
    pub(crate) fn allow(
        island: IslandId,
        principal: PrincipalId,
        allowed_patterns: Vec<FactKeyPattern>,
    ) -> Self {
        Self {
            island,
            principal,
            allowed_patterns,
        }
    }

    fn can_read(&self, session: &BusSession, key: &FactKey) -> bool {
        self.island == *session.island()
            && self.principal == *session.principal()
            && self
                .allowed_patterns
                .iter()
                .any(|pattern| pattern.matches(key))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WrittenProcessFact {
    pub(crate) key: FactKey,
    pub(crate) content_hash: FactContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredFactEntry {
    island: String,
    key: String,
    author: String,
    content_hash: String,
}

fn parse_key(entry: &StoredFactEntry) -> FactSourceResult<FactKey> {
    FactKey::parse(entry.key.clone())
        .map_err(|error| unavailable(format!("parse process fact key '{}': {error}", entry.key)))
}

fn durably_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("durable path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create durable parent '{}': {error}", parent.display()))?;
    let temp_path = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = File::create(&temp_path)
            .map_err(|error| format!("create temp file '{}': {error}", temp_path.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("write temp file '{}': {error}", temp_path.display()))?;
        file.sync_all()
            .map_err(|error| format!("sync temp file '{}': {error}", temp_path.display()))?;
    }
    fs::rename(&temp_path, path).map_err(|error| {
        format!(
            "rename temp file '{}' to '{}': {error}",
            temp_path.display(),
            path.display()
        )
    })?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .map_err(|error| format!("open dir '{}': {error}", path.display()))?
        .sync_all()
        .map_err(|error| format!("sync dir '{}': {error}", path.display()))
}

fn encode_file_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("_{byte:02x}")),
        }
    }
    encoded
}

fn unavailable(name: String) -> FactSourceError {
    FactSourceError::Unavailable { name }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mvp_bus::{FactKeyPattern, Grant};
    use mvp_projection::{CandidateStatus, FactKind, FactSource};

    use super::*;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn root(test_name: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ployz-mvp-process-fact-source-{test_name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn island(value: &str) -> IslandId {
        IslandId::new(value)
    }

    fn principal(value: &str) -> PrincipalId {
        PrincipalId::new(value)
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("fact key parses")
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact pattern parses")
    }

    fn identity_session(principal_id: &str) -> BusSession {
        let (_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        authority.grant_in(island("prod"), principal(principal_id), Grant::empty())
    }

    fn source(test_name: &str) -> ProcessFactSource {
        ProcessFactSource::new(
            root(test_name),
            ProcessFactReadPolicy::allow(
                island("prod"),
                principal("projection"),
                vec![pattern("/facts/>")],
            ),
        )
        .expect("process fact source creates")
    }

    #[test]
    fn lists_verified_candidates_and_reads_payloads() {
        let source = source("verified");
        let payload = Payload::from_static(br#"{"kind":"serving_commit"}"#);
        let written = source
            .write_payload(
                &island("prod"),
                &principal("coordinator"),
                key("/facts/serving/serving-1"),
                payload.clone(),
            )
            .expect("write payload");

        let candidates = source
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list candidates");
        let payloads = source
            .read_payloads(
                &island("prod"),
                &candidates,
                &identity_session("projection"),
            )
            .expect("read payloads");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].key(), &written.key);
        assert_eq!(candidates[0].kind(), FactKind::ServingCommit);
        assert_eq!(candidates[0].status(), CandidateStatus::Verified);
        assert_eq!(
            payloads.get(&written.content_hash).expect("payload exists"),
            &payload
        );
    }

    #[test]
    fn wrong_island_lists_no_candidates() {
        let source = source("wrong-island");
        source
            .write_payload(
                &island("prod"),
                &principal("coordinator"),
                key("/facts/serving/serving-1"),
                Payload::from_static(b"payload"),
            )
            .expect("write payload");

        let candidates = source
            .list_candidates(
                &island("laptop"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list candidates");

        assert!(candidates.is_empty());
    }

    #[test]
    fn duplicate_keys_are_returned_as_conflict_candidates() {
        let source = source("conflict");
        let fact_key = key("/facts/serving/serving-1");
        let first = source
            .write_payload(
                &island("prod"),
                &principal("coordinator-a"),
                fact_key.clone(),
                Payload::from_static(b"first"),
            )
            .expect("write first payload");
        let second = source
            .write_payload(
                &island("prod"),
                &principal("coordinator-b"),
                fact_key,
                Payload::from_static(b"second"),
            )
            .expect("write second payload");

        let candidates = source
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list candidates");
        let payloads = source
            .read_payloads(
                &island("prod"),
                &candidates,
                &identity_session("projection"),
            )
            .expect("read payloads");

        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.status() == CandidateStatus::Conflict)
        );
        assert!(payloads.contains_key(&first.content_hash));
        assert!(payloads.contains_key(&second.content_hash));
    }

    #[test]
    fn facts_are_readable_after_reopening_source() {
        let root = root("durable");
        let source = ProcessFactSource::new(
            root.clone(),
            ProcessFactReadPolicy::allow(
                island("prod"),
                principal("projection"),
                vec![pattern("/facts/>")],
            ),
        )
        .expect("source creates");
        source
            .write_payload(
                &island("prod"),
                &principal("coordinator"),
                key("/facts/serving/serving-1"),
                Payload::from_static(b"payload"),
            )
            .expect("write payload");

        let reopened = ProcessFactSource::new(
            root,
            ProcessFactReadPolicy::allow(
                island("prod"),
                principal("projection"),
                vec![pattern("/facts/>")],
            ),
        )
        .expect("source reopens");
        let candidates = reopened
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list candidates");

        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn partial_entries_are_ignored_without_success_claims() {
        let source = source("partial");
        let missing_payload = FactContentHash::new("b3:missing");
        let entry = StoredFactEntry {
            island: "prod".to_string(),
            key: "/facts/serving/missing".to_string(),
            author: "coordinator".to_string(),
            content_hash: missing_payload.as_str().to_string(),
        };
        fs::write(
            source.entry_path(
                &island("prod"),
                &key("/facts/serving/missing"),
                &missing_payload,
            ),
            serde_json::to_vec(&entry).expect("entry serializes"),
        )
        .expect("write missing-payload entry");

        let mismatched_hash = FactContentHash::new("b3:mismatched");
        let mismatched_entry = StoredFactEntry {
            island: "prod".to_string(),
            key: "/facts/serving/mismatched".to_string(),
            author: "coordinator".to_string(),
            content_hash: mismatched_hash.as_str().to_string(),
        };
        fs::write(source.payload_path(&mismatched_hash), b"different bytes")
            .expect("write mismatched payload");
        fs::write(
            source.entry_path(
                &island("prod"),
                &key("/facts/serving/mismatched"),
                &mismatched_hash,
            ),
            serde_json::to_vec(&mismatched_entry).expect("entry serializes"),
        )
        .expect("write mismatched entry");
        let orphan_payload = FactContentHash::for_payload(&Payload::from_static(b"orphan"));
        fs::write(source.payload_path(&orphan_payload), b"orphan").expect("write orphan payload");

        let candidates = source
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list candidates");

        assert!(candidates.is_empty());
    }

    #[test]
    fn read_policy_filters_candidates_and_denies_payload_reads() {
        let source = source("read-policy");
        source
            .write_payload(
                &island("prod"),
                &principal("coordinator"),
                key("/facts/serving/serving-1"),
                Payload::from_static(b"payload"),
            )
            .expect("write payload");

        let intruder_candidates = source
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("intruder"),
            )
            .expect("list intruder candidates");
        assert!(intruder_candidates.is_empty());

        let projection_candidates = source
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list projection candidates");
        let error = source
            .read_payloads(
                &island("prod"),
                &projection_candidates,
                &identity_session("intruder"),
            )
            .expect_err("unauthorized reader should not read payloads");

        assert!(matches!(
            error,
            FactSourceError::Bus(BusError::UnauthorizedFactRead { .. })
        ));
    }

    #[test]
    fn unmatched_entries_do_not_require_payload_verification() {
        let source = source("unmatched-payload");
        let missing_payload = FactContentHash::new("b3:missing");
        let entry = StoredFactEntry {
            island: "prod".to_string(),
            key: "/facts/node/node-1/joined/1".to_string(),
            author: "coordinator".to_string(),
            content_hash: missing_payload.as_str().to_string(),
        };
        fs::write(
            source.entry_path(
                &island("prod"),
                &key("/facts/node/node-1/joined/1"),
                &missing_payload,
            ),
            serde_json::to_vec(&entry).expect("entry serializes"),
        )
        .expect("write unmatched missing-payload entry");

        source
            .write_payload(
                &island("prod"),
                &principal("coordinator"),
                key("/facts/serving/serving-1"),
                Payload::from_static(b"payload"),
            )
            .expect("write matching payload");

        let candidates = source
            .list_candidates(
                &island("prod"),
                &pattern("/facts/serving/>"),
                &identity_session("projection"),
            )
            .expect("list candidates");

        assert_eq!(candidates.len(), 1);
    }
}
