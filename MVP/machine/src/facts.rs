use mvp_bus::{
    BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId, PrincipalId,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_mesh::{removal_started_fact_key, tombstone_fact_key};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, NodeRemovalStartedFact, NodeTombstonedFact,
    ProjectionFactPayload,
};
use mvp_routing::{ServingCommitId, ServingCommitPlan};
use serde::{Deserialize, Serialize};
use serde::{Deserializer, Serializer};

use crate::wire::{decode, encode};
use crate::{MachineRemoveError, MachineRemoveResult};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineRemoveId {
    target_node_id: NodeId,
    removal_epoch: u64,
}

impl MachineRemoveId {
    #[must_use]
    pub fn new(target_node_id: NodeId, removal_epoch: u64) -> Self {
        Self {
            target_node_id,
            removal_epoch,
        }
    }

    #[must_use]
    pub fn target_node_id(&self) -> &NodeId {
        &self.target_node_id
    }

    #[must_use]
    pub fn removal_epoch(&self) -> u64 {
        self.removal_epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineRemoveFactPayload {
    Decision(Box<MachineRemoveDecisionFact>),
    CleanupDone(MachineRemoveCleanupDoneFact),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRemoveDecisionFact {
    pub target_node_id: NodeId,
    pub removal_epoch: u64,
    pub tombstone_epoch: u64,
    pub reason: String,
    pub visible_nodes: VisibleNodes,
    pub serving_commit: ServingCommitPlan,
}

impl MachineRemoveDecisionFact {
    #[must_use]
    pub fn new(
        target_node_id: NodeId,
        removal_epoch: u64,
        tombstone_epoch: u64,
        reason: String,
        visible_nodes: VisibleNodes,
        serving_commit: ServingCommitPlan,
    ) -> Self {
        Self {
            target_node_id,
            removal_epoch,
            tombstone_epoch,
            reason,
            visible_nodes,
            serving_commit,
        }
    }

    #[must_use]
    pub fn remove_id(&self) -> MachineRemoveId {
        MachineRemoveId::new(self.target_node_id.clone(), self.removal_epoch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRemoveCleanupDoneFact {
    pub target_node_id: NodeId,
    pub removal_epoch: u64,
    pub tombstone_epoch: u64,
    pub serving_commit_id: ServingCommitId,
    pub serving_epoch: u64,
    #[serde(with = "fact_key_string")]
    pub tombstone_fact_key: FactKey,
}

impl MachineRemoveCleanupDoneFact {
    pub fn new(
        decision: &MachineRemoveDecisionFact,
        tombstone_key: FactKey,
    ) -> MachineRemoveResult<Self> {
        let expected_tombstone_key =
            tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch)?;
        if tombstone_key != expected_tombstone_key {
            return Err(MachineRemoveError::CommandFactMismatch {
                key: machine_remove_cleanup_done_fact_key(&decision.remove_id())?,
            });
        }
        Ok(Self {
            target_node_id: decision.target_node_id.clone(),
            removal_epoch: decision.removal_epoch,
            tombstone_epoch: decision.tombstone_epoch,
            serving_commit_id: decision.serving_commit.serving_commit_id.clone(),
            serving_epoch: decision.serving_commit.epoch,
            tombstone_fact_key: tombstone_key,
        })
    }

    #[must_use]
    pub fn remove_id(&self) -> MachineRemoveId {
        MachineRemoveId::new(self.target_node_id.clone(), self.removal_epoch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRemoveDecisionCandidate {
    pub fact: MachineRemoveDecisionFact,
    pub author: PrincipalId,
    pub content_hash: FactContentHash,
}

pub fn machine_remove_decision_fact_key(
    remove_id: &MachineRemoveId,
) -> MachineRemoveResult<FactKey> {
    FactKey::parse(format!(
        "/facts/machine-remove/{}/{}/decision",
        remove_id.target_node_id().as_str(),
        remove_id.removal_epoch()
    ))
    .map_err(MachineRemoveError::from)
}

pub fn machine_remove_cleanup_done_fact_key(
    remove_id: &MachineRemoveId,
) -> MachineRemoveResult<FactKey> {
    FactKey::parse(format!(
        "/facts/machine-remove/{}/{}/cleanup/done",
        remove_id.target_node_id().as_str(),
        remove_id.removal_epoch()
    ))
    .map_err(MachineRemoveError::from)
}

pub fn machine_remove_decision_fact_payload(
    fact: &MachineRemoveDecisionFact,
) -> MachineRemoveResult<FactPayload> {
    encode_machine_remove_fact(
        MachineRemoveFactPayload::Decision(Box::new(fact.clone())),
        "serialize machine remove decision fact",
    )
}

pub fn machine_remove_cleanup_done_fact_payload(
    fact: &MachineRemoveCleanupDoneFact,
) -> MachineRemoveResult<FactPayload> {
    encode_machine_remove_fact(
        MachineRemoveFactPayload::CleanupDone(fact.clone()),
        "serialize machine remove cleanup done fact",
    )
}

pub fn decode_machine_remove_decision_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> MachineRemoveResult<MachineRemoveDecisionFact> {
    match decode::<MachineRemoveFactPayload>(payload, "decode machine remove decision fact")? {
        MachineRemoveFactPayload::Decision(fact) => Ok(*fact),
        MachineRemoveFactPayload::CleanupDone(_) => {
            Err(MachineRemoveError::CommandFactKindMismatch {
                key: key.clone(),
                expected_kind: "decision",
            })
        }
    }
}

pub fn decode_machine_remove_cleanup_done_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> MachineRemoveResult<MachineRemoveCleanupDoneFact> {
    match decode::<MachineRemoveFactPayload>(payload, "decode machine remove cleanup done fact")? {
        MachineRemoveFactPayload::CleanupDone(fact) => Ok(fact),
        MachineRemoveFactPayload::Decision(_) => Err(MachineRemoveError::CommandFactKindMismatch {
            key: key.clone(),
            expected_kind: "cleanup_done",
        }),
    }
}

pub fn read_machine_remove_decision(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    remove_id: &MachineRemoveId,
) -> MachineRemoveResult<MachineRemoveDecisionCandidate> {
    let key = machine_remove_decision_fact_key(remove_id)?;
    let decoded = read_machine_remove_candidates(source, island, session, &key)?
        .into_iter()
        .map(|(candidate, payload)| {
            let fact = decode_machine_remove_decision_fact(&key, &payload)?;
            validate_decision_matches_key(&key, remove_id, &fact)?;
            Ok(candidate_to_decision(&candidate, fact))
        })
        .collect::<MachineRemoveResult<Vec<_>>>()?;
    select_single_candidate(decoded, key)
}

pub fn read_machine_remove_cleanup_done(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    remove_id: &MachineRemoveId,
) -> MachineRemoveResult<Option<MachineRemoveCleanupDoneFact>> {
    let key = machine_remove_cleanup_done_fact_key(remove_id)?;
    let decoded = read_machine_remove_candidates(source, island, session, &key)?
        .into_iter()
        .map(|(_candidate, payload)| {
            let fact = decode_machine_remove_cleanup_done_fact(&key, &payload)?;
            validate_cleanup_done_matches_key(&key, remove_id, &fact)?;
            Ok(fact)
        })
        .collect::<MachineRemoveResult<Vec<_>>>()?;
    select_optional_single(decoded, key)
}

pub fn validate_machine_remove_cleanup_done(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    decision: &MachineRemoveDecisionFact,
    cleanup_done: &MachineRemoveCleanupDoneFact,
) -> MachineRemoveResult<()> {
    let cleanup_key = machine_remove_cleanup_done_fact_key(&decision.remove_id())?;
    let expected_tombstone_key =
        tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch)?;
    let expected_cleanup_done =
        MachineRemoveCleanupDoneFact::new(decision, expected_tombstone_key.clone())?;
    if cleanup_done != &expected_cleanup_done {
        return Err(MachineRemoveError::CommandFactMismatch { key: cleanup_key });
    }
    require_expected_tombstone_fact(source, island, session, decision, &expected_tombstone_key)
}

pub fn validate_machine_remove_removal_started(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    decision: &MachineRemoveDecisionFact,
) -> MachineRemoveResult<FactKey> {
    let key = removal_started_fact_key(&decision.target_node_id, decision.removal_epoch)?;
    let candidates = list_recovery_candidates(source, island, session, &key)?;
    let payloads = source.read_payloads(island, &candidates, session)?;
    let expected = NodeRemovalStartedFact {
        node_id: decision.target_node_id.clone(),
        epoch: decision.removal_epoch,
        reason: decision.reason.clone(),
    };
    let readable_candidates = candidates
        .iter()
        .filter(|candidate| candidate_payload_is_recovery_authoritative(candidate.status()))
        .collect::<Vec<_>>();
    for candidate in &readable_candidates {
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let fact = decode_removal_started_fact(&key, payload)?;
        if fact == expected {
            return Ok(key);
        }
    }
    if candidates.is_empty() {
        Err(MachineRemoveError::CommandFactMissing { key })
    } else {
        Err(MachineRemoveError::CommandFactMismatch { key })
    }
}

fn require_expected_tombstone_fact(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    decision: &MachineRemoveDecisionFact,
    key: &FactKey,
) -> MachineRemoveResult<()> {
    let candidates = list_recovery_candidates(source, island, session, key)?;
    let payloads = source.read_payloads(island, &candidates, session)?;
    let expected = NodeTombstonedFact {
        node_id: decision.target_node_id.clone(),
        epoch: decision.tombstone_epoch,
        reason: decision.reason.clone(),
    };
    let readable_candidates = candidates
        .iter()
        .filter(|candidate| candidate_payload_is_recovery_authoritative(candidate.status()))
        .collect::<Vec<_>>();
    for candidate in &readable_candidates {
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let fact = decode_tombstone_fact(key, payload)?;
        if fact == expected {
            return Ok(());
        }
    }
    if candidates.is_empty() {
        Err(MachineRemoveError::CommandFactMissing { key: key.clone() })
    } else if readable_candidates.is_empty() {
        Err(MachineRemoveError::CommandFactMismatch { key: key.clone() })
    } else {
        Err(MachineRemoveError::CommandFactMismatch { key: key.clone() })
    }
}

fn validate_decision_matches_key(
    key: &FactKey,
    remove_id: &MachineRemoveId,
    fact: &MachineRemoveDecisionFact,
) -> MachineRemoveResult<()> {
    if fact.target_node_id == *remove_id.target_node_id()
        && fact.removal_epoch == remove_id.removal_epoch()
    {
        Ok(())
    } else {
        Err(MachineRemoveError::CommandFactMismatch { key: key.clone() })
    }
}

fn validate_cleanup_done_matches_key(
    key: &FactKey,
    remove_id: &MachineRemoveId,
    fact: &MachineRemoveCleanupDoneFact,
) -> MachineRemoveResult<()> {
    if fact.target_node_id == *remove_id.target_node_id()
        && fact.removal_epoch == remove_id.removal_epoch()
    {
        Ok(())
    } else {
        Err(MachineRemoveError::CommandFactMismatch { key: key.clone() })
    }
}

fn read_machine_remove_candidates(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    key: &FactKey,
) -> MachineRemoveResult<Vec<(FactCandidate, FactPayload)>> {
    let candidates = list_recovery_candidates(source, island, session, key)?;
    let payloads = source.read_payloads(island, &candidates, session)?;
    let decoded = candidates
        .into_iter()
        .filter_map(|candidate| {
            payloads
                .get(candidate.content_hash())
                .cloned()
                .map(|payload| (candidate, payload))
        })
        .collect();
    Ok(decoded)
}

fn list_recovery_candidates(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    key: &FactKey,
) -> MachineRemoveResult<Vec<FactCandidate>> {
    let pattern = FactKeyPattern::parse(key.as_str())?;
    Ok(source
        .list_candidates(island, &pattern, session)?
        .into_iter()
        .filter(|candidate| {
            candidate.key() == key
                && matches!(
                    candidate.status(),
                    CandidateStatus::Verified | CandidateStatus::Conflict
                )
        })
        .collect())
}

fn candidate_payload_is_recovery_authoritative(status: CandidateStatus) -> bool {
    matches!(status, CandidateStatus::Verified)
}

fn select_single_candidate(
    mut candidates: Vec<MachineRemoveDecisionCandidate>,
    key: FactKey,
) -> MachineRemoveResult<MachineRemoveDecisionCandidate> {
    if candidates.is_empty() {
        return Err(MachineRemoveError::CommandFactMissing { key });
    }
    candidates.sort_by(|left, right| left.content_hash.cmp(&right.content_hash));
    let winner = candidates.remove(0);
    if candidates.is_empty() {
        Ok(winner)
    } else {
        Err(MachineRemoveError::CommandFactConflict {
            key,
            principal: winner.author,
            content_hash: winner.content_hash,
        })
    }
}

fn select_optional_single<T>(
    mut candidates: Vec<T>,
    key: FactKey,
) -> MachineRemoveResult<Option<T>> {
    if candidates.len() <= 1 {
        return Ok(candidates.pop());
    }
    Err(MachineRemoveError::FactConflict { key })
}

fn candidate_to_decision(
    candidate: &FactCandidate,
    fact: MachineRemoveDecisionFact,
) -> MachineRemoveDecisionCandidate {
    MachineRemoveDecisionCandidate {
        fact,
        author: candidate.author().clone(),
        content_hash: candidate.content_hash().clone(),
    }
}

fn decode_tombstone_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> MachineRemoveResult<NodeTombstonedFact> {
    match ProjectionFactPayload::from_fact_bytes(payload.as_bytes()).map_err(|source| {
        MachineRemoveError::WirePayload {
            context: "decode tombstone fact",
            source,
        }
    })? {
        ProjectionFactPayload::NodeTombstoned(fact) => Ok(fact),
        _ => Err(MachineRemoveError::CommandFactKindMismatch {
            key: key.clone(),
            expected_kind: "node_tombstoned",
        }),
    }
}

fn decode_removal_started_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> MachineRemoveResult<NodeRemovalStartedFact> {
    match ProjectionFactPayload::from_fact_bytes(payload.as_bytes()).map_err(|source| {
        MachineRemoveError::WirePayload {
            context: "decode removal-started fact",
            source,
        }
    })? {
        ProjectionFactPayload::NodeRemovalStarted(fact) => Ok(fact),
        _ => Err(MachineRemoveError::CommandFactKindMismatch {
            key: key.clone(),
            expected_kind: "node_removal_started",
        }),
    }
}

fn encode_machine_remove_fact(
    payload: MachineRemoveFactPayload,
    context: &'static str,
) -> MachineRemoveResult<FactPayload> {
    encode(&payload, context).map(Into::into)
}

mod fact_key_string {
    use super::*;
    use serde::de::Error;

    pub fn serialize<S>(value: &FactKey, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<FactKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        FactKey::parse(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mvp_bus::{FactContentHash, FactKey, FactPayload, PrincipalId};
    use mvp_projection::{BackendEndpoint, DnsRecordFact, FactSourceError, RouteId};
    use mvp_routing::{DnsCommitId, GatewayCommitId, RouteCommitId};

    use super::*;

    #[test]
    fn decision_key_is_deterministic_for_target_and_epoch() {
        let key = machine_remove_decision_fact_key(&remove_id()).expect("key");

        assert_eq!(key.as_str(), "/facts/machine-remove/node-old/2/decision");
    }

    #[test]
    fn decision_payload_round_trips() {
        let decision = decision_fact();
        let key = machine_remove_decision_fact_key(&decision.remove_id()).expect("key");
        let payload = machine_remove_decision_fact_payload(&decision).expect("payload");

        let decoded = decode_machine_remove_decision_fact(&key, &payload).expect("decode");

        assert_eq!(decoded, decision);
        assert_eq!(decoded.visible_nodes.len(), 2);
        assert_eq!(
            decoded.serving_commit.serving_commit_id,
            ServingCommitId::new("serving-remove-1")
        );
    }

    #[test]
    fn cleanup_done_payload_round_trips() {
        let decision = decision_fact();
        let tombstone_key =
            tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch).expect("key");
        let cleanup_done =
            MachineRemoveCleanupDoneFact::new(&decision, tombstone_key).expect("cleanup");
        let key = machine_remove_cleanup_done_fact_key(&decision.remove_id()).expect("key");
        let payload = machine_remove_cleanup_done_fact_payload(&cleanup_done).expect("payload");

        let decoded = decode_machine_remove_cleanup_done_fact(&key, &payload).expect("decode");

        assert_eq!(decoded, cleanup_done);
    }

    #[test]
    fn decision_decoder_rejects_cleanup_done_payload() {
        let decision = decision_fact();
        let tombstone_key =
            tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch).expect("key");
        let cleanup_done =
            MachineRemoveCleanupDoneFact::new(&decision, tombstone_key).expect("cleanup");
        let payload = machine_remove_cleanup_done_fact_payload(&cleanup_done).expect("payload");
        let key = machine_remove_decision_fact_key(&decision.remove_id()).expect("key");

        let error = decode_machine_remove_decision_fact(&key, &payload).expect_err("wrong kind");

        assert!(matches!(
            error,
            MachineRemoveError::CommandFactKindMismatch {
                expected_kind: "decision",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn conflicting_decision_candidates_return_structured_error() {
        let source = TestFactSource::new()
            .with_payload(
                decision_key(),
                decision_payload(&decision_fact()),
                CandidateStatus::Conflict,
            )
            .with_payload(
                decision_key(),
                decision_payload(&alternate_decision_fact()),
                CandidateStatus::Conflict,
            );

        let error =
            read_machine_remove_decision(&source, &IslandId::new("prod"), &session(), &remove_id())
                .expect_err("conflict");

        assert!(matches!(
            error,
            MachineRemoveError::CommandFactConflict { .. }
        ));
    }

    #[tokio::test]
    async fn cleanup_validation_requires_expected_tombstone_fact() {
        let decision = decision_fact();
        let tombstone_key =
            tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch).expect("key");
        let cleanup_done =
            MachineRemoveCleanupDoneFact::new(&decision, tombstone_key.clone()).expect("cleanup");
        let source = TestFactSource::new();

        let error = validate_machine_remove_cleanup_done(
            &source,
            &IslandId::new("prod"),
            &session(),
            &decision,
            &cleanup_done,
        )
        .expect_err("missing tombstone");

        assert!(
            matches!(error, MachineRemoveError::CommandFactMissing { key } if key == tombstone_key)
        );
    }

    #[tokio::test]
    async fn cleanup_validation_accepts_expected_tombstone_fact() {
        let decision = decision_fact();
        let tombstone_key =
            tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch).expect("key");
        let cleanup_done =
            MachineRemoveCleanupDoneFact::new(&decision, tombstone_key.clone()).expect("cleanup");
        let tombstone_payload = ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: decision.target_node_id.clone(),
            epoch: decision.tombstone_epoch,
            reason: decision.reason.clone(),
        })
        .to_fact_bytes()
        .expect("serialize tombstone")
        .into();
        let source = TestFactSource::new().with_payload(
            tombstone_key,
            tombstone_payload,
            CandidateStatus::Verified,
        );

        validate_machine_remove_cleanup_done(
            &source,
            &IslandId::new("prod"),
            &session(),
            &decision,
            &cleanup_done,
        )
        .expect("valid cleanup");
    }

    #[tokio::test]
    async fn cleanup_validation_rejects_conflicted_tombstone_fact() {
        let decision = decision_fact();
        let tombstone_key =
            tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch).expect("key");
        let cleanup_done =
            MachineRemoveCleanupDoneFact::new(&decision, tombstone_key.clone()).expect("cleanup");
        let tombstone_payload = ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: decision.target_node_id.clone(),
            epoch: decision.tombstone_epoch,
            reason: decision.reason.clone(),
        })
        .to_fact_bytes()
        .expect("serialize tombstone")
        .into();
        let source = TestFactSource::new().with_payload(
            tombstone_key.clone(),
            tombstone_payload,
            CandidateStatus::Conflict,
        );

        let error = validate_machine_remove_cleanup_done(
            &source,
            &IslandId::new("prod"),
            &session(),
            &decision,
            &cleanup_done,
        )
        .expect_err("conflicted tombstone");

        assert!(
            matches!(error, MachineRemoveError::CommandFactMismatch { key } if key == tombstone_key)
        );
    }

    #[derive(Default)]
    struct TestFactSource {
        records: Vec<TestFactRecord>,
    }

    impl TestFactSource {
        fn new() -> Self {
            Self::default()
        }

        fn with_payload(
            mut self,
            key: FactKey,
            payload: FactPayload,
            status: CandidateStatus,
        ) -> Self {
            self.records.push(TestFactRecord {
                key,
                payload,
                status,
            });
            self
        }
    }

    impl FactSource for TestFactSource {
        fn list_candidates(
            &self,
            island: &IslandId,
            pattern: &FactKeyPattern,
            _session: &BusSession,
        ) -> Result<Vec<FactCandidate>, FactSourceError> {
            Ok(self
                .records
                .iter()
                .filter(|record| pattern.matches(&record.key))
                .map(|record| {
                    let classification = mvp_projection::classify_fact_key(&record.key);
                    FactCandidate::new(
                        island.clone(),
                        record.key.clone(),
                        PrincipalId::new("author"),
                        FactContentHash::for_payload(&record.payload),
                        classification.kind(),
                        classification.epoch(),
                        record.status,
                    )
                })
                .collect())
        }

        fn read_payloads(
            &self,
            _island: &IslandId,
            candidates: &[FactCandidate],
            _session: &BusSession,
        ) -> Result<BTreeMap<FactContentHash, FactPayload>, FactSourceError> {
            Ok(candidates
                .iter()
                .filter_map(|candidate| {
                    self.records
                        .iter()
                        .find(|record| {
                            record.key == *candidate.key()
                                && FactContentHash::for_payload(&record.payload)
                                    == *candidate.content_hash()
                        })
                        .map(|record| (candidate.content_hash().clone(), record.payload.clone()))
                })
                .collect())
        }
    }

    struct TestFactRecord {
        key: FactKey,
        payload: FactPayload,
        status: CandidateStatus,
    }

    fn remove_id() -> MachineRemoveId {
        MachineRemoveId::new(NodeId::new("node-old"), 2)
    }

    fn session() -> BusSession {
        let (_handle, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
        authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("operator"),
            mvp_bus::Grant::allow_all(),
        )
    }

    fn decision_key() -> FactKey {
        machine_remove_decision_fact_key(&remove_id()).expect("key")
    }

    fn decision_payload(decision: &MachineRemoveDecisionFact) -> FactPayload {
        machine_remove_decision_fact_payload(decision).expect("payload")
    }

    fn decision_fact() -> MachineRemoveDecisionFact {
        MachineRemoveDecisionFact::new(
            NodeId::new("node-old"),
            2,
            3,
            "graceful-remove".to_string(),
            VisibleNodes::new([NodeId::new("node-old"), NodeId::new("node-new")]),
            serving_commit(1),
        )
    }

    fn alternate_decision_fact() -> MachineRemoveDecisionFact {
        let mut fact = decision_fact();
        fact.reason = "alternate".to_string();
        fact
    }

    fn serving_commit(epoch: u64) -> ServingCommitPlan {
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new("serving-remove-1"),
            route_commit_id: RouteCommitId::new("route-remove-1"),
            gateway_commit_id: GatewayCommitId::new("gateway-remove-1"),
            dns_commit_id: DnsCommitId::new("dns-remove-1"),
            route_id: RouteId::new("web"),
            hostnames: vec!["web.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-new"),
                address: "fd00::2:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "fd00::1:8080".to_string(),
            }],
            dns_records: vec![DnsRecordFact {
                name: "web.example.test".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::2".to_string(),
                ttl_seconds: 30,
            }],
            epoch,
        }
    }
}
