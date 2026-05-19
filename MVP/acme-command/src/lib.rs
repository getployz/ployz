mod p2panda;

use mvp_acme::{
    AcmeChallengeId, AcmeHttp01ClearedFact, AcmeHttp01PresentedFact, AcmeKeyAuthorization,
};
use mvp_bus::{BusError, BusSession, FactKey, FactKeyPattern, PrincipalId};
use mvp_identity::VisibleNodes;
use mvp_lease::{
    LeaseBook, LeaseClaimed, LeaseContentHash, LeaseEpoch, LeaseFact, LeaseHolder, LeaseReleased,
    LeaseState, LeaseTimestamp,
};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactKind, FactSource, ProjectionFactPayload,
    payload_matches_key,
};
use thiserror::Error;

pub use p2panda::{PandaAcmeCommandAdapter, PandaAcmeHttp01Publisher};

#[derive(Debug, Error)]
pub enum AcmeCommandError {
    #[error("ACME lease conflict with {holder} at epoch {epoch}")]
    Conflict {
        holder: LeaseHolder,
        epoch: LeaseEpoch,
    },
    #[error("ACME challenge mismatch: expected {expected}, got {actual}")]
    ChallengeMismatch { expected: String, actual: String },
    #[error("ACME lease is stale")]
    StaleLease,
    #[error("ACME lease candidate {key} is not readable: {status:?}")]
    UnreadableLeaseCandidate {
        key: FactKey,
        status: CandidateStatus,
    },
    #[error("ACME lease candidate {key} is missing payload bytes")]
    MissingLeasePayload { key: FactKey },
    #[error("ACME lease candidate {key} payload does not match its key")]
    MalformedLeaseCandidate { key: FactKey },
    #[error("ACME lease epoch overflow")]
    EpochOverflow,
    #[error("principal {principal} cannot write fact {key}")]
    UnauthorizedFactWrite {
        principal: PrincipalId,
        key: FactKey,
    },
    #[error(transparent)]
    Acme(#[from] mvp_acme::AcmeCoordinationError),
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    FactSource(#[from] mvp_projection::FactSourceError),
    #[error(transparent)]
    Panda(#[from] mvp_p2panda_facts::PandaFactError),
    #[error("ACME command projection payload {operation} failed: {message}")]
    ProjectionPayload {
        operation: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeLeaseHandle {
    challenge: AcmeChallengeId,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
}

impl AcmeLeaseHandle {
    #[must_use]
    pub fn new(
        challenge: AcmeChallengeId,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
    ) -> Self {
        Self {
            challenge,
            holder,
            epoch,
            claim_hash,
        }
    }

    #[must_use]
    pub fn challenge(&self) -> &AcmeChallengeId {
        &self.challenge
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> LeaseContentHash {
        self.claim_hash
    }

    pub fn assert_challenge(&self, challenge: &AcmeChallengeId) -> Result<(), AcmeCommandError> {
        if &self.challenge == challenge {
            return Ok(());
        }
        Err(AcmeCommandError::ChallengeMismatch {
            expected: challenge_label(&self.challenge),
            actual: challenge_label(challenge),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeClaimCommand {
    challenge: AcmeChallengeId,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
}

impl AcmeClaimCommand {
    #[must_use]
    pub fn new(
        challenge: AcmeChallengeId,
        acquired_at: LeaseTimestamp,
        expires_at: LeaseTimestamp,
    ) -> Self {
        Self {
            challenge,
            acquired_at,
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmePresentHttp01Command {
    challenge: AcmeChallengeId,
    lease: AcmeLeaseHandle,
    thumbprint: String,
    published_at: LeaseTimestamp,
}

impl AcmePresentHttp01Command {
    #[must_use]
    pub fn new(
        challenge: AcmeChallengeId,
        lease: AcmeLeaseHandle,
        thumbprint: impl Into<String>,
        published_at: LeaseTimestamp,
    ) -> Self {
        Self {
            challenge,
            lease,
            thumbprint: thumbprint.into(),
            published_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeClearHttp01Command {
    challenge: AcmeChallengeId,
    lease: AcmeLeaseHandle,
    cleared_at: LeaseTimestamp,
}

impl AcmeClearHttp01Command {
    #[must_use]
    pub fn new(
        challenge: AcmeChallengeId,
        lease: AcmeLeaseHandle,
        cleared_at: LeaseTimestamp,
    ) -> Self {
        Self {
            challenge,
            lease,
            cleared_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCommandResult {
    lease: AcmeLeaseHandle,
    visible_nodes: VisibleNodes,
}

impl ClaimCommandResult {
    #[must_use]
    pub fn lease(&self) -> &AcmeLeaseHandle {
        &self.lease
    }

    #[must_use]
    pub fn into_lease(self) -> AcmeLeaseHandle {
        self.lease
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &VisibleNodes {
        &self.visible_nodes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmePresentResult {
    key_authorization: AcmeKeyAuthorization,
    visible_nodes: VisibleNodes,
}

impl AcmePresentResult {
    #[must_use]
    pub fn key_authorization(&self) -> &AcmeKeyAuthorization {
        &self.key_authorization
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &VisibleNodes {
        &self.visible_nodes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeClearResult {
    release_recorded: bool,
    visible_nodes: VisibleNodes,
}

impl AcmeClearResult {
    #[must_use]
    pub fn release_recorded(&self) -> bool {
        self.release_recorded
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &VisibleNodes {
        &self.visible_nodes
    }
}

pub(crate) struct AcmeCommandExecutor {
    session: BusSession,
    visible_nodes: VisibleNodes,
}

impl AcmeCommandExecutor {
    #[must_use]
    pub(crate) fn new(session: BusSession, visible_nodes: VisibleNodes) -> Self {
        Self {
            session,
            visible_nodes,
        }
    }

    #[must_use]
    pub(crate) fn session(&self) -> &BusSession {
        &self.session
    }

    pub(crate) fn prepare_claim(
        &self,
        source: &impl FactSource,
        command: AcmeClaimCommand,
    ) -> Result<PreparedFact<ClaimCommandResult>, AcmeCommandError> {
        let state = lease_state_from_source(
            source,
            &self.session,
            &command.challenge,
            command.acquired_at,
        )?;
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
                return Err(AcmeCommandError::Conflict {
                    holder: current.holder.clone(),
                    epoch: current.epoch,
                });
            }
            LeaseState::Expired {
                next_epoch: None, ..
            }
            | LeaseState::Released {
                next_epoch: None, ..
            } => return Err(AcmeCommandError::EpochOverflow),
        };
        let fact = LeaseClaimed::new(
            command.challenge.lease_resource().clone(),
            LeaseHolder::new(self.session.principal().as_str()),
            epoch,
            command.acquired_at,
            command.expires_at,
        );
        let claim_hash = LeaseFact::Claimed(fact.clone()).content_hash();
        let key = FactKey::parse(command.challenge.lease_claimed_fact_key(epoch))?;
        Ok(PreparedFact {
            key,
            payload: ProjectionFactPayload::LeaseClaimed(fact),
            result: ClaimCommandResult {
                lease: AcmeLeaseHandle::new(
                    command.challenge,
                    LeaseHolder::new(self.session.principal().as_str()),
                    epoch,
                    claim_hash,
                ),
                visible_nodes: self.visible_nodes.clone(),
            },
        })
    }

    pub(crate) fn prepare_present(
        &self,
        source: &impl FactSource,
        command: AcmePresentHttp01Command,
    ) -> Result<PreparedFact<AcmePresentResult>, AcmeCommandError> {
        command.lease.assert_challenge(&command.challenge)?;
        assert_current_lease(source, &self.session, &command.lease, command.published_at)?;
        let key_authorization = AcmeKeyAuthorization::parse_for_token(
            command.challenge.token(),
            format!("{}.{}", command.challenge.token(), command.thumbprint),
        )?;
        let fact = AcmeHttp01PresentedFact::from_parts(
            command.challenge.clone(),
            key_authorization.clone(),
            command.lease.holder.clone(),
            command.lease.epoch,
            command.lease.claim_hash,
            command.published_at,
        )?;
        let key = FactKey::parse(command.challenge.presented_fact_key(command.lease.epoch))?;
        Ok(PreparedFact {
            key,
            payload: ProjectionFactPayload::AcmeHttp01Presented(fact),
            result: AcmePresentResult {
                key_authorization,
                visible_nodes: self.visible_nodes.clone(),
            },
        })
    }

    pub(crate) fn prepare_clear(
        &self,
        source: &impl FactSource,
        command: AcmeClearHttp01Command,
    ) -> Result<PreparedClear, AcmeCommandError> {
        command.lease.assert_challenge(&command.challenge)?;
        assert_current_lease(source, &self.session, &command.lease, command.cleared_at)?;
        let release = LeaseReleased::new_at(
            command.challenge.lease_resource().clone(),
            command.lease.holder.clone(),
            command.lease.epoch,
            command.lease.claim_hash,
            command.cleared_at,
        );
        let release_key = FactKey::parse(command.challenge.lease_released_fact_key(
            command.lease.epoch,
            command.lease.claim_hash,
            release.release,
        ))?;
        let clear_key = FactKey::parse(
            command
                .challenge
                .cleared_fact_key(command.lease.epoch, command.lease.claim_hash),
        )?;
        let clear = AcmeHttp01ClearedFact::from_parts(
            command.challenge,
            command.lease.holder,
            command.lease.epoch,
            command.lease.claim_hash,
            command.cleared_at,
        );
        Ok(PreparedClear {
            release: PreparedPayload {
                key: release_key,
                payload: ProjectionFactPayload::LeaseReleased(release),
            },
            clear: PreparedPayload {
                key: clear_key,
                payload: ProjectionFactPayload::AcmeHttp01Cleared(clear),
            },
            result: AcmeClearResult {
                release_recorded: true,
                visible_nodes: self.visible_nodes.clone(),
            },
        })
    }
}

pub(crate) struct PreparedFact<T> {
    pub(crate) key: FactKey,
    pub(crate) payload: ProjectionFactPayload,
    pub(crate) result: T,
}

pub(crate) struct PreparedPayload {
    pub(crate) key: FactKey,
    pub(crate) payload: ProjectionFactPayload,
}

pub(crate) struct PreparedClear {
    pub(crate) release: PreparedPayload,
    pub(crate) clear: PreparedPayload,
    pub(crate) result: AcmeClearResult,
}

pub(crate) trait AcmeFactWriter {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> Result<(), AcmeCommandError>;

    fn write(
        &mut self,
        session: &BusSession,
        key: FactKey,
        payload: ProjectionFactPayload,
    ) -> impl std::future::Future<Output = Result<(), AcmeCommandError>> + Send;
}

#[must_use]
pub fn challenge_label(challenge: &AcmeChallengeId) -> String {
    format!("{} {}", challenge.hostname(), challenge.token())
}

pub fn lease_state_from_source(
    source: &impl FactSource,
    session: &BusSession,
    challenge: &AcmeChallengeId,
    now: LeaseTimestamp,
) -> Result<LeaseState, AcmeCommandError> {
    let pattern = FactKeyPattern::parse(format!("/facts/lease/{}/>", challenge.lease_resource()))?;
    let candidates = source.list_candidates(session.island(), &pattern, session)?;
    let payloads = source.read_payloads(session.island(), &candidates, session)?;
    let book = LeaseBook::new();
    for candidate in lease_candidates(&candidates) {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            return Err(AcmeCommandError::UnreadableLeaseCandidate {
                key: candidate.key().clone(),
                status: candidate.status(),
            });
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            return Err(AcmeCommandError::MissingLeasePayload {
                key: candidate.key().clone(),
            });
        };
        let payload =
            ProjectionFactPayload::from_fact_bytes(payload.as_bytes()).map_err(|error| {
                AcmeCommandError::ProjectionPayload {
                    operation: "decode",
                    message: error.to_string(),
                }
            })?;
        if !payload_matches_key(candidate, &payload) {
            return Err(AcmeCommandError::MalformedLeaseCandidate {
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
            | ProjectionFactPayload::AcmeHttp01Cleared(_) => {}
        }
    }
    Ok(book.state(challenge.lease_resource(), now))
}

fn assert_current_lease(
    source: &impl FactSource,
    session: &BusSession,
    lease: &AcmeLeaseHandle,
    now: LeaseTimestamp,
) -> Result<(), AcmeCommandError> {
    match lease_state_from_source(source, session, &lease.challenge, now)? {
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
        | LeaseState::Released { .. } => Err(AcmeCommandError::StaleLease),
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

#[cfg(test)]
mod tests;
