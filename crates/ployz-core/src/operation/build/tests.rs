use super::*;
use crate::build::{
    BuildCacheScope, BuildExecutorAssignment, BuildExecutorEvidence, GitCommit, GitRepositoryUrl,
    GitSource, VerifiedGitCommit,
};
use crate::deploy::PlatformImage;
use crate::image::{OciDigest, OciPlatform};

fn id() -> OperationId {
    OperationId::try_new("build-test").expect("id")
}
fn cluster_evidence(machine_id: &MachineId) -> BuildExecutorEvidence {
    BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    })
}
fn external_evidence(executor_id: &str, image_seed: &str) -> BuildExecutorEvidence {
    BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::External {
        pool_id: crate::ids::BuildPoolId::try_new("pool-a").expect("pool"),
        executor_id: crate::ids::BuildExecutorId::try_new(executor_id).expect("executor"),
        image_seed: MachineId::try_new(image_seed).expect("seed"),
    })
}
fn status0() -> OperationStatus {
    status_for_target(BuildTarget::Cluster)
}
fn status_for_target(target: BuildTarget) -> OperationStatus {
    OperationStatus::build_accepted(
        id(),
        target,
        GitSource::try_new(
            "https://example.com/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source")
        .evidence(),
        BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("test").expect("scope"),
        },
        BuildPlatforms::try_new([OciPlatform::try_new("linux", "amd64").expect("platform")])
            .expect("platforms"),
        EventSequence::try_new(1).expect("sequence"),
    )
}
fn building_status(target: BuildTarget) -> OperationStatus {
    let accepted = status_for_target(target.clone());
    let OperationProjection::StatusChanged { status: placing } = project_event_from_status(
        &accepted,
        BuildTransition::Placing.event(&id()),
        EventSequence::try_new(2).expect("sequence"),
    )
    .expect("placing") else {
        panic!("changed")
    };
    let executor = match &target {
        BuildTarget::Cluster => {
            cluster_evidence(&MachineId::try_new("machine-a").expect("machine"))
        }
        BuildTarget::External { pool_id } => {
            BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::External {
                pool_id: pool_id.clone(),
                executor_id: crate::ids::BuildExecutorId::try_new("executor-a").expect("executor"),
                image_seed: MachineId::try_new("seed-a").expect("seed"),
            })
        }
    };
    let OperationProjection::StatusChanged { status: placed } = project_event_from_status(
        &placing,
        OperationEvent::BuildPlatformPlaced {
            operation_id: id(),
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            executor,
        },
        EventSequence::try_new(3).expect("sequence"),
    )
    .expect("placed") else {
        panic!("changed")
    };
    let OperationProjection::StatusChanged { status: building } = project_event_from_status(
        &placed,
        BuildTransition::Building.event(&id()),
        EventSequence::try_new(4).expect("sequence"),
    )
    .expect("building") else {
        panic!("changed")
    };
    *building
}
fn receipt() -> PushedImageReceipt {
    receipt_for(
        OciPlatform::try_new("linux", "amd64").expect("platform"),
        MachineId::try_new("machine-a").expect("machine"),
    )
}

fn receipt_for(platform: OciPlatform, seed: MachineId) -> PushedImageReceipt {
    PushedImageReceipt::try_new([(
        platform,
        PlatformImage {
            seed,
            manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
                .expect("digest"),
            image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("digest"),
            availability_expires_at: crate::deploy::ImageAvailabilityExpiresAt::try_new(
                4_102_444_800,
            )
            .expect("expiry"),
        },
    )])
    .expect("receipt")
}

fn railpack_toolchain() -> BuildToolchainEvidence {
    BuildToolchainEvidence {
        buildkit_image: OciDigest::try_new(format!("sha256:{}", "3".repeat(64))).expect("digest"),
        adapter: BuildAdapterToolchainEvidence::Railpack {
            helper_version: InstallArtifactVersion::try_new("v0.31.0").expect("version"),
            helper_sha256: InstallSha256Digest::try_new("4".repeat(64)).expect("digest"),
            frontend_image: OciDigest::try_new(format!("sha256:{}", "5".repeat(64)))
                .expect("digest"),
        },
    }
}

#[test]
fn projector_accepts_only_ordered_transitions_and_terminal_is_final() {
    let building = building_status(BuildTarget::Cluster);
    let OperationProjection::StatusChanged { status: completed } = project_event_from_status(
        &building,
        BuildTransition::Completed { receipt: receipt() }.event(&id()),
        EventSequence::try_new(5).expect("seq"),
    )
    .expect("completed") else {
        panic!("changed")
    };
    assert!(completed.is_terminal());
    assert!(
        project_event_from_status(
            &completed,
            BuildTransition::Failed {
                failure: BuildOperationFailure::ReceiptAssemblyFailed {
                    message: FailureMessage::try_new("late").expect("message")
                }
            }
            .event(&id()),
            EventSequence::try_new(6).expect("seq")
        )
        .is_err()
    );
}

#[test]
fn placed_build_can_cancel_before_start_without_cleanup() {
    let accepted = status0();
    let OperationProjection::StatusChanged { status: placing } = project_event_from_status(
        &accepted,
        BuildTransition::Placing.event(&id()),
        EventSequence::try_new(2).expect("sequence"),
    )
    .expect("placing") else {
        panic!("changed")
    };
    let OperationProjection::StatusChanged { status: placed } = project_event_from_status(
        &placing,
        OperationEvent::BuildPlatformPlaced {
            operation_id: id(),
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            executor: cluster_evidence(&MachineId::try_new("machine-a").expect("machine")),
        },
        EventSequence::try_new(3).expect("sequence"),
    )
    .expect("placed") else {
        panic!("changed")
    };

    let OperationProjection::StatusChanged { status: cancelled } = project_event_from_status(
        &placed,
        BuildTransition::Cancelled {
            reason: CancellationReason::try_new("cancelled before start").expect("reason"),
            cleanup: BuildCleanupEvidence::NotRequired,
        }
        .event(&id()),
        EventSequence::try_new(4).expect("sequence"),
    )
    .expect("cancelled") else {
        panic!("changed")
    };

    let encoded = serde_json::to_value(&cancelled).expect("status");
    assert_eq!(
        serde_json::from_value::<OperationStatus>(encoded).expect("valid status"),
        *cancelled
    );
}

#[test]
fn current_build_status_wire_rejects_cross_field_provenance_contradictions() {
    let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
    let external = building_status(BuildTarget::External { pool_id });
    let encoded = serde_json::to_value(&external).expect("status");
    assert_eq!(
        serde_json::from_value::<OperationStatus>(encoded).expect("valid status"),
        external
    );

    let mut wrong_pool = serde_json::to_value(&external).expect("status");
    *wrong_pool
        .pointer_mut("/executor_assignments/0/executor/pool_id")
        .expect("assignment pool") = serde_json::json!("pool-b");
    assert!(serde_json::from_value::<OperationStatus>(wrong_pool).is_err());

    let mut unrequested_platform = serde_json::to_value(&external).expect("status");
    *unrequested_platform
        .pointer_mut("/executor_assignments/0/platform/architecture")
        .expect("assignment platform") = serde_json::json!("arm64");
    assert!(serde_json::from_value::<OperationStatus>(unrequested_platform).is_err());

    let mut incomplete_building = serde_json::to_value(&external).expect("status");
    incomplete_building
        .get_mut("platforms")
        .and_then(serde_json::Value::as_array_mut)
        .expect("platforms")
        .push(serde_json::json!({"os":"linux","architecture":"arm64"}));
    assert!(serde_json::from_value::<OperationStatus>(incomplete_building).is_err());

    let building = building_status(BuildTarget::Cluster);
    let OperationProjection::StatusChanged { status: completed } = project_event_from_status(
        &building,
        BuildTransition::Completed { receipt: receipt() }.event(&id()),
        EventSequence::try_new(5).expect("sequence"),
    )
    .expect("completed") else {
        panic!("changed")
    };
    let mut wrong_terminal_seed = serde_json::to_value(completed).expect("status");
    *wrong_terminal_seed
        .pointer_mut("/state/receipt/platforms/0/1/seed")
        .expect("receipt seed") = serde_json::json!("machine-b");
    assert!(serde_json::from_value::<OperationStatus>(wrong_terminal_seed).is_err());
}

#[test]
fn completed_receipt_must_match_placed_platform() {
    let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
    let building = building_status(BuildTarget::External { pool_id });
    let receipt = receipt_for(
        OciPlatform::try_new("linux", "arm64").expect("platform"),
        MachineId::try_new("seed-a").expect("seed"),
    );
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Completed { receipt }.event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn completed_receipt_image_seed_must_match_placed_executor() {
    let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
    let building = building_status(BuildTarget::External { pool_id });
    let receipt = receipt_for(
        OciPlatform::try_new("linux", "amd64").expect("platform"),
        MachineId::try_new("seed-b").expect("seed"),
    );
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Completed { receipt }.event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn platform_failure_must_match_placed_platform() {
    let building = building_status(BuildTarget::Cluster);
    let failure = BuildPlatformFailure::MachineUnavailable {
        message: FailureMessage::try_new("failed").expect("message"),
    };
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Failed {
                failure: BuildOperationFailure::PlatformFailed {
                    platform: OciPlatform::try_new("linux", "arm64").expect("platform"),
                    machine_id: MachineId::try_new("machine-a").expect("machine"),
                    failure,
                },
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn platform_failure_image_seed_must_match_placed_executor() {
    let building = building_status(BuildTarget::Cluster);
    let failure = BuildPlatformFailure::MachineUnavailable {
        message: FailureMessage::try_new("failed").expect("message"),
    };
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Failed {
                failure: BuildOperationFailure::PlatformFailed {
                    platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                    machine_id: MachineId::try_new("machine-b").expect("machine"),
                    failure,
                },
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn external_platform_failure_must_match_exact_executor_and_seed() {
    let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
    let building = building_status(BuildTarget::External { pool_id });
    let failure = |executor| BuildOperationFailure::ExternalPlatformFailed {
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        executor,
        failure: BuildPlatformFailure::MachineUnavailable {
            message: FailureMessage::try_new("failed").expect("message"),
        },
    };

    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Failed {
                failure: failure(external_evidence("executor-a", "seed-a")),
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_ok()
    );
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Failed {
                failure: failure(external_evidence("executor-b", "seed-a")),
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Failed {
                failure: BuildOperationFailure::PlatformFailed {
                    platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                    machine_id: MachineId::try_new("seed-a").expect("seed"),
                    failure: BuildPlatformFailure::MachineUnavailable {
                        message: FailureMessage::try_new("failed").expect("message"),
                    },
                },
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn cluster_terminal_failure_wire_shape_remains_machine_scoped() {
    let failure = BuildOperationFailure::PlatformFailed {
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        machine_id: MachineId::try_new("machine-a").expect("machine"),
        failure: BuildPlatformFailure::MachineUnavailable {
            message: FailureMessage::try_new("offline").expect("message"),
        },
    };

    assert_eq!(
        serde_json::to_value(failure).expect("failure"),
        serde_json::json!({
            "kind": "platform_failed",
            "platform": {"os": "linux", "architecture": "amd64"},
            "machine_id": "machine-a",
            "failure": {"kind": "machine_unavailable", "message": "offline"},
        })
    );
}

#[test]
fn external_runtime_and_image_seed_failures_have_distinct_wire_evidence() {
    let executor_unavailable = BuildPlatformFailure::ExecutorUnavailable {
        message: FailureMessage::try_new("executor offline").expect("message"),
    };
    let image_seed_unavailable = BuildPlatformFailure::ImageSeedUnavailable {
        image_seed: MachineId::try_new("seed-a").expect("seed"),
    };

    assert_eq!(
        serde_json::to_value(executor_unavailable).expect("executor failure"),
        serde_json::json!({
            "kind": "executor_unavailable",
            "message": "executor offline",
        })
    );
    assert_eq!(
        serde_json::to_value(image_seed_unavailable).expect("seed failure"),
        serde_json::json!({
            "kind": "image_seed_unavailable",
            "image_seed": "seed-a",
        })
    );
}

#[test]
fn terminal_cleanup_must_not_name_unplaced_image_seeds() {
    let building = building_status(BuildTarget::Cluster);
    let cleanup = BuildCleanupEvidence::Completed {
        machine_ids: vec![MachineId::try_new("machine-b").expect("machine")],
    };
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Cancelled {
                reason: CancellationReason::try_new("cancelled").expect("reason"),
                cleanup,
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn external_cleanup_must_match_exact_executor_and_seed() {
    let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
    let building = building_status(BuildTarget::External { pool_id });

    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Cancelled {
                reason: CancellationReason::try_new("cancelled").expect("reason"),
                cleanup: BuildCleanupEvidence::ExternalCompleted {
                    executors: vec![external_evidence("executor-a", "seed-a")],
                },
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_ok()
    );
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Cancelled {
                reason: CancellationReason::try_new("cancelled").expect("reason"),
                cleanup: BuildCleanupEvidence::ExternalUnconfirmed {
                    executors: vec![external_evidence("executor-a", "seed-b")],
                },
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
    assert!(
        project_event_from_status(
            &building,
            BuildTransition::Cancelled {
                reason: CancellationReason::try_new("cancelled").expect("reason"),
                cleanup: BuildCleanupEvidence::Completed {
                    machine_ids: vec![MachineId::try_new("seed-a").expect("seed")],
                },
            }
            .event(&id()),
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn accepted_build_cleanup_must_be_provenance_free() {
    let accepted = status0();
    assert!(
        project_event_from_status(
            &accepted,
            BuildTransition::TimedOut {
                failure: BuildTimeoutFailure::DeadlineExceeded {
                    message: FailureMessage::try_new("timed out").expect("message"),
                },
                cleanup: BuildCleanupEvidence::Unconfirmed {
                    machine_ids: vec![MachineId::try_new("machine-a").expect("machine")],
                },
            }
            .event(&id()),
            EventSequence::try_new(2).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn log_gap_is_nonterminal_and_preserves_completed_machine_outcome() {
    let building = building_status(BuildTarget::Cluster);
    let OperationProjection::StatusChanged {
        status: gap_recorded,
    } = project_event_from_status(
        &building,
        OperationEvent::BuildPlatformLogGap {
            operation_id: id(),
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            executor: cluster_evidence(&MachineId::try_new("machine-a").expect("machine")),
            expected_sequence: 2,
            final_sequence: 3,
        },
        EventSequence::try_new(5).expect("sequence"),
    )
    .expect("gap evidence")
    else {
        panic!("changed")
    };

    let OperationProjection::StatusChanged { status: completed } = project_event_from_status(
        &gap_recorded,
        BuildTransition::Completed { receipt: receipt() }.event(&id()),
        EventSequence::try_new(6).expect("sequence"),
    )
    .expect("completed") else {
        panic!("changed")
    };
    assert!(matches!(
        *completed,
        OperationStatus::Build { ref status }
            if matches!(status.state(), BuildOperationState::Completed { .. })
    ));
}

#[test]
fn submitted_event_and_status_are_credential_free() {
    let source = GitSource::try_new(
        "https://example.com/repo.git",
        "0123456789abcdef0123456789abcdef01234567",
        "git",
        "do-not-persist",
        None::<String>,
    )
    .expect("source");
    let evidence = source.evidence();
    let adapter = BuildAdapter::Railpack {
        cache_scope: BuildCacheScope::try_new("test").expect("scope"),
    };
    let platforms =
        BuildPlatforms::try_new([OciPlatform::try_new("linux", "amd64").expect("platform")])
            .expect("platforms");
    let event = OperationEvent::BuildSubmitted {
        operation_id: id(),
        target: BuildTarget::Cluster,
        source: evidence.clone(),
        adapter: adapter.clone(),
        platforms: platforms.clone(),
    };
    let status = OperationStatus::build_accepted(
        id(),
        BuildTarget::Cluster,
        evidence,
        adapter,
        platforms,
        EventSequence::try_new(1).expect("sequence"),
    );
    assert!(
        !serde_json::to_string(&event)
            .expect("event")
            .contains("do-not-persist")
    );
    assert!(
        !serde_json::to_string(&status)
            .expect("status")
            .contains("do-not-persist")
    );
}

#[test]
fn submitted_event_must_match_the_admitted_build_contract() {
    let accepted = status0();
    let different_source = GitSource::try_new(
        "https://example.com/other.git",
        "0123456789abcdef0123456789abcdef01234567",
        "git",
        "secret",
        None::<String>,
    )
    .expect("source");
    let OperationStatus::Build { status } = &accepted else {
        panic!("build status")
    };

    assert!(
        project_event_from_status(
            &accepted,
            OperationEvent::BuildSubmitted {
                operation_id: id(),
                target: BuildTarget::Cluster,
                source: different_source.evidence(),
                adapter: status.adapter().clone(),
                platforms: status.platforms().clone(),
            },
            EventSequence::try_new(2).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn evidence_must_name_a_declared_platform_in_the_correct_stage() {
    let accepted = status0();
    let OperationProjection::StatusChanged { status: placing } = project_event_from_status(
        &accepted,
        BuildTransition::Placing.event(&id()),
        EventSequence::try_new(2).expect("sequence"),
    )
    .expect("placing") else {
        panic!("changed")
    };

    assert!(
        project_event_from_status(
            &placing,
            OperationEvent::BuildPlatformPlaced {
                operation_id: id(),
                platform: OciPlatform::try_new("linux", "arm64").expect("platform"),
                executor: cluster_evidence(&MachineId::try_new("machine-arm").expect("machine"),),
            },
            EventSequence::try_new(3).expect("sequence"),
        )
        .is_err()
    );
    assert!(
        project_event_from_status(
            &placing,
            OperationEvent::BuildPlatformLog {
                operation_id: id(),
                platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                executor: cluster_evidence(&MachineId::try_new("machine-amd").expect("machine"),),
                chunk: BuildLogChunk::try_new("too early").expect("chunk"),
            },
            EventSequence::try_new(3).expect("sequence"),
        )
        .is_err()
    );
    assert!(
        project_event_from_status(
            &placing,
            OperationEvent::BuildPlatformToolchainVerified {
                operation_id: id(),
                platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                executor: cluster_evidence(&MachineId::try_new("machine-amd").expect("machine"),),
                toolchain: railpack_toolchain(),
            },
            EventSequence::try_new(3).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn cluster_evidence_rejects_origin_and_image_seed_contradictions() {
    let building = building_status(BuildTarget::Cluster);
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let other_machine = MachineId::try_new("machine-b").expect("machine");

    let invalid_wire = serde_json::json!({
        "machine_id": machine_id,
        "executor_origin": {"origin": "cluster", "machine_id": other_machine},
    });
    assert!(serde_json::from_value::<BuildExecutorEvidence>(invalid_wire).is_err());

    let event = OperationEvent::BuildPlatformCompleted {
        operation_id: id(),
        platform: platform.clone(),
        executor: cluster_evidence(&machine_id),
        image: PlatformImage {
            seed: other_machine.clone(),
            manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
                .expect("digest"),
            image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("digest"),
            availability_expires_at: crate::deploy::ImageAvailabilityExpiresAt::try_new(
                4_102_444_800,
            )
            .expect("expiry"),
        },
    };
    assert!(
        project_event_from_status(
            &building,
            event,
            EventSequence::try_new(5).expect("sequence"),
        )
        .is_err()
    );
}

#[test]
fn flat_executor_evidence_does_not_weaken_event_unknown_field_rejection() {
    let mut event = serde_json::to_value(OperationEvent::BuildPlatformPlaced {
        operation_id: id(),
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        executor: cluster_evidence(&MachineId::try_new("machine-a").expect("machine")),
    })
    .expect("event");
    event
        .as_object_mut()
        .expect("event object")
        .insert("credential".into(), serde_json::json!("must-not-fit"));

    assert!(serde_json::from_value::<OperationEvent>(event).is_err());
}

#[test]
fn external_evidence_requires_the_admitted_pool_and_external_origin() {
    let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
    let building = building_status(BuildTarget::External {
        pool_id: pool_id.clone(),
    });
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let machine_id = MachineId::try_new("seed-a").expect("machine");
    let failure = BuildPlatformFailure::MachineUnavailable {
        message: FailureMessage::try_new("failed").expect("message"),
    };

    for executor in [
        cluster_evidence(&machine_id),
        BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::External {
            pool_id: crate::ids::BuildPoolId::try_new("pool-b").expect("pool"),
            executor_id: crate::ids::BuildExecutorId::try_new("executor-a").expect("executor"),
            image_seed: machine_id.clone(),
        }),
    ] {
        assert!(
            project_event_from_status(
                &building,
                OperationEvent::BuildPlatformFailed {
                    operation_id: id(),
                    platform: platform.clone(),
                    executor,
                    failure: failure.clone(),
                },
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }
}

#[test]
fn verified_build_evidence_is_accepted_while_building() {
    let building = building_status(BuildTarget::Cluster);
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let OperationProjection::StatusChanged { status: verified } = project_event_from_status(
        &building,
        OperationEvent::BuildCommitVerified {
            operation_id: id(),
            platform: platform.clone(),
            executor: cluster_evidence(&machine_id),
            commit: VerifiedGitCommit {
                url: GitRepositoryUrl::try_new("https://example.com/repo.git").expect("url"),
                commit: GitCommit::try_new("0123456789abcdef0123456789abcdef01234567")
                    .expect("commit"),
                subdir: None,
            },
        },
        EventSequence::try_new(5).expect("sequence"),
    )
    .expect("verified commit") else {
        panic!("changed")
    };

    assert!(
        project_event_from_status(
            &verified,
            OperationEvent::BuildPlatformToolchainVerified {
                operation_id: id(),
                platform,
                executor: cluster_evidence(&machine_id),
                toolchain: railpack_toolchain(),
            },
            EventSequence::try_new(6).expect("sequence"),
        )
        .is_ok()
    );
}

#[test]
fn core_process_loss_terminally_interrupts_a_build() {
    let accepted = status0();
    let evidence = accepted
        .interruption_evidence(OperationInterruptionCause::PriorCoreProcessLoss)
        .expect("interruption evidence");
    let OperationProjection::StatusChanged { status } = project_event_from_status(
        &accepted,
        OperationEvent::OperationInterrupted {
            operation_id: id(),
            evidence: evidence.clone(),
        },
        EventSequence::try_new(2).expect("sequence"),
    )
    .expect("interrupted") else {
        panic!("changed")
    };

    assert!(status.is_terminal());
    assert_eq!(status.terminal_interruption_evidence(), Some(&evidence));
}

#[test]
fn log_chunks_are_bounded() {
    assert!(BuildLogChunk::try_new("x".repeat(MAX_BUILD_LOG_CHUNK_BYTES)).is_ok());
    assert!(matches!(
        BuildLogChunk::try_new("x".repeat(MAX_BUILD_LOG_CHUNK_BYTES + 1)),
        Err(BuildLogChunkError::TooLarge { .. })
    ));
}

#[test]
fn insufficient_host_disk_has_actionable_wire_evidence() {
    let failure = BuildPlatformFailure::InsufficientHostDisk {
        available_bytes: 1,
        required_free_bytes: 2,
    };
    assert_eq!(
        serde_json::to_value(failure).expect("serialize failure"),
        serde_json::json!({
            "kind": "insufficient_host_disk",
            "available_bytes": 1,
            "required_free_bytes": 2
        })
    );
}

fn project_event_from_status(
    current: &OperationStatus,
    event: OperationEvent,
    sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    super::super::project_operation_event(current, event, sequence)
}
