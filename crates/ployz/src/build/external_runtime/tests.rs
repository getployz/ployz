use std::path::Path;

use super::*;
use ployz_core::build::{
    BuildAdapter, BuildContextPath, BuildSource, GitSource, VerifiedBuildSource,
};
use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
use ployz_core::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildToolchainEvidence};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsClientAuth, NatsClientUrl, NatsTlsTrust};

use crate::build::executor_context::{
    ExecutorContextPublication, ExecutorIdentityPaths, load_or_create_identity,
    publish_executor_context,
};
use crate::build::external_service::executor_endpoints;
use crate::build::watch_lifecycle::executor_principal;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn shutdown_failure_precedes_wait_failure_after_both_complete() {
    let error = finish_executor_session(
        Err(BuildExecutionError::ExecutorIdleTimedOut {
            wait_timeout: Duration::from_secs(1),
        }),
        Err(PloyzctlExecutionError::from(
            BuildExecutionError::ExecutorRuntime {
                message: "cleanup evidence failed".to_owned(),
            },
        )),
    )
    .expect_err("cleanup failure takes precedence");

    assert!(error.to_string().contains("cleanup evidence failed"));
    assert!(!error.to_string().contains("idle"));
}

#[test]
fn configured_identity_drives_principal_and_exact_endpoint_subjects() {
    let identity = identity();

    assert_eq!(
        executor_principal(&identity),
        NatsPrincipal::BuildExecutor {
            pool_id: identity.pool_id.clone(),
            executor_id: identity.executor_id.clone(),
        }
    );
    let [readiness, start, cancel] = executor_endpoints(&identity);
    assert_eq!(
        readiness.subject,
        "plz.v1.rpc.build_executor.query.pool_ci.executor_1.readiness.get"
    );
    assert_eq!(
        start.subject,
        "plz.v1.rpc.build_executor.command.pool_ci.executor_1.build.start"
    );
    assert_eq!(
        cancel.subject,
        "plz.v1.rpc.build_executor.command.pool_ci.executor_1.build.cancel"
    );
}

#[test]
fn executor_context_is_the_only_connection_authority() {
    let temporary = tempfile::tempdir().expect("temporary context root");
    let identity = identity();
    let paths = ExecutorIdentityPaths::new(temporary.path(), identity.clone());
    load_or_create_identity(&paths).expect("executor identity");
    publish_executor_context(
        &paths,
        ExecutorContextPublication {
            organization_id: &ployz_core::ids::SubjectToken::try_new("org-1")
                .expect("organization id"),
            nats_url: NatsClientUrl::try_new("tls://executor.example:4222").expect("NATS URL"),
            ca_pem: "executor-ca",
            credential_expires_at:
                ployz_core::nats_config::BuildExecutorCredentialExpiresAt::try_new(u64::MAX)
                    .expect("expiry"),
        },
    )
    .expect("published executor context");
    let config = PloyzctlRuntimeConfig {
        nats_url: Some("tls://operator.example:4222".to_owned()),
        nats_ca_file: Some(PathBuf::from("operator-ca")),
        nats_seed_file: Some(PathBuf::from("operator-seed")),
        build_executor: crate::dispatcher::BuildExecutorRuntimeConfig {
            context_root: Some(temporary.path().to_owned()),
            ..crate::dispatcher::BuildExecutorRuntimeConfig::default()
        },
        ..PloyzctlRuntimeConfig::default()
    };

    let (connect, expires_at) =
        executor_connect_config(&config, &identity).expect("executor connection config");

    assert_eq!(connect.url.as_str(), "tls://executor.example:4222");
    assert_eq!(connect.principal, executor_principal(&identity));
    assert_eq!(expires_at.unix_seconds(), u64::MAX);
    assert!(matches!(connect.auth, NatsClientAuth::NkeySeed(_)));
    assert!(
        matches!(connect.trust, NatsTlsTrust::ClusterCa(path) if path != Path::new("operator-ca"))
    );
}

#[test]
fn explicit_workspace_wins_and_shutdown_cleanup_fails_closed() {
    let explicit = Path::new("/explicit/workspace");
    assert_eq!(
        resolve_workspace_root(Some(explicit), &identity()).expect("explicit workspace"),
        explicit
    );
    assert_eq!(
        shutdown_cleanup_result(BuildExecutorCleanupOutcome::Confirmed),
        Ok(())
    );
    assert_eq!(
        shutdown_cleanup_result(BuildExecutorCleanupOutcome::Unconfirmed),
        Err(BuildExecutionError::ExecutorShutdownCleanupUnconfirmed)
    );
}

#[test]
fn provenance_and_timeout_fail_before_acceptance() {
    let identity = identity();
    let mut request = start_request();
    assert_eq!(
        validate_start_provenance_and_timeout(
            &identity,
            &BuildExecutorAdmission::AnyOperation,
            &request,
        ),
        Ok(())
    );

    let expected_operation = OperationId::try_new("op_expected").expect("operation");
    assert!(matches!(
        validate_start_provenance_and_timeout(
            &identity,
            &BuildExecutorAdmission::ExactOperation(expected_operation.clone()),
            &request,
        ),
        Err(BuildExecutorStartDomainError::OperationIdentityMismatch {
            expected,
            actual,
        }) if expected == expected_operation && actual == request.operation_id
    ));
    assert_eq!(
        validate_start_provenance_and_timeout(
            &identity,
            &BuildExecutorAdmission::ExactOperation(request.operation_id.clone()),
            &request,
        ),
        Ok(())
    );

    request.assignment = BuildExecutorAssignment::External {
        pool_id: BuildPoolId::try_new("pool_other").expect("pool"),
        executor_id: identity.executor_id.clone(),
        image_seed: MachineId::try_new("machine_seed").expect("machine"),
    };
    assert!(matches!(
        validate_start_provenance_and_timeout(
            &identity,
            &BuildExecutorAdmission::AnyOperation,
            &request,
        ),
        Err(BuildExecutorStartDomainError::ExecutorIdentityMismatch { .. })
    ));

    request = start_request();
    request.timeout_millis = 0;
    assert!(matches!(
        validate_start_provenance_and_timeout(
            &identity,
            &BuildExecutorAdmission::AnyOperation,
            &request,
        ),
        Err(BuildExecutorStartDomainError::InvalidTimeout { timeout_millis: 0 })
    ));
    request.timeout_millis =
        u64::try_from(BUILD_MAX_EXECUTION_TIMEOUT.as_millis()).expect("timeout") + 1;
    assert!(matches!(
        validate_start_provenance_and_timeout(
            &identity,
            &BuildExecutorAdmission::AnyOperation,
            &request,
        ),
        Err(BuildExecutorStartDomainError::InvalidTimeout { .. })
    ));
}

#[test]
fn readiness_is_closed_and_adapter_specific() {
    let mut request = start_request();
    let native = OciPlatform::try_new("linux", "amd64").expect("platform");
    let unavailable = BuildExecutorReadiness {
        native_platform: native.clone(),
        capability: BuildExecutorCapability::RuntimeUnavailable,
    };
    assert_eq!(
        validate_point_of_use_readiness(&request, &unavailable),
        Err(BuildExecutorStartDomainError::RuntimeUnavailable)
    );

    request.platform = OciPlatform::try_new("linux", "arm64").expect("platform");
    let docker_only = BuildExecutorReadiness {
        native_platform: native.clone(),
        capability: BuildExecutorCapability::DockerfileOnly,
    };
    assert!(matches!(
        validate_point_of_use_readiness(&request, &docker_only),
        Err(BuildExecutorStartDomainError::PlatformMismatch { .. })
    ));

    request.platform = native;
    request.adapter = BuildAdapter::Railpack {
        cache_scope: ployz_core::build::BuildCacheScope::try_new("scope").expect("cache scope"),
    };
    assert_eq!(
        validate_point_of_use_readiness(&request, &docker_only),
        Err(BuildExecutorStartDomainError::ToolchainUnavailable {
            adapter: BuildAdapterKind::Railpack,
        })
    );
}

#[tokio::test]
async fn one_active_registration_rechecks_after_the_preflight_race_window() {
    let mut state = accepting_state(None);
    state.ensure_accepting().expect("initial preflight");
    let (first, _first_rx, first_task) = active_build("op_build_first");
    state.register(first).expect("first registration");
    let (second, _second_rx, second_task) = active_build("op_build_second");
    assert!(matches!(
        state.register(second),
        Err(BuildExecutorStartDomainError::AlreadyRunning)
    ));
    first_task.abort();
    second_task.abort();
}

#[tokio::test]
async fn cancel_requires_the_exact_active_operation_and_full_assignment() {
    let (active, mut cancelled, task) = active_build("op_build_first");
    let assignment = active.assignment.clone();
    let mut state = accepting_state(Some(active));
    let mismatched_assignment = BuildExecutorAssignment::External {
        pool_id: identity().pool_id,
        executor_id: identity().executor_id,
        image_seed: MachineId::try_new("machine_other").expect("machine"),
    };
    assert!(matches!(
        cancel_active(
            &mut state,
            BuildExecutorCancelRequest {
                operation_id: OperationId::try_new("op_build_first").expect("operation"),
                assignment: mismatched_assignment,
            },
        ),
        Err(BuildExecutorCancelDomainError::AssignmentMismatch { .. })
    ));
    assert!(!*cancelled.borrow());

    let not_running = cancel_active(
        &mut state,
        BuildExecutorCancelRequest {
            operation_id: OperationId::try_new("op_build_other").expect("operation"),
            assignment: assignment.clone(),
        },
    )
    .expect("wrong operation is not active");
    assert_eq!(not_running.outcome, BuildExecutorCancelOutcome::NotRunning);
    assert!(!*cancelled.borrow());

    let requested = cancel_active(
        &mut state,
        BuildExecutorCancelRequest {
            operation_id: OperationId::try_new("op_build_first").expect("operation"),
            assignment,
        },
    )
    .expect("exact cancel");
    assert_eq!(requested.outcome, BuildExecutorCancelOutcome::Requested);
    cancelled.changed().await.expect("cancellation delivered");
    assert!(*cancelled.borrow());
    task.abort();
}

#[tokio::test]
async fn once_distinguishes_terminal_start_from_idle_timeout() {
    let expiry = future_expiry(60);
    let (terminal_tx, terminal_rx) = oneshot::channel();
    terminal_tx.send(()).expect("terminal sends");
    wait_for_once_terminal(terminal_rx, Duration::from_secs(1), expiry)
        .await
        .expect("terminal response stops once mode");

    let (_idle_tx, idle_rx) = oneshot::channel();
    assert_eq!(
        wait_for_once_terminal(idle_rx, Duration::from_millis(1), future_expiry(60)).await,
        Err(BuildExecutionError::ExecutorIdleTimedOut {
            wait_timeout: Duration::from_millis(1),
        })
    );
}

#[tokio::test]
async fn once_credential_expiry_wins_when_the_idle_budget_elapses_together() {
    let (_sender, receiver) = oneshot::channel();
    let expiry = future_expiry(60);
    let error =
        wait_for_once_terminal_with_lifetime(receiver, Duration::ZERO, Duration::ZERO, expiry)
            .await
            .expect_err("expiry wins the biased terminal selection");
    assert!(matches!(
        error,
        BuildExecutionError::ExecutorCredential { .. }
    ));
}

#[tokio::test]
async fn controlled_watch_rejects_expired_authority_before_waiting_for_a_signal() {
    let expired = ployz_core::nats_config::BuildExecutorCredentialExpiresAt::try_new(1)
        .expect("positive expiry");
    assert!(matches!(
        wait_for_controlled_stop(expired).await,
        Err(BuildExecutionError::ExecutorCredential { .. })
    ));
}

#[test]
fn every_startup_stage_rejects_authority_at_expiry() {
    let expires_at =
        ployz_core::nats_config::BuildExecutorCredentialExpiresAt::try_new(100).expect("expiry");
    let at_expiry = std::time::UNIX_EPOCH + Duration::from_secs(100);

    for stage in [
        ExecutorStartupStage::ReadinessProbe,
        ExecutorStartupStage::OrphanRecovery,
        ExecutorStartupStage::ServiceBinding,
        ExecutorStartupStage::AdmissionOpening,
    ] {
        let error = ensure_startup_authority_at(expires_at, stage, at_expiry)
            .expect_err("authority closes at its expiry");
        assert!(matches!(
            error,
            BuildExecutionError::ExecutorCredential { .. }
        ));
        assert!(error.to_string().contains(stage.description()));
    }
}

#[tokio::test]
async fn every_paused_startup_stage_is_interrupted_at_expiry() {
    let expires_at = future_expiry(60);

    for stage in [
        ExecutorStartupStage::ReadinessProbe,
        ExecutorStartupStage::OrphanRecovery,
        ExecutorStartupStage::ServiceBinding,
    ] {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stage_owned = StartupStageDrop(Arc::clone(&dropped));
        let error = await_startup_stage_with_lifetime::<(), _>(
            expires_at,
            stage,
            Duration::ZERO,
            async move {
                let _stage_owned = stage_owned;
                std::future::pending().await
            },
        )
        .await
        .expect_err("expiry interrupts a paused startup stage");
        assert!(matches!(
            error,
            BuildExecutionError::ExecutorCredential { .. }
        ));
        assert!(error.to_string().contains(stage.description()));
        assert!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            "interrupting the stage drops its partial resources"
        );
    }
}

#[test]
fn expired_authority_cannot_open_admission() {
    let mut state = RuntimeState::new_connected();
    let generation = state.admission_generation().expect("connected generation");
    let expires_at =
        ployz_core::nats_config::BuildExecutorCredentialExpiresAt::try_new(100).expect("expiry");
    let at_expiry = std::time::UNIX_EPOCH + Duration::from_secs(100);

    let error = state
        .open_admission_before_expiry_at(generation, expires_at, at_expiry)
        .expect_err("authority closes before admission");
    assert!(matches!(
        error,
        BuildExecutionError::ExecutorCredential { .. }
    ));
    assert_eq!(
        state.ensure_accepting(),
        Err(BuildExecutorStartDomainError::RuntimeStopped)
    );
    assert_eq!(state.admission, RuntimeAdmission::Closed);
}

#[tokio::test]
async fn terminal_admission_never_reopens_and_closing_does_not_cancel_active_build() {
    let (active, cancelled, task) = active_build("op_build_first");
    let mut state = accepting_state(Some(active));
    let generation = state.admission_generation().expect("connected generation");
    state.record_disconnected();
    assert!(!*cancelled.borrow());
    state.terminate_admission();
    assert!(!state.open_admission(generation));
    assert_eq!(state.admission, RuntimeAdmission::Terminal);
    task.abort();
}

#[test]
fn initial_ack_records_connected_generation_without_opening_admission() {
    let mut state = RuntimeState::new_closed();
    assert_eq!(state.connection, RuntimeConnection::AwaitingInitial);
    assert_eq!(state.admission_generation(), None);

    state.record_connected();

    assert_eq!(state.admission_generation(), Some(AdmissionGeneration(0)));
    assert_eq!(
        state.ensure_accepting(),
        Err(BuildExecutorStartDomainError::RuntimeStopped)
    );
}

#[tokio::test]
async fn paused_startup_and_reconnect_health_cannot_open_a_stale_generation() {
    let (active, cancelled, task) = active_build("op_build_first");
    let state = Arc::new(Mutex::new(RuntimeState::new_closed()));
    let startup_generation = {
        let mut state = state.lock().await;
        state.record_connected();
        state.admission_generation().expect("startup generation")
    };
    let (startup_paused, resume_startup, startup_open) =
        paused_open(Arc::clone(&state), startup_generation);
    startup_paused.await.expect("startup reached open barrier");
    state.lock().await.record_disconnected();
    resume_startup.send(()).expect("resume startup");
    assert!(!startup_open.await.expect("startup open task"));
    assert_eq!(
        state.lock().await.ensure_accepting(),
        Err(BuildExecutorStartDomainError::RuntimeStopped)
    );

    let reconnect_generation = {
        let mut state = state.lock().await;
        state.active = Some(active);
        state.record_connected();
        state
            .admission_generation()
            .expect("reconnected generation")
    };
    let (health_paused, resume_health, health_open) =
        paused_open(Arc::clone(&state), reconnect_generation);
    health_paused.await.expect("health reached open barrier");
    state.lock().await.record_disconnected();
    resume_health.send(()).expect("resume health");
    assert!(!health_open.await.expect("health open task"));
    assert!(
        !*cancelled.borrow(),
        "disconnect does not cancel active work"
    );

    let mut state = state.lock().await;
    state.record_connected();
    let current_generation = state.admission_generation().expect("current generation");
    assert!(state.open_admission(current_generation));
    assert_eq!(state.admission, RuntimeAdmission::Open);
    assert!(
        !*cancelled.borrow(),
        "reopening does not cancel active work"
    );
    task.abort();
}

#[test]
fn timeout_and_seed_probe_failures_keep_typed_external_provenance() {
    let request = start_request();
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let timeout = external_build_error(
        EngineError::TimedOut {
            log_summary: BuildLogSummary::new(7, 11),
        },
        BuildExecutorCleanupOutcome::Unconfirmed,
        acceptance.clone(),
    );
    assert_eq!(
        timeout,
        BuildExecutorStartDomainError::TimedOut {
            acceptance: Box::new(acceptance),
            message: failure("build exceeded its operation deadline"),
            cleanup: BuildExecutorCleanupOutcome::Unconfirmed,
            log_summary: BuildLogSummary::new(7, 11),
        }
    );

    let image_seed = request.assignment.image_seed().clone();
    assert_eq!(
        map_seed_probe(
            image_seed.clone(),
            Err(
                crate::deploy::image_push::ImagePushError::UnexpectedResponse {
                    message: "unreachable".to_owned(),
                }
            ),
        ),
        Err(BuildExecutorStartDomainError::ImageSeedUnavailable { image_seed })
    );
}

#[test]
fn successful_push_with_unconfirmed_cleanup_is_a_typed_platform_failure() {
    let request = start_request();
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let log_summary = BuildLogSummary::new(9, 17);
    let result = finish_external_build(
        Ok(successful_output(&request, log_summary)),
        BuildExecutorCleanupOutcome::Unconfirmed,
        acceptance.clone(),
    );

    assert_eq!(
        result,
        Err(BuildExecutorStartDomainError::PlatformFailed {
            acceptance: Box::new(acceptance),
            failure: BuildPlatformFailure::ExecutorUnavailable {
                message: failure("build workspace cleanup did not finish successfully"),
            },
            log_summary,
        })
    );
}

#[test]
fn successful_push_with_confirmed_cleanup_carries_positive_proof() {
    let request = start_request();
    let acceptance = BuildExecutorAcceptance::from_start_request(&request);
    let log_summary = BuildLogSummary::new(9, 17);
    let success = finish_external_build(
        Ok(successful_output(&request, log_summary)),
        BuildExecutorCleanupOutcome::Confirmed,
        acceptance.clone(),
    )
    .expect("confirmed cleanup permits success");

    assert_eq!(success.acceptance, acceptance);
    assert_eq!(
        success.cleanup,
        BuildExecutorSuccessCleanupEvidence::confirmed()
    );
    assert_eq!(success.log_summary, log_summary);
}

fn successful_output(
    request: &BuildExecutorStartRequest,
    log_summary: BuildLogSummary,
) -> ExternalBuildOutput {
    let digest = OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
    ExternalBuildOutput {
        acceptance: BuildExecutorAcceptance::from_start_request(request),
        image: PlatformImage {
            seed: request.assignment.image_seed().clone(),
            manifest_digest: digest.clone(),
            image_id: digest.clone(),
            availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
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

fn identity() -> BuildExecutorIdentity {
    BuildExecutorIdentity {
        pool_id: BuildPoolId::try_new("pool_ci").expect("pool"),
        executor_id: BuildExecutorId::try_new("executor_1").expect("executor"),
    }
}

fn start_request() -> BuildExecutorStartRequest {
    let identity = identity();
    BuildExecutorStartRequest {
        operation_id: OperationId::try_new("op_build_external").expect("operation"),
        assignment: BuildExecutorAssignment::External {
            pool_id: identity.pool_id,
            executor_id: identity.executor_id,
            image_seed: MachineId::try_new("machine_seed").expect("machine"),
        },
        source: BuildSource::Git {
            git: GitSource::try_new(
                "https://git.example/repo.git",
                SHA,
                "builder",
                "secret",
                None::<String>,
            )
            .expect("source"),
        },
        adapter: BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new("Dockerfile").expect("path"),
            target: None,
        },
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        timeout_millis: 1_000,
    }
}

fn active_build(
    operation_id: &str,
) -> (
    ActiveBuild,
    watch::Receiver<bool>,
    tokio::task::JoinHandle<()>,
) {
    let request = start_request();
    let (cancel, cancel_rx) = watch::channel(false);
    let task = tokio::spawn(std::future::pending());
    (
        ActiveBuild {
            operation_id: OperationId::try_new(operation_id).expect("operation"),
            assignment: request.assignment,
            platform: request.platform,
            cancel,
            supervisor: task.abort_handle(),
        },
        cancel_rx,
        task,
    )
}

fn accepting_state(active: Option<ActiveBuild>) -> RuntimeState {
    let mut state = RuntimeState::new_connected();
    let generation = state.admission_generation().expect("connected generation");
    assert!(state.open_admission(generation));
    state.active = active;
    state
}

fn paused_open(
    state: Arc<Mutex<RuntimeState>>,
    generation: AdmissionGeneration,
) -> (
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<bool>,
) {
    let (paused_tx, paused_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        paused_tx.send(()).expect("report paused open");
        resume_rx.await.expect("resume paused open");
        state.lock().await.open_admission(generation)
    });
    (paused_rx, resume_tx, task)
}

fn future_expiry(seconds: u64) -> ployz_core::nats_config::BuildExecutorCredentialExpiresAt {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    ployz_core::nats_config::BuildExecutorCredentialExpiresAt::try_new(now + seconds)
        .expect("future expiry")
}

struct StartupStageDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for StartupStageDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
