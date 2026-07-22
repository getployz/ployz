use super::*;
use ployz_core::storage::{PreparedStorageOrigin, ZfsDatasetRoot};
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
        .arg("sleep 30")
        .arg("storage-prepare")
        .arg(operation_id.as_str())
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
    runtime
        .cancel(&operation_id, reason("operator cancelled"))
        .await
        .expect("cancel");
    runtime
        .cancel(&operation_id, reason("operator cancelled again"))
        .await
        .expect("repeated cancel");
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
    reconstructed
        .cancel(&operation_id, reason("operator cancelled recovered work"))
        .await
        .expect("cancel");
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
