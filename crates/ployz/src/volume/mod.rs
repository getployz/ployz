//! Volume transfer product ports.

use crate::error::VolumeFailure;
use crate::operation::{ClaimHash, FenceEpoch, MutationContext, PrincipalId, ResourceId};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeId(String);

impl VolumeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, VolumeFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeOwner(String);

impl VolumeOwner {
    pub fn parse(value: impl Into<String>) -> Result<Self, VolumeFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeSnapshotId(String);

impl VolumeSnapshotId {
    pub fn parse(value: impl Into<String>) -> Result<Self, VolumeFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CleanupArtifactId(String);

impl CleanupArtifactId {
    pub fn parse(value: impl Into<String>) -> Result<Self, VolumeFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnershipEpoch(u64);

impl OwnershipEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }

    fn immediately_follows(self, previous: Self) -> bool {
        previous.0.checked_add(1).is_some_and(|next| next == self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceWatermark(u64);

impl SourceWatermark {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeTransferPlan {
    pub volume: VolumeId,
    pub source: VolumeOwner,
    pub target: VolumeOwner,
    pub expected_source_watermark: SourceWatermark,
    pub next_epoch: OwnershipEpoch,
    pub cleanup_artifact: CleanupArtifactId,
}

impl VolumeTransferPlan {
    fn resource(&self) -> Result<ResourceId, VolumeFailure> {
        ResourceId::parse(format!("volume:{}", self.volume.as_str()))
            .map_err(|_| VolumeFailure::InvalidPayload)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeTransferRequest {
    pub plan: VolumeTransferPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeClaimCheck {
    Current,
    Missing,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeTransferGuard {
    resource: ResourceId,
    holder: PrincipalId,
    epoch: FenceEpoch,
    claim_hash: ClaimHash,
}

impl VolumeTransferGuard {
    fn from_context(
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<Self, VolumeFailure> {
        let Some(fence) = context.submitted_fence() else {
            return Err(VolumeFailure::StaleFence);
        };
        if fence.resource != plan.resource()? || &fence.holder != context.authority().principal() {
            return Err(VolumeFailure::StaleFence);
        }
        Ok(Self {
            resource: fence.resource.clone(),
            holder: fence.holder.clone(),
            epoch: fence.epoch,
            claim_hash: fence.claim_hash.clone(),
        })
    }

    #[must_use]
    pub fn resource(&self) -> &ResourceId {
        &self.resource
    }

    #[must_use]
    pub fn holder(&self) -> &PrincipalId {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> FenceEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> &ClaimHash {
        &self.claim_hash
    }
}

pub trait VolumeClaimPort {
    fn begin_transfer(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
        guard: &VolumeTransferGuard,
    ) -> Result<(), VolumeFailure>;

    fn check_transfer_claim(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
        guard: &VolumeTransferGuard,
    ) -> Result<VolumeClaimCheck, VolumeFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceWriteStatus {
    Stopped,
    StillOpen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReceipt {
    pub snapshot: VolumeSnapshotId,
    pub source_watermark: SourceWatermark,
}

impl SnapshotReceipt {
    fn has_expected_watermark(&self, plan: &VolumeTransferPlan) -> bool {
        self.source_watermark == plan.expected_source_watermark
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalDeltaReceipt {
    pub source_watermark: SourceWatermark,
}

impl FinalDeltaReceipt {
    fn has_expected_watermark(&self, plan: &VolumeTransferPlan) -> bool {
        self.source_watermark == plan.expected_source_watermark
    }
}

pub trait VolumeSourcePort {
    fn stop_writes(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
    ) -> Result<SourceWriteStatus, VolumeFailure>;

    fn snapshot(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
    ) -> Result<SnapshotReceipt, VolumeFailure>;

    fn final_delta(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
    ) -> Result<FinalDeltaReceipt, VolumeFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveReceipt {
    pub snapshot: VolumeSnapshotId,
    pub target: VolumeOwner,
}

impl ReceiveReceipt {
    fn matches_transfer(&self, snapshot: &SnapshotReceipt, plan: &VolumeTransferPlan) -> bool {
        self.snapshot == snapshot.snapshot && self.target == plan.target
    }
}

pub trait VolumeTargetPort {
    fn receive(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
        snapshot: &SnapshotReceipt,
        final_delta: &FinalDeltaReceipt,
    ) -> Result<ReceiveReceipt, VolumeFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipCommit {
    pub volume: VolumeId,
    pub owner: VolumeOwner,
    pub epoch: OwnershipEpoch,
    pub source_watermark: SourceWatermark,
}

impl OwnershipCommit {
    fn matches_plan(&self, plan: &VolumeTransferPlan) -> bool {
        self.volume == plan.volume
            && self.owner == plan.target
            && self.epoch == plan.next_epoch
            && self.source_watermark == plan.expected_source_watermark
    }

    fn matches_source_owner(&self, plan: &VolumeTransferPlan) -> bool {
        self.volume == plan.volume && self.owner == plan.source
    }

    fn can_advance_to(&self, plan: &VolumeTransferPlan) -> bool {
        self.matches_source_owner(plan) && plan.next_epoch.immediately_follows(self.epoch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipObservation {
    Present(OwnershipCommit),
    Missing,
}

pub trait VolumeOwnershipPort {
    fn commit_ownership(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
        receive: &ReceiveReceipt,
    ) -> Result<OwnershipCommit, VolumeFailure>;

    fn observe_ownership(
        &self,
        context: &MutationContext,
        volume: &VolumeId,
    ) -> Result<OwnershipObservation, VolumeFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupStatus {
    Done,
    Pending(CleanupPending),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPending {
    artifact: CleanupArtifactId,
    reason: Option<CleanupFailureReason>,
}

impl CleanupPending {
    #[must_use]
    pub fn new(artifact: CleanupArtifactId, reason: Option<CleanupFailureReason>) -> Self {
        Self { artifact, reason }
    }

    #[must_use]
    pub fn from_failure(failure: VolumeCleanupFailure) -> Self {
        Self {
            artifact: failure.artifact,
            reason: Some(failure.reason),
        }
    }

    #[must_use]
    pub fn artifact(&self) -> &CleanupArtifactId {
        &self.artifact
    }

    #[must_use]
    pub fn reason(&self) -> Option<&CleanupFailureReason> {
        self.reason.as_ref()
    }
}

pub trait VolumeCleanupPort {
    fn cleanup_source_artifact(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        commit: &OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeCleanupFailure>;

    fn cleanup_status(
        &self,
        context: &MutationContext,
        commit: &OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeFailure>;

    fn record_cleanup_pending(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        commit: &OwnershipCommit,
        pending: &CleanupPending,
    ) -> Result<(), VolumeFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeCleanupFailure {
    pub artifact: CleanupArtifactId,
    pub reason: CleanupFailureReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupFailureReason {
    DeleteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeTransferOutcome {
    pub ownership: OwnershipCommit,
    pub cleanup: CleanupStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VolumeTransferAction {
    AlreadyTransferred(OwnershipCommit),
    Apply,
}

impl VolumeTransferAction {
    fn from_observation(
        observation: OwnershipObservation,
        plan: &VolumeTransferPlan,
    ) -> Result<Self, VolumeFailure> {
        match observation {
            OwnershipObservation::Present(ownership) if ownership.matches_plan(plan) => {
                Ok(Self::AlreadyTransferred(ownership))
            }
            OwnershipObservation::Present(ownership) if ownership.can_advance_to(plan) => {
                Ok(Self::Apply)
            }
            OwnershipObservation::Present(ownership) if ownership.matches_source_owner(plan) => {
                Err(VolumeFailure::StaleOwnership)
            }
            OwnershipObservation::Missing => Err(VolumeFailure::SourceNotOwner),
            OwnershipObservation::Present(_) => Err(VolumeFailure::SourceNotOwner),
        }
    }
}

pub struct VolumeTransferEngine<G, S, T, W, C> {
    claims: G,
    source: S,
    target: T,
    ownership: W,
    cleanup: C,
}

impl<G, S, T, W, C> VolumeTransferEngine<G, S, T, W, C> {
    #[must_use]
    pub fn new(claims: G, source: S, target: T, ownership: W, cleanup: C) -> Self {
        Self {
            claims,
            source,
            target,
            ownership,
            cleanup,
        }
    }
}

impl<G, S, T, W, C> VolumeTransferEngine<G, S, T, W, C>
where
    G: VolumeClaimPort,
    S: VolumeSourcePort,
    T: VolumeTargetPort,
    W: VolumeOwnershipPort,
    C: VolumeCleanupPort,
{
    pub fn transfer(
        &self,
        context: &MutationContext,
        request: VolumeTransferRequest,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let observation = self
            .ownership
            .observe_ownership(context, &request.plan.volume)?;
        match VolumeTransferAction::from_observation(observation, &request.plan)? {
            VolumeTransferAction::AlreadyTransferred(ownership) => {
                self.observe_cleanup(context, ownership, &request.plan.cleanup_artifact)
            }
            VolumeTransferAction::Apply => self.execute_transfer(context, &request.plan),
        }
    }

    fn execute_transfer(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let guard = self.begin_transfer(context, plan)?;
        if self.source.stop_writes(context, &guard, plan)? != SourceWriteStatus::Stopped {
            return Err(VolumeFailure::SourceWriteStillOpen);
        }
        self.ensure_current_claim(context, &guard, plan)?;
        let snapshot = self.source.snapshot(context, &guard, plan)?;
        if !snapshot.has_expected_watermark(plan) {
            return Err(VolumeFailure::SnapshotFailed);
        }
        self.ensure_current_claim(context, &guard, plan)?;
        let final_delta = self.source.final_delta(context, &guard, plan)?;
        if !final_delta.has_expected_watermark(plan) {
            return Err(VolumeFailure::SnapshotFailed);
        }
        self.ensure_current_claim(context, &guard, plan)?;
        let receive = self
            .target
            .receive(context, &guard, plan, &snapshot, &final_delta)?;
        if !receive.matches_transfer(&snapshot, plan) {
            return Err(VolumeFailure::ReceiveFailed);
        }
        self.ensure_current_claim(context, &guard, plan)?;
        let committed = self
            .ownership
            .commit_ownership(context, &guard, plan, &receive)?;
        if !committed.matches_plan(plan) {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        let ownership = self.observed_desired_ownership(context, plan)?;
        if ownership != committed {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        self.cleanup_after_transfer(context, &guard, plan, ownership)
    }

    fn observe_cleanup(
        &self,
        context: &MutationContext,
        ownership: OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let cleanup = self.cleanup.cleanup_status(context, &ownership, artifact)?;
        Ok(VolumeTransferOutcome { ownership, cleanup })
    }

    fn cleanup_after_transfer(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
        ownership: OwnershipCommit,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        self.ensure_current_claim(context, guard, plan)?;
        let cleanup = match self.cleanup.cleanup_source_artifact(
            context,
            guard,
            &ownership,
            &plan.cleanup_artifact,
        ) {
            Ok(CleanupStatus::Done) => CleanupStatus::Done,
            Ok(CleanupStatus::Pending(pending)) => {
                self.record_pending_cleanup(context, guard, plan, &ownership, pending)?
            }
            Err(error) => {
                let pending = CleanupPending::from_failure(error);
                self.record_pending_cleanup(context, guard, plan, &ownership, pending)?
            }
        };
        Ok(VolumeTransferOutcome { ownership, cleanup })
    }

    fn record_pending_cleanup(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
        ownership: &OwnershipCommit,
        pending: CleanupPending,
    ) -> Result<CleanupStatus, VolumeFailure> {
        self.ensure_current_claim(context, guard, plan)?;
        self.cleanup
            .record_cleanup_pending(context, guard, ownership, &pending)?;
        Ok(CleanupStatus::Pending(pending))
    }

    fn observed_desired_ownership(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<OwnershipCommit, VolumeFailure> {
        match self.ownership.observe_ownership(context, &plan.volume)? {
            OwnershipObservation::Present(ownership) if ownership.matches_plan(plan) => {
                Ok(ownership)
            }
            OwnershipObservation::Present(_) | OwnershipObservation::Missing => {
                Err(VolumeFailure::OwnershipCommitRejected)
            }
        }
    }

    fn begin_transfer(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<VolumeTransferGuard, VolumeFailure> {
        let guard = VolumeTransferGuard::from_context(context, plan)?;
        self.claims.begin_transfer(context, plan, &guard)?;
        Ok(guard)
    }

    fn ensure_current_claim(
        &self,
        context: &MutationContext,
        guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
    ) -> Result<(), VolumeFailure> {
        match self.claims.check_transfer_claim(context, plan, guard)? {
            VolumeClaimCheck::Current => Ok(()),
            VolumeClaimCheck::Missing | VolumeClaimCheck::Stale => Err(VolumeFailure::StaleFence),
        }
    }
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, VolumeFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(VolumeFailure::InvalidPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_volume_id_is_invalid_payload() {
        assert_eq!(VolumeId::parse(""), Err(VolumeFailure::InvalidPayload));
    }

    #[test]
    fn volume_plan_derives_claim_resource() {
        let resource = plan().resource().expect("resource");

        assert_eq!(resource.as_str(), "volume:data");
    }

    #[test]
    fn ownership_commit_must_match_transfer_plan() {
        let plan = plan();
        let commit = OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.target.clone(),
            epoch: plan.next_epoch,
            source_watermark: plan.expected_source_watermark,
        };
        let wrong_owner = OwnershipCommit {
            owner: VolumeOwner::parse("node-c").expect("owner"),
            ..commit.clone()
        };

        assert!(commit.matches_plan(&plan));
        assert!(!wrong_owner.matches_plan(&plan));
    }

    #[test]
    fn receipts_must_match_transfer_plan() {
        let plan = plan();
        let snapshot = SnapshotReceipt {
            snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
            source_watermark: plan.expected_source_watermark,
        };
        let stale_snapshot = SnapshotReceipt {
            source_watermark: SourceWatermark::new(4),
            ..snapshot.clone()
        };
        let final_delta = FinalDeltaReceipt {
            source_watermark: plan.expected_source_watermark,
        };
        let stale_final_delta = FinalDeltaReceipt {
            source_watermark: SourceWatermark::new(4),
        };
        let receive = ReceiveReceipt {
            snapshot: snapshot.snapshot.clone(),
            target: plan.target.clone(),
        };
        let wrong_receive = ReceiveReceipt {
            target: VolumeOwner::parse("node-c").expect("target"),
            ..receive.clone()
        };

        assert!(snapshot.has_expected_watermark(&plan));
        assert!(!stale_snapshot.has_expected_watermark(&plan));
        assert!(final_delta.has_expected_watermark(&plan));
        assert!(!stale_final_delta.has_expected_watermark(&plan));
        assert!(receive.matches_transfer(&snapshot, &plan));
        assert!(!wrong_receive.matches_transfer(&snapshot, &plan));
    }

    #[test]
    fn transfer_action_comes_from_observed_ownership() {
        let plan = plan();
        let desired_owner = OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.target.clone(),
            epoch: plan.next_epoch,
            source_watermark: plan.expected_source_watermark,
        };
        let source_owner = OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.source.clone(),
            epoch: OwnershipEpoch::new(1),
            source_watermark: plan.expected_source_watermark,
        };
        let source_owner_before_transfer_watermark = OwnershipCommit {
            source_watermark: SourceWatermark::new(4),
            ..source_owner.clone()
        };
        let conflicting_owner = OwnershipCommit {
            owner: VolumeOwner::parse("node-c").expect("owner"),
            ..source_owner.clone()
        };

        assert_eq!(
            VolumeTransferAction::from_observation(
                OwnershipObservation::Present(desired_owner.clone()),
                &plan
            ),
            Ok(VolumeTransferAction::AlreadyTransferred(desired_owner))
        );
        assert_eq!(
            VolumeTransferAction::from_observation(
                OwnershipObservation::Present(source_owner),
                &plan
            ),
            Ok(VolumeTransferAction::Apply)
        );
        assert_eq!(
            VolumeTransferAction::from_observation(
                OwnershipObservation::Present(source_owner_before_transfer_watermark),
                &plan
            ),
            Ok(VolumeTransferAction::Apply)
        );
        assert_eq!(
            VolumeTransferAction::from_observation(OwnershipObservation::Missing, &plan),
            Err(VolumeFailure::SourceNotOwner)
        );
        assert_eq!(
            VolumeTransferAction::from_observation(
                OwnershipObservation::Present(conflicting_owner),
                &plan
            ),
            Err(VolumeFailure::SourceNotOwner)
        );
    }

    fn plan() -> VolumeTransferPlan {
        VolumeTransferPlan {
            volume: VolumeId::parse("data").expect("volume"),
            source: VolumeOwner::parse("node-a").expect("source"),
            target: VolumeOwner::parse("node-b").expect("target"),
            expected_source_watermark: SourceWatermark::new(5),
            next_epoch: OwnershipEpoch::new(2),
            cleanup_artifact: CleanupArtifactId::parse("source-temp-data")
                .expect("cleanup artifact"),
        }
    }
}
