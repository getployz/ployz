use super::*;

#[cfg(unix)]
#[tokio::test]
async fn durable_acceptance_is_private_and_contains_no_credentials() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
        status_path.clone(),
    );
    let request = build_request("private-evidence", 10_000);
    let operation_id = request.operation_id.clone();
    runtime.start(request).await.expect("accepted");

    let path = status::raw_path(&status_path, &operation_id);
    let bytes = std::fs::read(&path).expect("raw evidence");
    assert!(!bytes.windows(6).any(|bytes| bytes == b"secret"));
    assert!(!bytes.windows(8).any(|bytes| bytes == b"username"));
    assert_eq!(
        std::fs::metadata(&status_path)
            .expect("directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(path).expect("file").permissions().mode() & 0o777,
        0o600
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn stale_running_evidence_becomes_failed_only_after_cleanup_succeeds() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let request = build_request("stale-running", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let repository = BuildStatusRepository::new(status_path.clone());
    repository
        .write(&BuildStatusRecord::new(BuildExecutorStatus {
            acceptance: acceptance.clone(),
            state: BuildExecutorState::Running {
                log_summary: BuildLogSummary::new(4, 5),
            },
        }))
        .expect("running evidence");

    let failed = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::failing_recovery()),
        status_path.clone(),
    );
    assert!(failed.recover_orphans().await.is_err());
    assert!(matches!(
        repository
            .read(&request.operation_id)
            .expect("record")
            .expect("present")
            .status
            .state,
        BuildExecutorState::Running { .. }
    ));

    let recovered = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
        status_path,
    );
    recovered.recover_orphans().await.expect("recovery");
    assert!(matches!(
        recovered.status(&acceptance).await.expect("status").state,
        BuildExecutorState::Failed {
            failure: BuildExecutorStatusFailure::PlatformFailed {
                failure: BuildPlatformFailure::MachineUnavailable { .. }
            },
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            ..
        }
    ));
}

#[tokio::test]
async fn restart_preserves_completed_failed_and_cancelled_evidence() {
    let root = tempfile::tempdir().expect("status root");
    let repository = BuildStatusRepository::new(root.path().join("status"));
    let completed_request = build_request("completed-restart", 10_000);
    let completed_acceptance = BuildExecutorAcceptance::from_start_request(&completed_request);
    let completed = BuildExecutorStatus::from_start_ok(
        successful_machine_output(&completed_request, BuildLogSummary::new(3, 4)).into_result(),
    );
    let failed_request = build_request("failed-restart", 10_000);
    let failed_acceptance = BuildExecutorAcceptance::from_start_request(&failed_request);
    let failed = failed_status(
        failed_acceptance.clone(),
        BuildPlatformFailure::MachineUnavailable {
            message: failure_message("failed before restart"),
        },
        MachineBuildCleanupOutcome::Confirmed,
        BuildLogSummary::new(5, 6),
    );
    let cancelled_request = build_request("cancelled-restart", 10_000);
    let cancelled_acceptance = BuildExecutorAcceptance::from_start_request(&cancelled_request);
    let cancelled = BuildExecutorStatus {
        acceptance: cancelled_acceptance.clone(),
        state: BuildExecutorState::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary::new(7, 8),
        },
    };
    for status in [completed.clone(), failed.clone(), cancelled.clone()] {
        repository
            .write(&BuildStatusRecord::new(status))
            .expect("terminal evidence");
    }

    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
        root.path().join("status"),
    );
    runtime.recover_orphans().await.expect("recovery");
    assert_eq!(runtime.status(&completed_acceptance).await, Ok(completed));
    assert_eq!(runtime.status(&failed_acceptance).await, Ok(failed));
    assert_eq!(runtime.status(&cancelled_acceptance).await, Ok(cancelled));
}

#[tokio::test]
async fn corrupt_same_operation_evidence_blocks_relaunch_and_status() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let request = build_request("corrupt", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    std::fs::create_dir_all(
        status::raw_path(&status_path, &request.operation_id)
            .parent()
            .expect("running directory"),
    )
    .expect("status directory");
    std::fs::write(
        status::raw_path(&status_path, &request.operation_id),
        b"not-json",
    )
    .expect("corrupt evidence");
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
        status_path,
    );

    assert_eq!(
        runtime.start(request).await,
        Err(MachineBuildStartDomainError::AlreadyRunning)
    );
    assert!(matches!(
        runtime.status(&acceptance).await,
        Err(MachineBuildStatusDomainError::EvidenceUnavailable { .. })
    ));
}

#[tokio::test]
async fn unrelated_corrupt_evidence_does_not_block_recovery_or_new_work() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let corrupt = build_request("corrupt-unrelated", 10_000);
    std::fs::create_dir_all(status::raw_path(&status_path, &corrupt.operation_id))
        .expect("corrupt directory evidence");
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::cooperative(true)),
        status_path,
    );
    runtime.recover_orphans().await.expect("unrelated recovery");
    let request = build_request("healthy-unrelated", 10_000);
    runtime.start(request).await.expect("healthy acceptance");
    runtime.shutdown().await;
}

#[tokio::test]
async fn startup_does_not_parse_unrelated_terminal_history() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let corrupt = build_request("corrupt-terminal-history", 10_000);
    let terminal_path = status::terminal_raw_path(&status_path, &corrupt.operation_id);
    std::fs::create_dir_all(terminal_path.parent().expect("terminal directory"))
        .expect("terminal directory");
    std::fs::write(terminal_path, b"not-json").expect("corrupt terminal history");
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
        status_path,
    );

    runtime
        .recover_orphans()
        .await
        .expect("running-only recovery");
    assert!(runtime.lifecycle.state.lock().await.active.is_empty());
}

#[tokio::test]
async fn failed_terminal_write_poison_reserves_the_operation() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let effects = Arc::new(TestBuildEffects::cooperative(true));
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
        status_path.clone(),
    );
    let request = build_request("terminal-write-failure", 10_000);
    let acceptance = runtime
        .start(request.clone())
        .await
        .expect("accepted")
        .executor;
    effects.ingest_started.notified().await;
    std::fs::remove_file(status::raw_path(&status_path, &request.operation_id))
        .expect("remove running record");
    std::fs::remove_dir(status_path.join("running")).expect("remove running directory");
    std::fs::remove_dir(&status_path).expect("remove status directory");
    std::fs::write(&status_path, b"blocks-directory-recreation").expect("blocking file");
    assert_eq!(
        runtime.cancel(&request.operation_id).await,
        MachineBuildCancelOutcome::Requested
    );

    loop {
        match runtime.status(&acceptance).await {
            Err(MachineBuildStatusDomainError::EvidenceUnavailable { .. }) => break,
            Ok(BuildExecutorStatus {
                state: BuildExecutorState::Running { .. },
                ..
            }) => tokio::task::yield_now().await,
            other => panic!("unexpected status while terminal write fails: {other:?}"),
        }
    }
    assert_eq!(
        runtime.start(request).await,
        Err(MachineBuildStartDomainError::AlreadyRunning)
    );
}

#[test]
fn terminal_evidence_is_final_and_idempotent() {
    let root = tempfile::tempdir().expect("status root");
    let repository = BuildStatusRepository::new(root.path().join("status"));
    let request = build_request("terminal-final", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let completed = BuildExecutorStatus::from_start_ok(
        successful_machine_output(&request, BuildLogSummary::new(3, 4)).into_result(),
    );
    repository
        .write(&BuildStatusRecord::new(completed.clone()))
        .expect("terminal evidence");
    repository
        .write(&BuildStatusRecord::new(completed.clone()))
        .expect("idempotent terminal evidence");

    let conflicting = failed_status(
        acceptance,
        BuildPlatformFailure::MachineUnavailable {
            message: failure_message("late failure"),
        },
        MachineBuildCleanupOutcome::Confirmed,
        BuildLogSummary::new(5, 6),
    );
    assert!(
        repository
            .write(&BuildStatusRecord::new(conflicting))
            .is_err()
    );
    assert_eq!(
        repository
            .read(&request.operation_id)
            .expect("record")
            .expect("present")
            .status,
        completed
    );
}

#[test]
fn running_progress_cannot_regress() {
    let root = tempfile::tempdir().expect("status root");
    let repository = BuildStatusRepository::new(root.path().join("status"));
    let request = build_request("running-monotonic", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let running = |log_summary| BuildExecutorStatus {
        acceptance: acceptance.clone(),
        state: BuildExecutorState::Running { log_summary },
    };
    repository
        .write(&BuildStatusRecord::new(running(BuildLogSummary::new(5, 8))))
        .expect("running evidence");
    assert!(
        repository
            .write(&BuildStatusRecord::new(running(BuildLogSummary::new(4, 8))))
            .is_err()
    );
    assert_eq!(
        repository
            .read(&request.operation_id)
            .expect("record")
            .expect("present")
            .status,
        running(BuildLogSummary::new(5, 8))
    );
}

#[test]
fn terminal_progress_must_dominate_persisted_running_progress() {
    let root = tempfile::tempdir().expect("status root");
    let repository = BuildStatusRepository::new(root.path().join("status"));
    let request = build_request("terminal-progress", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    repository
        .write(&BuildStatusRecord::new(BuildExecutorStatus {
            acceptance: acceptance.clone(),
            state: BuildExecutorState::Running {
                log_summary: BuildLogSummary::new(5, 8),
            },
        }))
        .expect("running evidence");
    let terminal = failed_status(
        acceptance,
        BuildPlatformFailure::MachineUnavailable {
            message: failure_message("late terminal"),
        },
        MachineBuildCleanupOutcome::Confirmed,
        BuildLogSummary::new(4, 8),
    );

    assert!(repository.write(&BuildStatusRecord::new(terminal)).is_err());
    assert!(matches!(
        repository
            .read(&request.operation_id)
            .expect("record")
            .expect("present")
            .status
            .state,
        BuildExecutorState::Running {
            log_summary: BuildLogSummary {
                final_log_sequence: 5,
                omitted_log_bytes: 8,
            }
        }
    ));
}

#[test]
fn concurrent_terminal_writers_choose_one_final_status() {
    let root = tempfile::tempdir().expect("status root");
    let repository = BuildStatusRepository::new(root.path().join("status"));
    let request = build_request("terminal-race", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    repository
        .write(&BuildStatusRecord::new(BuildExecutorStatus {
            acceptance: acceptance.clone(),
            state: BuildExecutorState::Running {
                log_summary: BuildLogSummary::none(),
            },
        }))
        .expect("running evidence");
    let completed = BuildExecutorStatus::from_start_ok(
        successful_machine_output(&request, BuildLogSummary::new(3, 4)).into_result(),
    );
    let failed = failed_status(
        acceptance,
        BuildPlatformFailure::MachineUnavailable {
            message: failure_message("racing failure"),
        },
        MachineBuildCleanupOutcome::Confirmed,
        BuildLogSummary::new(5, 6),
    );
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let writers = [completed.clone(), failed.clone()].map(|status| {
        let repository = repository.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            repository.write(&BuildStatusRecord::new(status))
        })
    });
    let results = writers.map(|writer| writer.join().expect("writer"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let persisted = repository
        .read(&request.operation_id)
        .expect("record")
        .expect("present")
        .status;
    assert!(persisted == completed || persisted == failed);
}

#[tokio::test]
async fn shutdown_preserves_a_terminal_winner_over_a_late_cancel() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let effects = Arc::new(TestBuildEffects::cooperative(true));
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
        status_path.clone(),
    );
    let request = build_request("shutdown-terminal-winner", 10_000);
    let acceptance = runtime
        .start(request.clone())
        .await
        .expect("accepted")
        .executor;
    effects.ingest_started.notified().await;
    let winner = BuildExecutorStatus::from_start_ok(
        successful_machine_output(&request, BuildLogSummary::new(3, 4)).into_result(),
    );
    runtime
        .status
        .write(&BuildStatusRecord::new(winner.clone()))
        .expect("terminal winner");

    runtime.shutdown().await;
    assert_eq!(runtime.status(&acceptance).await, Ok(winner.clone()));

    let retry_effects = Arc::new(TestBuildEffects::cooperative(true));
    let restarted = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        retry_effects.clone(),
        status_path,
    );
    let retried = restarted.start(request).await.expect("idempotent retry");
    assert_eq!(retried.executor, acceptance);
    assert!(restarted.lifecycle.state.lock().await.active.is_empty());
    assert!(!retry_effects.task_active.load(Ordering::SeqCst));
    assert_eq!(restarted.status(&acceptance).await, Ok(winner));
    restarted.shutdown().await;
}

#[tokio::test]
async fn terminal_history_is_not_evicted_and_oldest_retry_is_idempotent() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    let repository = BuildStatusRepository::new(status_path.clone());
    let mut first = None;
    for index in 0..129 {
        let request = build_request(&format!("terminal-history-{index}"), 10_000);
        let status = BuildExecutorStatus::from_start_ok(
            successful_machine_output(&request, BuildLogSummary::new(index + 1, 0)).into_result(),
        );
        repository
            .write(&BuildStatusRecord::new(status.clone()))
            .expect("terminal evidence");
        if index == 0 {
            first = Some((
                request.clone(),
                BuildExecutorAcceptance::from_start_request(&request),
                status,
            ));
        }
    }
    let (request, acceptance, status) = first.expect("first evidence");
    assert_eq!(
        repository
            .read(&request.operation_id)
            .expect("record")
            .expect("retained")
            .status,
        status
    );

    let effects = Arc::new(TestBuildEffects::cooperative(true));
    let runtime = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
        status_path,
    );
    let retried = runtime
        .start(request.clone())
        .await
        .expect("oldest exact retry");
    assert_eq!(retried.executor, acceptance);
    assert!(runtime.lifecycle.state.lock().await.active.is_empty());
    assert!(!effects.task_active.load(Ordering::SeqCst));
    assert_eq!(runtime.status(&acceptance).await, Ok(status));
    let mut conflicting = request;
    conflicting.timeout_millis += 1;
    let conflicting_acceptance = BuildExecutorAcceptance::from_start_request(&conflicting);
    assert_eq!(
        runtime.start(conflicting).await,
        Err(MachineBuildStartDomainError::AlreadyRunning)
    );
    assert!(matches!(
        runtime.status(&conflicting_acceptance).await,
        Err(MachineBuildStatusDomainError::NotFound { .. })
    ));
    runtime.shutdown().await;
}
