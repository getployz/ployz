//! Volume transfer product ports.

use crate::error::{PrimitiveFailure, VolumeFailure};
use crate::operation::{CommandBackend, CommandContext, CommandEnvelope, MutationContext};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeId(String);

impl VolumeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, VolumeFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VolumeOwner(String);

impl VolumeOwner {
    pub fn parse(value: impl Into<String>) -> Result<Self, VolumeFailure> {
        parse_non_empty(value, Self)
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnershipEpoch(u64);

impl OwnershipEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceWatermark(u64);

impl SourceWatermark {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
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
    pub mode: VolumeTransferMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeTransferMode {
    Start,
    VerifyCommittedOwnership,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalDeltaReceipt {
    pub source_watermark: SourceWatermark,
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
    Pending(CleanupArtifactId),
}

pub trait VolumeCleanupPort {
    fn cleanup_source_artifact(
        &self,
        context: &MutationContext,
        commit: &OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeCleanupFailure>;
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
pub enum VolumeTransferCommand {}

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
    O: CommandBackend,
{
    pub fn transfer(
        &self,
        command: CommandEnvelope<VolumeTransferCommand>,
        request: VolumeTransferRequest,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        self.commands
            .run(command, map_primitive_to_volume, |context| {
                self.transfer_scoped(context, &request.plan, &request.mode)
            })
    }

    fn transfer_scoped(
        &self,
        context: &CommandContext<'_>,
        plan: &VolumeTransferPlan,
        mode: &VolumeTransferMode,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let mutation = context.mutation();
        if let VolumeTransferMode::VerifyCommittedOwnership = mode {
            return self.finish_verified_transfer(context, plan);
        }

        self.ensure_current_claim(mutation, plan)?;
        if self.source.stop_writes(mutation, plan)? != SourceWriteStatus::Stopped {
            return Err(VolumeFailure::SourceWriteStillOpen);
        }
        self.ensure_current_claim(mutation, plan)?;
        let snapshot = self.source.snapshot(mutation, plan)?;
        if snapshot.source_watermark != plan.expected_source_watermark {
            return Err(VolumeFailure::SnapshotFailed);
        }
        self.ensure_current_claim(mutation, plan)?;
        let final_delta = self.source.final_delta(mutation, plan)?;
        if final_delta.source_watermark != plan.expected_source_watermark {
            return Err(VolumeFailure::SnapshotFailed);
        }
        self.ensure_current_claim(mutation, plan)?;
        let receive = self
            .target
            .receive(mutation, plan, &snapshot, &final_delta)?;
        if receive.snapshot != snapshot.snapshot || receive.target != plan.target {
            return Err(VolumeFailure::ReceiveFailed);
        }
        self.ensure_current_claim(mutation, plan)?;
        let ownership = self.ownership.commit_ownership(mutation, plan, &receive)?;

        context.checkpoint().map_err(map_primitive_to_volume)?;
        self.finish_transfer(context, plan, ownership)
    }

    fn finish_verified_transfer(
        &self,
        context: &CommandContext<'_>,
        plan: &VolumeTransferPlan,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let mutation = context.mutation();
        self.ensure_current_claim(mutation, plan)?;
        let OwnershipVerification::Verified(ownership) =
            self.ownership.verify_ownership(mutation, plan)?
        else {
            return Err(VolumeFailure::OwnershipCommitRejected);
        };
        if ownership.volume != plan.volume
            || ownership.owner != plan.target
            || ownership.epoch != plan.next_epoch
            || ownership.source_watermark != plan.expected_source_watermark
        {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        self.finish_cleanup(context, ownership, &plan.cleanup_artifact)
    }

    fn finish_transfer(
        &self,
        context: &CommandContext<'_>,
        plan: &VolumeTransferPlan,
        ownership: OwnershipCommit,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let mutation = context.mutation();
        let OwnershipVerification::Verified(verified) =
            self.ownership.verify_ownership(mutation, plan)?
        else {
            return Err(VolumeFailure::OwnershipCommitRejected);
        };
        if verified != ownership {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        if ownership.volume != plan.volume
            || ownership.owner != plan.target
            || ownership.epoch != plan.next_epoch
            || ownership.source_watermark != plan.expected_source_watermark
        {
            return Err(VolumeFailure::OwnershipCommitRejected);
        }
        self.finish_cleanup(context, ownership, &plan.cleanup_artifact)
    }

    fn finish_cleanup(
        &self,
        context: &CommandContext<'_>,
        ownership: OwnershipCommit,
        artifact: &CleanupArtifactId,
    ) -> Result<VolumeTransferOutcome, VolumeFailure> {
        let cleanup =
            match self
                .cleanup
                .cleanup_source_artifact(context.mutation(), &ownership, artifact)
            {
                Ok(status) => status,
                Err(error) => CleanupStatus::Pending(error.artifact),
            };
        if matches!(cleanup, CleanupStatus::Pending(_)) {
            context.checkpoint().map_err(map_primitive_to_volume)?;
        }

        Ok(VolumeTransferOutcome { ownership, cleanup })
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
        | PrimitiveFailure::TerminalAlreadyWritten
        | PrimitiveFailure::ReplayUnavailable => VolumeFailure::OwnershipCommitRejected,
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
}
