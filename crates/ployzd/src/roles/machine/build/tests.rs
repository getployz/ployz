use super::*;
use ployz_core::machine::rpc::MachineRpcResponse;
use std::sync::atomic::Ordering;

pub(super) struct TestBuildEffects {
    pub(super) ingest_started: tokio::sync::Notify,
    pub(super) cleanup_calls: std::sync::atomic::AtomicUsize,
    pub(super) task_active: std::sync::atomic::AtomicBool,
    cleanup_completes: bool,
}

impl TestBuildEffects {
    fn new(cleanup_completes: bool) -> Self {
        Self {
            ingest_started: tokio::sync::Notify::new(),
            cleanup_calls: std::sync::atomic::AtomicUsize::new(0),
            task_active: std::sync::atomic::AtomicBool::new(false),
            cleanup_completes,
        }
    }

    pub(super) async fn execute_and_ingest(
        &self,
        log_progress: BuildLogProgress,
    ) -> Result<MachineBuildStartRpcOk, BuildExecutionError> {
        self.task_active.store(true, Ordering::SeqCst);
        let _active = TestTaskActive(&self.task_active);
        log_progress.set_for_test(7, 11);
        self.ingest_started.notify_one();
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
            active: Arc::new(Mutex::new(BTreeMap::new())),
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
