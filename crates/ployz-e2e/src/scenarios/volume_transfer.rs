use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use ployz::error::{PrimitiveFailure, VolumeFailure};
use ployz::operation::{
    AuthorityDecision, AuthorityEpoch, AuthorityPort, ClaimHash, CommandEnvelope, CommandIssuer,
    CommandRunner, FenceEpoch, IdempotencyKey, MutationContext, MutationIntent, OperationId,
    PrincipalId, ResourceId, ScopeId, SubmittedFenceToken,
};
use ployz::volume::{
    CleanupFailureReason, CleanupStatus, FinalDeltaReceipt, OwnershipCommit, OwnershipEpoch,
    OwnershipVerification, ReceiveReceipt, SnapshotReceipt, SourceWatermark, SourceWriteStatus,
    VolumeClaimCheck, VolumeClaimPort, VolumeCleanupFailure, VolumeCleanupPort, VolumeId,
    VolumeOwner, VolumeOwnershipPort, VolumeSnapshotId, VolumeSourcePort, VolumeTargetPort,
    VolumeTransferCommand, VolumeTransferEngine, VolumeTransferMode, VolumeTransferPlan,
    VolumeTransferRequest,
};
use polis::{EvidenceKind, TerminalMarker};

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
    commit: Option<OwnershipCommit>,
}

impl VolumeOwnershipPort for FakeOwnership {
    fn commit_ownership(
        &self,
        _context: &MutationContext,
        plan: &VolumeTransferPlan,
        _receive: &ReceiveReceipt,
    ) -> Result<OwnershipCommit, VolumeFailure> {
        Ok(self.commit.clone().unwrap_or_else(|| OwnershipCommit {
            volume: plan.volume.clone(),
            owner: plan.target.clone(),
            epoch: plan.next_epoch,
            source_watermark: plan.expected_source_watermark,
        }))
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
    evidence: Rc<RefCell<Vec<EvidenceKind>>>,
    terminal: Rc<RefCell<Vec<TerminalMarker>>>,
    replay: bool,
}

impl polis::OperationBackend for FakeOperations {
    fn start_or_replay(
        &self,
        request: &polis::OperationRequest,
    ) -> polis::Result<polis::BackendOperationStart> {
        if self.replay {
            return Ok(polis::BackendOperationStart::Replayed {
                operation: request.operation().clone(),
                terminal: None,
            });
        }
        Ok(polis::BackendOperationStart::Started)
    }

    fn record(
        &self,
        _operation: &polis::OperationId,
        evidence: polis::OperationEvidence,
    ) -> polis::Result<()> {
        self.evidence.borrow_mut().push(evidence.kind);
        Ok(())
    }

    fn close(
        &self,
        _operation: &polis::OperationId,
        marker: polis::TerminalMarker,
    ) -> polis::Result<()> {
        self.terminal.borrow_mut().push(marker);
        Ok(())
    }
}

fn command() -> CommandEnvelope<VolumeTransferCommand> {
    CommandIssuer::new(AllowAuthority)
        .issue::<VolumeTransferCommand>(MutationIntent {
            operation: OperationId::parse("volume-transfer-1").expect("operation"),
            idempotency: IdempotencyKey::parse("idem-volume-1").expect("idempotency"),
            principal: PrincipalId::parse("node-a").expect("principal"),
            scope: ScopeId::parse("cluster").expect("scope"),
            command: ployz::operation::CommandKind::parse("volume-transfer").expect("command"),
            payload_hash: vec![1],
            resources: vec![
                ployz::operation::FingerprintedResource::parse("volume:data").expect("volume"),
            ],
            submitted_fence: Some(SubmittedFenceToken {
                resource: ResourceId::parse("volume:data").expect("resource"),
                holder: PrincipalId::parse("node-a").expect("holder"),
                epoch: FenceEpoch::new(3).expect("fence epoch"),
                claim_hash: ClaimHash::parse("claim-hash-a").expect("claim hash"),
            }),
            deadline: UNIX_EPOCH + Duration::from_secs(60),
        })
        .expect("command")
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

fn request(mode: VolumeTransferMode) -> VolumeTransferRequest {
    VolumeTransferRequest { plan: plan(), mode }
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
    CommandRunner<FakeOperations>,
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
        FakeOwnership {
            verifications,
            commit: None,
        },
        FakeCleanup { result: cleanup },
        CommandRunner::new(FakeOperations::default()),
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
        transfer.transfer(command(), request(VolumeTransferMode::Start)),
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
            Rc::new(RefCell::new(VecDeque::new())),
            Ok(CleanupStatus::Done),
        );

        assert_eq!(
            transfer.transfer(command(), request(VolumeTransferMode::Start)),
            Err(VolumeFailure::StaleFence)
        );
        assert_eq!(mutations.borrow().as_slice(), expected_mutations.as_slice());
    }
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
        .transfer(command(), request(VolumeTransferMode::Start))
        .expect("ownership committed");

    assert_eq!(
        outcome.ownership.owner,
        VolumeOwner::parse("node-b").expect("target")
    );
    assert!(matches!(outcome.cleanup, CleanupStatus::Pending(_)));
}

#[test]
fn cleanup_pending_records_checkpoint_before_terminal_success() {
    let operations = FakeOperations::default();
    let transfer = VolumeTransferEngine::new(
        FakeClaims {
            checks: Rc::new(RefCell::new(VecDeque::from(vec![
                VolumeClaimCheck::Current,
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
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
        },
        FakeOwnership {
            verifications: Rc::new(RefCell::new(VecDeque::new())),
            commit: None,
        },
        FakeCleanup {
            result: Err(VolumeCleanupFailure {
                artifact: ployz::volume::CleanupArtifactId::parse("actual-temp-data")
                    .expect("cleanup artifact"),
                reason: CleanupFailureReason::DeleteFailed,
            }),
        },
        CommandRunner::new(operations.clone()),
    );

    let outcome = transfer
        .transfer(command(), request(VolumeTransferMode::Start))
        .expect("ownership committed");

    assert!(matches!(outcome.cleanup, CleanupStatus::Pending(_)));
    assert_eq!(
        operations.evidence.borrow().as_slice(),
        [
            EvidenceKind::Checkpoint(
                b"volume.ownership_committed;volume=4:data;owner=6:node-b;epoch=1:2;watermark=1:5;"
                    .to_vec()
            ),
            EvidenceKind::Checkpoint(
                b"volume.cleanup_pending;artifact=16:actual-temp-data;".to_vec()
            )
        ]
    );
    assert_eq!(
        operations.terminal.borrow().as_slice(),
        [TerminalMarker::Succeeded]
    );
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
            commit: None,
        },
        FakeCleanup {
            result: Ok(CleanupStatus::Done),
        },
        CommandRunner::new(FakeOperations::default()),
    );

    assert_eq!(
        transfer.transfer(command(), request(VolumeTransferMode::Start)),
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
    let operations = FakeOperations::default();
    let transfer = VolumeTransferEngine::new(
        FakeClaims {
            checks: Rc::new(RefCell::new(VecDeque::from(vec![
                VolumeClaimCheck::Current,
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
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
        },
        FakeOwnership {
            verifications: Rc::new(RefCell::new(VecDeque::from([
                OwnershipVerification::Verified(wrong_commit.clone()),
            ]))),
            commit: Some(wrong_commit),
        },
        FakeCleanup {
            result: Ok(CleanupStatus::Done),
        },
        CommandRunner::new(operations.clone()),
    );

    assert_eq!(
        transfer.transfer(command(), request(VolumeTransferMode::Start)),
        Err(VolumeFailure::OwnershipCommitRejected)
    );
    assert!(operations.evidence.borrow().is_empty());
}

#[test]
fn explicit_recovery_command_requires_verified_ownership() {
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
        first_attempt.transfer(command(), request(VolumeTransferMode::Start)),
        Err(VolumeFailure::OwnershipCommitRejected)
    );

    let retry_mutations = Rc::new(RefCell::new(Vec::new()));
    let retry_operations = FakeOperations::default();
    let second_attempt = VolumeTransferEngine::new(
        FakeClaims {
            checks: Rc::new(RefCell::new(VecDeque::from(vec![
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
            ]))),
        },
        FakeSource {
            mutations: retry_mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
        },
        FakeOwnership {
            verifications,
            commit: None,
        },
        FakeCleanup {
            result: Ok(CleanupStatus::Done),
        },
        CommandRunner::new(retry_operations.clone()),
    );
    second_attempt
        .transfer(
            command(),
            request(VolumeTransferMode::VerifyCommittedOwnership),
        )
        .expect("verified ownership resumes success");
    assert!(retry_mutations.borrow().is_empty());
    assert_eq!(
        retry_operations.evidence.borrow().as_slice(),
        [EvidenceKind::Checkpoint(
            b"volume.ownership_committed;volume=4:data;owner=6:node-b;epoch=1:2;watermark=1:5;"
                .to_vec()
        )]
    );
}

#[test]
fn idempotent_replay_does_not_run_volume_recovery_work() {
    let operations = FakeOperations {
        replay: true,
        ..FakeOperations::default()
    };
    let mutations = Rc::new(RefCell::new(Vec::new()));
    let transfer = VolumeTransferEngine::new(
        FakeClaims {
            checks: Rc::new(RefCell::new(VecDeque::from(vec![
                VolumeClaimCheck::Current,
                VolumeClaimCheck::Current,
            ]))),
        },
        FakeSource {
            mutations: mutations.clone(),
        },
        FakeTarget {
            receive: ReceiveReceipt {
                snapshot: VolumeSnapshotId::parse("snap-1").expect("snapshot"),
                target: VolumeOwner::parse("node-b").expect("target"),
            },
        },
        FakeOwnership {
            verifications: Rc::new(RefCell::new(VecDeque::from([
                OwnershipVerification::Verified(OwnershipCommit {
                    volume: VolumeId::parse("data").expect("volume"),
                    owner: VolumeOwner::parse("node-b").expect("owner"),
                    epoch: OwnershipEpoch::new(2),
                    source_watermark: SourceWatermark::new(5),
                }),
            ]))),
            commit: None,
        },
        FakeCleanup {
            result: Ok(CleanupStatus::Done),
        },
        CommandRunner::new(operations.clone()),
    );

    assert_eq!(
        transfer.transfer(
            command(),
            request(VolumeTransferMode::VerifyCommittedOwnership),
        ),
        Err(VolumeFailure::OwnershipCommitRejected)
    );
    assert!(mutations.borrow().is_empty());
    assert!(operations.evidence.borrow().is_empty());
    assert!(operations.terminal.borrow().is_empty());
}
