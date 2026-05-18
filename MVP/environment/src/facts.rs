use mvp_bus::{BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, PrincipalId};
use mvp_identity::VisibleNodes;
use mvp_projection::{CandidateStatus, FactSource};
use mvp_routing::ServingCommitId;
use serde::{Deserialize, Serialize};

use crate::{
    EnvironmentBranchId, EnvironmentCommandId, EnvironmentEpoch, EnvironmentError,
    EnvironmentHeadId, EnvironmentId, EnvironmentResult, EnvironmentRouteRef, EnvironmentVolumeRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvironmentFactPayload {
    Head(EnvironmentHeadFact),
    Branch(EnvironmentBranchFact),
    PromoteDecision(EnvironmentPromoteDecisionFact),
    RollbackDecision(EnvironmentRollbackDecisionFact),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentHeadReference {
    pub environment: EnvironmentId,
    pub epoch: EnvironmentEpoch,
    pub head_id: EnvironmentHeadId,
    pub serving_commit_id: ServingCommitId,
    pub volume_refs: Vec<EnvironmentVolumeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentHeadFact {
    pub environment: EnvironmentId,
    pub epoch: EnvironmentEpoch,
    pub head_id: EnvironmentHeadId,
    pub source_command_id: EnvironmentCommandId,
    pub serving_commit_id: ServingCommitId,
    pub previous_head: Option<EnvironmentHeadReference>,
    pub volume_refs: Vec<EnvironmentVolumeRef>,
    pub source_branch_id: Option<EnvironmentBranchId>,
}

impl EnvironmentHeadFact {
    #[must_use]
    pub fn reference(&self) -> EnvironmentHeadReference {
        EnvironmentHeadReference {
            environment: self.environment.clone(),
            epoch: self.epoch,
            head_id: self.head_id.clone(),
            serving_commit_id: self.serving_commit_id.clone(),
            volume_refs: self.volume_refs.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentBranchFact {
    pub source_environment: EnvironmentId,
    pub source_epoch: EnvironmentEpoch,
    pub source_head_id: EnvironmentHeadId,
    pub branch_id: EnvironmentBranchId,
    pub branch_environment: EnvironmentId,
    pub route_refs: Vec<EnvironmentRouteRef>,
    pub forked_volume_refs: Vec<EnvironmentVolumeRef>,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentPromoteDecisionFact {
    pub environment: EnvironmentId,
    pub command_id: EnvironmentCommandId,
    pub previous_head: EnvironmentHeadReference,
    pub branch_head: EnvironmentHeadReference,
    pub target_serving_commit_id: ServingCommitId,
    pub target_volume_refs: Vec<EnvironmentVolumeRef>,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnvironmentRollbackDecisionFact {
    pub environment: EnvironmentId,
    pub command_id: EnvironmentCommandId,
    pub current_head: EnvironmentHeadReference,
    pub rollback_target: EnvironmentHeadReference,
    pub target_serving_commit_id: ServingCommitId,
    pub target_volume_refs: Vec<EnvironmentVolumeRef>,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentHeadCandidate {
    pub fact: EnvironmentHeadFact,
    pub key: FactKey,
    pub author: PrincipalId,
    pub content_hash: FactContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSupersededHead {
    pub candidate: EnvironmentHeadCandidate,
    pub superseded_by_epoch: EnvironmentEpoch,
    pub superseded_by_principal: PrincipalId,
    pub superseded_by_content_hash: FactContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentHeadReadModel {
    pub current: Option<EnvironmentHeadCandidate>,
    pub superseded: Vec<EnvironmentSupersededHead>,
}

impl EnvironmentHeadReadModel {
    pub fn require_current(
        self,
        environment: &EnvironmentId,
    ) -> EnvironmentResult<EnvironmentHeadCandidate> {
        self.current
            .ok_or_else(|| EnvironmentError::MissingCurrentHead {
                environment: environment.clone(),
            })
    }
}

pub fn current_environment_head(
    source: &(impl FactSource + ?Sized),
    session: &BusSession,
    environment: &EnvironmentId,
) -> EnvironmentResult<Option<EnvironmentHeadCandidate>> {
    Ok(read_environment_heads(source, session, environment)?.current)
}

pub fn read_environment_heads(
    source: &(impl FactSource + ?Sized),
    session: &BusSession,
    environment: &EnvironmentId,
) -> EnvironmentResult<EnvironmentHeadReadModel> {
    let pattern = environment_head_pattern(environment)?;
    let candidates = source.list_candidates(session.island(), &pattern, session)?;
    let payloads = source.read_payloads(session.island(), &candidates, session)?;
    let mut heads = Vec::new();
    for candidate in &candidates {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            return Err(EnvironmentError::UnreadableHeadCandidate {
                key: candidate.key().clone(),
                status: candidate.status(),
            });
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            return Err(EnvironmentError::MissingPayload {
                key: candidate.key().clone(),
            });
        };
        let fact = decode_environment_head_fact(candidate.key(), payload)?;
        heads.push(EnvironmentHeadCandidate {
            fact,
            key: candidate.key().clone(),
            author: candidate.author().clone(),
            content_hash: candidate.content_hash().clone(),
        });
    }
    Ok(reduce_environment_heads(heads))
}

pub fn require_expected_environment_epoch(
    source: &(impl FactSource + ?Sized),
    session: &BusSession,
    environment: &EnvironmentId,
    expected: EnvironmentEpoch,
) -> EnvironmentResult<EnvironmentHeadCandidate> {
    let current =
        read_environment_heads(source, session, environment)?.require_current(environment)?;
    if current.fact.epoch != expected {
        return Err(EnvironmentError::StaleExpectedEpoch {
            environment: environment.clone(),
            expected,
            actual: Some(current.fact.epoch),
        });
    }
    Ok(current)
}

pub fn environment_head_fact_key(
    environment: &EnvironmentId,
    epoch: EnvironmentEpoch,
) -> EnvironmentResult<FactKey> {
    FactKey::parse(format!(
        "/facts/environment/{}/head/{}",
        environment.as_str(),
        epoch.get()
    ))
    .map_err(EnvironmentError::from)
}

pub fn environment_branch_fact_key(
    environment: &EnvironmentId,
    branch_id: &EnvironmentBranchId,
) -> EnvironmentResult<FactKey> {
    FactKey::parse(format!(
        "/facts/environment/{}/branch/{}",
        environment.as_str(),
        branch_id.as_str()
    ))
    .map_err(EnvironmentError::from)
}

pub fn environment_promote_decision_fact_key(
    environment: &EnvironmentId,
    command_id: &EnvironmentCommandId,
) -> EnvironmentResult<FactKey> {
    FactKey::parse(format!(
        "/facts/environment/{}/promote/{}",
        environment.as_str(),
        command_id.as_str()
    ))
    .map_err(EnvironmentError::from)
}

pub fn environment_rollback_decision_fact_key(
    environment: &EnvironmentId,
    command_id: &EnvironmentCommandId,
) -> EnvironmentResult<FactKey> {
    FactKey::parse(format!(
        "/facts/environment/{}/rollback/{}",
        environment.as_str(),
        command_id.as_str()
    ))
    .map_err(EnvironmentError::from)
}

pub fn environment_head_fact_payload(fact: &EnvironmentHeadFact) -> EnvironmentResult<FactPayload> {
    encode_environment_fact(EnvironmentFactPayload::Head(fact.clone()))
}

pub fn environment_branch_fact_payload(
    fact: &EnvironmentBranchFact,
) -> EnvironmentResult<FactPayload> {
    encode_environment_fact(EnvironmentFactPayload::Branch(fact.clone()))
}

pub fn environment_promote_decision_fact_payload(
    fact: &EnvironmentPromoteDecisionFact,
) -> EnvironmentResult<FactPayload> {
    encode_environment_fact(EnvironmentFactPayload::PromoteDecision(fact.clone()))
}

pub fn environment_rollback_decision_fact_payload(
    fact: &EnvironmentRollbackDecisionFact,
) -> EnvironmentResult<FactPayload> {
    encode_environment_fact(EnvironmentFactPayload::RollbackDecision(fact.clone()))
}

pub fn decode_environment_head_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> EnvironmentResult<EnvironmentHeadFact> {
    match decode_environment_fact(key, payload)? {
        EnvironmentFactPayload::Head(fact) => {
            validate_head_matches_key(key, &fact)?;
            Ok(fact)
        }
        EnvironmentFactPayload::Branch(_)
        | EnvironmentFactPayload::PromoteDecision(_)
        | EnvironmentFactPayload::RollbackDecision(_) => {
            Err(EnvironmentError::WrongFactKeyShape { key: key.clone() })
        }
    }
}

pub fn decode_environment_branch_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> EnvironmentResult<EnvironmentBranchFact> {
    match decode_environment_fact(key, payload)? {
        EnvironmentFactPayload::Branch(fact) => {
            validate_branch_matches_key(key, &fact)?;
            Ok(fact)
        }
        EnvironmentFactPayload::Head(_)
        | EnvironmentFactPayload::PromoteDecision(_)
        | EnvironmentFactPayload::RollbackDecision(_) => {
            Err(EnvironmentError::WrongFactKeyShape { key: key.clone() })
        }
    }
}

fn reduce_environment_heads(mut heads: Vec<EnvironmentHeadCandidate>) -> EnvironmentHeadReadModel {
    heads.sort_by(|left, right| {
        right
            .fact
            .epoch
            .cmp(&left.fact.epoch)
            .then_with(|| left.content_hash.cmp(&right.content_hash))
    });
    let Some((current, superseded)) = heads.split_first() else {
        return EnvironmentHeadReadModel {
            current: None,
            superseded: Vec::new(),
        };
    };
    EnvironmentHeadReadModel {
        current: Some(current.clone()),
        superseded: superseded
            .iter()
            .cloned()
            .map(|candidate| EnvironmentSupersededHead {
                candidate,
                superseded_by_epoch: current.fact.epoch,
                superseded_by_principal: current.author.clone(),
                superseded_by_content_hash: current.content_hash.clone(),
            })
            .collect(),
    }
}

fn encode_environment_fact(payload: EnvironmentFactPayload) -> EnvironmentResult<FactPayload> {
    serde_json::to_vec(&payload)
        .map(FactPayload::from)
        .map_err(|error| EnvironmentError::Serialization {
            message: error.to_string(),
        })
}

fn decode_environment_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> EnvironmentResult<EnvironmentFactPayload> {
    serde_json::from_slice::<EnvironmentFactPayload>(payload.as_bytes()).map_err(|error| {
        EnvironmentError::DecodePayload {
            key: key.clone(),
            message: error.to_string(),
        }
    })
}

fn validate_head_matches_key(key: &FactKey, fact: &EnvironmentHeadFact) -> EnvironmentResult<()> {
    let (environment, epoch) = parse_head_fact_key(key)?;
    if environment != fact.environment || epoch != fact.epoch {
        return Err(EnvironmentError::FactKeyPayloadMismatch {
            key: key.clone(),
            environment: fact.environment.clone(),
        });
    }
    Ok(())
}

fn validate_branch_matches_key(
    key: &FactKey,
    fact: &EnvironmentBranchFact,
) -> EnvironmentResult<()> {
    let (environment, branch_id) = parse_branch_fact_key(key)?;
    if environment != fact.source_environment || branch_id != fact.branch_id {
        return Err(EnvironmentError::FactKeyPayloadMismatch {
            key: key.clone(),
            environment: fact.source_environment.clone(),
        });
    }
    Ok(())
}

fn parse_head_fact_key(key: &FactKey) -> EnvironmentResult<(EnvironmentId, EnvironmentEpoch)> {
    let segments = key.segments().collect::<Vec<_>>();
    let ["facts", "environment", environment, "head", epoch] = segments.as_slice() else {
        return Err(EnvironmentError::WrongFactKeyShape { key: key.clone() });
    };
    let epoch = epoch
        .parse::<u64>()
        .map_err(|_| EnvironmentError::WrongFactKeyShape { key: key.clone() })?;
    Ok((
        EnvironmentId::parse(*environment)?,
        EnvironmentEpoch::new(epoch)?,
    ))
}

fn parse_branch_fact_key(key: &FactKey) -> EnvironmentResult<(EnvironmentId, EnvironmentBranchId)> {
    let segments = key.segments().collect::<Vec<_>>();
    let ["facts", "environment", environment, "branch", branch_id] = segments.as_slice() else {
        return Err(EnvironmentError::WrongFactKeyShape { key: key.clone() });
    };
    Ok((
        EnvironmentId::parse(*environment)?,
        EnvironmentBranchId::parse(*branch_id)?,
    ))
}

fn environment_head_pattern(environment: &EnvironmentId) -> EnvironmentResult<FactKeyPattern> {
    FactKeyPattern::parse(format!(
        "/facts/environment/{}/head/*",
        environment.as_str()
    ))
    .map_err(EnvironmentError::from)
}
