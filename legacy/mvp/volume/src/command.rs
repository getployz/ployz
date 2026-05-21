use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use mvp_bus::{BusActorHandle, BusSession, FactContentHash, FactKey, FactPayload, Payload};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{
    LeaseBook, LeaseClaimed, LeaseContentHash, LeaseEpoch, LeaseFact, LeaseHolder, LeaseState,
    LeaseTimestamp,
};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactKind, FactSource, ProjectionFactPayload,
    payload_matches_key,
};

use crate::{
    VolumeError, VolumeId, VolumeNamespace, VolumeOwnershipCandidate, VolumeOwnershipEvidence,
    VolumeOwnershipFact, VolumeOwnershipLeaseEvidence, VolumeReceiveReply, VolumeReceiveRequest,
    VolumeResult, VolumeSnapshotReply, VolumeSnapshotRequest, VolumeTransferId,
    current_volume_owner, volume_ownership_fact_key, volume_receive_subject,
    volume_snapshot_subject,
};

pub trait VolumeFactWriter {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> VolumeResult<()>;

    fn write_fact<'a>(
        &'a mut self,
        session: &'a BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeFactWriteOutcome>> + Send + 'a>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeFactWriteOutcome {
    Inserted,
    AlreadyPresent,
    Conflict,
}

pub trait VolumeParticipantClient {
    fn snapshot<'a>(
        &'a self,
        session: &'a BusSession,
        request: VolumeSnapshotRequest,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeSnapshotReply>> + Send + 'a>>;

    fn receive<'a>(
        &'a self,
        session: &'a BusSession,
        request: VolumeReceiveRequest,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeReceiveReply>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct BusVolumeParticipantClient {
    bus: BusActorHandle,
}

impl BusVolumeParticipantClient {
    #[must_use]
    pub fn new(bus: BusActorHandle) -> Self {
        Self { bus }
    }
}

impl VolumeParticipantClient for BusVolumeParticipantClient {
    fn snapshot<'a>(
        &'a self,
        session: &'a BusSession,
        request: VolumeSnapshotRequest,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeSnapshotReply>> + Send + 'a>> {
        Box::pin(async move {
            let subject = volume_snapshot_subject(&request.source)?;
            let response = self
                .bus
                .request(session, subject, encode_payload(&request)?, timeout)
                .await?;
            decode_payload(response.payload(), "snapshot reply")
        })
    }

    fn receive<'a>(
        &'a self,
        session: &'a BusSession,
        request: VolumeReceiveRequest,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeReceiveReply>> + Send + 'a>> {
        Box::pin(async move {
            let subject = volume_receive_subject(&request.target)?;
            let response = self
                .bus
                .request(session, subject, encode_payload(&request)?, timeout)
                .await?;
            decode_payload(response.payload(), "receive reply")
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeTransferTimeouts {
    pub participant: Duration,
}

impl VolumeTransferTimeouts {
    #[must_use]
    pub fn new(participant: Duration) -> Self {
        Self { participant }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeTransferCommand {
    namespace: VolumeNamespace,
    volume: VolumeId,
    transfer_id: VolumeTransferId,
    target: NodeId,
    observed_at: LeaseTimestamp,
    commit_observed_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
}

impl VolumeTransferCommand {
    #[must_use]
    pub fn new(
        namespace: VolumeNamespace,
        volume: VolumeId,
        transfer_id: VolumeTransferId,
        target: NodeId,
        observed_at: LeaseTimestamp,
        commit_observed_at: LeaseTimestamp,
        expires_at: LeaseTimestamp,
    ) -> Self {
        Self {
            namespace,
            volume,
            transfer_id,
            target,
            observed_at,
            commit_observed_at,
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeTransferResult {
    pub visible_nodes: VisibleNodes,
    pub old_owner: NodeId,
    pub new_owner: NodeId,
    pub transfer_id: VolumeTransferId,
    pub snapshot_id: crate::VolumeSnapshotId,
    pub snapshot_guid: crate::VolumeSnapshotGuid,
    pub bytes_transferred: u64,
    pub lease_holder: LeaseHolder,
    pub lease_epoch: LeaseEpoch,
    pub lease_claim_hash: LeaseContentHash,
    pub source_cleanup: VolumeSourceCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeSourceCleanup {
    Deferred,
}

pub struct VolumeTransferExecutor<C> {
    session: BusSession,
    client: C,
    visible_nodes: VisibleNodes,
    timeouts: VolumeTransferTimeouts,
}

impl<C> VolumeTransferExecutor<C>
where
    C: VolumeParticipantClient,
{
    #[must_use]
    pub fn new(
        session: BusSession,
        client: C,
        visible_nodes: VisibleNodes,
        timeouts: VolumeTransferTimeouts,
    ) -> Self {
        Self {
            session,
            client,
            visible_nodes,
            timeouts,
        }
    }

    pub async fn transfer(
        &self,
        store: &mut (impl VolumeFactWriter + FactSource),
        command: VolumeTransferCommand,
    ) -> VolumeResult<VolumeTransferResult> {
        let Some(current) =
            current_volume_owner(store, &self.session, &command.namespace, &command.volume)?
        else {
            return Err(VolumeError::MissingCurrentOwner {
                namespace: command.namespace,
                volume: command.volume,
            });
        };
        let old_owner = current.fact.owner.clone();
        let next_epoch = next_ownership_epoch(&current.fact)?;
        let ownership_key =
            volume_ownership_fact_key(&command.namespace, &command.volume, next_epoch)?;
        store.preflight(&self.session, &ownership_key)?;
        let lease = self.prepare_lease_claim(store, &command).await?;
        let snapshot = self
            .client
            .snapshot(
                &self.session,
                VolumeSnapshotRequest {
                    source: old_owner.clone(),
                    target: command.target.clone(),
                    namespace: command.namespace.clone(),
                    volume: command.volume.clone(),
                    transfer_id: command.transfer_id.clone(),
                },
                self.timeouts.participant,
            )
            .await?;
        validate_snapshot_reply(&command, &old_owner, &snapshot)?;
        let receive = self
            .client
            .receive(
                &self.session,
                VolumeReceiveRequest {
                    source: old_owner.clone(),
                    target: command.target.clone(),
                    namespace: command.namespace.clone(),
                    volume: command.volume.clone(),
                    transfer_id: command.transfer_id.clone(),
                    snapshot_id: snapshot.snapshot_id.clone(),
                    snapshot_guid: snapshot.snapshot_guid.clone(),
                    bytes: snapshot.bytes,
                },
                self.timeouts.participant,
            )
            .await?;
        validate_receive_reply(&command, &old_owner, &snapshot, &receive)?;
        assert_current_volume_lease(store, &self.session, &lease, command.commit_observed_at)?;
        assert_current_owner_unchanged(store, &self.session, &current)?;
        let fact = VolumeOwnershipFact {
            namespace: command.namespace,
            volume: command.volume,
            epoch: next_epoch,
            owner: command.target.clone(),
            evidence: VolumeOwnershipEvidence::transferred(
                old_owner.clone(),
                command.transfer_id,
                receive.snapshot_id.clone(),
                receive.snapshot_guid.clone(),
                receive.bytes,
                VolumeOwnershipLeaseEvidence::new(
                    lease.holder.clone(),
                    lease.epoch,
                    lease.claim_hash,
                ),
            ),
            visible_nodes: self.visible_nodes.clone(),
        };
        let payload = fact.to_payload()?;
        let content_hash = FactContentHash::for_payload(&payload);
        store.preflight(&self.session, &ownership_key)?;
        let write_outcome = store
            .write_fact(&self.session, ownership_key.clone(), payload)
            .await?;
        reject_conflict_write(ownership_key.clone(), write_outcome)?;
        assert_current_owner_matches(store, &self.session, &fact, &content_hash)?;
        Ok(VolumeTransferResult {
            visible_nodes: self.visible_nodes.clone(),
            old_owner,
            new_owner: command.target,
            transfer_id: receive.transfer_id,
            snapshot_id: receive.snapshot_id,
            snapshot_guid: receive.snapshot_guid,
            bytes_transferred: receive.bytes,
            lease_holder: lease.holder,
            lease_epoch: lease.epoch,
            lease_claim_hash: lease.claim_hash,
            source_cleanup: VolumeSourceCleanup::Deferred,
        })
    }

    async fn prepare_lease_claim(
        &self,
        store: &mut (impl VolumeFactWriter + FactSource),
        command: &VolumeTransferCommand,
    ) -> VolumeResult<VolumeLeaseHandle> {
        let resource = command.volume.lease_resource(&command.namespace);
        let state = lease_state_from_source(store, &self.session, &resource, command.observed_at)?;
        let epoch = match state {
            LeaseState::Vacant { next_epoch, .. }
            | LeaseState::Expired {
                next_epoch: Some(next_epoch),
                ..
            }
            | LeaseState::Released {
                next_epoch: Some(next_epoch),
                ..
            } => next_epoch,
            LeaseState::Active { current, .. } => {
                return Err(VolumeError::LeaseConflict {
                    holder: current.holder.clone(),
                    epoch: current.epoch,
                });
            }
            LeaseState::Expired {
                next_epoch: None, ..
            }
            | LeaseState::Released {
                next_epoch: None, ..
            } => return Err(VolumeError::EpochOverflow),
        };
        let holder = LeaseHolder::new(self.session.principal().as_str());
        let claim = LeaseClaimed::new(
            resource.clone(),
            holder.clone(),
            epoch,
            command.observed_at,
            command.expires_at,
        );
        let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
        let key = FactKey::parse(format!("/facts/lease/{resource}/claimed/{epoch}"))?;
        let payload = ProjectionFactPayload::LeaseClaimed(claim)
            .to_fact_bytes()
            .map(FactPayload::from)
            .map_err(|error| VolumeError::ProjectionPayload {
                operation: "encode",
                message: error.to_string(),
            })?;
        store.preflight(&self.session, &key)?;
        let write_outcome = store
            .write_fact(&self.session, key.clone(), payload)
            .await?;
        reject_conflict_write(key, write_outcome)?;
        Ok(VolumeLeaseHandle {
            resource,
            holder,
            epoch,
            claim_hash,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeLeaseHandle {
    resource: mvp_lease::LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
}

fn next_ownership_epoch(fact: &VolumeOwnershipFact) -> VolumeResult<u64> {
    let current_key = volume_ownership_fact_key(&fact.namespace, &fact.volume, fact.epoch)?;
    fact.epoch
        .checked_add(1)
        .ok_or(VolumeError::OwnershipEpochOverflow { key: current_key })
}

fn reject_conflict_write(key: FactKey, outcome: VolumeFactWriteOutcome) -> VolumeResult<()> {
    match outcome {
        VolumeFactWriteOutcome::Inserted | VolumeFactWriteOutcome::AlreadyPresent => Ok(()),
        VolumeFactWriteOutcome::Conflict => Err(VolumeError::FactConflict { key }),
    }
}

fn assert_current_owner_unchanged(
    source: &impl FactSource,
    session: &BusSession,
    expected: &VolumeOwnershipCandidate,
) -> VolumeResult<()> {
    let current = current_volume_owner(
        source,
        session,
        &expected.fact.namespace,
        &expected.fact.volume,
    )?;
    let Some(current) = current else {
        return Err(VolumeError::StaleOwnership {
            namespace: expected.fact.namespace.clone(),
            volume: expected.fact.volume.clone(),
            expected_epoch: expected.fact.epoch,
            actual_epoch: None,
        });
    };
    if current.fact == expected.fact && current.content_hash == expected.content_hash {
        return Ok(());
    }
    Err(VolumeError::StaleOwnership {
        namespace: expected.fact.namespace.clone(),
        volume: expected.fact.volume.clone(),
        expected_epoch: expected.fact.epoch,
        actual_epoch: Some(current.fact.epoch),
    })
}

fn assert_current_owner_matches(
    source: &impl FactSource,
    session: &BusSession,
    expected: &VolumeOwnershipFact,
    expected_hash: &FactContentHash,
) -> VolumeResult<()> {
    let current = current_volume_owner(source, session, &expected.namespace, &expected.volume)?;
    let Some(current) = current else {
        return Err(VolumeError::StaleOwnership {
            namespace: expected.namespace.clone(),
            volume: expected.volume.clone(),
            expected_epoch: expected.epoch,
            actual_epoch: None,
        });
    };
    if current.fact == *expected && &current.content_hash == expected_hash {
        return Ok(());
    }
    Err(VolumeError::StaleOwnership {
        namespace: expected.namespace.clone(),
        volume: expected.volume.clone(),
        expected_epoch: expected.epoch,
        actual_epoch: Some(current.fact.epoch),
    })
}

fn validate_snapshot_reply(
    command: &VolumeTransferCommand,
    old_owner: &NodeId,
    reply: &VolumeSnapshotReply,
) -> VolumeResult<()> {
    if &reply.source != old_owner {
        return Err(VolumeError::SnapshotSourceMismatch {
            expected: old_owner.clone(),
            actual: reply.source.clone(),
        });
    }
    if reply.transfer_id != command.transfer_id
        || reply.namespace != command.namespace
        || reply.volume != command.volume
    {
        return Err(VolumeError::SnapshotEvidenceMismatch {
            transfer_id: command.transfer_id.clone(),
        });
    }
    Ok(())
}

fn validate_receive_reply(
    command: &VolumeTransferCommand,
    old_owner: &NodeId,
    snapshot: &VolumeSnapshotReply,
    reply: &VolumeReceiveReply,
) -> VolumeResult<()> {
    if reply.target != command.target {
        return Err(VolumeError::ReceiveTargetMismatch {
            expected: command.target.clone(),
            actual: reply.target.clone(),
        });
    }
    if &reply.source != old_owner
        || reply.transfer_id != command.transfer_id
        || reply.namespace != command.namespace
        || reply.volume != command.volume
    {
        return Err(VolumeError::ReceiveEvidenceMismatch {
            transfer_id: command.transfer_id.clone(),
        });
    }
    if reply.snapshot_id != snapshot.snapshot_id
        || reply.snapshot_guid != snapshot.snapshot_guid
        || reply.bytes != snapshot.bytes
    {
        return Err(VolumeError::ReceivedSnapshotMismatch {
            snapshot_id: reply.snapshot_id.clone(),
            snapshot_guid: reply.snapshot_guid.clone(),
        });
    }
    Ok(())
}

fn lease_state_from_source(
    source: &impl FactSource,
    session: &BusSession,
    resource: &mvp_lease::LeaseResource,
    now: LeaseTimestamp,
) -> VolumeResult<LeaseState> {
    let pattern = mvp_bus::FactKeyPattern::parse(format!("/facts/lease/{resource}/>"))?;
    let candidates = source.list_candidates(session.island(), &pattern, session)?;
    let payloads = source.read_payloads(session.island(), &candidates, session)?;
    let book = LeaseBook::new();
    for candidate in lease_candidates(&candidates) {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            return Err(VolumeError::UnreadableLeaseCandidate {
                key: candidate.key().clone(),
                status: candidate.status(),
            });
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            return Err(VolumeError::MissingLeasePayload {
                key: candidate.key().clone(),
            });
        };
        let payload =
            ProjectionFactPayload::from_fact_bytes(payload.as_bytes()).map_err(|error| {
                VolumeError::ProjectionPayload {
                    operation: "decode",
                    message: error.to_string(),
                }
            })?;
        if !payload_matches_key(candidate, &payload) {
            return Err(VolumeError::MalformedLeaseCandidate {
                key: candidate.key().clone(),
            });
        }
        match payload {
            ProjectionFactPayload::LeaseClaimed(fact) => {
                book.record_observed_fact(LeaseFact::Claimed(fact));
            }
            ProjectionFactPayload::LeaseRenewed(fact) => {
                book.record_observed_fact(LeaseFact::Renewed(fact));
            }
            ProjectionFactPayload::LeaseReleased(fact) => {
                book.record_observed_fact(LeaseFact::Released(fact));
            }
            ProjectionFactPayload::NodeJoined(_)
            | ProjectionFactPayload::PeerAdmitted(_)
            | ProjectionFactPayload::NodeRemovalStarted(_)
            | ProjectionFactPayload::NodeTombstoned(_)
            | ProjectionFactPayload::ServiceRegistered(_)
            | ProjectionFactPayload::ServingCommit(_)
            | ProjectionFactPayload::RouteCommit(_)
            | ProjectionFactPayload::GatewayCommit(_)
            | ProjectionFactPayload::DnsCommit(_)
            | ProjectionFactPayload::AcmeHttp01Presented(_)
            | ProjectionFactPayload::AcmeHttp01Cleared(_)
            | ProjectionFactPayload::AcmeCertificateActivated(_) => {}
        }
    }
    Ok(book.state(resource, now))
}

fn assert_current_volume_lease(
    source: &impl FactSource,
    session: &BusSession,
    lease: &VolumeLeaseHandle,
    now: LeaseTimestamp,
) -> VolumeResult<()> {
    match lease_state_from_source(source, session, &lease.resource, now)? {
        LeaseState::Active { current, .. }
            if current.holder == lease.holder
                && current.epoch == lease.epoch
                && current.content_hash == lease.claim_hash =>
        {
            Ok(())
        }
        LeaseState::Active { .. }
        | LeaseState::Vacant { .. }
        | LeaseState::Expired { .. }
        | LeaseState::Released { .. } => Err(VolumeError::StaleLease {
            resource: lease.resource.clone(),
            holder: lease.holder.clone(),
            epoch: lease.epoch,
            claim_hash: lease.claim_hash,
        }),
    }
}

fn lease_candidates(candidates: &[FactCandidate]) -> impl Iterator<Item = &FactCandidate> {
    candidates.iter().filter(|candidate| {
        matches!(
            candidate.kind(),
            FactKind::LeaseClaimed | FactKind::LeaseRenewed | FactKind::LeaseReleased
        )
    })
}

pub fn encode_payload<T: serde::Serialize>(value: &T) -> VolumeResult<Payload> {
    serde_json::to_vec(value)
        .map(Payload::from)
        .map_err(|error| VolumeError::Serialization {
            message: error.to_string(),
        })
}

pub fn decode_payload<T: serde::de::DeserializeOwned>(
    payload: &Payload,
    operation: &'static str,
) -> VolumeResult<T> {
    serde_json::from_slice(payload.as_bytes()).map_err(|error| VolumeError::ProjectionPayload {
        operation,
        message: error.to_string(),
    })
}
