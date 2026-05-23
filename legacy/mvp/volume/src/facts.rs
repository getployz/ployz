use mvp_bus::{BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, PrincipalId};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{LeaseContentHash, LeaseEpoch, LeaseHolder};
use mvp_projection::{CandidateStatus, FactSource};
use serde::{Deserialize, Serialize};

use crate::{
    VolumeError, VolumeId, VolumeNamespace, VolumeResult, VolumeSnapshotGuid, VolumeSnapshotId,
    VolumeTransferId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeOwnershipFact {
    pub namespace: VolumeNamespace,
    pub volume: VolumeId,
    pub epoch: u64,
    pub owner: NodeId,
    pub evidence: VolumeOwnershipEvidence,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeOwnershipEvidence {
    Initialized {
        initialized_by: LeaseHolder,
    },
    Transferred {
        source: NodeId,
        transfer_id: VolumeTransferId,
        snapshot_id: VolumeSnapshotId,
        snapshot_guid: VolumeSnapshotGuid,
        bytes_transferred: u64,
        lease_holder: LeaseHolder,
        lease_epoch: LeaseEpoch,
        lease_claim_hash: LeaseContentHash,
    },
}

impl VolumeOwnershipEvidence {
    #[must_use]
    pub fn transferred(
        source: NodeId,
        transfer_id: VolumeTransferId,
        snapshot_id: VolumeSnapshotId,
        snapshot_guid: VolumeSnapshotGuid,
        bytes_transferred: u64,
        lease: VolumeOwnershipLeaseEvidence,
    ) -> Self {
        Self::Transferred {
            source,
            transfer_id,
            snapshot_id,
            snapshot_guid,
            bytes_transferred,
            lease_holder: lease.holder,
            lease_epoch: lease.epoch,
            lease_claim_hash: lease.claim_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeOwnershipLeaseEvidence {
    pub holder: LeaseHolder,
    pub epoch: LeaseEpoch,
    pub claim_hash: LeaseContentHash,
}

impl VolumeOwnershipLeaseEvidence {
    #[must_use]
    pub fn new(holder: LeaseHolder, epoch: LeaseEpoch, claim_hash: LeaseContentHash) -> Self {
        Self {
            holder,
            epoch,
            claim_hash,
        }
    }
}

impl VolumeOwnershipFact {
    pub fn to_payload(&self) -> VolumeResult<FactPayload> {
        serde_json::to_vec(self)
            .map(FactPayload::from)
            .map_err(|error| VolumeError::Serialization {
                message: error.to_string(),
            })
    }

    pub fn from_payload(key: &FactKey, payload: &FactPayload) -> VolumeResult<Self> {
        let fact = serde_json::from_slice::<Self>(payload.as_bytes()).map_err(|error| {
            VolumeError::DecodeOwnershipPayload {
                key: key.clone(),
                message: error.to_string(),
            }
        })?;
        let (namespace, volume, epoch) = parse_ownership_fact_key(key)?;
        if namespace != fact.namespace || volume != fact.volume || epoch != fact.epoch {
            return Err(VolumeError::FactKeyPayloadMismatch {
                key: key.clone(),
                namespace: fact.namespace,
                volume: fact.volume,
            });
        }
        Ok(fact)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeOwnershipCandidate {
    pub fact: VolumeOwnershipFact,
    pub author: PrincipalId,
    pub content_hash: FactContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeOwnershipReadModel {
    pub current: Option<VolumeOwnershipCandidate>,
    pub superseded: Vec<VolumeOwnershipCandidate>,
}

pub fn current_volume_owner(
    source: &impl FactSource,
    session: &BusSession,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
) -> VolumeResult<Option<VolumeOwnershipCandidate>> {
    Ok(current_volume_ownership(source, session, namespace, volume)?.current)
}

pub fn current_volume_ownership(
    source: &impl FactSource,
    session: &BusSession,
    namespace: &VolumeNamespace,
    volume: &VolumeId,
) -> VolumeResult<VolumeOwnershipReadModel> {
    let pattern = volume_ownership_pattern(namespace, volume)?;
    let candidates = source.list_candidates(session.island(), &pattern, session)?;
    let payloads = source.read_payloads(session.island(), &candidates, session)?;
    let mut ownership = Vec::new();
    for candidate in &candidates {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            return Err(VolumeError::UnreadableOwnershipCandidate {
                key: candidate.key().clone(),
                status: candidate.status(),
            });
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            return Err(VolumeError::MissingOwnershipPayload {
                key: candidate.key().clone(),
            });
        };
        let fact = VolumeOwnershipFact::from_payload(candidate.key(), payload)?;
        ownership.push(VolumeOwnershipCandidate {
            fact,
            author: candidate.author().clone(),
            content_hash: candidate.content_hash().clone(),
        });
    }
    ownership.sort_by(|left, right| {
        right
            .fact
            .epoch
            .cmp(&left.fact.epoch)
            .then_with(|| left.content_hash.cmp(&right.content_hash))
    });
    let Some((current, superseded)) = ownership.split_first() else {
        return Ok(VolumeOwnershipReadModel {
            current: None,
            superseded: Vec::new(),
        });
    };
    Ok(VolumeOwnershipReadModel {
        current: Some(current.clone()),
        superseded: superseded.to_vec(),
    })
}

pub fn volume_ownership_fact_key(
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    epoch: u64,
) -> VolumeResult<FactKey> {
    FactKey::parse(format!(
        "/facts/volume/{}/{}/ownership/{epoch}",
        namespace.as_str(),
        volume.as_str()
    ))
    .map_err(VolumeError::from)
}

fn parse_ownership_fact_key(key: &FactKey) -> VolumeResult<(VolumeNamespace, VolumeId, u64)> {
    let segments = key.segments().collect::<Vec<_>>();
    let ["facts", "volume", namespace, volume, "ownership", epoch] = segments.as_slice() else {
        return Err(VolumeError::WrongFactKeyShape { key: key.clone() });
    };
    let epoch = epoch
        .parse::<u64>()
        .map_err(|_| VolumeError::WrongFactKeyShape { key: key.clone() })?;
    Ok((
        VolumeNamespace::parse(*namespace)?,
        VolumeId::parse(*volume)?,
        epoch,
    ))
}

fn volume_ownership_pattern(
    namespace: &VolumeNamespace,
    volume: &VolumeId,
) -> VolumeResult<FactKeyPattern> {
    FactKeyPattern::parse(format!(
        "/facts/volume/{}/{}/ownership/*",
        namespace.as_str(),
        volume.as_str()
    ))
    .map_err(VolumeError::from)
}
