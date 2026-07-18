use super::*;
use ployz_core::machine::rpc::MachineRpcResponse;
use std::sync::atomic::Ordering;

pub(super) struct TestBuildEffects {
    pub(super) ingest_started: tokio::sync::Notify,
    pub(super) cleanup_calls: std::sync::atomic::AtomicUsize,
    pub(super) task_active: std::sync::atomic::AtomicBool,
    cleanup_completes: bool,
    observes_cancellation: bool,
}

impl TestBuildEffects {
    fn new(cleanup_completes: bool) -> Self {
        Self {
            ingest_started: tokio::sync::Notify::new(),
            cleanup_calls: std::sync::atomic::AtomicUsize::new(0),
            task_active: std::sync::atomic::AtomicBool::new(false),
            cleanup_completes,
            observes_cancellation: false,
        }
    }

    fn cooperative(cleanup_completes: bool) -> Self {
        Self {
            observes_cancellation: true,
            ..Self::new(cleanup_completes)
        }
    }

    pub(super) async fn execute_and_ingest(
        &self,
        log_progress: BuildLogProgress,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<MachineBuildStartRpcOk, BuildExecutionError> {
        self.task_active.store(true, Ordering::SeqCst);
        let _active = TestTaskActive(&self.task_active);
        log_progress.set_for_test(7, 11);
        self.ingest_started.notify_one();
        if self.observes_cancellation {
            let _ = cancelled.changed().await;
            return Err(BuildExecutionError::Cancelled {
                log_summary: BuildLogSummary::new(7, 11),
            });
        }
        std::future::pending().await
    }

    pub(super) async fn force_cleanup(&self) -> Result<(), BuildExecutionError> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        if self.cleanup_completes {
            Ok(())
        } else {
            std::future::pending().await
        }
    }
}

struct TestTaskActive<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for TestTaskActive<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl MachineBuildRuntime {
    fn new_for_test(machine_id: MachineId, effects: Arc<TestBuildEffects>) -> Self {
        Self {
            machine_id,
            effects: BuildEffects::Test(effects),
            lifecycle: Arc::new(BuildRuntimeLifecycle {
                state: Mutex::new(BuildRuntimeState {
                    phase: BuildRuntimePhase::Accepting,
                    active: BTreeMap::new(),
                }),
                changed: Notify::new(),
            }),
            machine_slot: Arc::new(Semaphore::new(1)),
            local_platform: local_platform().expect("supported test platform"),
        }
    }
}

fn build_request(operation_id: &str, timeout_millis: u64) -> MachineBuildStartRpcRequest {
    MachineBuildStartRpcRequest {
        operation_id: OperationId::try_new(operation_id).expect("operation"),
        source: ployz_core::build::GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source"),
        adapter: ployz_core::build::BuildAdapter::Railpack {
            cache_scope: ployz_core::build::BuildCacheScope::try_new("test").expect("scope"),
        },
        platform: local_platform().expect("platform"),
        timeout_millis,
    }
}

#[test]
fn local_platform_uses_oci_architecture_names() {
    let platform = local_platform().expect("supported test host");
    assert_eq!(platform.os(), std::env::consts::OS);
    assert!(matches!(platform.architecture(), "amd64" | "arm64"));
}

#[test]
fn build_timeout_accepts_the_shared_limit_and_rejects_larger_requests() {
    assert_eq!(
        requested_build_timeout(BUILD_MAX_EXECUTION_TIMEOUT.as_millis() as u64),
        Ok(BUILD_MAX_EXECUTION_TIMEOUT)
    );
    assert!(matches!(
        requested_build_timeout(BUILD_MAX_EXECUTION_TIMEOUT.as_millis() as u64 + 1),
        Err(MachineBuildStartDomainError::TimedOut { message, .. })
            if message.as_str() == "build timeout must be between 1ms and 30m"
    ));
}

#[test]
fn execution_timeout_maps_to_typed_machine_timeout_with_cleanup() {
    let log_summary = BuildLogSummary::new(7, 11);
    assert!(matches!(
        machine_build_error(
            BuildExecutionError::TimedOut { log_summary },
            MachineBuildCleanupOutcome::Confirmed,
        ),
        MachineBuildStartDomainError::TimedOut {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: actual,
            ..
        } if actual == log_summary
    ));
}

#[tokio::test]
async fn supervisor_completion_observed_before_wait_is_not_lost() {
    let completion = Arc::new(BuildSupervisorCompletion::new());
    drop(BuildSupervisorCompletionGuard(completion.clone()));

    tokio::time::timeout(std::time::Duration::from_millis(10), completion.wait())
        .await
        .expect("completed supervisor is observed without waiting for another notification");
}

#[tokio::test]
async fn build_start_rejects_malformed_requests_at_the_transport_boundary() {
    let response = handle_build_start(
        MachineId::try_new("machine-a").expect("machine"),
        None,
        NatsServiceRequest {
            payload: b"not-json".to_vec(),
            headers: None,
        },
    )
    .await;

    assert!(matches!(
        response,
        NatsServiceResponse::TransportError { .. }
    ));
}

#[tokio::test]
async fn unavailable_build_runtime_is_typed_machine_evidence() {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let request = MachineBuildStartRpcRequest {
        operation_id: OperationId::try_new("build-1").expect("operation"),
        source: ployz_core::build::GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source"),
        adapter: ployz_core::build::BuildAdapter::Railpack {
            cache_scope: ployz_core::build::BuildCacheScope::try_new("test").expect("scope"),
        },
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        timeout_millis: 1_000,
    };
    let response = handle_build_start(
        machine_id.clone(),
        None,
        NatsServiceRequest {
            payload: serde_json::to_vec(&request).expect("request"),
            headers: None,
        },
    )
    .await;
    let NatsServiceResponse::DomainError { payload } = response else {
        panic!("unavailable runtime should be a domain error");
    };
    let response: MachineBuildStartRpcResponse =
        serde_json::from_slice(&payload).expect("typed response");

    assert!(matches!(
        response,
        MachineRpcResponse::DomainError {
            machine_id: actual,
            error: MachineBuildStartDomainError::PlatformFailed {
                failure: BuildPlatformFailure::MachineUnavailable { .. },
                ..
            }
        } if actual == machine_id
    ));
}

#[tokio::test]
async fn shutdown_closes_build_admission_with_stable_typed_evidence() {
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects,
    );
    runtime.shutdown().await;

    assert!(matches!(
        runtime.start(build_request("after-shutdown", 1_000)).await,
        Err(MachineBuildStartDomainError::PlatformFailed {
            failure: BuildPlatformFailure::MachineUnavailable { message },
            ..
        }) if message.as_str() == "machine build runtime is shutting down"
    ));
}

#[tokio::test]
async fn shutdown_cancels_active_build_and_waits_for_cleanup() {
    let effects = Arc::new(TestBuildEffects::cooperative(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let start_runtime = runtime.clone();
    let start = tokio::spawn(async move {
        start_runtime
            .start(build_request("shutdown-active", 10_000))
            .await
    });
    effects.ingest_started.notified().await;

    runtime.shutdown().await;

    assert!(matches!(
        start.await.expect("start task"),
        Err(MachineBuildStartDomainError::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            ..
        })
    ));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
    assert!(!effects.task_active.load(Ordering::SeqCst));
    assert!(runtime.lifecycle.state.lock().await.active.is_empty());
    assert!(runtime.lifecycle.state.lock().await.phase == BuildRuntimePhase::Stopped);
}

#[tokio::test]
async fn cache_prune_waits_for_active_build_cleanup() {
    let effects = Arc::new(TestBuildEffects::cooperative(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let operation_id = OperationId::try_new("build-before-prune").expect("operation");
    let start_runtime = runtime.clone();
    let request = build_request(operation_id.as_str(), 10_000);
    let start = tokio::spawn(async move { start_runtime.start(request).await });
    effects.ingest_started.notified().await;

    let mut prune = Box::pin(runtime.prune_cache());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut prune)
            .await
            .is_err()
    );

    assert_eq!(
        runtime.cancel(&operation_id).await,
        MachineBuildCancelOutcome::Requested
    );
    assert!(matches!(
        start.await.expect("start task"),
        Err(MachineBuildStartDomainError::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            ..
        })
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), prune)
        .await
        .expect("prune proceeds after build cleanup")
        .expect("prune succeeds");
}

#[tokio::test]
async fn shutdown_waits_for_the_machine_slot_before_stopping() {
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
    );
    let slot = runtime
        .machine_slot
        .clone()
        .acquire_owned()
        .await
        .expect("machine slot");
    let shutdown_runtime = runtime.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_runtime.shutdown().await });

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut shutdown)
            .await
            .is_err()
    );
    assert!(runtime.lifecycle.state.lock().await.phase == BuildRuntimePhase::ShuttingDown);

    drop(slot);
    tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
        .await
        .expect("shutdown proceeds after slot release")
        .expect("shutdown task");
    assert!(runtime.lifecycle.state.lock().await.phase == BuildRuntimePhase::Stopped);
}

#[tokio::test(start_paused = true)]
async fn shutdown_rejects_cache_prune_waiting_behind_a_build() {
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let start_runtime = runtime.clone();
    let start = tokio::spawn(async move {
        start_runtime
            .start(build_request("build-during-shutdown", 10_000))
            .await
    });
    effects.ingest_started.notified().await;
    let mut prune = Box::pin(runtime.prune_cache());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), &mut prune)
            .await
            .is_err()
    );

    let shutdown_runtime = runtime.clone();
    let shutdown = tokio::spawn(async move { shutdown_runtime.shutdown().await });
    tokio::task::yield_now().await;
    tokio::time::advance(BUILD_TASK_DRAIN_TIMEOUT).await;
    tokio::task::yield_now().await;

    assert!(matches!(
        prune.await,
        Err(BuildExecutionError::Infrastructure {
            action: "prune build cache",
            ..
        })
    ));
    shutdown.await.expect("shutdown task");
    let _ = start.await.expect("start task");
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_during_ingestion_aborts_then_cleans_once_without_late_success() {
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let start_runtime = runtime.clone();
    let start = tokio::spawn(async move {
        start_runtime
            .start(build_request("build-timeout", 100))
            .await
    });
    effects.ingest_started.notified().await;

    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(BUILD_TASK_DRAIN_TIMEOUT).await;
    tokio::task::yield_now().await;

    let result = start.await.expect("start task");
    assert!(matches!(
        result,
        Err(MachineBuildStartDomainError::TimedOut {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary {
                final_log_sequence: 7,
                omitted_log_bytes: 11,
            },
            ..
        })
    ));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
    assert!(!effects.task_active.load(Ordering::SeqCst));

    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
    assert!(!effects.task_active.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_ingestion_aborts_then_returns_typed_cleanup() {
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let operation_id = OperationId::try_new("build-cancel").expect("operation");
    let start_runtime = runtime.clone();
    let request = build_request(operation_id.as_str(), 10_000);
    let start = tokio::spawn(async move { start_runtime.start(request).await });
    effects.ingest_started.notified().await;

    assert_eq!(
        runtime.cancel(&operation_id).await,
        MachineBuildCancelOutcome::Requested
    );
    tokio::task::yield_now().await;
    tokio::time::advance(BUILD_TASK_DRAIN_TIMEOUT).await;
    tokio::task::yield_now().await;

    assert!(matches!(
        start.await.expect("start task"),
        Err(MachineBuildStartDomainError::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary {
                final_log_sequence: 7,
                omitted_log_bytes: 11,
            },
        })
    ));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
    assert!(!effects.task_active.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn bounded_cleanup_reports_unconfirmed_when_it_cannot_finish() {
    let effects = Arc::new(TestBuildEffects::new(false));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let start_runtime = runtime.clone();
    let start = tokio::spawn(async move {
        start_runtime
            .start(build_request("build-cleanup-timeout", 100))
            .await
    });
    effects.ingest_started.notified().await;

    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(BUILD_TASK_DRAIN_TIMEOUT).await;
    tokio::task::yield_now().await;
    tokio::time::advance(BUILD_FORCE_CLEANUP_TIMEOUT).await;
    tokio::task::yield_now().await;

    assert!(matches!(
        start.await.expect("start task"),
        Err(MachineBuildStartDomainError::TimedOut {
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            log_summary: BuildLogSummary {
                final_log_sequence: 7,
                omitted_log_bytes: 11,
            },
            ..
        })
    ));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
    assert!(!effects.task_active.load(Ordering::SeqCst));
}
