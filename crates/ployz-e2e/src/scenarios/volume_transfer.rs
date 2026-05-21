use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::error::{PrimitiveFailure, VolumeFailure};
use ployz::operation::{
    AuthorityContext, AuthorityEpoch, EvidenceKind, FenceEpoch, FenceToken, IdempotencyKey,
    MutationContext, OperationEvidence, OperationId, OperationPort, PrincipalId, ResourceId,
    ScopeId, TerminalMarker,
};
use ployz::volume::{
    CleanupFailureReason, CleanupStatus, FinalDeltaReceipt, OwnershipCommit, OwnershipEpoch,
    OwnershipVerification, ReceiveReceipt, SnapshotReceipt, SourceWatermark, SourceWriteStatus,
    VolumeClaimCheck, VolumeClaimPort, VolumeCleanupFailure, VolumeCleanupPort, VolumeId,
    VolumeOwner, VolumeOwnershipPort, VolumeSnapshotId, VolumeSourcePort, VolumeTargetPort,
    VolumeTransferEngine, VolumeTransferFingerprint, VolumeTransferMode, VolumeTransferPlan,
    VolumeTransferRequest,
};

#[derive(Clone)]
struct FakeClaims {
    checks: Rc<RefCell<VecDeque<VolumeClaimCheck>>>,
}

impl VolumeClaimPort for FakeClaims {
    fn check_transfer_claim(
        &self,
        _context: &MutationContext,
        _plan: &VolumeTransferPlan,
    ) -> Result<VolumeClaimCheck, VolumeFailure> {
        Ok(self
            .checks
            .borrow_mut()
            .pop_front()
            .unwrap_or(VolumeClaimCheck::Current))
    }
}

#[derive(Clone)]
struct FakeSource {
    mutations: Rc<RefCell<Vec<&'static str>>>,
}

impl VolumeSourcePort for FakeSource {
    fn stop_writes(
        &self,
        _context: &MutationContext,
        _plan: &VolumeTransferPlan,
    ) -> Result<SourceWriteStatus, VolumeFailure> {
        self.mutations.borrow_mut().push("stop_writes");
        Ok(SourceWriteStatus::Stopped)
    }

    fn snapshot(
        &self,
        _context: &MutationContext,
        _plan: &VolumeTransferPlan,
    ) -> Result<SnapshotReceipt, VolumeFailure> {
        self.mutations.borrow_mut().push("snapshot");
        Ok(SnapshotReceipt {
            snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
            source_watermark: SourceWatermark::new(5),
        })
    }

    fn final_delta(
        &self,
        _context: &MutationContext,
        _plan: &VolumeTransferPlan,
    ) -> Result<FinalDeltaReceipt, VolumeFailure> {
        self.mutations.borrow_mut().push("final_delta");
        Ok(FinalDeltaReceipt {
            source_watermark: SourceWatermark::new(5),
        })
    }
}

#[derive(Clone)]
struct FakeTarget {
    receive: ReceiveReceipt,
}

impl VolumeTargetPort for FakeTarget {
    fn receive(
        &self,
        _context: &MutationContext,
        _plan: &VolumeTransferPlan,
        _snapshot: &SnapshotReceipt,
        _final_delta: &FinalDeltaReceipt,
    ) -> Result<ReceiveReceipt, VolumeFailure> {
        Ok(self.receive.clone())
    }
}

#[derive(Clone)]
struct FakeOwnership {
    verifications: Rc<RefCell<VecDeque<OwnershipVerification>>>,
}

impl VolumeOwnershipPort for FakeOwnership {
    fn commit_ownership(
        &self,
        _context: &MutationContext,
        plan: &VolumeTransferPlan,
        _receive: &ReceiveReceipt,
    ) -> Result<OwnershipCommit, VolumeFailure> {
        Ok(OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.target.clone(),
            epoch: plan.next_epoch,
            source_watermark: plan.expected_source_watermark,
        })
    }

    fn verify_ownership(
        &self,
        _context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<OwnershipVerification, VolumeFailure> {
        let fallback = OwnershipVerification::Verified(OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.target.clone(),
            epoch: plan.next_epoch,
            source_watermark: plan.expected_source_watermark,
        });
        Ok(self
            .verifications
            .borrow_mut()
            .pop_front()
            .unwrap_or(fallback))
    }
}

#[derive(Clone)]
struct FakeCleanup {
    result: Result<CleanupStatus, VolumeCleanupFailure>,
}

impl VolumeCleanupPort for FakeCleanup {
    fn cleanup_source_artifact(
        &self,
        _context: &MutationContext,
        _commit: &OwnershipCommit,
        _artifact: &ployz::volume::CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeCleanupFailure> {
        self.result.clone()
    }
}

#[derive(Clone, Default)]
struct FakeOperations {
    terminal: Rc<RefCell<Vec<TerminalMarker>>>,
}

impl OperationPort for FakeOperations {
    fn record_evidence(&self, evidence: OperationEvidence) -> Result<(), PrimitiveFailure> {
        assert_eq!(evidence.kind, EvidenceKind::Checkpoint);
        Ok(())
    }

    fn terminalize(
        &self,
        _operation: &OperationId,
        marker: TerminalMarker,
    ) -> Result<(), PrimitiveFailure> {
        self.terminal.borrow_mut().push(marker);
        Ok(())
    }
}

fn context() -> MutationContext {
    MutationContext {
        operation: OperationId::parse("volume-transfer-1").expect("operation"),
        idempotency: IdempotencyKey::parse("idem-volume-1").expect("idempotency"),
        authority: AuthorityContext {
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            epoch: AuthorityEpoch::new(7),
        },
        fence: Some(FenceToken {
            resource: ResourceId::parse("volume:data").expect("resource"),
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: FenceEpoch::new(3),
        }),
        deadline: UNIX_EPOCH + Duration::from_secs(60),
    }
}

fn plan() -> VolumeTransferPlan {
    VolumeTransferPlan {
        volume: VolumeId::parse("data").expect("volume"),
        source: VolumeOwner::parse("node-a").expect("source"),
        target: VolumeOwner::parse("node-b").expect("target"),
        expected_source_watermark: SourceWatermark::new(5),
        next_epoch: OwnershipEpoch::new(2),
        cleanup_artifact: ployz::volume::CleanupArtifactId::parse("source-temp-data")
            .expect("cleanup artifact"),
    }
}

fn engine(
    claims: Vec<VolumeClaimCheck>,
    mutations: Rc<RefCell<Vec<&'static str>>>,
    verifications: Rc<RefCell<VecDeque<OwnershipVerification>>>,
    cleanup: Result<CleanupStatus, VolumeCleanupFailure>,
) -> VolumeTransferEngine<
    FakeClaims,
    FakeSource,
    FakeTarget,
    FakeOwnership,
    FakeCleanup,
    FakeOperations,
> {
    VolumeTransferEngine::new(
        FakeClaims {
            checks: Rc::new(RefCell::new(VecDeque::from(claims))),
        },
        FakeSource { mutations },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
        },
        FakeOwnership { verifications },
        FakeCleanup { result: cleanup },
        FakeOperations::default(),
    )
}

#[test]
fn stale_claim_rejects_before_source_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = engine(
        vec![VolumeClaimCheck::Stale],
        mutations.clone(),
        Rc::new(RefCell::new(VecDeque::new())),
        Ok(CleanupStatus::Done),
    );

    assert_eq!(
        transfer.transfer(&context(), &plan()),
        Err(VolumeFailure::StaleFence)
    );
    assert!(mutations.borrow().is_empty());
}

#[test]
fn stale_claim_rejects_before_each_later_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = engine(
        vec![VolumeClaimCheck::Current, VolumeClaimCheck::Stale],
        mutations.clone(),
        Rc::new(RefCell::new(VecDeque::new())),
        Ok(CleanupStatus::Done),
    );

    assert_eq!(
        transfer.transfer(&context(), &plan()),
        Err(VolumeFailure::StaleFence)
    );
    assert_eq!(mutations.borrow().as_slice(), ["stop_writes"]);
}

#[test]
fn cleanup_failure_remains_visible_without_rewriting_ownership() {
    let transfer = engine(
        vec![VolumeClaimCheck::Current; 5],
        Rc::new(RefCell::new(Vec::new())),
        Rc::new(RefCell::new(VecDeque::new())),
        Err(VolumeCleanupFailure {
            artifact: ployz::volume::CleanupArtifactId::parse("source-temp-data")
                .expect("cleanup artifact"),
            reason: CleanupFailureReason::DeleteFailed,
        }),
    );

    let outcome = transfer
        .transfer(&context(), &plan())
        .expect("ownership committed");

    assert_eq!(
        outcome.ownership.owner,
        VolumeOwner::parse("node-b").expect("target")
    );
    assert!(matches!(outcome.cleanup, CleanupStatus::Pending(_)));
}

#[test]
fn receive_receipt_must_match_snapshot_and_target_before_ownership_commit() {
    let transfer = VolumeTransferEngine::new(
        FakeClaims {
            checks: Rc::new(RefCell::new(VecDeque::from(vec![
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
            ]))),
        },
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("wrong-snap").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
        },
        FakeOwnership {
            verifications: Rc::new(RefCell::new(VecDeque::new())),
        },
        FakeCleanup {
            result: Ok(CleanupStatus::Done),
        },
        FakeOperations::default(),
    );

    assert_eq!(
        transfer.transfer(&context(), &plan()),
        Err(VolumeFailure::ReceiveFailed)
    );
}

#[test]
fn retry_after_ownership_checkpoint_requires_verified_ownership() {
    let verifications = Rc::new(RefCell::new(VecDeque::from([
        OwnershipVerification::Missing,
        OwnershipVerification::Verified(OwnershipCommit {
            volume: VolumeId::parse("data").expect("volume"),
            owner: VolumeOwner::parse("node-b").expect("owner"),
            epoch: OwnershipEpoch::new(2),
            source_watermark: SourceWatermark::new(5),
        }),
    ])));

    let first_attempt = engine(
        vec![VolumeClaimCheck::Current; 5],
        Rc::new(RefCell::new(Vec::new())),
        verifications.clone(),
        Ok(CleanupStatus::Done),
    );
    assert_eq!(
        first_attempt.transfer(&context(), &plan()),
        Err(VolumeFailure::OwnershipCommitRejected)
    );

    let retry_mutations = Rc::new(RefCell::new(Vec::new()));
    let second_attempt = engine(
        vec![VolumeClaimCheck::Current],
        retry_mutations.clone(),
        verifications,
        Ok(CleanupStatus::Done),
    );
    second_attempt
        .transfer_request(VolumeTransferRequest {
            context: context(),
            plan: plan(),
            mode: VolumeTransferMode::ResumeAfterOwnershipCheckpoint(
                VolumeTransferFingerprint::new(&context(), &plan()),
            ),
        })
        .expect("verified ownership resumes success");
    assert!(retry_mutations.borrow().is_empty());
}
