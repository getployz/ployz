use std::collections::{BTreeMap, BTreeSet};

use mvp_bus::{FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId, PrincipalId};
use mvp_projection::{CandidateStatus, FactCandidate, classify_fact_key};
use p2panda_core::{Body, Header, PrivateKey, PublicKey, validate_operation};
use p2panda_store::{LogStore, MemoryStore};
use p2panda_stream::operation::{IngestError, IngestResult, ingest_operation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PloyzPandaFactError {
    #[error("p2panda ingest failed: {0}")]
    Ingest(#[from] IngestError),
    #[error("p2panda operation failed validation")]
    InvalidOperation,
    #[error("p2panda store failed: {message}")]
    Store { message: String },
}

pub type Result<T> = std::result::Result<T, PloyzPandaFactError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IslandLog(String);

impl From<&IslandId> for IslandLog {
    fn from(value: &IslandId) -> Self {
        Self(value.as_str().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PloyzFactExtensions {
    island: String,
    key: String,
    author: String,
}

impl PloyzFactExtensions {
    fn new(island: &IslandId, key: &FactKey, author: &PrincipalId) -> Self {
        Self {
            island: island.as_str().to_owned(),
            key: key.as_str().to_owned(),
            author: author.as_str().to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PloyzPandaAuthor {
    principal: PrincipalId,
    key: PrivateKey,
}

impl PloyzPandaAuthor {
    #[must_use]
    pub fn new(principal: PrincipalId) -> Self {
        Self {
            principal,
            key: PrivateKey::new(),
        }
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        self.key.public_key()
    }
}

pub struct PloyzPandaFactStore {
    store: MemoryStore<IslandLog, PloyzFactExtensions>,
}

impl PloyzPandaFactStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: MemoryStore::new(),
        }
    }

    pub async fn write_fact_payload(
        &mut self,
        island: &IslandId,
        author: &PloyzPandaAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<FactContentHash> {
        let body = Body::new(payload.as_bytes());
        let mut header = Header {
            version: 1,
            public_key: author.public_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: 0,
            seq_num: self.next_seq_num(island, author).await?,
            backlink: self.latest_hash(island, author).await?,
            previous: Vec::new(),
            extensions: PloyzFactExtensions::new(island, &key, &author.principal),
        };
        header.sign(&author.key);
        let header_bytes = header.to_bytes();
        let content_hash = FactContentHash::new(format!("b3:{}", body.hash().to_hex()));
        let result = ingest_operation(
            &mut self.store,
            header,
            Some(body),
            header_bytes,
            &IslandLog::from(island),
            false,
        )
        .await?;

        match result {
            IngestResult::Complete(operation) => {
                validate_operation(&operation)
                    .map_err(|_| PloyzPandaFactError::InvalidOperation)?;
                Ok(content_hash)
            }
            IngestResult::Retry(_, _, _, _) | IngestResult::Outdated(_) => {
                Err(PloyzPandaFactError::InvalidOperation)
            }
        }
    }

    pub async fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
    ) -> Result<Vec<FactCandidate>> {
        let mut candidates = Vec::new();
        for (public_key, _height) in self
            .store
            .get_log_heights(&IslandLog::from(island))
            .await
            .map_err(store_error)?
        {
            let Some(operations) = self
                .store
                .get_log(&public_key, &IslandLog::from(island), None)
                .await
                .map_err(store_error)?
            else {
                continue;
            };

            for (header, body) in operations {
                let key = FactKey::parse(header.extensions.key.clone()).map_err(store_error)?;
                if !pattern.matches(&key) {
                    continue;
                }
                let Some(payload_hash) = body.as_ref().map(Body::hash) else {
                    candidates.push(candidate_from_header(
                        island,
                        key,
                        header.extensions.author,
                        FactContentHash::new("missing"),
                        CandidateStatus::Unverified,
                    ));
                    continue;
                };
                candidates.push(candidate_from_header(
                    island,
                    key,
                    header.extensions.author,
                    FactContentHash::new(format!("b3:{}", payload_hash.to_hex())),
                    CandidateStatus::Verified,
                ));
            }
        }
        Ok(candidates)
    }

    pub async fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
    ) -> Result<BTreeMap<FactContentHash, FactPayload>> {
        let wanted = candidates
            .iter()
            .map(FactCandidate::content_hash)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut payloads = BTreeMap::new();
        for (public_key, _height) in self
            .store
            .get_log_heights(&IslandLog::from(island))
            .await
            .map_err(store_error)?
        {
            let Some(operations) = self
                .store
                .get_log(&public_key, &IslandLog::from(island), None)
                .await
                .map_err(store_error)?
            else {
                continue;
            };
            for (_header, body) in operations {
                let Some(body) = body else {
                    continue;
                };
                let content_hash = FactContentHash::new(format!("b3:{}", body.hash().to_hex()));
                if wanted.contains(&content_hash) {
                    payloads.insert(content_hash, FactPayload::from(body.to_bytes()));
                }
            }
        }
        Ok(payloads)
    }

    async fn next_seq_num(&self, island: &IslandId, author: &PloyzPandaAuthor) -> Result<u64> {
        let latest = self
            .store
            .latest_operation(&author.public_key(), &IslandLog::from(island))
            .await
            .map_err(store_error)?;
        Ok(latest.map_or(0, |(header, _)| header.seq_num + 1))
    }

    async fn latest_hash(
        &self,
        island: &IslandId,
        author: &PloyzPandaAuthor,
    ) -> Result<Option<p2panda_core::Hash>> {
        let latest = self
            .store
            .latest_operation(&author.public_key(), &IslandLog::from(island))
            .await
            .map_err(store_error)?;
        Ok(latest.map(|(header, _)| header.hash()))
    }
}

impl Default for PloyzPandaFactStore {
    fn default() -> Self {
        Self::new()
    }
}

fn candidate_from_header(
    island: &IslandId,
    key: FactKey,
    author: String,
    content_hash: FactContentHash,
    status: CandidateStatus,
) -> FactCandidate {
    let classification = classify_fact_key(&key);
    FactCandidate::new(
        island.clone(),
        key,
        PrincipalId::new(author),
        content_hash,
        classification.kind(),
        classification.epoch(),
        status,
    )
}

fn store_error(source: impl ToString) -> PloyzPandaFactError {
    PloyzPandaFactError::Store {
        message: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use mvp_bus::Payload;
    use mvp_projection::{CandidateStatus, FactKind};

    use super::*;

    fn island() -> IslandId {
        IslandId::new("prod")
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("pattern parses")
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("key parses")
    }

    #[tokio::test]
    async fn signed_operation_decodes_into_fact_candidate() {
        let island = island();
        let mut store = PloyzPandaFactStore::new();
        let author = PloyzPandaAuthor::new(PrincipalId::new("node-a"));

        let hash = store
            .write_fact_payload(
                &island,
                &author,
                key("/facts/node/node-a/joined/1"),
                Payload::from(String::from(r#"{"node":"node-a"}"#)),
            )
            .await
            .expect("write succeeds");

        let candidates = store
            .list_candidates(&island, &pattern("/facts/node/>"))
            .await
            .expect("candidates list");

        let [candidate] = candidates.as_slice() else {
            panic!("expected one candidate");
        };
        assert_eq!(candidate.author().as_str(), "node-a");
        assert_eq!(candidate.content_hash(), &hash);
        assert_eq!(candidate.kind(), FactKind::NodeJoined);
        assert_eq!(candidate.epoch(), 1);
        assert_eq!(candidate.status(), CandidateStatus::Verified);
    }

    #[tokio::test]
    async fn conflicting_operations_both_reach_projection_boundary() {
        let island = island();
        let mut store = PloyzPandaFactStore::new();
        let author_a = PloyzPandaAuthor::new(PrincipalId::new("node-a"));
        let author_b = PloyzPandaAuthor::new(PrincipalId::new("node-b"));
        let fact_key = key("/facts/lease/acme.example/claimed/7");

        store
            .write_fact_payload(
                &island,
                &author_a,
                fact_key.clone(),
                Payload::from(String::from("a")),
            )
            .await
            .expect("first write succeeds");
        store
            .write_fact_payload(
                &island,
                &author_b,
                fact_key,
                Payload::from(String::from("b")),
            )
            .await
            .expect("second write succeeds");

        let mut candidates = store
            .list_candidates(&island, &pattern("/facts/lease/>"))
            .await
            .expect("candidates list");
        candidates.sort_by(|left, right| left.content_hash().cmp(right.content_hash()));

        let [left, right] = candidates.as_slice() else {
            panic!("expected two candidates");
        };
        assert_eq!(left.key(), right.key());
        assert_eq!(left.kind(), FactKind::LeaseClaimed);
        assert_eq!(left.epoch(), 7);
        assert_eq!(right.epoch(), 7);
        assert_ne!(left.content_hash(), right.content_hash());
    }

    #[tokio::test]
    async fn payloads_are_read_by_candidate_content_hash() {
        let island = island();
        let mut store = PloyzPandaFactStore::new();
        let author = PloyzPandaAuthor::new(PrincipalId::new("node-a"));

        store
            .write_fact_payload(
                &island,
                &author,
                key("/facts/acme/http01/example.com/token/presented/3"),
                Payload::from(String::from("key-auth")),
            )
            .await
            .expect("write succeeds");

        let candidates = store
            .list_candidates(&island, &pattern("/facts/acme/>"))
            .await
            .expect("candidates list");
        let payloads = store
            .read_payloads(&island, &candidates)
            .await
            .expect("payloads read");

        let [candidate] = candidates.as_slice() else {
            panic!("expected one candidate");
        };
        assert_eq!(
            payloads
                .get(candidate.content_hash())
                .expect("payload present")
                .as_bytes(),
            b"key-auth"
        );
    }
}
