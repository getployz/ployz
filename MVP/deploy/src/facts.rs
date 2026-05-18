use std::cmp::Ordering;
use std::future::Future;
use std::pin::Pin;

use mvp_bus::{
    BusActorHandle, BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload,
    FactWriteOutcome, IslandId, PrincipalId,
};
use mvp_identity::VisibleNodes;
use mvp_projection::{BackendEndpoint, CandidateStatus, FactCandidate, FactSource};
use mvp_routing::ServingCommitId;
use serde::{Deserialize, Serialize};

use crate::wire::{decode, encode};
use crate::{DeployError, DeployId, DeployManifest, DeployResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployFactPayload {
    Decision(Box<DeployDecisionFact>),
    CleanupDone(DeployCleanupDoneFact),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployDecisionFact {
    pub deploy_id: DeployId,
    pub manifest: DeployManifest,
    pub visible_nodes: VisibleNodes,
    pub expected_serving_commit_id: ServingCommitId,
    pub serving_epoch: u64,
}

impl DeployDecisionFact {
    #[must_use]
    pub fn new(manifest: DeployManifest, visible_nodes: VisibleNodes) -> Self {
        Self {
            deploy_id: manifest.deploy_id.clone(),
            expected_serving_commit_id: manifest.serving_commit.serving_commit_id.clone(),
            serving_epoch: manifest.serving_commit.epoch,
            manifest,
            visible_nodes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployCleanupDoneFact {
    pub deploy_id: DeployId,
    pub serving_commit_id: ServingCommitId,
    pub cleanup_targets: Vec<BackendEndpoint>,
    pub serving_epoch: u64,
}

impl DeployCleanupDoneFact {
    #[must_use]
    pub fn new(manifest: &DeployManifest) -> Self {
        Self {
            deploy_id: manifest.deploy_id.clone(),
            serving_commit_id: manifest.serving_commit.serving_commit_id.clone(),
            cleanup_targets: manifest.serving_commit.old_backends_to_drain.clone(),
            serving_epoch: manifest.serving_commit.epoch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenDeployFact {
    key: FactKey,
    content_hash: FactContentHash,
    status: DeployFactWriteStatus,
}

impl WrittenDeployFact {
    #[must_use]
    pub fn inserted(key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            key,
            content_hash,
            status: DeployFactWriteStatus::Inserted,
        }
    }

    #[must_use]
    pub fn already_present(key: FactKey, content_hash: FactContentHash) -> Self {
        Self {
            key,
            content_hash,
            status: DeployFactWriteStatus::AlreadyPresent,
        }
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        &self.key
    }

    #[must_use]
    pub fn content_hash(&self) -> &FactContentHash {
        &self.content_hash
    }

    #[must_use]
    pub fn status(&self) -> DeployFactWriteStatus {
        self.status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployFactWriteStatus {
    Inserted,
    AlreadyPresent,
}

pub trait DeployFactWriter: Send + Sync {
    fn write_decision<'a>(
        &'a self,
        fact: DeployDecisionFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>>;

    fn write_cleanup_done<'a>(
        &'a self,
        fact: DeployCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct BusDeployFactWriter {
    bus: BusActorHandle,
    session: BusSession,
}

impl BusDeployFactWriter {
    #[must_use]
    pub fn new(bus: BusActorHandle, session: BusSession) -> Self {
        Self { bus, session }
    }
}

impl DeployFactWriter for BusDeployFactWriter {
    fn write_decision<'a>(
        &'a self,
        fact: DeployDecisionFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            write_bus_deploy_fact(
                &self.bus,
                &self.session,
                deploy_decision_fact_key(&fact.deploy_id)?,
                encode_deploy_fact(
                    DeployFactPayload::Decision(Box::new(fact)),
                    "serialize deploy decision fact",
                )?,
            )
            .await
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: DeployCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            write_bus_deploy_fact(
                &self.bus,
                &self.session,
                deploy_cleanup_done_fact_key(&fact.deploy_id)?,
                encode_deploy_fact(
                    DeployFactPayload::CleanupDone(fact),
                    "serialize deploy cleanup done fact",
                )?,
            )
            .await
        })
    }
}

pub fn deploy_decision_fact_key(deploy_id: &DeployId) -> DeployResult<FactKey> {
    FactKey::parse(format!("/facts/deploy/{deploy_id}/decision")).map_err(DeployError::from)
}

pub fn deploy_cleanup_done_fact_key(deploy_id: &DeployId) -> DeployResult<FactKey> {
    FactKey::parse(format!("/facts/deploy/{deploy_id}/cleanup/done")).map_err(DeployError::from)
}

pub fn deploy_decision_fact_payload(fact: &DeployDecisionFact) -> DeployResult<FactPayload> {
    encode_deploy_fact(
        DeployFactPayload::Decision(Box::new(fact.clone())),
        "serialize deploy decision fact",
    )
}

pub fn deploy_cleanup_done_fact_payload(fact: &DeployCleanupDoneFact) -> DeployResult<FactPayload> {
    encode_deploy_fact(
        DeployFactPayload::CleanupDone(fact.clone()),
        "serialize deploy cleanup done fact",
    )
}

pub fn decode_deploy_decision_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> DeployResult<DeployDecisionFact> {
    match decode::<DeployFactPayload>(payload, "decode deploy decision fact")? {
        DeployFactPayload::Decision(fact) => Ok(*fact),
        DeployFactPayload::CleanupDone(_) => Err(DeployError::DeployFactKindMismatch {
            key: key.clone(),
            expected_kind: "decision",
        }),
    }
}

pub fn decode_deploy_cleanup_done_fact(
    key: &FactKey,
    payload: &FactPayload,
) -> DeployResult<DeployCleanupDoneFact> {
    match decode::<DeployFactPayload>(payload, "decode deploy cleanup done fact")? {
        DeployFactPayload::CleanupDone(fact) => Ok(fact),
        DeployFactPayload::Decision(_) => Err(DeployError::DeployFactKindMismatch {
            key: key.clone(),
            expected_kind: "cleanup_done",
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployDecisionCandidate {
    pub fact: DeployDecisionFact,
    pub author: PrincipalId,
    pub content_hash: FactContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployDecisionSelection {
    pub winner: DeployDecisionCandidate,
    pub superseded: Vec<DeployDecisionCandidate>,
}

pub fn read_deploy_decision(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    deploy_id: &DeployId,
) -> DeployResult<DeployDecisionSelection> {
    let key = deploy_decision_fact_key(deploy_id)?;
    let pattern = FactKeyPattern::parse(key.as_str())?;
    let candidates = source
        .list_candidates(island, &pattern, session)?
        .into_iter()
        .filter(|candidate| {
            candidate.key() == &key
                && matches!(
                    candidate.status(),
                    CandidateStatus::Verified | CandidateStatus::Conflict
                )
        })
        .collect::<Vec<_>>();
    let payloads = source.read_payloads(island, &candidates, session)?;
    let mut decoded = Vec::new();
    for candidate in &candidates {
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let fact = decode_deploy_decision_fact(&key, payload)?;
        if fact.deploy_id != *deploy_id
            || fact.expected_serving_commit_id != fact.manifest.serving_commit.serving_commit_id
            || fact.serving_epoch != fact.manifest.serving_commit.epoch
        {
            return Err(DeployError::DeployFactMismatch {
                deploy_id: deploy_id.clone(),
            });
        }
        decoded.push(candidate_to_decision(candidate, fact));
    }
    select_deploy_decision(decoded, key)
}

pub fn read_deploy_cleanup_done(
    source: &dyn FactSource,
    island: &IslandId,
    session: &BusSession,
    deploy_id: &DeployId,
) -> DeployResult<Option<DeployCleanupDoneFact>> {
    let key = deploy_cleanup_done_fact_key(deploy_id)?;
    let pattern = FactKeyPattern::parse(key.as_str())?;
    let candidates = source
        .list_candidates(island, &pattern, session)?
        .into_iter()
        .filter(|candidate| {
            candidate.key() == &key
                && matches!(
                    candidate.status(),
                    CandidateStatus::Verified | CandidateStatus::Conflict
                )
        })
        .collect::<Vec<_>>();
    let payloads = source.read_payloads(island, &candidates, session)?;
    let mut decoded = Vec::new();
    for candidate in &candidates {
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let fact = decode_deploy_cleanup_done_fact(&key, payload)?;
        if fact.deploy_id != *deploy_id {
            return Err(DeployError::DeployFactMismatch {
                deploy_id: deploy_id.clone(),
            });
        }
        decoded.push((candidate.content_hash().clone(), fact));
    }
    decoded.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(decoded.into_iter().next().map(|(_hash, fact)| fact))
}

pub fn select_deploy_decision(
    mut candidates: Vec<DeployDecisionCandidate>,
    key: FactKey,
) -> DeployResult<DeployDecisionSelection> {
    if candidates.is_empty() {
        return Err(DeployError::DeployFactMissing { key });
    }
    candidates.sort_by(compare_decision_candidates);
    let winner = candidates.remove(0);
    Ok(DeployDecisionSelection {
        winner,
        superseded: candidates,
    })
}

fn candidate_to_decision(
    candidate: &FactCandidate,
    fact: DeployDecisionFact,
) -> DeployDecisionCandidate {
    DeployDecisionCandidate {
        fact,
        author: candidate.author().clone(),
        content_hash: candidate.content_hash().clone(),
    }
}

fn compare_decision_candidates(
    left: &DeployDecisionCandidate,
    right: &DeployDecisionCandidate,
) -> Ordering {
    right
        .fact
        .serving_epoch
        .cmp(&left.fact.serving_epoch)
        .then_with(|| left.content_hash.cmp(&right.content_hash))
}

fn encode_deploy_fact(
    payload: DeployFactPayload,
    context: &'static str,
) -> DeployResult<FactPayload> {
    encode(&payload, context).map(Into::into)
}

async fn write_bus_deploy_fact(
    bus: &BusActorHandle,
    session: &BusSession,
    key: FactKey,
    payload: FactPayload,
) -> DeployResult<WrittenDeployFact> {
    match bus
        .write_fact_payload(session, key.clone(), payload)
        .await?
    {
        FactWriteOutcome::Inserted(fact) => Ok(WrittenDeployFact::inserted(
            key,
            fact.content_hash().clone(),
        )),
        FactWriteOutcome::AlreadyPresent(fact) => Ok(WrittenDeployFact::already_present(
            key,
            fact.content_hash().clone(),
        )),
        FactWriteOutcome::Conflict(fact) => Err(DeployError::DeployFactConflict {
            key,
            principal: fact.author().clone(),
            content_hash: fact.content_hash().clone(),
        }),
    }
}
