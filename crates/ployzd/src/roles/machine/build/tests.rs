use super::*;
use crate::roles::machine::protocol::{
    MachineBuildCachePruneDomainError, MachineBuildCancelDomainError,
};
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildLogChunk, BuildToolchainEvidence};
use std::sync::atomic::Ordering;

pub(super) struct TestBuildEffects {
    pub(super) ingest_started: tokio::sync::Notify,
    pub(super) prune_started: tokio::sync::Notify,
    pub(super) cleanup_calls: std::sync::atomic::AtomicUsize,
    pub(super) task_active: std::sync::atomic::AtomicBool,
    cleanup_completes: bool,
    observes_cancellation: bool,
    prune_completes: bool,
    recovery_completes: bool,
    progress_to_success: Option<(usize, std::time::Duration)>,
}

impl TestBuildEffects {
    pub(super) async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        if self.recovery_completes {
            Ok(())
        } else {
            Err(BuildExecutionError::Infrastructure {
                action: "recover test build orphans",
                message: "injected cleanup failure".to_owned(),
                log_summary: BuildLogSummary::none(),
            })
        }
    }

    pub(super) fn new(cleanup_completes: bool) -> Self {
        Self {
            ingest_started: tokio::sync::Notify::new(),
            prune_started: tokio::sync::Notify::new(),
            cleanup_calls: std::sync::atomic::AtomicUsize::new(0),
            task_active: std::sync::atomic::AtomicBool::new(false),
            cleanup_completes,
            observes_cancellation: false,
            prune_completes: true,
            recovery_completes: true,
            progress_to_success: None,
        }
    }

    fn cooperative(cleanup_completes: bool) -> Self {
        Self {
            observes_cancellation: true,
            ..Self::new(cleanup_completes)
        }
    }

    fn blocking_prune() -> Self {
        Self {
            prune_completes: false,
            ..Self::new(true)
        }
    }

    fn failing_recovery() -> Self {
        Self {
            recovery_completes: false,
            ..Self::new(true)
        }
    }

    fn progressing_to_success(pulses: usize, interval: std::time::Duration) -> Self {
        Self {
            progress_to_success: Some((pulses, interval)),
            ..Self::new(true)
        }
    }

    pub(super) async fn execute_and_ingest(
        &self,
        request: &MachineBuildStartRpcRequest,
        log_progress: BuildLogProgress,
        mut cancelled: watch::Receiver<bool>,
    ) -> Result<MachineBuildOutput, BuildExecutionError> {
        self.task_active.store(true, Ordering::SeqCst);
        let _active = TestTaskActive(&self.task_active);
        log_progress.set_for_test(7, 11);
        self.ingest_started.notify_one();
        if let Some((pulses, interval)) = self.progress_to_success {
            for sequence in 8..8 + u64::try_from(pulses).expect("test pulse count") {
                tokio::time::sleep(interval).await;
                log_progress.set_for_test(sequence, 11);
            }
            let (final_log_sequence, omitted_log_bytes) = log_progress.summary();
            return Ok(successful_machine_output(
                request,
                BuildLogSummary::new(final_log_sequence, omitted_log_bytes),
            ));
        }
        if self.observes_cancellation {
            let _ = cancelled.changed().await;
            return Err(BuildExecutionError::Cancelled {
                log_summary: BuildLogSummary::new(7, 11),
            });
        }
        std::future::pending().await
    }

    pub(super) async fn prune_cache(
        &self,
    ) -> Result<ployz_core::operation::BuildCachePruneEvidence, BuildExecutionError> {
        self.prune_started.notify_one();
        if !self.prune_completes {
            std::future::pending().await
        }
        Ok(ployz_core::operation::BuildCachePruneEvidence {
            before_available_bytes: 0,
            reclaimed_bytes: 0,
            after_available_bytes: 0,
        })
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

fn build_request(operation_id: &str, timeout_millis: u64) -> MachineBuildStartRpcRequest {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    MachineBuildStartRpcRequest {
        operation_id: OperationId::try_new(operation_id).expect("operation"),
        assignment: BuildExecutorAssignment::Cluster { machine_id },
        source: ployz_core::build::GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source")
        .into(),
        adapter: ployz_core::build::BuildAdapter::Railpack {
            cache_scope: ployz_core::build::BuildCacheScope::try_new("test").expect("scope"),
        },
        platform: local_platform().expect("platform"),
        timeout_millis,
    }
}

async fn terminal_status(
    runtime: &MachineBuildRuntime,
    acceptance: &BuildExecutorAcceptance,
) -> BuildExecutorStatus {
    loop {
        let status = runtime.status(acceptance).await.expect("build status");
        if !matches!(status, BuildExecutorStatus::Running { .. }) {
            return status;
        }
        tokio::task::yield_now().await;
    }
}

#[test]
fn local_platform_uses_oci_architecture_names() {
    let platform = local_platform().expect("supported test host");
    assert_eq!(platform.os(), std::env::consts::OS);
    assert!(matches!(platform.architecture(), "amd64" | "arm64"));
}

#[test]
fn machine_log_route_preserves_subject_and_cluster_frame_bytes() {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let operation_id = OperationId::try_new("build-1").expect("operation");
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let route = machine_build_log_route(&machine_id, &operation_id);
    assert_eq!(
        route.subject,
        "plz.v1.signal.machine.machine-a.build.operation.build-1.log"
    );
    let frame = crate::roles::machine::protocol::MachineBuildLogFrame {
        operation_id,
        assignment: route.assignment,
        platform,
        sequence: 3,
        chunk: BuildLogChunk::try_new("hello").expect("chunk"),
    };
    assert_eq!(
        serde_json::to_value(frame).expect("frame"),
        serde_json::json!({
            "operation_id": "build-1",
            "assignment": {"executor": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 3,
            "chunk": "hello",
        })
    );
}

#[test]
fn build_timeout_accepts_the_shared_limit_and_rejects_larger_requests() {
    assert_eq!(
        requested_build_timeout(BUILD_MAX_EXECUTION_TIMEOUT.as_millis() as u64),
        Ok(BUILD_MAX_EXECUTION_TIMEOUT)
    );
    assert!(matches!(
        requested_build_timeout(BUILD_MAX_EXECUTION_TIMEOUT.as_millis() as u64 + 1),
        Err(MachineBuildStartDomainError::InvalidTimeout { timeout_millis })
            if timeout_millis == BUILD_MAX_EXECUTION_TIMEOUT.as_millis() as u64 + 1
    ));
}

#[test]
fn execution_timeout_maps_to_typed_machine_timeout_with_cleanup() {
    let log_summary = BuildLogSummary::new(7, 11);
    let acceptance = BuildExecutorAcceptance::from_start_request(&build_request("build-1", 1_000));
    let expected_acceptance = acceptance.clone();
    assert!(matches!(
        finish_machine_build_status(
            Err(BuildExecutionError::TimedOut { log_summary }),
            MachineBuildCleanupOutcome::Confirmed,
            acceptance,
            log_summary,
        ),
        BuildExecutorStatus::Failed {
            acceptance: actual_acceptance,
            failure: BuildExecutorStatusFailure::Stalled { .. },
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: actual,
        } if actual == log_summary && actual_acceptance == expected_acceptance
    ));
}

#[test]
fn machine_success_requires_and_carries_confirmed_cleanup_proof() {
    let request = build_request("build-success", 1_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let log_summary = BuildLogSummary::new(7, 11);
    let confirmed = finish_machine_build_status(
        Ok(successful_machine_output(&request, log_summary)),
        MachineBuildCleanupOutcome::Confirmed,
        acceptance.clone(),
        log_summary,
    );
    let BuildExecutorStatus::Completed { result: confirmed } = confirmed else {
        panic!("confirmed cleanup permits success");
    };
    assert_eq!(
        confirmed.cleanup,
        BuildExecutorSuccessCleanupEvidence::confirmed()
    );
    assert_eq!(confirmed.acceptance, acceptance);
    assert_eq!(confirmed.log_summary, log_summary);

    let unconfirmed = finish_machine_build_status(
        Ok(successful_machine_output(&request, log_summary)),
        MachineBuildCleanupOutcome::Unconfirmed,
        acceptance.clone(),
        log_summary,
    );
    assert_eq!(
        unconfirmed,
        BuildExecutorStatus::Failed {
            acceptance,
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            failure: BuildExecutorStatusFailure::PlatformFailed {
                failure: BuildPlatformFailure::MachineUnavailable {
                    message: failure_message("build workspace cleanup did not finish successfully",),
                },
            },
            log_summary,
        }
    );
}

fn successful_machine_output(
    request: &MachineBuildStartRpcRequest,
    log_summary: BuildLogSummary,
) -> MachineBuildOutput {
    let machine_id = request.assignment.image_seed().clone();
    let digest = ployz_core::image::OciDigest::try_new(format!("sha256:{}", "a".repeat(64)))
        .expect("digest");
    MachineBuildOutput {
        acceptance: BuildExecutorAcceptance::from_start_request(request),
        image: PlatformImage {
            seed: machine_id,
            manifest_digest: digest.clone(),
            image_id: digest.clone(),
            availability_expires_at: ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(
                4_102_444_800,
            )
            .expect("expiry"),
        },
        verified_source: VerifiedBuildSource::from_source(&request.source),
        toolchain: BuildToolchainEvidence {
            buildkit_image: digest,
            adapter: BuildAdapterToolchainEvidence::Dockerfile,
        },
        log_summary,
    }
}

#[test]
fn cancel_rejects_misaddressed_cluster_provenance() {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let request = MachineBuildCancelRpcRequest {
        operation_id: OperationId::try_new("build-1").expect("operation"),
        assignment: BuildExecutorAssignment::Cluster {
            machine_id: MachineId::try_new("machine-b").expect("machine"),
        },
    };

    assert!(matches!(
        validate_cancel_provenance(&machine_id, &request),
        Err(MachineBuildCancelDomainError::AssignmentMismatch { expected, actual })
            if *expected == (BuildExecutorAssignment::Cluster { machine_id })
                && actual == request.assignment
    ));
}

#[tokio::test]
async fn start_rejects_wrong_origin_before_registration() {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(machine_id.clone(), effects.clone());
    let mut request = build_request("wrong-origin", 1_000);
    let actual = BuildExecutorAssignment::External {
        pool_id: ployz_core::build::BuildPoolId::try_new("pool-a").expect("pool id"),
        executor_id: ployz_core::build::BuildExecutorId::try_new("executor-a")
            .expect("executor id"),
        image_seed: machine_id.clone(),
    };
    request.assignment = actual.clone();

    assert_eq!(
        runtime.start(request).await,
        Err(MachineBuildStartDomainError::AssignmentMismatch {
            expected: Box::new(BuildExecutorAssignment::Cluster { machine_id }),
            actual: Box::new(actual),
        })
    );
    assert!(runtime.lifecycle.state.lock().await.active.is_empty());
    assert!(!effects.task_active.load(Ordering::SeqCst));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn start_rejects_external_assignment_with_different_seed_before_registration() {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(machine_id.clone(), effects.clone());
    let mut request = build_request("wrong-seed", 1_000);
    let actual = BuildExecutorAssignment::External {
        pool_id: ployz_core::build::BuildPoolId::try_new("pool-a").expect("pool id"),
        executor_id: ployz_core::build::BuildExecutorId::try_new("executor-a")
            .expect("executor id"),
        image_seed: MachineId::try_new("machine-b").expect("machine"),
    };
    request.assignment = actual.clone();

    assert_eq!(
        runtime.start(request).await,
        Err(MachineBuildStartDomainError::AssignmentMismatch {
            expected: Box::new(BuildExecutorAssignment::Cluster { machine_id }),
            actual: Box::new(actual),
        })
    );
    assert!(runtime.lifecycle.state.lock().await.active.is_empty());
    assert!(!effects.task_active.load(Ordering::SeqCst));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 0);
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
async fn unavailable_build_runtime_is_typed_machine_evidence_for_every_handler() {
    let machine_id = MachineId::try_new("machine-a").expect("machine");
    let request = MachineBuildStartRpcRequest {
        operation_id: OperationId::try_new("build-1").expect("operation"),
        assignment: BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        },
        source: ployz_core::build::GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source")
        .into(),
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
            error: MachineBuildStartDomainError::RuntimeUnavailable
        } if actual == machine_id
    ));

    let cancel_response = handle_build_cancel(
        machine_id.clone(),
        None,
        NatsServiceRequest {
            payload: serde_json::to_vec(&MachineBuildCancelRpcRequest {
                operation_id: OperationId::try_new("build-1").expect("operation"),
                assignment: BuildExecutorAssignment::Cluster {
                    machine_id: machine_id.clone(),
                },
            })
            .expect("request"),
            headers: None,
        },
    )
    .await;
    let NatsServiceResponse::DomainError { payload } = cancel_response else {
        panic!("unavailable runtime should reject cancellation with a domain error");
    };
    let cancel_response: MachineBuildCancelRpcResponse =
        serde_json::from_slice(&payload).expect("typed response");
    assert!(matches!(
        cancel_response,
        MachineRpcResponse::DomainError {
            machine_id: actual,
            error: MachineBuildCancelDomainError::CancelFailed { message },
        } if actual == machine_id && message.as_str() == "machine build runtime is unavailable"
    ));

    let prune_response = handle_build_cache_prune(
        machine_id.clone(),
        None,
        NatsServiceRequest {
            payload: serde_json::to_vec(&MachineBuildCachePruneRpcRequest {
                operation_id: OperationId::try_new("prune-1").expect("operation"),
            })
            .expect("request"),
            headers: None,
        },
    )
    .await;
    let NatsServiceResponse::DomainError { payload } = prune_response else {
        panic!("unavailable runtime should reject cache pruning with a domain error");
    };
    let prune_response: MachineBuildCachePruneRpcResponse =
        serde_json::from_slice(&payload).expect("typed response");
    assert!(matches!(
        prune_response,
        MachineRpcResponse::DomainError {
            machine_id: actual,
            error: MachineBuildCachePruneDomainError::PruneFailed { message },
        } if actual == machine_id && message.as_str() == "machine build runtime is unavailable"
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
        Err(MachineBuildStartDomainError::RuntimeStopped)
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

    let acceptance = start.await.expect("start task").expect("accepted").executor;
    assert!(matches!(
        runtime.status(&acceptance).await.expect("status"),
        BuildExecutorStatus::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            ..
        }
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
    let acceptance = start.await.expect("start task").expect("accepted").executor;
    assert!(matches!(
        terminal_status(&runtime, &acceptance).await,
        BuildExecutorStatus::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            ..
        }
    ));
    tokio::time::timeout(std::time::Duration::from_secs(1), prune)
        .await
        .expect("prune proceeds after build cleanup")
        .expect("prune succeeds");
}

#[tokio::test(start_paused = true)]
async fn cache_prune_times_out_the_whole_effect_and_releases_the_machine_slot() {
    let effects = Arc::new(TestBuildEffects::blocking_prune());
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let prune_runtime = runtime.clone();
    let prune = tokio::spawn(async move { prune_runtime.prune_cache().await });
    effects.prune_started.notified().await;

    tokio::time::advance(BUILD_CACHE_PRUNE_MAX_EXECUTION_TIMEOUT).await;
    tokio::task::yield_now().await;

    assert!(matches!(
        prune.await.expect("prune task"),
        Err(BuildExecutionError::Infrastructure {
            action: "prune build cache",
            message,
            ..
        }) if message == "build cache prune timed out after 600s"
    ));
    let _slot = runtime
        .machine_slot
        .clone()
        .try_acquire_owned()
        .expect("timed-out prune releases the machine slot");
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

    let acceptance = start.await.expect("start task").expect("accepted").executor;
    let result = terminal_status(&runtime, &acceptance).await;
    assert!(matches!(
        result,
        BuildExecutorStatus::Failed {
            acceptance: _,
            failure: BuildExecutorStatusFailure::Stalled { .. },
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary {
                final_log_sequence: 7,
                omitted_log_bytes: 11,
            },
        }
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

    let acceptance = start.await.expect("start task").expect("accepted").executor;
    assert!(matches!(
        terminal_status(&runtime, &acceptance).await,
        BuildExecutorStatus::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary {
                final_log_sequence: 7,
                omitted_log_bytes: 11,
            },
            ..
        }
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

    let acceptance = start.await.expect("start task").expect("accepted").executor;
    assert!(matches!(
        terminal_status(&runtime, &acceptance).await,
        BuildExecutorStatus::Failed {
            acceptance: _,
            failure: BuildExecutorStatusFailure::Stalled { .. },
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            log_summary: BuildLogSummary {
                final_log_sequence: 7,
                omitted_log_bytes: 11,
            },
        }
    ));
    assert_eq!(effects.cleanup_calls.load(Ordering::SeqCst), 1);
    assert!(!effects.task_active.load(Ordering::SeqCst));
}

#[tokio::test]
async fn start_persists_acceptance_and_returns_before_the_effect_completes() {
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects,
    );
    let request = build_request("prompt-acceptance", 10_000);
    let expected = BuildExecutorAcceptance::from_start_request(&request);

    let accepted =
        tokio::time::timeout(std::time::Duration::from_millis(50), runtime.start(request))
            .await
            .expect("start returns promptly")
            .expect("accepted");

    assert_eq!(accepted.executor, expected);
    assert!(matches!(
        runtime.status(&expected).await.expect("status"),
        BuildExecutorStatus::Running { .. }
    ));
    runtime.shutdown().await;
}

#[tokio::test]
async fn exact_retry_is_idempotent_and_conflicting_commitment_fails_closed() {
    let effects = Arc::new(TestBuildEffects::new(true));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects,
    );
    let request = build_request("retry", 10_000);
    let first = runtime
        .start(request.clone())
        .await
        .expect("first acceptance");
    let retry = runtime
        .start(request.clone())
        .await
        .expect("retry acceptance");
    assert_eq!(retry, first);

    let mut conflicting = request;
    conflicting.timeout_millis = 9_999;
    assert_eq!(
        runtime.start(conflicting).await,
        Err(MachineBuildStartDomainError::AlreadyRunning)
    );
    assert_eq!(runtime.lifecycle.state.lock().await.active.len(), 1);
    runtime.shutdown().await;
}

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
    assert!(
        !bytes
            .windows(b"secret".len())
            .any(|bytes| bytes == b"secret")
    );
    assert!(
        !bytes
            .windows(b"username".len())
            .any(|bytes| bytes == b"username")
    );
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
    let commitment = request_commitment(&request).expect("commitment");
    let repository = BuildStatusRepository::new(status_path.clone());
    repository
        .write(&BuildStatusRecord::new(
            commitment,
            acceptance.clone(),
            BuildExecutorStatus::Running {
                acceptance: acceptance.clone(),
                log_summary: BuildLogSummary::new(4, 5),
            },
        ))
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
            .status,
        BuildExecutorStatus::Running { .. }
    ));

    let recovered = MachineBuildRuntime::new_for_test_with_status_path(
        MachineId::try_new("machine-a").expect("machine"),
        Arc::new(TestBuildEffects::new(true)),
        status_path,
    );
    recovered.recover_orphans().await.expect("recovery");
    assert!(matches!(
        recovered.status(&acceptance).await.expect("status"),
        BuildExecutorStatus::Failed {
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
    let completed = BuildExecutorStatus::Completed {
        result: Box::new(
            successful_machine_output(&completed_request, BuildLogSummary::new(3, 4)).into_result(),
        ),
    };
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
    let cancelled = BuildExecutorStatus::Cancelled {
        acceptance: cancelled_acceptance.clone(),
        cleanup: MachineBuildCleanupOutcome::Confirmed,
        log_summary: BuildLogSummary::new(7, 8),
    };
    for (request, acceptance, status) in [
        (
            &completed_request,
            completed_acceptance.clone(),
            completed.clone(),
        ),
        (&failed_request, failed_acceptance.clone(), failed.clone()),
        (
            &cancelled_request,
            cancelled_acceptance.clone(),
            cancelled.clone(),
        ),
    ] {
        repository
            .write(&BuildStatusRecord::new(
                request_commitment(request).expect("commitment"),
                acceptance,
                status,
            ))
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
async fn corrupt_evidence_blocks_relaunch_and_reports_unavailability() {
    let root = tempfile::tempdir().expect("status root");
    let status_path = root.path().join("status");
    std::fs::create_dir_all(&status_path).expect("status directory");
    let request = build_request("corrupt", 10_000);
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
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
    std::fs::remove_dir(&status_path).expect("remove status directory");
    std::fs::write(&status_path, b"blocks-directory-recreation").expect("blocking file");
    assert_eq!(
        runtime.cancel(&request.operation_id).await,
        MachineBuildCancelOutcome::Requested
    );

    loop {
        match runtime.status(&acceptance).await {
            Err(MachineBuildStatusDomainError::EvidenceUnavailable { .. }) => break,
            Ok(BuildExecutorStatus::Running { .. }) => tokio::task::yield_now().await,
            other => panic!("unexpected status while terminal write fails: {other:?}"),
        }
    }
    assert_eq!(
        runtime.start(request).await,
        Err(MachineBuildStartDomainError::AlreadyRunning)
    );
}

#[tokio::test(start_paused = true)]
async fn strictly_advancing_activity_renews_the_silence_budget_until_success() {
    let effects = Arc::new(TestBuildEffects::progressing_to_success(
        4,
        std::time::Duration::from_millis(80),
    ));
    let runtime = MachineBuildRuntime::new_for_test(
        MachineId::try_new("machine-a").expect("machine"),
        effects.clone(),
    );
    let request = build_request("progressing", 100);
    let acceptance = runtime.start(request).await.expect("accepted").executor;
    effects.ingest_started.notified().await;

    for _ in 0..4 {
        tokio::time::advance(std::time::Duration::from_millis(80)).await;
        tokio::task::yield_now().await;
    }

    assert!(matches!(
        terminal_status(&runtime, &acceptance).await,
        BuildExecutorStatus::Completed { .. }
    ));
}
