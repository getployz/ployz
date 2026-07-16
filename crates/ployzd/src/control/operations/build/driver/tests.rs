use std::pin::Pin;

use super::*;
use ployz_core::deploy::PlatformImage;
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::operation::BuildLogChunk;

#[test]
fn caller_budget_is_explicit_and_deterministic() {
    assert_eq!(
        execution_timeout_millis(Duration::from_millis(7_321)),
        7_321
    );
    assert_eq!(
        build_control_request_timeout(ployz_core::build::BUILD_MAX_EXECUTION_TIMEOUT),
        ployz_core::build::BUILD_MAX_MACHINE_RESPONSE_LIFETIME
            + ployz_core::build::BUILD_CONTROL_RESPONSE_MARGIN
    );
}

#[test]
fn log_frames_require_exact_provenance_and_next_sequence() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let platform = ployz_core::image::OciPlatform::try_new("linux", "amd64").expect("platform");
    let frame = MachineBuildLogFrame {
        operation_id: operation_id.clone(),
        machine_id: machine_id.clone(),
        platform: platform.clone(),
        sequence: 1,
        chunk: BuildLogChunk::try_new("line").expect("log chunk"),
    };

    assert!(valid_next_log_frame(
        &operation_id,
        &machine_id,
        &platform,
        1,
        &frame
    ));
    assert!(!valid_next_log_frame(
        &operation_id,
        &machine_id,
        &platform,
        2,
        &frame
    ));
    assert!(!valid_next_log_frame(
        &OperationId::try_new("build-2").expect("operation id"),
        &machine_id,
        &platform,
        1,
        &frame,
    ));
}

#[tokio::test]
async fn valid_log_is_observed_while_machine_call_is_pending() {
    let (sender, mut pending_call) = tokio::sync::oneshot::channel::<()>();
    let frame = MachineBuildLogFrame {
        operation_id: OperationId::try_new("build-1").expect("operation id"),
        machine_id: MachineId::try_new("machine-a").expect("machine id"),
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        sequence: 1,
        chunk: BuildLogChunk::try_new("live").expect("chunk"),
    };
    let mut logs = futures_util::stream::iter([frame.clone()]);

    assert!(matches!(
        next_machine_call_or_log(Pin::new(&mut pending_call), &mut logs, true).await,
        MachineCallOrLog::Log(Some(actual)) if actual == frame
    ));
    assert!(!sender.is_closed());
}

#[tokio::test(start_paused = true)]
async fn ready_spam_does_not_extend_the_absolute_log_deadline() {
    let deadline = tokio::time::sleep(BUILD_LOG_DRAIN_TIMEOUT);
    tokio::pin!(deadline);
    let mut spam = futures_util::stream::repeat(());

    for _ in 0..100 {
        assert!(matches!(
            next_log_before_deadline(deadline.as_mut(), &mut spam, true).await,
            LogBeforeDeadline::Log(Some(()))
        ));
    }
    tokio::time::advance(BUILD_LOG_DRAIN_TIMEOUT).await;
    assert!(matches!(
        next_log_before_deadline(deadline.as_mut(), &mut spam, true).await,
        LogBeforeDeadline::Deadline
    ));
}

#[tokio::test]
async fn cancellation_fence_prevents_a_late_machine_start() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let active = Mutex::new(BTreeMap::from([(
        operation_id.clone(),
        ActiveBuild::Running {
            started: BTreeSet::new(),
            cancellation_reason: Some(CancellationReason::try_new("stop").expect("reason")),
            watch_start_sequence: EventSequence::try_new(1).expect("sequence"),
        },
    )]));

    assert!(!claim_machine_start(&active, &operation_id, &machine_id).await);
    assert!(matches!(
        active.lock().await.get(&operation_id),
        Some(ActiveBuild::Running { started, .. }) if started.is_empty()
    ));
}

#[tokio::test]
async fn cancellation_after_claim_fences_build_start() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let active = Mutex::new(BTreeMap::from([(
        operation_id.clone(),
        ActiveBuild::Running {
            started: BTreeSet::new(),
            cancellation_reason: None,
            watch_start_sequence: EventSequence::try_new(1).expect("sequence"),
        },
    )]));
    assert!(claim_machine_start(&active, &operation_id, &machine_id).await);
    {
        let mut builds = active.lock().await;
        let Some(ActiveBuild::Running {
            cancellation_reason,
            ..
        }) = builds.get_mut(&operation_id)
        else {
            panic!("running build")
        };
        *cancellation_reason = Some(CancellationReason::try_new("stop").expect("reason"));
    }
    assert!(!machine_start_is_authorized(&active, &operation_id, &machine_id).await);
    release_machine_start_claim(&active, &operation_id, &machine_id).await;
    assert!(matches!(
        active.lock().await.get(&operation_id),
        Some(ActiveBuild::Running { started, .. }) if started.is_empty()
    ));
}

#[test]
fn not_running_retries_use_one_absolute_deadline() {
    let now = tokio::time::Instant::now();
    let deadline = now + Duration::from_millis(60);
    assert_eq!(
        cancel_retry_delay(now, deadline),
        Some(Duration::from_millis(25))
    );
    assert_eq!(
        cancel_retry_delay(now + Duration::from_millis(50), deadline),
        Some(Duration::from_millis(10))
    );
    assert_eq!(cancel_retry_delay(deadline, deadline), None);
    assert_eq!(
        cancel_retry_delay(deadline + Duration::from_millis(1), deadline),
        None
    );
}

#[test]
fn cancellation_fanout_targets_every_started_machine() {
    let first = MachineId::try_new("machine-a").expect("machine id");
    let second = MachineId::try_new("machine-b").expect("machine id");
    let mut build = ActiveBuild::Running {
        started: BTreeSet::from([second.clone(), first.clone()]),
        cancellation_reason: None,
        watch_start_sequence: EventSequence::try_new(1).expect("sequence"),
    };
    let ActiveBuild::Running {
        started,
        cancellation_reason,
        ..
    } = &mut build
    else {
        unreachable!()
    };
    *cancellation_reason = Some(CancellationReason::try_new("stop").expect("reason"));
    assert_eq!(started, &BTreeSet::from([first, second]));
    assert!(cancellation_reason.is_some());
}

#[tokio::test]
async fn cancellation_wins_before_finalization_and_finalization_fences_late_cancel() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let reason = CancellationReason::try_new("stop").expect("reason");
    let active = Mutex::new(BTreeMap::from([(
        operation_id.clone(),
        ActiveBuild::Running {
            started: BTreeSet::new(),
            cancellation_reason: Some(reason.clone()),
            watch_start_sequence: EventSequence::try_new(1).expect("sequence"),
        },
    )]));
    assert_eq!(
        claim_finalization(&active, &operation_id).await,
        Some(reason)
    );

    let other_id = OperationId::try_new("build-2").expect("operation id");
    active.lock().await.insert(
        other_id.clone(),
        ActiveBuild::Running {
            started: BTreeSet::new(),
            cancellation_reason: None,
            watch_start_sequence: EventSequence::try_new(1).expect("sequence"),
        },
    );
    assert_eq!(claim_finalization(&active, &other_id).await, None);
    assert!(matches!(
        active.lock().await.get(&other_id),
        Some(ActiveBuild::Finalizing { .. })
    ));
}

#[tokio::test]
async fn accepted_cancellation_wins_rejected_admission_arbitration() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let reason = CancellationReason::try_new("stop").expect("reason");
    let active = Mutex::new(BTreeMap::from([(
        operation_id.clone(),
        ActiveBuild::Running {
            started: BTreeSet::new(),
            cancellation_reason: Some(reason.clone()),
            watch_start_sequence: EventSequence::try_new(1).expect("sequence"),
        },
    )]));

    assert!(matches!(
        claim_rejected_admission(&active, &operation_id).await,
        RejectedAdmissionOutcome::Cancelled(actual) if actual == reason
    ));
    assert!(matches!(
        active.lock().await.get(&operation_id),
        Some(ActiveBuild::Finalizing { started }) if started.is_empty()
    ));
}

#[test]
fn platform_failure_prevents_partial_receipt_assembly() {
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let failure = platform_failure(
        platform.clone(),
        machine_id.clone(),
        BuildPlatformFailure::AdapterFailed {
            message: FailureMessage::try_new("adapter failed").expect("message"),
        },
    );
    let image = PlatformImage {
        seed: machine_id,
        manifest_digest: OciDigest::try_new(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("digest"),
        image_id: OciDigest::try_new(
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .expect("image id"),
    };

    assert_eq!(
        assemble_receipt(vec![(platform, image)], Some(failure.clone())),
        Err(Box::new(failure))
    );
}

#[test]
fn timeout_cleanup_is_completed_only_when_every_machine_confirms() {
    let confirmed = MachineId::try_new("confirmed").expect("machine id");
    let unconfirmed = MachineId::try_new("unconfirmed").expect("machine id");
    let message = FailureMessage::try_new("deadline exceeded").expect("message");

    assert_eq!(
        timeout_cleanup(&[(
            confirmed.clone(),
            message.clone(),
            MachineBuildCleanupOutcome::Confirmed,
        )]),
        BuildCleanupEvidence::Completed {
            machine_ids: vec![confirmed.clone()],
        }
    );
    assert_eq!(
        timeout_cleanup(&[
            (
                confirmed,
                message.clone(),
                MachineBuildCleanupOutcome::Confirmed,
            ),
            (
                unconfirmed.clone(),
                message,
                MachineBuildCleanupOutcome::Unconfirmed,
            ),
        ]),
        BuildCleanupEvidence::Unconfirmed {
            machine_ids: vec![unconfirmed],
        }
    );
}

#[tokio::test]
async fn cancellation_cleanup_is_unconfirmed_until_the_machine_confirms() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let active = Mutex::new(BTreeMap::from([(
        operation_id.clone(),
        ActiveBuild::Finalizing {
            started: BTreeSet::from([machine_id.clone()]),
        },
    )]));

    assert_eq!(
        cleanup_for(&operation_id, &active, Vec::new()).await,
        BuildCleanupEvidence::Unconfirmed {
            machine_ids: vec![machine_id.clone()],
        }
    );
    assert_eq!(
        cleanup_for(&operation_id, &active, vec![machine_id.clone()]).await,
        BuildCleanupEvidence::Completed {
            machine_ids: vec![machine_id],
        }
    );
}
