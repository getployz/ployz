use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::error::VolumeFailure;
use ployz::operation::{
    AuthorityEpoch, ClaimHash, FenceEpoch, IdempotencyKey, MutationContext, OperationId,
    PrincipalId, ResourceId, ScopeId, SubmittedFenceToken,
};
use ployz::volume::{
    CleanupFailureReason, CleanupPending, CleanupStatus, CurrentSourceOwnership, FinalDeltaReceipt,
    OwnershipCommit, OwnershipEpoch, OwnershipObservation, ReceiveReceipt, SnapshotReceipt,
    SourceWatermark, SourceWriteStatus, VolumeClaimCheck, VolumeClaimPort, VolumeCleanupFailure,
    VolumeCleanupPort, VolumeId, VolumeOwner, VolumeOwnershipPort, VolumeSnapshotId,
    VolumeSourcePort, VolumeTargetPort, VolumeTransferEngine, VolumeTransferGuard,
    VolumeTransferPlan, VolumeTransferRequest,
};

#[derive(Clone)]
struct FakeClaims {
    checks: Rc<RefCell<VecDeque<VolumeClaimCheck>>>,
    begun: Rc<RefCell<bool>>,
}

impl VolumeClaimPort for FakeClaims {
    fn begin_transfer(
        &self,
        context: &MutationContext,
        plan: &VolumeTransferPlan,
    ) -> Result<VolumeTransferGuard, VolumeFailure> {
        if *self.begun.borrow() {
            return Err(VolumeFailure::TransferInProgress);
        }
        let Some(fence) = context.submitted_fence() else {
            return Err(VolumeFailure::StaleFence);
        };
        if fence.resource != plan.resource()? || &fence.holder != context.authority().principal() {
            return Err(VolumeFailure::StaleFence);
        }
        let Some(check) = self.checks.borrow_mut().pop_front() else {
            panic!("unexpected transfer claim check");
        };
        if check != VolumeClaimCheck::Current {
            return Err(VolumeFailure::StaleFence);
        }
        *self.begun.borrow_mut() = true;
        VolumeTransferGuard::for_acquired_claim(
            context,
            plan,
            fence.epoch,
            fence.claim_hash.clone(),
        )
    }

    fn check_transfer_claim(
        &self,
        _context: &MutationContext,
        _plan: &VolumeTransferPlan,
        _guard: &VolumeTransferGuard,
    ) -> Result<VolumeClaimCheck, VolumeFailure> {
        let Some(check) = self.checks.borrow_mut().pop_front() else {
            panic!("unexpected transfer claim check");
        };
        Ok(check)
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
        _guard: &VolumeTransferGuard,
        _plan: &VolumeTransferPlan,
    ) -> Result<SourceWriteStatus, VolumeFailure> {
        self.mutations.borrow_mut().push("stop_writes");
        Ok(SourceWriteStatus::Stopped)
    }

    fn snapshot(
        &self,
        _context: &MutationContext,
        _guard: &VolumeTransferGuard,
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
        _guard: &VolumeTransferGuard,
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
    received: Option<Rc<RefCell<bool>>>,
}

impl VolumeTargetPort for FakeTarget {
    fn receive(
        &self,
        _context: &MutationContext,
        _guard: &VolumeTransferGuard,
        _plan: &VolumeTransferPlan,
        _snapshot: &SnapshotReceipt,
        _final_delta: &FinalDeltaReceipt,
    ) -> Result<ReceiveReceipt, VolumeFailure> {
        if let Some(received) = &self.received {
            *received.borrow_mut() = true;
        }
        Ok(self.receive.clone())
    }
}

#[derive(Clone)]
struct FakeOwnership {
    observations: Rc<RefCell<VecDeque<OwnershipObservation>>>,
    committed: Rc<RefCell<Option<OwnershipCommit>>>,
    commit: Option<OwnershipCommit>,
}

impl VolumeOwnershipPort for FakeOwnership {
    fn commit_ownership(
        &self,
        _context: &MutationContext,
        _guard: &VolumeTransferGuard,
        plan: &VolumeTransferPlan,
        current_source: &CurrentSourceOwnership,
        _receive: &ReceiveReceipt,
    ) -> Result<OwnershipCommit, VolumeFailure> {
        assert_eq!(current_source.ownership().volume, plan.volume);
        assert_eq!(current_source.ownership().owner, plan.source);
        assert_eq!(
            current_source.ownership().epoch.value() + 1,
            plan.next_epoch.value()
        );
        let commit = self.commit.clone().unwrap_or_else(|| OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.target.clone(),
            epoch: plan.next_epoch,
            source_watermark: plan.expected_source_watermark,
        });
        *self.committed.borrow_mut() = Some(commit.clone());
        Ok(commit)
    }

    fn observe_ownership(
        &self,
        _context: &MutationContext,
        _volume: &VolumeId,
    ) -> Result<OwnershipObservation, VolumeFailure> {
        if let Some(observation) = self.observations.borrow_mut().pop_front() {
            return Ok(observation);
        }
        Ok(self
            .committed
            .borrow()
            .clone()
            .map_or(OwnershipObservation::Missing, OwnershipObservation::Present))
    }
}

#[derive(Clone)]
struct FakeCleanup {
    result: Rc<RefCell<Result<CleanupStatus, VolumeCleanupFailure>>>,
    pending: Rc<RefCell<Option<CleanupPending>>>,
}

impl VolumeCleanupPort for FakeCleanup {
    fn cleanup_source_artifact(
        &self,
        _context: &MutationContext,
        _guard: &VolumeTransferGuard,
        _commit: &OwnershipCommit,
        _artifact: &ployz::volume::CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeCleanupFailure> {
        self.result.borrow().clone()
    }

    fn cleanup_status(
        &self,
        _context: &MutationContext,
        _commit: &OwnershipCommit,
        _artifact: &ployz::volume::CleanupArtifactId,
    ) -> Result<CleanupStatus, VolumeFailure> {
        if let Some(pending) = self.pending.borrow().clone() {
            return Ok(CleanupStatus::Pending(pending));
        }
        match self.result.borrow().clone() {
            Ok(status) => Ok(status),
            Err(error) => Ok(CleanupStatus::Pending(CleanupPending::from_failure(error))),
        }
    }

    fn record_cleanup_pending(
        &self,
        _context: &MutationContext,
        _guard: &VolumeTransferGuard,
        _commit: &OwnershipCommit,
        pending: &CleanupPending,
    ) -> Result<(), VolumeFailure> {
        *self.pending.borrow_mut() = Some(pending.clone());
        Ok(())
    }
}

fn fake_claims(checks: Vec<VolumeClaimCheck>) -> FakeClaims {
    FakeClaims {
        checks: Rc::new(RefCell::new(VecDeque::from(checks))),
        begun: Rc::new(RefCell::new(false)),
    }
}

fn begun_claims(checks: Vec<VolumeClaimCheck>) -> FakeClaims {
    FakeClaims {
        checks: Rc::new(RefCell::new(VecDeque::from(checks))),
        begun: Rc::new(RefCell::new(true)),
    }
}

fn fake_cleanup(result: Result<CleanupStatus, VolumeCleanupFailure>) -> FakeCleanup {
    FakeCleanup {
        result: Rc::new(RefCell::new(result)),
        pending: Rc::new(RefCell::new(None)),
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

fn request() -> VolumeTransferRequest {
    VolumeTransferRequest { plan: plan() }
}

fn context(fence: Option<SubmittedFenceToken>) -> MutationContext {
    MutationContext::test_authorized(
        OperationId::parse("volume-transfer-1").expect("operation"),
        IdempotencyKey::parse("idem-volume-1").expect("idempotency"),
        PrincipalId::parse("node-a").expect("principal"),
        ScopeId::parse("cluster").expect("scope"),
        AuthorityEpoch::new(7),
        fence,
        UNIX_EPOCH + Duration::from_secs(60),
    )
}

fn current_fence() -> SubmittedFenceToken {
    fence("volume:data", "node-a")
}

fn fence(resource: &str, holder: &str) -> SubmittedFenceToken {
    SubmittedFenceToken {
        resource: ResourceId::parse(resource).expect("resource"),
        holder: PrincipalId::parse(holder).expect("holder"),
        epoch: FenceEpoch::new(3).expect("fence epoch"),
        claim_hash: ClaimHash::parse("claim-hash-a").expect("claim hash"),
    }
}

fn desired_ownership() -> OwnershipCommit {
    let plan = plan();
    OwnershipCommit {
        volume: plan.volume,
        owner: plan.target,
        epoch: plan.next_epoch,
        source_watermark: plan.expected_source_watermark,
    }
}

fn source_ownership() -> OwnershipCommit {
    let plan = plan();
    OwnershipCommit {
        volume: plan.volume,
        owner: plan.source,
        epoch: OwnershipEpoch::new(1),
        source_watermark: plan.expected_source_watermark,
    }
}

fn source_observations() -> Rc<RefCell<VecDeque<OwnershipObservation>>> {
    Rc::new(RefCell::new(VecDeque::from([
        OwnershipObservation::Present(source_ownership()),
        OwnershipObservation::Present(source_ownership()),
    ])))
}

fn engine(
    claims: Vec<VolumeClaimCheck>,
    mutations: Rc<RefCell<Vec<&'static str>>>,
    observations: Rc<RefCell<VecDeque<OwnershipObservation>>>,
    cleanup: Result<CleanupStatus, VolumeCleanupFailure>,
) -> VolumeTransferEngine<FakeClaims, FakeSource, FakeTarget, FakeOwnership, FakeCleanup> {
    VolumeTransferEngine::new(
        fake_claims(claims),
        FakeSource { mutations },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations,
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(cleanup),
    )
}

#[test]
fn stale_claim_rejects_before_source_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = engine(
        vec![VolumeClaimCheck::Stale],
        mutations.clone(),
        source_observations(),
        Ok(CleanupStatus::Done),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::StaleFence)
    );
    assert!(mutations.borrow().is_empty());
}

#[test]
fn stale_claim_rejects_before_each_later_mutation() {
    let cases = [
        (
            vec![VolumeClaimCheck::Current, VolumeClaimCheck::Stale],
            vec!["stop_writes"],
        ),
        (
            vec![
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Stale,
            ],
            vec!["stop_writes", "snapshot"],
        ),
        (
            vec![
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Stale,
            ],
            vec!["stop_writes", "snapshot", "final_delta"],
        ),
        (
            vec![
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Stale,
            ],
            vec!["stop_writes", "snapshot", "final_delta"],
        ),
    ];

    for (checks, expected_mutations) in cases {
        let mutations = Rc::new(RefCell::new(Vec::new()));
        let transfer = engine(
            checks,
            mutations.clone(),
            source_observations(),
            Ok(CleanupStatus::Done),
        );

        assert_eq!(
            transfer.transfer(&context(Some(current_fence())), request()),
            Err(VolumeFailure::StaleFence)
        );
        assert_eq!(mutations.borrow().as_slice(), expected_mutations.as_slice());
    }
}

#[test]
fn cleanup_failure_remains_visible_without_rewriting_ownership() {
    let transfer = engine(
        vec![VolumeClaimCheck::Current; 8],
        Rc::new(RefCell::new(Vec::new())),
        source_observations(),
        Err(VolumeCleanupFailure {
            artifact: ployz::volume::CleanupArtifactId::parse("source-temp-data")
                .expect("cleanup artifact"),
            reason: CleanupFailureReason::DeleteFailed,
        }),
    );

    let outcome = transfer
        .transfer(&context(Some(current_fence())), request())
        .expect("ownership committed");

    assert_eq!(
        outcome.ownership.owner,
        VolumeOwner::parse("node-b").expect("target")
    );
    let CleanupStatus::Pending(pending) = outcome.cleanup else {
        panic!("cleanup should be pending");
    };
    assert_eq!(pending.reason(), Some(&CleanupFailureReason::DeleteFailed));
}

#[test]
fn cleanup_pending_remains_visible_after_transfer() {
    let transfer = engine(
        vec![VolumeClaimCheck::Current; 8],
        Rc::new(RefCell::new(Vec::new())),
        source_observations(),
        Err(VolumeCleanupFailure {
            artifact: ployz::volume::CleanupArtifactId::parse("actual-temp-data")
                .expect("cleanup artifact"),
            reason: CleanupFailureReason::DeleteFailed,
        }),
    );

    let outcome = transfer
        .transfer(&context(Some(current_fence())), request())
        .expect("ownership committed");

    assert!(matches!(outcome.cleanup, CleanupStatus::Pending(_)));
}

#[test]
fn cleanup_pending_is_observed_on_second_run_after_cleanup_failure() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let cleanup = fake_cleanup(Err(VolumeCleanupFailure {
        artifact: ployz::volume::CleanupArtifactId::parse("actual-temp-data")
            .expect("cleanup artifact"),
        reason: CleanupFailureReason::DeleteFailed,
    }));
    let cleanup_result = cleanup.result.clone();
    let ownership = FakeOwnership {
        observations: source_observations(),
        committed: Rc::new(RefCell::new(None)),
        commit: None,
    };
    let first = VolumeTransferEngine::new(
        fake_claims(vec![VolumeClaimCheck::Current; 8]),
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        ownership.clone(),
        cleanup.clone(),
    );

    let first_outcome = first
        .transfer(&context(Some(current_fence())), request())
        .expect("ownership committed");
    assert!(matches!(first_outcome.cleanup, CleanupStatus::Pending(_)));
    *cleanup_result.borrow_mut() = Ok(CleanupStatus::Done);

    let second_mutations = Rc::new(RefCell::new(Vec::new()));
    let second = VolumeTransferEngine::new(
        fake_claims(Vec::new()),
        FakeSource {
            mutations: second_mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        ownership,
        cleanup,
    );

    let second_outcome = second
        .transfer(&context(None), request())
        .expect("observed ownership reads cleanup visibility");
    assert!(matches!(second_outcome.cleanup, CleanupStatus::Pending(_)));
    assert!(second_mutations.borrow().is_empty());
}

#[test]
fn cleanup_pending_returned_by_cleanup_port_is_recorded_for_second_run() {
    let pending = CleanupPending::new(
        ployz::volume::CleanupArtifactId::parse("already-pending-temp").expect("cleanup artifact"),
        Some(CleanupFailureReason::DeleteFailed),
    );
    let cleanup = fake_cleanup(Ok(CleanupStatus::Pending(pending)));
    let cleanup_result = cleanup.result.clone();
    let ownership = FakeOwnership {
        observations: source_observations(),
        committed: Rc::new(RefCell::new(None)),
        commit: None,
    };
    let first = VolumeTransferEngine::new(
        fake_claims(vec![VolumeClaimCheck::Current; 8]),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        ownership.clone(),
        cleanup.clone(),
    );

    let first_outcome = first
        .transfer(&context(Some(current_fence())), request())
        .expect("ownership committed");
    assert!(matches!(first_outcome.cleanup, CleanupStatus::Pending(_)));
    *cleanup_result.borrow_mut() = Ok(CleanupStatus::Done);

    let second = VolumeTransferEngine::new(
        fake_claims(Vec::new()),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        ownership,
        cleanup,
    );

    let second_outcome = second
        .transfer(&context(None), request())
        .expect("observed ownership reads recorded pending cleanup");
    assert!(matches!(second_outcome.cleanup, CleanupStatus::Pending(_)));
}

#[test]
fn cleanup_pending_record_rechecks_claim_after_cleanup_result() {
    let transfer = engine(
        vec![
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Stale,
        ],
        Rc::new(RefCell::new(Vec::new())),
        source_observations(),
        Err(VolumeCleanupFailure {
            artifact: ployz::volume::CleanupArtifactId::parse("actual-temp-data")
                .expect("cleanup artifact"),
            reason: CleanupFailureReason::DeleteFailed,
        }),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::StaleFence)
    );
}

#[test]
fn receive_receipt_must_match_snapshot_and_target_before_ownership_commit() {
    let transfer = VolumeTransferEngine::new(
        fake_claims(vec![
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
        ]),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("wrong-snap").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: source_observations(),
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::ReceiveFailed)
    );
}

#[test]
fn committed_ownership_must_match_transfer_plan() {
    let wrong_commit = OwnershipCommit {
        volume: VolumeId::parse("data").expect("volume"),
        owner: VolumeOwner::parse("node-c").expect("owner"),
        epoch: OwnershipEpoch::new(2),
        source_watermark: SourceWatermark::new(5),
    };
    let transfer = VolumeTransferEngine::new(
        fake_claims(vec![
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
        ]),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: source_observations(),
            committed: Rc::new(RefCell::new(None)),
            commit: Some(wrong_commit),
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::OwnershipCommitRejected)
    );
}

#[test]
fn current_source_owner_is_rechecked_after_receive_before_commit() {
    let committed = Rc::new(RefCell::new(None));
    let received = Rc::new(RefCell::new(false));
    let changed_owner = OwnershipCommit {
        owner: VolumeOwner::parse("node-c").expect("owner"),
        ..source_ownership()
    };
    let transfer = VolumeTransferEngine::new(
        fake_claims(vec![VolumeClaimCheck::Current; 5]),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: Some(received.clone()),
        },
        FakeOwnership {
            observations: Rc::new(RefCell::new(VecDeque::from([
                OwnershipObservation::Present(source_ownership()),
                OwnershipObservation::Present(changed_owner),
            ]))),
            committed: committed.clone(),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::SourceNotOwner)
    );
    assert!(*received.borrow());
    assert_eq!(*committed.borrow(), None);
}

#[test]
fn stale_claim_after_preflight_rejects_before_ownership_commit() {
    let committed = Rc::new(RefCell::new(None));
    let transfer = VolumeTransferEngine::new(
        fake_claims(vec![
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Current,
            VolumeClaimCheck::Stale,
        ]),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: source_observations(),
            committed: committed.clone(),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::StaleFence)
    );
    assert_eq!(*committed.borrow(), None);
}

#[test]
fn already_transferred_during_preflight_returns_observed_cleanup() {
    let committed = Rc::new(RefCell::new(None));
    let transfer = VolumeTransferEngine::new(
        fake_claims(vec![VolumeClaimCheck::Current; 5]),
        FakeSource {
            mutations: Rc::new(RefCell::new(Vec::new())),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: Rc::new(RefCell::new(VecDeque::from([
                OwnershipObservation::Present(source_ownership()),
                OwnershipObservation::Present(desired_ownership()),
            ]))),
            committed: committed.clone(),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    let outcome = transfer
        .transfer(&context(Some(current_fence())), request())
        .expect("desired ownership observed during preflight");

    assert_eq!(outcome.ownership, desired_ownership());
    assert_eq!(outcome.cleanup, CleanupStatus::Done);
    assert_eq!(*committed.borrow(), None);
}

#[test]
fn concurrent_transfer_guard_rejects_before_source_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = VolumeTransferEngine::new(
        begun_claims(Vec::new()),
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: source_observations(),
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::TransferInProgress)
    );
    assert!(mutations.borrow().is_empty());
}

#[test]
fn stale_source_ownership_epoch_rejects_without_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let stale_source = OwnershipCommit {
        epoch: OwnershipEpoch::new(10),
        ..source_ownership()
    };
    let transfer = VolumeTransferEngine::new(
        fake_claims(Vec::new()),
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: Rc::new(RefCell::new(VecDeque::from([
                OwnershipObservation::Present(stale_source),
            ]))),
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::StaleOwnership)
    );
    assert!(mutations.borrow().is_empty());
}

#[test]
fn missing_ownership_rejects_without_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = VolumeTransferEngine::new(
        fake_claims(Vec::new()),
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: Rc::new(RefCell::new(VecDeque::new())),
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::SourceNotOwner)
    );
    assert!(mutations.borrow().is_empty());
}

#[test]
fn existing_desired_ownership_returns_without_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = VolumeTransferEngine::new(
        fake_claims(Vec::new()),
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: Rc::new(RefCell::new(VecDeque::from([
                OwnershipObservation::Present(desired_ownership()),
            ]))),
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    let outcome = transfer
        .transfer(&context(None), request())
        .expect("observed ownership is enough");

    assert_eq!(outcome.cleanup, CleanupStatus::Done);
    assert!(mutations.borrow().is_empty());
}

#[test]
fn conflicting_observed_owner_rejects_without_mutation() {
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let conflict = OwnershipCommit {
        owner: VolumeOwner::parse("node-c").expect("owner"),
        ..source_ownership()
    };
    let transfer = VolumeTransferEngine::new(
        fake_claims(Vec::new()),
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
            received: None,
        },
        FakeOwnership {
            observations: Rc::new(RefCell::new(VecDeque::from([
                OwnershipObservation::Present(conflict),
            ]))),
            committed: Rc::new(RefCell::new(None)),
            commit: None,
        },
        fake_cleanup(Ok(CleanupStatus::Done)),
    );

    assert_eq!(
        transfer.transfer(&context(Some(current_fence())), request()),
        Err(VolumeFailure::SourceNotOwner)
    );
    assert!(mutations.borrow().is_empty());
}

#[test]
fn missing_or_wrong_fence_rejects_before_mutation() {
    for fence in [
        None,
        Some(fence("volume:other", "node-a")),
        Some(fence("volume:data", "node-b")),
    ] {
        let mutations = Rc::new(RefCell::new(Vec::new()));
        let transfer = engine(
            vec![VolumeClaimCheck::Current],
            mutations.clone(),
            source_observations(),
            Ok(CleanupStatus::Done),
        );

        assert_eq!(
            transfer.transfer(&context(fence), request()),
            Err(VolumeFailure::StaleFence)
        );
        assert!(mutations.borrow().is_empty());
    }
}
