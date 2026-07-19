//! External Build Executor command runtime.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_build_executor::{
    BuildExecutionError as EngineError, BuildExecutionRequest, BuildExecutionResult,
    BuildLogDestination, BuildLogProgress, DockerBuildExecutor,
};
use ployz_core::build::{
    BUILD_FORCE_CLEANUP_TIMEOUT, BUILD_MAX_EXECUTION_TIMEOUT, BUILD_START_ENDPOINT_TIMEOUT,
    BUILD_TASK_DRAIN_TIMEOUT, BuildAdapterKind, BuildExecutorAcceptance, BuildExecutorAssignment,
    BuildExecutorCancelDomainError, BuildExecutorCancelOk, BuildExecutorCancelOutcome,
    BuildExecutorCancelRequest, BuildExecutorCancelResponse, BuildExecutorCapability,
    BuildExecutorCleanupOutcome, BuildExecutorIdentity, BuildExecutorReadiness,
    BuildExecutorReadinessAnswer, BuildExecutorReadinessRequest, BuildExecutorStartDomainError,
    BuildExecutorStartOk, BuildExecutorStartRequest, BuildExecutorStartResponse,
    BuildExecutorSuccessCleanupEvidence, BuildLogSummary,
};
use ployz_core::deploy::PlatformImage;
use ployz_core::operation::{BuildPlatformFailure, FailureMessage};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::connect_authenticated;
use ployz_nats::service_runtime::{
    EndpointExecutionPolicy, NatsServiceRequest, NatsServiceResponse, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata,
    ServiceMetadataEntry, ServiceVersion,
};
use ployz_nats::subjects::{
    BUILD_EXECUTOR_SERVICE_NAME, BuildExecutorServiceEndpoint, build_executor_log,
    build_executor_service,
};
use tokio::sync::{Mutex, Notify, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::{Instant, timeout};

use super::command::{BuildExecutorCommand, BuildExecutorRunMode};
use super::runtime::BuildExecutionError;
use crate::deploy::image_push::{probe_image_seed, push_validated_oci_layout};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    PloyzctlExecutionOutput, nats_connect_config, with_cluster_context_from_disk,
};

const SHUTDOWN_TIMEOUT: Duration = BUILD_TASK_DRAIN_TIMEOUT
    .saturating_add(BUILD_FORCE_CLEANUP_TIMEOUT)
    .saturating_add(Duration::from_secs(5));

pub(crate) async fn run(
    command: BuildExecutorCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config.clone())?;
    let mut connect = nats_connect_config(&config)?;
    connect.principal = executor_principal(&BuildExecutorIdentity {
        pool_id: command.pool_id.clone(),
        executor_id: command.executor_id.clone(),
    });
    let client = connect_authenticated(&connect, config.nats_connect_timeout())
        .await
        .map_err(crate::execution_support::ExecutionSupportError::NatsConnect)?;
    let workspace_root = command
        .workspace_root
        .clone()
        .map_or_else(
            || {
                std::env::current_dir()
                    .map(|directory| directory.join(".ployz").join("build-executor"))
            },
            Ok,
        )
        .map_err(|error| BuildExecutionError::ExecutorRuntime {
            message: format!("failed to resolve Build Executor workspace: {error}"),
        })?;
    let identity = BuildExecutorIdentity {
        pool_id: command.pool_id,
        executor_id: command.executor_id,
    };
    let startup_readiness = probe_readiness().await?;
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = ExternalBuildRuntime::new(identity.clone(), client.clone(), workspace_root);
    if startup_readiness.capability != BuildExecutorCapability::RuntimeUnavailable {
        runtime.recover_orphans().await?;
    }
    let service = start_executor_service(client, identity, runtime.clone(), terminal_tx).await?;

    let wait_result =
        match command.mode {
            BuildExecutorRunMode::Once { wait_timeout } => {
                wait_for_once_terminal(&mut terminal_rx, wait_timeout).await
            }
            BuildExecutorRunMode::Watch => tokio::signal::ctrl_c().await.map_err(|error| {
                BuildExecutionError::ExecutorRuntime {
                    message: format!("failed to listen for Ctrl-C: {error}"),
                }
            }),
        };

    runtime.shutdown().await;
    service.shutdown().await.map_err(|error| {
        PloyzctlExecutionError::from(BuildExecutionError::ExecutorRuntime {
            message: error.to_string(),
        })
    })?;
    wait_result?;
    Ok(PloyzctlExecutionOutput::stdout(
        "Build Executor stopped.\n".to_owned(),
    ))
}

async fn wait_for_once_terminal(
    terminal: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    wait_timeout: Duration,
) -> Result<(), BuildExecutionError> {
    match timeout(wait_timeout, terminal.recv()).await {
        Ok(Some(())) => Ok(()),
        Ok(None) => Err(BuildExecutionError::ExecutorRuntime {
            message: "Build Executor terminal channel closed".to_owned(),
        }),
        Err(_) => Err(BuildExecutionError::ExecutorIdleTimedOut { wait_timeout }),
    }
}

fn executor_principal(identity: &BuildExecutorIdentity) -> NatsPrincipal {
    NatsPrincipal::BuildExecutor {
        pool_id: identity.pool_id.clone(),
        executor_id: identity.executor_id.clone(),
    }
}

async fn probe_readiness() -> Result<BuildExecutorReadiness, BuildExecutionError> {
    let native_platform = ployz_build_executor::native_oci_platform().map_err(|message| {
        BuildExecutionError::ExecutorRuntime {
            message: format!("failed to determine native build platform: {message}"),
        }
    })?;
    let capability = match ployz_build_executor::probe_docker_runtime().await {
        Ok(actual) if actual == native_platform => {
            if ployz_build_executor::railpack_helper_is_ready(&native_platform).await {
                BuildExecutorCapability::DockerfileAndRailpack
            } else {
                BuildExecutorCapability::DockerfileOnly
            }
        }
        Ok(_) | Err(_) => BuildExecutorCapability::RuntimeUnavailable,
    };
    Ok(BuildExecutorReadiness {
        native_platform,
        capability,
    })
}

fn validate_point_of_use_readiness(
    request: &BuildExecutorStartRequest,
    readiness: &BuildExecutorReadiness,
) -> Result<(), BuildExecutorStartDomainError> {
    if request.platform != readiness.native_platform {
        return Err(BuildExecutorStartDomainError::PlatformMismatch {
            expected: request.platform.clone(),
            actual: readiness.native_platform.clone(),
        });
    }
    if readiness.capability == BuildExecutorCapability::RuntimeUnavailable {
        return Err(BuildExecutorStartDomainError::RuntimeUnavailable);
    }
    if !readiness.capability.supports(&request.adapter) {
        return Err(BuildExecutorStartDomainError::ToolchainUnavailable {
            adapter: BuildAdapterKind::from(&request.adapter),
        });
    }
    Ok(())
}

fn validate_start_provenance_and_timeout(
    identity: &BuildExecutorIdentity,
    request: &BuildExecutorStartRequest,
) -> Result<(), BuildExecutorStartDomainError> {
    match &request.assignment {
        BuildExecutorAssignment::External {
            pool_id,
            executor_id,
            image_seed: _,
        } if *pool_id == identity.pool_id && *executor_id == identity.executor_id => {}
        actual @ (BuildExecutorAssignment::Cluster { .. }
        | BuildExecutorAssignment::External { .. }) => {
            return Err(BuildExecutorStartDomainError::ExecutorIdentityMismatch {
                expected: identity.clone(),
                actual: Box::new(actual.clone()),
            });
        }
    }
    let requested_timeout = Duration::from_millis(request.timeout_millis);
    if requested_timeout.is_zero() || requested_timeout > BUILD_MAX_EXECUTION_TIMEOUT {
        return Err(BuildExecutorStartDomainError::InvalidTimeout {
            timeout_millis: request.timeout_millis,
        });
    }
    Ok(())
}

async fn start_executor_service(
    client: async_nats::Client,
    identity: BuildExecutorIdentity,
    runtime: ExternalBuildRuntime,
    terminal: tokio::sync::mpsc::UnboundedSender<()>,
) -> Result<RunningNatsService, BuildExecutionError> {
    let endpoints = executor_endpoints(&identity);
    let [readiness_endpoint, start_endpoint, cancel_endpoint] = &endpoints;
    let spec = NatsServiceSpec::new(
        format!(
            "{BUILD_EXECUTOR_SERVICE_NAME}.{}.{}",
            identity.pool_id.as_str(),
            identity.executor_id.as_str()
        ),
        BUILD_EXECUTOR_SERVICE_NAME,
        ServiceVersion::new(1, 0, 0),
        "External Dockerfile and Railpack build executor",
        ServiceMetadata::from_entries(vec![
            ServiceMetadataEntry::new("pool_id", identity.pool_id.as_str()),
            ServiceMetadataEntry::new("executor_id", identity.executor_id.as_str()),
        ]),
        endpoints.to_vec(),
    );
    let mut service = start_nats_service(client, &spec)
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    let readiness_identity = identity.clone();
    service
        .bind_endpoint(readiness_endpoint, move |request| {
            let identity = readiness_identity.clone();
            async move {
                match decode_json_request::<BuildExecutorReadinessRequest>(&request) {
                    Ok(BuildExecutorReadinessRequest {}) => match probe_readiness().await {
                        Ok(readiness) => {
                            NatsServiceResponse::json_ok(&BuildExecutorReadinessAnswer {
                                identity,
                                readiness,
                            })
                        }
                        Err(error) => NatsServiceResponse::transport_error(
                            ployz_nats::service_runtime::NatsServiceError::internal(
                                error.to_string(),
                            ),
                        ),
                    },
                    Err(response) => response,
                }
            }
        })
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    let start_runtime = runtime.clone();
    service
        .bind_endpoint_with_policy(
            start_endpoint,
            EndpointExecutionPolicy::new(NonZeroUsize::MIN, BUILD_START_ENDPOINT_TIMEOUT),
            move |request| {
                let runtime = start_runtime.clone();
                let terminal = terminal.clone();
                async move {
                    let response = handle_start(runtime, request).await;
                    notify_terminal_start(&response, &terminal);
                    response
                }
            },
        )
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    let cancel_runtime = runtime;
    service
        .bind_endpoint(cancel_endpoint, move |request| {
            let runtime = cancel_runtime.clone();
            async move { handle_cancel(runtime, request).await }
        })
        .await
        .map_err(|error| executor_error(error.to_string()))?;
    Ok(service)
}

fn notify_terminal_start(
    response: &NatsServiceResponse,
    terminal: &tokio::sync::mpsc::UnboundedSender<()>,
) {
    if matches!(
        response,
        NatsServiceResponse::Ok { .. } | NatsServiceResponse::DomainError { .. }
    ) {
        let _ = terminal.send(());
    }
}

fn executor_endpoints(identity: &BuildExecutorIdentity) -> [NatsServiceEndpointSpec; 3] {
    [
        executor_endpoint(identity, BuildExecutorServiceEndpoint::ReadinessGet),
        executor_endpoint(identity, BuildExecutorServiceEndpoint::BuildStart),
        executor_endpoint(identity, BuildExecutorServiceEndpoint::BuildCancel),
    ]
}

fn executor_endpoint(
    identity: &BuildExecutorIdentity,
    endpoint: BuildExecutorServiceEndpoint,
) -> NatsServiceEndpointSpec {
    let (name, execution) = match endpoint {
        BuildExecutorServiceEndpoint::ReadinessGet => ("readiness.get", EndpointExecution::Query),
        BuildExecutorServiceEndpoint::BuildStart => ("build.start", EndpointExecution::MachineRpc),
        BuildExecutorServiceEndpoint::BuildCancel => {
            ("build.cancel", EndpointExecution::MachineRpc)
        }
    };
    NatsServiceEndpointSpec::new(
        name,
        build_executor_service(&identity.pool_id, &identity.executor_id, endpoint),
        execution,
    )
}

async fn handle_start(
    runtime: ExternalBuildRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<BuildExecutorStartRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match runtime.start(request).await {
        Ok(ok) => NatsServiceResponse::json_ok(&BuildExecutorStartResponse::Ok(Box::new(ok))),
        Err(error) => {
            NatsServiceResponse::json_domain_error(&BuildExecutorStartResponse::DomainError {
                error,
            })
        }
    }
}

async fn handle_cancel(
    runtime: ExternalBuildRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<BuildExecutorCancelRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match runtime.cancel(request).await {
        Ok(ok) => NatsServiceResponse::json_ok(&BuildExecutorCancelResponse::Ok(ok)),
        Err(error) => {
            NatsServiceResponse::json_domain_error(&BuildExecutorCancelResponse::DomainError {
                error,
            })
        }
    }
}

#[derive(Clone)]
struct ExternalBuildRuntime {
    identity: BuildExecutorIdentity,
    client: async_nats::Client,
    executor: Arc<DockerBuildExecutor>,
    state: Arc<Mutex<RuntimeState>>,
    changed: Arc<Notify>,
}

struct RuntimeState {
    accepting: bool,
    active: Option<ActiveBuild>,
}

impl RuntimeState {
    fn ensure_accepting(&self) -> Result<(), BuildExecutorStartDomainError> {
        if !self.accepting {
            return Err(BuildExecutorStartDomainError::RuntimeStopped);
        }
        if self.active.is_some() {
            return Err(BuildExecutorStartDomainError::AlreadyRunning);
        }
        Ok(())
    }

    fn register(&mut self, active: ActiveBuild) -> Result<(), BuildExecutorStartDomainError> {
        self.ensure_accepting()?;
        self.active = Some(active);
        Ok(())
    }
}

struct ActiveBuild {
    operation_id: ployz_core::ids::OperationId,
    assignment: BuildExecutorAssignment,
    platform: ployz_core::image::OciPlatform,
    cancel: watch::Sender<bool>,
    supervisor: AbortHandle,
}

impl ExternalBuildRuntime {
    fn new(
        identity: BuildExecutorIdentity,
        client: async_nats::Client,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            identity,
            client,
            executor: Arc::new(DockerBuildExecutor::new(workspace_root)),
            state: Arc::new(Mutex::new(RuntimeState {
                accepting: true,
                active: None,
            })),
            changed: Arc::new(Notify::new()),
        }
    }

    async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        self.executor
            .recover_orphans()
            .await
            .map_err(|error| executor_error(error.to_string()))
    }

    async fn start(
        &self,
        request: BuildExecutorStartRequest,
    ) -> Result<BuildExecutorStartOk, BuildExecutorStartDomainError> {
        validate_start_provenance_and_timeout(&self.identity, &request)?;
        {
            let state = self.state.lock().await;
            state.ensure_accepting()?;
        }
        let readiness = probe_readiness()
            .await
            .map_err(|_| BuildExecutorStartDomainError::RuntimeUnavailable)?;
        validate_point_of_use_readiness(&request, &readiness)?;
        let image_seed = request.assignment.image_seed().clone();
        map_seed_probe(
            image_seed.clone(),
            probe_image_seed(
                &self.client,
                &image_seed,
                Duration::from_millis(request.timeout_millis),
            )
            .await,
        )?;

        let acceptance = BuildExecutorAcceptance::from_start_request(&request);
        let fallback_acceptance = acceptance.clone();
        let operation_id = request.operation_id.clone();
        let assignment = request.assignment.clone();
        let platform = request.platform.clone();
        let (cancel, cancel_rx) = watch::channel(false);
        let (result_tx, result_rx) = oneshot::channel();
        let (launch, launch_rx) = oneshot::channel();
        let runtime = self.clone();
        let task_operation_id = operation_id.clone();
        let supervisor_cancel = cancel.clone();
        let supervisor = tokio::spawn(async move {
            if launch_rx.await.is_err() {
                return;
            }
            let result = runtime
                .run_build(request, acceptance, supervisor_cancel, cancel_rx)
                .await;
            runtime.remove_active(&task_operation_id).await;
            let _ = result_tx.send(result);
        });
        {
            let mut state = self.state.lock().await;
            if let Err(error) = state.register(ActiveBuild {
                operation_id: operation_id.clone(),
                assignment,
                platform,
                cancel,
                supervisor: supervisor.abort_handle(),
            }) {
                supervisor.abort();
                return Err(error);
            }
        }
        if launch.send(()).is_err() {
            supervisor.abort();
            self.remove_active(&operation_id).await;
        }
        result_rx.await.unwrap_or_else(|_| {
            Err(BuildExecutorStartDomainError::PlatformFailed {
                acceptance: Box::new(fallback_acceptance),
                failure: BuildPlatformFailure::ExecutorUnavailable {
                    message: failure("Build Executor stopped before returning a result"),
                },
                log_summary: BuildLogSummary::none(),
            })
        })
    }

    async fn run_build(
        &self,
        request: BuildExecutorStartRequest,
        acceptance: BuildExecutorAcceptance,
        cancel: watch::Sender<bool>,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> Result<BuildExecutorStartOk, BuildExecutorStartDomainError> {
        let timeout_duration = Duration::from_millis(request.timeout_millis);
        let deadline = Instant::now() + timeout_duration;
        let log_progress = BuildLogProgress::default();
        let log_destination = BuildLogDestination::new(
            self.client.clone(),
            build_executor_log(
                &self.identity.pool_id,
                &self.identity.executor_id,
                &request.operation_id,
            ),
            request.assignment.clone(),
        );
        let result = {
            let execution = self.executor.execute(
                BuildExecutionRequest::new(
                    &request.operation_id,
                    &request.source,
                    &request.adapter,
                    &request.platform,
                    &log_destination,
                ),
                cancel_rx.clone(),
                log_progress.clone(),
                deadline,
            );
            tokio::pin!(execution);
            let completion = tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => BuildCompletion::TimedOut,
                changed = cancel_rx.changed() => {
                    let _ = changed;
                    BuildCompletion::Cancelled
                }
                result = &mut execution => BuildCompletion::Finished(Box::new(result)),
            };
            if !matches!(completion, BuildCompletion::Finished(_)) {
                let _ = cancel.send(true);
                let _ = timeout(BUILD_TASK_DRAIN_TIMEOUT, &mut execution).await;
            }
            match completion {
                BuildCompletion::Finished(result) => *result,
                BuildCompletion::Cancelled => Err(EngineError::Cancelled {
                    log_summary: progress_summary(&log_progress),
                }),
                BuildCompletion::TimedOut => Err(EngineError::TimedOut {
                    log_summary: progress_summary(&log_progress),
                }),
            }
        };
        let pushed = match result {
            Ok(result) => {
                self.push_result(&request, &mut cancel_rx, deadline, result)
                    .await
            }
            Err(error) => Err(error),
        };
        let cleanup = match timeout(
            BUILD_FORCE_CLEANUP_TIMEOUT,
            self.executor
                .force_cleanup(&request.operation_id, &request.platform),
        )
        .await
        {
            Ok(Ok(())) => BuildExecutorCleanupOutcome::Confirmed,
            Ok(Err(_)) | Err(_) => BuildExecutorCleanupOutcome::Unconfirmed,
        };
        finish_external_build(pushed, cleanup, acceptance)
    }

    async fn push_result(
        &self,
        request: &BuildExecutorStartRequest,
        cancel_rx: &mut watch::Receiver<bool>,
        deadline: Instant,
        result: BuildExecutionResult,
    ) -> Result<ExternalBuildOutput, EngineError> {
        let log_summary = result.log_summary;
        let push = push_validated_oci_layout(
            &self.client,
            &result.layout,
            &request.platform,
            request.assignment.image_seed(),
        );
        tokio::pin!(push);
        let receipt = tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => {
                return Err(EngineError::TimedOut { log_summary });
            }
            changed = cancel_rx.changed() => {
                let _ = changed;
                return Err(EngineError::Cancelled { log_summary });
            }
            receipt = &mut push => receipt.map_err(|error| EngineError::Platform {
                failure: BuildPlatformFailure::ImagePushFailed {
                    message: failure(error.to_string()),
                },
                log_summary,
            })?,
        };
        let Some((_, image)) = receipt
            .receipt()
            .platforms()
            .find(|(platform, _)| *platform == &request.platform)
        else {
            return Err(EngineError::Platform {
                failure: BuildPlatformFailure::ImagePushFailed {
                    message: failure("image seed receipt omitted the requested platform"),
                },
                log_summary,
            });
        };
        Ok(ExternalBuildOutput {
            acceptance: BuildExecutorAcceptance::from_start_request(request),
            image: image.clone(),
            verified_commit: result.verified_commit,
            toolchain: result.toolchain,
            log_summary,
        })
    }

    async fn cancel(
        &self,
        request: BuildExecutorCancelRequest,
    ) -> Result<BuildExecutorCancelOk, BuildExecutorCancelDomainError> {
        let mut state = self.state.lock().await;
        cancel_active(&mut state, request)
    }

    async fn shutdown(&self) {
        let cleanup_target = {
            let mut state = self.state.lock().await;
            state.accepting = false;
            state.active.as_ref().map(|active| {
                let _ = active.cancel.send(true);
                (
                    active.operation_id.clone(),
                    active.platform.clone(),
                    active.supervisor.clone(),
                )
            })
        };
        if timeout(SHUTDOWN_TIMEOUT, self.wait_until_idle())
            .await
            .is_ok()
        {
            return;
        }
        let Some((operation_id, platform, supervisor)) = cleanup_target else {
            return;
        };
        supervisor.abort();
        let _ = timeout(
            BUILD_FORCE_CLEANUP_TIMEOUT,
            self.executor.force_cleanup(&operation_id, &platform),
        )
        .await;
        self.remove_active(&operation_id).await;
    }

    async fn wait_until_idle(&self) {
        loop {
            let notified = self.changed.notified();
            if self.state.lock().await.active.is_none() {
                return;
            }
            notified.await;
        }
    }

    async fn remove_active(&self, operation_id: &ployz_core::ids::OperationId) {
        let mut state = self.state.lock().await;
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.operation_id == *operation_id)
        {
            state.active = None;
            self.changed.notify_waiters();
        }
    }
}

struct ExternalBuildOutput {
    acceptance: BuildExecutorAcceptance,
    image: PlatformImage,
    verified_commit: ployz_core::build::VerifiedGitCommit,
    toolchain: ployz_core::operation::BuildToolchainEvidence,
    log_summary: BuildLogSummary,
}

impl ExternalBuildOutput {
    fn into_start_ok(self) -> BuildExecutorStartOk {
        BuildExecutorStartOk {
            acceptance: self.acceptance,
            cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
            image: self.image,
            verified_commit: self.verified_commit,
            toolchain: self.toolchain,
            log_summary: self.log_summary,
        }
    }
}

fn finish_external_build(
    pushed: Result<ExternalBuildOutput, EngineError>,
    cleanup: BuildExecutorCleanupOutcome,
    acceptance: BuildExecutorAcceptance,
) -> Result<BuildExecutorStartOk, BuildExecutorStartDomainError> {
    match (pushed, cleanup) {
        (Ok(output), BuildExecutorCleanupOutcome::Confirmed) => Ok(output.into_start_ok()),
        (Ok(output), BuildExecutorCleanupOutcome::Unconfirmed) => {
            Err(BuildExecutorStartDomainError::PlatformFailed {
                acceptance: Box::new(output.acceptance),
                failure: BuildPlatformFailure::ExecutorUnavailable {
                    message: failure("build workspace cleanup did not finish successfully"),
                },
                log_summary: output.log_summary,
            })
        }
        (Err(error), cleanup) => Err(external_build_error(error, cleanup, acceptance)),
    }
}

fn map_seed_probe(
    image_seed: ployz_core::ids::MachineId,
    result: Result<(), crate::deploy::image_push::ImagePushError>,
) -> Result<(), BuildExecutorStartDomainError> {
    result.map_err(|_| BuildExecutorStartDomainError::ImageSeedUnavailable { image_seed })
}

fn cancel_active(
    state: &mut RuntimeState,
    request: BuildExecutorCancelRequest,
) -> Result<BuildExecutorCancelOk, BuildExecutorCancelDomainError> {
    let Some(active) = state.active.as_mut() else {
        return Ok(BuildExecutorCancelOk {
            assignment: request.assignment,
            outcome: BuildExecutorCancelOutcome::NotRunning,
        });
    };
    if request.assignment != active.assignment {
        return Err(BuildExecutorCancelDomainError::AssignmentMismatch {
            expected: Box::new(active.assignment.clone()),
            actual: request.assignment,
        });
    }
    if request.operation_id != active.operation_id {
        return Ok(BuildExecutorCancelOk {
            assignment: request.assignment,
            outcome: BuildExecutorCancelOutcome::NotRunning,
        });
    }
    active
        .cancel
        .send(true)
        .map_err(|error| BuildExecutorCancelDomainError::CancelFailed {
            message: failure(error.to_string()),
        })?;
    Ok(BuildExecutorCancelOk {
        assignment: request.assignment,
        outcome: BuildExecutorCancelOutcome::Requested,
    })
}

enum BuildCompletion {
    Finished(Box<Result<BuildExecutionResult, EngineError>>),
    Cancelled,
    TimedOut,
}

fn external_build_error(
    error: EngineError,
    cleanup: BuildExecutorCleanupOutcome,
    acceptance: BuildExecutorAcceptance,
) -> BuildExecutorStartDomainError {
    let log_summary = error.log_summary();
    match error {
        EngineError::Cancelled { .. } => BuildExecutorStartDomainError::Cancelled {
            acceptance: Box::new(acceptance),
            cleanup,
            log_summary,
        },
        EngineError::TimedOut { .. } => BuildExecutorStartDomainError::TimedOut {
            acceptance: Box::new(acceptance),
            message: failure("build exceeded its operation deadline"),
            cleanup,
            log_summary,
        },
        EngineError::Platform { failure, .. } => BuildExecutorStartDomainError::PlatformFailed {
            acceptance: Box::new(acceptance),
            failure,
            log_summary,
        },
        EngineError::Infrastructure {
            action, message, ..
        } => BuildExecutorStartDomainError::PlatformFailed {
            acceptance: Box::new(acceptance),
            failure: BuildPlatformFailure::ExecutorUnavailable {
                message: failure(format!("{action}: {message}")),
            },
            log_summary,
        },
    }
}

fn progress_summary(progress: &BuildLogProgress) -> BuildLogSummary {
    let (final_sequence, omitted_bytes) = progress.summary();
    BuildLogSummary::new(final_sequence, omitted_bytes)
}

fn failure(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("Build Executor failures are non-empty")
}

fn executor_error(message: impl Into<String>) -> BuildExecutionError {
    BuildExecutionError::ExecutorRuntime {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{BuildAdapter, BuildContextPath, GitSource, VerifiedGitCommit};
    use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
    use ployz_core::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
    use ployz_core::image::{OciDigest, OciPlatform};
    use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildToolchainEvidence};

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

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
    fn provenance_and_timeout_fail_before_acceptance() {
        let identity = identity();
        let mut request = start_request();
        assert_eq!(
            validate_start_provenance_and_timeout(&identity, &request),
            Ok(())
        );

        request.assignment = BuildExecutorAssignment::External {
            pool_id: BuildPoolId::try_new("pool_other").expect("pool"),
            executor_id: identity.executor_id.clone(),
            image_seed: MachineId::try_new("machine_seed").expect("machine"),
        };
        assert!(matches!(
            validate_start_provenance_and_timeout(&identity, &request),
            Err(BuildExecutorStartDomainError::ExecutorIdentityMismatch { .. })
        ));

        request = start_request();
        request.timeout_millis = 0;
        assert!(matches!(
            validate_start_provenance_and_timeout(&identity, &request),
            Err(BuildExecutorStartDomainError::InvalidTimeout { timeout_millis: 0 })
        ));
        request.timeout_millis =
            u64::try_from(BUILD_MAX_EXECUTION_TIMEOUT.as_millis()).expect("timeout") + 1;
        assert!(matches!(
            validate_start_provenance_and_timeout(&identity, &request),
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
        let mut state = RuntimeState {
            accepting: true,
            active: None,
        };
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
        let mut state = RuntimeState {
            accepting: true,
            active: Some(active),
        };
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
    async fn once_distinguishes_any_terminal_start_response_from_idle_timeout() {
        let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::unbounded_channel();
        let rejection =
            NatsServiceResponse::json_domain_error(&BuildExecutorStartResponse::DomainError {
                error: BuildExecutorStartDomainError::AlreadyRunning,
            });
        notify_terminal_start(&rejection, &terminal_tx);
        wait_for_once_terminal(&mut terminal_rx, Duration::from_secs(1))
            .await
            .expect("preacceptance rejection is terminal");

        let (_idle_tx, mut idle_rx) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(
            wait_for_once_terminal(&mut idle_rx, Duration::from_millis(1)).await,
            Err(BuildExecutionError::ExecutorIdleTimedOut {
                wait_timeout: Duration::from_millis(1),
            })
        );
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
            verified_commit: VerifiedGitCommit::from_source(&request.source),
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
            source: GitSource::try_new(
                "https://git.example/repo.git",
                SHA,
                "builder",
                "secret",
                None::<String>,
            )
            .expect("source"),
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
}
