//! Volume transfer product ports.

use crate::error::{PrimitiveFailure, VolumeFailure};
use crate::operation::{
    AttemptBackend, AttemptCheckpoint, AttemptContext, AttemptFailureDisposition, AttemptIssue,
    AttemptIssuer, AttemptProductError, AttemptSpec, AuthorityPort, IssuedAttempt, MutationContext,
    ResourceId, SubmittedFenceToken,
};

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

pub trait VolumeClaimPort {
    fn check_transfer_claim(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
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
        plan: &VolumeTransferPlan,
    ) -> Result<SourceWriteStatus, VolumeFailure>;

    fn snapshot(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<SnapshotReceipt, VolumeFailure>;

    fn final_delta(
        &self,
        context: &MutationContext,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipVerification {
    Verified(OwnershipCommit),
    Missing,
    Mismatch,
}

pub trait VolumeOwnershipPort {
    fn commit_ownership(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
        receive: &ReceiveReceipt,
    ) -> Result<OwnershipCommit, VolumeFailure>;

    fn verify_ownership(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<OwnershipVerification, VolumeFailure>;
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
        commit: &OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeCleanupFailure>;

    fn cleanup_status(
        &self,
        context: &MutationContext,
        commit: &OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeFailure>;
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

enum VolumeCheckpoint<'a> {
    OwnershipCommitted(&'a OwnershipCommit),
    CleanupPending(&'a CleanupPending),
}

impl VolumeCheckpoint<'_> {
    fn command_checkpoint(&self) -> AttemptCheckpoint {
        match self {
            Self::OwnershipCommitted(commit) => {
                AttemptCheckpoint::new("volume.ownership_committed")
                    .field("volume", commit.volume.as_str())
                    .field("owner", commit.owner.as_str())
                    .field("epoch", commit.epoch.value().to_string())
                    .field("watermark", commit.source_watermark.value().to_string())
            }
            Self::CleanupPending(pending) => AttemptCheckpoint::new("volume.cleanup_pending")
                .field("artifact", pending.artifact().as_str())
                .field(
                    "reason",
                    pending
                        .reason()
                        .map_or("unknown", CleanupFailureReason::as_str),
                ),
        }
    }
}

impl CleanupFailureReason {
    #[must_use]
    fn as_str(&self) -> &'static str {
        match self {
            Self::DeleteFailed => "delete_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeTransferCommand {}

pub struct IssuedVolumeTransferCommand {
    envelope: IssuedAttempt<VolumeTransferCommand>,
    request: VolumeTransferRequest,
}

impl VolumeTransferCommand {
    /// Issues a transfer attempt for the submitted fence.
    ///
    /// The submitted fence participates in the command fingerprint. Reacquiring
    /// or refreshing the fence is a new transfer attempt and must use a fresh
    /// idempotency key.
    pub fn issue<A>(
        issuer: &AttemptIssuer<A>,
        command: AttemptIssue,
        request: VolumeTransferRequest,
        submitted_fence: SubmittedFenceToken,
    ) -> Result<IssuedVolumeTransferCommand, PrimitiveFailure>
    where
        A: AuthorityPort,
    {
        let expected_resource =
            ResourceId::parse(format!("volume:{}", request.plan.volume.as_str()))?;
        if submitted_fence.resource != expected_resource {
            return Err(PrimitiveFailure::StaleFence);
        }

        let envelope = issuer.issue(
            command,
            volume_transfer_attempt_spec(&request, submitted_fence),
        )?;
        Ok(IssuedVolumeTransferCommand { envelope, request })
    }
}

fn volume_transfer_attempt_spec(
    request: &VolumeTransferRequest,
    submitted_fence: SubmittedFenceToken,
) -> AttemptSpec {
    AttemptSpec::new("volume-transfer", "ployz.volume.transfer.v1")
        .field("volume", request.plan.volume.as_str())
        .field("source", request.plan.source.as_str())
        .field("target", request.plan.target.as_str())
        .field_u64(
            "expected_source_watermark",
            request.plan.expected_source_watermark.value(),
        )
        .field_u64("next_epoch", request.plan.next_epoch.value())
        .field("cleanup_artifact", request.plan.cleanup_artifact.as_str())
        .resource(format!("volume:{}", request.plan.volume.as_str()))
        .submitted_fence(submitted_fence)
}

pub struct VolumeTransferEngine<G, S, T, W, C, O> {
    claims: G,
    source: S,
    target: T,
    ownership: W,
    cleanup: C,
    commands: O,
}

impl<G, S, T, W, C, O> VolumeTransferEngine<G, S, T, W, C, O> {
    #[must_use]
    pub fn new(claims: G, source: S, target: T, ownership: W, cleanup: C, commands: O) -> Self {
        Self {
            claims,
            source,
            target,
            ownership,
            cleanup,
            commands,
        }
    }
}

impl<G, S, T, W, C, O> VolumeTransferEngine<G, S, T, W, C, O>
where
    G: VolumeClaimPort,
    S: VolumeSourcePort,
    T: VolumeTargetPort,
    W: VolumeOwnershipPort,
    C: VolumeCleanupPort,
    O: AttemptBackend,
{
    pub fn transfer(
        &self,
        command: IssuedVolumeTransferCommand,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let IssuedVolumeTransferCommand { envelope, request } = command;
        self.commands.run_with_replay_and_failure_disposition(
            envelope,
            AttemptFailureDisposition::Interrupted,
            |context| self.transfer_scoped(context, &request.plan),
            |mutation| self.verify_replayed_success(mutation, &request.plan),
        )
    }

    fn transfer_scoped(
        &self,
        context: &AttemptContext<'_>,
        plan: &VolumeTransferPlan,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let mutation = context.mutation();
        self.ensure_current_claim(mutation, plan)?;
        if self.source.stop_writes(mutation, plan)? != SourceWriteStatus::Stopped {
            return Err(VolumeFailure::SourceWriteStillOpen);
        }
        self.ensure_current_claim(mutation, plan)?;
        let snapshot = self.source.snapshot(mutation, plan)?;
        if !snapshot.has_expected_watermark(plan) {
            return Err(VolumeFailure::SnapshotFailed);
        }
        self.ensure_current_claim(mutation, plan)?;
        let final_delta = self.source.final_delta(mutation, plan)?;
        if !final_delta.has_expected_watermark(plan) {
            return Err(VolumeFailure::SnapshotFailed);
        }
        self.ensure_current_claim(mutation, plan)?;
        let receive = self
            .target
            .receive(mutation, plan, &snapshot, &final_delta)?;
        if !receive.matches_transfer(&snapshot, plan) {
            return Err(VolumeFailure::ReceiveFailed);
        }
        self.ensure_current_claim(mutation, plan)?;
        let ownership = self.ownership.commit_ownership(mutation, plan, &receive)?;

        self.finish_transfer(context, plan, ownership)
    }

    fn verify_replayed_success(
        &self,
        mutation: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let ownership = self.verified_ownership(mutation, plan)?;
        let cleanup = self
            .cleanup
            .cleanup_status(mutation, &ownership, &plan.cleanup_artifact)?;
        Ok(VolumeTransferOutcome { ownership, cleanup })
    }

    fn finish_transfer(
        &self,
        context: &AttemptContext<'_>,
        plan: &VolumeTransferPlan,
        ownership: OwnershipCommit,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let mutation = context.mutation();
        let verified = self.verified_ownership(mutation, plan)?;
        if verified != ownership {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        if !ownership.matches_plan(plan) {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        context
            .checkpoint(VolumeCheckpoint::OwnershipCommitted(&ownership).command_checkpoint())
            .map_err(map_primitive_to_volume)?;
        self.finish_cleanup(context, ownership, &plan.cleanup_artifact)
    }

    fn finish_cleanup(
        &self,
        context: &AttemptContext<'_>,
        ownership: OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let cleanup =
            match self
                .cleanup
                .cleanup_source_artifact(context.mutation(), &ownership, artifact)
            {
                Ok(status) => status,
                Err(error) => CleanupStatus::Pending(CleanupPending::from_failure(error)),
            };
        if let CleanupStatus::Pending(pending) = &cleanup {
            context
                .checkpoint(VolumeCheckpoint::CleanupPending(pending).command_checkpoint())
                .map_err(map_primitive_to_volume)?;
        }

        Ok(VolumeTransferOutcome { ownership, cleanup })
    }

    fn verified_ownership(
        &self,
        mutation: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<OwnershipCommit, VolumeFailure> {
        let OwnershipVerification::Verified(ownership) =
            self.ownership.verify_ownership(mutation, plan)?
        else {
            return Err(VolumeFailure::OwnershipCommitRejected);
        };
        if !ownership.matches_plan(plan) {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        Ok(ownership)
    }

    fn ensure_current_claim(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<(), VolumeFailure> {
        match self.claims.check_transfer_claim(context, plan)? {
            VolumeClaimCheck::Current => Ok(()),
            VolumeClaimCheck::Missing | VolumeClaimCheck::Stale => Err(VolumeFailure::StaleFence),
        }
    }
}

fn map_primitive_to_volume(error: PrimitiveFailure) -> VolumeFailure {
    match error {
        PrimitiveFailure::StaleFence => VolumeFailure::StaleFence,
        PrimitiveFailure::MalformedPayload => VolumeFailure::InvalidPayload,
        PrimitiveFailure::Conflict
        | PrimitiveFailure::Unauthorized
        | PrimitiveFailure::Timeout
        | PrimitiveFailure::NoResponder
        | PrimitiveFailure::FreshnessUnknown
        | PrimitiveFailure::TerminalAlreadyWritten => VolumeFailure::OwnershipCommitRejected,
        PrimitiveFailure::OperationAlreadySucceeded => VolumeFailure::OperationAlreadySucceeded,
        PrimitiveFailure::OperationInProgress => VolumeFailure::OperationInProgress,
        PrimitiveFailure::OperationAlreadyFailed => VolumeFailure::OperationAlreadyFailed,
        PrimitiveFailure::OperationInterrupted => VolumeFailure::Interrupted,
    }
}

impl AttemptProductError for VolumeFailure {
    fn from_primitive_failure(error: PrimitiveFailure) -> Self {
        map_primitive_to_volume(error)
    }

    fn terminalization_failed(product: Self, terminalization: PrimitiveFailure) -> Self {
        Self::AttemptTerminalizationFailed {
            product: Box::new(product),
            terminalization,
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
    use crate::operation::{
        AttemptIssuer, AuthorityDecision, AuthorityEpoch, AuthorityPort, ClaimHash, FenceEpoch,
        IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };

    #[test]
    fn empty_volume_id_is_invalid_payload() {
        assert_eq!(VolumeId::parse(""), Err(VolumeFailure::InvalidPayload));
    }

    #[test]
    fn volume_maps_replay_states_to_visible_failures() {
        assert_eq!(
            map_primitive_to_volume(PrimitiveFailure::OperationInProgress),
            VolumeFailure::OperationInProgress
        );
        assert_eq!(
            map_primitive_to_volume(PrimitiveFailure::OperationAlreadyFailed),
            VolumeFailure::OperationAlreadyFailed
        );
        assert_eq!(
            map_primitive_to_volume(PrimitiveFailure::OperationAlreadySucceeded),
            VolumeFailure::OperationAlreadySucceeded
        );
        assert_eq!(
            map_primitive_to_volume(PrimitiveFailure::OperationInterrupted),
            VolumeFailure::Interrupted
        );
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
    fn volume_transfer_command_issue_derives_payload_from_plan() {
        let request = VolumeTransferRequest { plan: plan() };
        let changed_target = VolumeTransferRequest {
            plan: VolumeTransferPlan {
                target: VolumeOwner::parse("node-c").expect("target"),
                ..request.plan.clone()
            },
        };

        let first = VolumeTransferCommand::issue(
            &AttemptIssuer::new(AllowAuthority),
            issue(),
            request.clone(),
            fence("volume:data"),
        )
        .expect("first command");
        let second = VolumeTransferCommand::issue(
            &AttemptIssuer::new(AllowAuthority),
            issue(),
            changed_target,
            fence("volume:data"),
        )
        .expect("second command");

        assert_ne!(
            first.envelope.fingerprint_for_test().payload_hash(),
            second.envelope.fingerprint_for_test().payload_hash()
        );
    }

    #[test]
    fn volume_transfer_command_issue_rejects_fence_for_wrong_volume() {
        let result = VolumeTransferCommand::issue(
            &AttemptIssuer::new(AllowAuthority),
            issue(),
            VolumeTransferRequest { plan: plan() },
            fence("volume:other"),
        );

        assert!(matches!(result, Err(PrimitiveFailure::StaleFence)));
    }

    struct AllowAuthority;

    impl AuthorityPort for AllowAuthority {
        fn decide(
            &self,
            _principal: &PrincipalId,
            _scope: &ScopeId,
        ) -> Result<AuthorityDecision, PrimitiveFailure> {
            Ok(AuthorityDecision::Allowed(AuthorityEpoch::new(7)))
        }
    }

    fn issue() -> AttemptIssue {
        AttemptIssue {
            operation: OperationId::parse("volume-transfer-1").expect("operation"),
            idempotency: IdempotencyKey::parse("idem-volume-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            deadline: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    fn fence(resource: &str) -> SubmittedFenceToken {
        SubmittedFenceToken {
            resource: ResourceId::parse(resource).expect("resource"),
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: FenceEpoch::new(3).expect("fence epoch"),
            claim_hash: ClaimHash::parse("claim-hash-a").expect("claim hash"),
        }
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
