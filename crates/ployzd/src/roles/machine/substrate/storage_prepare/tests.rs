use super::*;
use ployz_core::storage::{PreparedStorageOrigin, PreparedStorageState, ZfsDatasetRoot};
use std::ffi::OsStr;

fn operation(value: &str) -> OperationId {
    OperationId::try_new(value).expect("operation id")
}

fn reason(value: &str) -> CancellationReason {
    CancellationReason::try_new(value).expect("cancellation reason")
}

fn prepared(pool: &str) -> PreparedStorageState {
    let pool = ZfsPoolName::try_new(pool).expect("pool");
    PreparedStorageState::try_new(
        pool.clone(),
        PreparedStorageOrigin::Adopted,
        ZfsDatasetRoot::for_pool(&pool),
    )
    .expect("prepared state")
}

fn command_argv(command: &tokio::process::Command) -> Vec<String> {
    let command = command.as_std();
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(OsStr::to_string_lossy)
        .map(|argument| argument.into_owned())
        .collect()
}

fn substrate_guard(operation_id: &OperationId) -> PrivilegedSubstrateGuard {
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let gate = gate.try_lock_owned().expect("guard");
    PrivilegedSubstrateGuard {
        _gate: gate,
        owner: Arc::new(std::sync::Mutex::new(Some(operation_id.clone()))),
        operation_id: operation_id.clone(),
    }
}

#[cfg(target_os = "linux")]
fn spawn_operation_process(operation_id: &OperationId) -> tokio::process::Child {
    operation_process_command(operation_id, true)
        .spawn()
        .expect("operation child")
}

#[cfg(target_os = "linux")]
fn operation_process_command(
    operation_id: &OperationId,
    kill_on_drop: bool,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 30; :")
        .arg("storage-prepare")
        .arg(operation_id.as_str())
        .process_group(0)
        .kill_on_drop(kill_on_drop);
    command
}

#[test]
fn command_uses_the_shared_privileged_substrate_lock() {
    let operation_id = operation("op_storage_prepare");
    let pool = ZfsPoolName::try_new("tank").expect("pool");
    let command = PrivilegedHostEffect::StoragePrepare {
        operation_id: &operation_id,
        pool: Some(&pool),
    }
    .into_command();

    assert_eq!(
        command_argv(&command),
        [
            "flock",
            "--no-fork",
            "--nonblock",
            MACHINE_SUBSTRATE_LOCK_FILE,
            "ployz",
            "host",
            "storage-prepare",
            "--operation-id",
            "op_storage_prepare",
            "--pool",
            "tank",
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn report_is_a_closed_five_state_projection_and_stale_running_becomes_interrupted() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let repository = StorageEvidenceRepository::new(directory.path());
    let missing = operation("op_missing");
    assert_eq!(
        repository.report(&missing).expect("missing report"),
        MachineStoragePrepareReport::NotFound
    );

    let running = operation("op_running");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut running_child = runtime.block_on(async { spawn_operation_process(&running) });
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: running.clone(),
            launched_at_unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as u64,
            process: read_process_identity(running_child.id().expect("pid"))
                .expect("test process identity"),
        })
        .expect("running evidence");
    assert_eq!(
        repository.report(&running).expect("running report"),
        MachineStoragePrepareReport::Running
    );

    let completed = operation("op_completed");
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Completed {
            operation_id: completed.clone(),
            prepared: prepared("tank"),
        })
        .expect("completed evidence");
    assert_eq!(
        repository.report(&completed).expect("completed report"),
        MachineStoragePrepareReport::Completed {
            pool: ZfsPoolName::try_new("tank").expect("pool"),
        }
    );

    let failed = operation("op_failed");
    let failure = StorageEffectFailure::OperationTimedOut;
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Failed {
            operation_id: failed.clone(),
            failure: failure.clone(),
        })
        .expect("failed evidence");
    assert_eq!(
        repository.report(&failed).expect("failed report"),
        MachineStoragePrepareReport::Failed { failure }
    );

    let cancelled = operation("op_cancelled");
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Cancelled {
            operation_id: cancelled.clone(),
            reason: reason("operator cancelled"),
        })
        .expect("cancelled evidence");
    assert_eq!(
        repository.report(&cancelled).expect("cancelled report"),
        MachineStoragePrepareReport::Cancelled {
            reason: reason("operator cancelled"),
        }
    );

    let stale = operation("op_stale");
    let mut stale_identity = read_process_identity(std::process::id()).expect("identity");
    stale_identity.pid = u32::MAX;
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: stale.clone(),
            launched_at_unix_millis: 1,
            process: stale_identity,
        })
        .expect("stale evidence");
    assert!(matches!(
        repository.report(&stale).expect("stale report"),
        MachineStoragePrepareReport::Failed {
            failure: StorageEffectFailure::Interrupted { .. }
        }
    ));
    runtime.block_on(async {
        running_child.kill().await.expect("stop running child");
    });
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn active_and_terminal_replays_acknowledge_without_spawning_and_other_operation_is_busy() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    let completed = operation("op_terminal_replay");
    StorageEvidenceRepository::new(directory.path())
        .persist_evidence(&MachineStoragePreparationEvidence::Completed {
            operation_id: completed.clone(),
            prepared: prepared("tank"),
        })
        .expect("terminal evidence");
    runtime
        .start(completed, None)
        .await
        .expect("terminal replay acknowledgement");

    let owner = operation("op_owner");
    let mut owner_child = spawn_operation_process(&owner);
    StorageEvidenceRepository::new(directory.path())
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: owner.clone(),
            launched_at_unix_millis: 1,
            process: read_process_identity(owner_child.id().expect("pid")).expect("identity"),
        })
        .expect("running evidence");
    let (cancel, _cancel_rx) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: owner.clone(),
        cancel: Some(cancel),
    });
    runtime
        .start(owner.clone(), None)
        .await
        .expect("active replay acknowledgement");
    assert_eq!(
        runtime.start(operation("op_other"), None).await,
        Err(MachineStoragePrepareDomainError::Busy {
            owner_operation_id: owner,
        })
    );
    owner_child.kill().await.expect("stop owner child");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn accepted_running_is_queryable_then_original_deadline_reaps_and_fails() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_millis(30));
    let operation_id = operation("op_timeout");
    let child = operation_process_command(&operation_id, false)
        .spawn()
        .expect("operation child");
    let pid = child.id().expect("pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as u64,
            process: read_process_identity(pid).expect("child identity"),
        })
        .expect("running evidence");
    let (cancel, cancel_rx) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: operation_id.clone(),
        cancel: Some(cancel),
    });
    let guard = substrate_guard(&operation_id);
    let (accepted, accepted_rx) = oneshot::channel();
    tokio::spawn(supervise_storage_prepare_child(
        runtime.clone(),
        operation_id.clone(),
        child,
        guard,
        cancel_rx,
        accepted,
    ));

    accepted_rx
        .await
        .expect("acceptance channel")
        .expect("accepted");
    assert_eq!(
        repository.report(&operation_id).expect("report"),
        MachineStoragePrepareReport::Running
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.state.lock().await.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("supervisor completion");
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(
        repository.report(&operation_id).expect("timeout report"),
        MachineStoragePrepareReport::Failed {
            failure: StorageEffectFailure::OperationTimedOut,
        }
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_reaps_exact_child_and_is_idempotent() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    let operation_id = operation("op_cancel");
    let child = spawn_operation_process(&operation_id);
    let pid = child.id().expect("pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as u64,
            process: read_process_identity(pid).expect("child identity"),
        })
        .expect("running evidence");
    let (cancel, cancel_rx) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: operation_id.clone(),
        cancel: Some(cancel),
    });
    let guard = substrate_guard(&operation_id);
    let (accepted, accepted_rx) = oneshot::channel();
    tokio::spawn(supervise_storage_prepare_child(
        runtime.clone(),
        operation_id.clone(),
        child,
        guard,
        cancel_rx,
        accepted,
    ));
    accepted_rx
        .await
        .expect("acceptance channel")
        .expect("accepted");
    let cancelled = runtime
        .cancel(&operation_id, reason("operator cancelled"))
        .await
        .expect("cancel");
    assert_eq!(
        cancelled,
        MachineStoragePrepareReport::Cancelled {
            reason: reason("operator cancelled"),
        }
    );
    let repeated = runtime
        .cancel(&operation_id, reason("operator cancelled again"))
        .await
        .expect("repeated cancel");
    assert_eq!(repeated, cancelled);
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.state.lock().await.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancel completion");
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(
        repository.report(&operation_id).expect("report"),
        MachineStoragePrepareReport::Cancelled {
            reason: reason("operator cancelled"),
        }
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn runtime_reconstruction_adopts_the_exact_live_process_and_preserves_its_deadline() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let operation_id = operation("op_recovered");
    let child = operation_process_command(&operation_id, false)
        .spawn()
        .expect("operation child");
    let pid = child.id().expect("pid");
    StorageEvidenceRepository::new(directory.path())
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as u64,
            process: read_process_identity(pid).expect("identity"),
        })
        .expect("running evidence");
    drop(child);

    let reconstructed = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    reconstructed.recover().await.expect("recover runtime");
    assert_eq!(
        reconstructed
            .state
            .lock()
            .await
            .as_ref()
            .map(|active| active.operation_id.clone()),
        Some(operation_id.clone())
    );
    assert_eq!(
        StorageEvidenceRepository::new(directory.path())
            .report(&operation_id)
            .expect("report"),
        MachineStoragePrepareReport::Running
    );
    let cancelled = reconstructed
        .cancel(&operation_id, reason("operator cancelled recovered work"))
        .await
        .expect("cancel");
    assert_eq!(
        cancelled,
        MachineStoragePrepareReport::Cancelled {
            reason: reason("operator cancelled recovered work"),
        }
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while reconstructed.state.lock().await.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("recovered supervisor completion");
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(
        StorageEvidenceRepository::new(directory.path())
            .report(&operation_id)
            .expect("report"),
        MachineStoragePrepareReport::Cancelled {
            reason: reason("operator cancelled recovered work"),
        }
    );
}

#[test]
fn launch_anchored_budget_never_renews() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let repository = StorageEvidenceRepository::new(directory.path());
    let operation_id = operation("op_old");
    #[cfg(target_os = "linux")]
    let process = read_process_identity(std::process::id()).expect("identity");
    #[cfg(not(target_os = "linux"))]
    let process = StoragePreparationProcessIdentity {
        boot_id: "boot".to_owned(),
        pid: 1,
        start_time_ticks: 1,
        expected_command: "ployz".to_owned(),
    };
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: 1,
            process,
        })
        .expect("running evidence");
    assert_eq!(
        remaining_budget(&repository, &operation_id, Duration::from_secs(1)).expect("budget"),
        Duration::ZERO
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn leader_exit_reaps_its_remaining_descendant_before_terminal_evidence() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let descendant_file = directory.path().join("descendant.pid");
    let operation_id = operation("op_descendant");
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 30 & echo $! > \"$1\"; sleep 1")
        .arg("storage-prepare")
        .arg(&descendant_file)
        .arg(operation_id.as_str())
        .process_group(0)
        .kill_on_drop(false);
    let child = command.spawn().expect("operation child");
    let leader_pid = child.id().expect("leader pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis() as u64,
            process: read_process_identity(leader_pid).expect("leader identity"),
        })
        .expect("running evidence");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    let (cancel, cancel_rx) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: operation_id.clone(),
        cancel: Some(cancel),
    });
    let (accepted, accepted_rx) = oneshot::channel();
    tokio::spawn(supervise_storage_prepare_child(
        runtime.clone(),
        operation_id.clone(),
        child,
        substrate_guard(&operation_id),
        cancel_rx,
        accepted,
    ));
    accepted_rx
        .await
        .expect("acceptance channel")
        .expect("accepted");
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.state.lock().await.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervisor completion");
    let descendant_pid: u32 = std::fs::read_to_string(descendant_file)
        .expect("descendant pid")
        .trim()
        .parse()
        .expect("numeric descendant pid");
    assert!(!process_group_is_live(leader_pid));
    assert!(
        !Path::new(&format!("/proc/{descendant_pid}")).exists() || {
            read_process_state_and_group(descendant_pid).is_some_and(|(state, _)| state == 'Z')
        }
    );
    assert!(matches!(
        repository.report(&operation_id).expect("terminal report"),
        MachineStoragePrepareReport::Failed { .. }
    ));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn termination_refusal_does_not_replace_running_evidence() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let actual_operation = operation("op_actual");
    let requested_operation = operation("op_wrong");
    let mut child = spawn_operation_process(&actual_operation);
    let group_id = child.id().expect("pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: requested_operation.clone(),
            launched_at_unix_millis: 1,
            process: read_process_identity(group_id).expect("identity"),
        })
        .expect("running evidence");
    assert!(
        terminate_storage_prepare_child(&repository, &requested_operation, group_id, &mut child,)
            .await
            .is_err()
    );
    assert!(matches!(
        repository
            .read_optional(&requested_operation)
            .expect("evidence"),
        Some(MachineStoragePreparationEvidence::Running { .. })
    ));
    terminate_owned_process_group(group_id)
        .await
        .expect("test cleanup");
    let _ = child.wait().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_refusal_is_not_acknowledged_and_retains_running_evidence() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    let actual_operation = operation("op_actual_cancel");
    let requested_operation = operation("op_refused_cancel");
    let mut child = spawn_operation_process(&actual_operation);
    let group_id = child.id().expect("pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: requested_operation.clone(),
            launched_at_unix_millis: 1,
            process: read_process_identity(group_id).expect("identity"),
        })
        .expect("running evidence");

    assert!(
        runtime
            .cancel(&requested_operation, reason("must be refused"))
            .await
            .is_err()
    );
    assert!(matches!(
        repository
            .read_optional(&requested_operation)
            .expect("evidence"),
        Some(MachineStoragePreparationEvidence::Running { .. })
    ));

    terminate_owned_process_group(group_id)
        .await
        .expect("test cleanup");
    let _ = child.wait().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn paused_supervisor_cannot_produce_an_unbounded_or_false_cancel_acknowledgement() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30))
        .with_cancel_ack_budget(Duration::from_millis(30));
    let operation_id = operation("op_paused_cancel");
    let mut child = spawn_operation_process(&operation_id);
    let group_id = child.id().expect("pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: 1,
            process: read_process_identity(group_id).expect("identity"),
        })
        .expect("running evidence");
    let (cancel, paused_receiver) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: operation_id.clone(),
        cancel: Some(cancel),
    });

    let result = tokio::time::timeout(
        Duration::from_millis(200),
        runtime.cancel(&operation_id, reason("operator cancelled")),
    )
    .await
    .expect("bounded acknowledgement");
    assert!(result.is_err());
    assert!(matches!(
        repository.read_optional(&operation_id).expect("evidence"),
        Some(MachineStoragePreparationEvidence::Running { .. })
    ));

    drop(paused_receiver);
    terminate_owned_process_group(group_id)
        .await
        .expect("test cleanup");
    let _ = child.wait().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_bounds_the_entire_cancel_acknowledgement_and_state_clear_wait() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30))
        .with_cancel_ack_budget(Duration::from_millis(30));
    let operation_id = operation("op_paused_shutdown");
    let mut child = spawn_operation_process(&operation_id);
    let group_id = child.id().expect("pid");
    let repository = StorageEvidenceRepository::new(directory.path());
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: 1,
            process: read_process_identity(group_id).expect("identity"),
        })
        .expect("running evidence");
    let (cancel, paused_receiver) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: operation_id.clone(),
        cancel: Some(cancel),
    });

    tokio::time::timeout(Duration::from_millis(250), runtime.shutdown())
        .await
        .expect("bounded shutdown");
    assert!(runtime.state.lock().await.is_some());
    assert!(matches!(
        repository.read_optional(&operation_id).expect("evidence"),
        Some(MachineStoragePreparationEvidence::Running { .. })
    ));

    drop(paused_receiver);
    terminate_owned_process_group(group_id)
        .await
        .expect("test cleanup");
    let _ = child.wait().await;
}

#[tokio::test]
async fn only_matching_active_supervision_suppresses_stale_running_terminalization() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    let operation_id = operation("op_supervised_report");
    #[cfg(target_os = "linux")]
    let mut stale_identity = read_process_identity(std::process::id()).expect("identity");
    #[cfg(not(target_os = "linux"))]
    let mut stale_identity = StoragePreparationProcessIdentity {
        boot_id: "boot".to_owned(),
        pid: 1,
        start_time_ticks: 1,
        expected_command: "ployz".to_owned(),
    };
    stale_identity.pid = u32::MAX;
    StorageEvidenceRepository::new(directory.path())
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: 1,
            process: stale_identity,
        })
        .expect("running evidence");
    let (cancel, _cancel_rx) = oneshot::channel();
    *runtime.state.lock().await = Some(ActiveStoragePreparation {
        operation_id: operation_id.clone(),
        cancel: Some(cancel),
    });

    assert_eq!(
        runtime.report(&operation_id).await.expect("active report"),
        MachineStoragePrepareReport::Running
    );
    *runtime.state.lock().await = None;
    assert!(matches!(
        runtime.report(&operation_id).await.expect("stale report"),
        MachineStoragePrepareReport::Failed {
            failure: StorageEffectFailure::Interrupted { .. }
        }
    ));
}

#[test]
fn terminal_completion_wins_a_cancel_race_without_overwrite() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let runtime = StoragePrepareRuntime::new(directory.path(), Duration::from_secs(30));
    let operation_id = operation("op_completed_before_cancel");
    StorageEvidenceRepository::new(directory.path())
        .persist_evidence(&MachineStoragePreparationEvidence::Completed {
            operation_id: operation_id.clone(),
            prepared: prepared("tank"),
        })
        .expect("completed evidence");

    let report = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(runtime.cancel(&operation_id, reason("too late")))
        .expect("terminal replay");
    assert_eq!(
        report,
        MachineStoragePrepareReport::Completed {
            pool: ZfsPoolName::try_new("tank").expect("pool"),
        }
    );
}

#[test]
fn a_clock_rollback_fails_the_remaining_budget_closed() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let repository = StorageEvidenceRepository::new(directory.path());
    let operation_id = operation("op_future_launch");
    #[cfg(target_os = "linux")]
    let process = read_process_identity(std::process::id()).expect("identity");
    #[cfg(not(target_os = "linux"))]
    let process = StoragePreparationProcessIdentity {
        boot_id: "boot".to_owned(),
        pid: 1,
        start_time_ticks: 1,
        expected_command: "ployz".to_owned(),
    };
    let future_launch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
        + 60_000;
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Running {
            operation_id: operation_id.clone(),
            launched_at_unix_millis: future_launch,
            process,
        })
        .expect("running evidence");
    assert_eq!(
        remaining_budget(&repository, &operation_id, Duration::from_secs(30)).expect("budget"),
        Duration::ZERO
    );
}

#[test]
fn completion_wins_over_a_stale_running_failure_transition() {
    let directory = tempfile::tempdir().expect("evidence directory");
    let repository = StorageEvidenceRepository::new(directory.path());
    let operation_id = operation("op_completion_race");
    repository
        .persist_evidence(&MachineStoragePreparationEvidence::Completed {
            operation_id: operation_id.clone(),
            prepared: prepared("tank"),
        })
        .expect("completed evidence");
    assert!(matches!(
        repository
            .transition_running_to_failure(
                &operation_id,
                &StorageEffectFailure::Interrupted {
                    message: "stale report".to_owned(),
                },
            )
            .expect("transition result"),
        Some(MachineStoragePreparationEvidence::Completed { .. })
    ));
}
