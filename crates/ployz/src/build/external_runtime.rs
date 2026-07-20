//! External Build Executor command runtime.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_build_executor::{
    BuildExecutionError as EngineError, BuildExecutionRequest, BuildExecutionResult,
    BuildLogDestination, BuildLogProgress, DockerBuildExecutor,
};
use ployz_core::build::{
    BUILD_FORCE_CLEANUP_TIMEOUT, BUILD_MAX_EXECUTION_TIMEOUT, BUILD_TASK_DRAIN_TIMEOUT,
    BuildAdapterKind, BuildExecutorAcceptance, BuildExecutorAssignment,
    BuildExecutorCancelDomainError, BuildExecutorCancelOk, BuildExecutorCancelOutcome,
    BuildExecutorCancelRequest, BuildExecutorCapability, BuildExecutorCleanupOutcome,
    BuildExecutorIdentity, BuildExecutorReadiness, BuildExecutorStartDomainError,
    BuildExecutorStartOk, BuildExecutorStartRequest, BuildExecutorSuccessCleanupEvidence,
    BuildLogSummary,
};
use ployz_core::deploy::PlatformImage;
use ployz_core::operation::{BuildPlatformFailure, FailureMessage};
use ployz_nats::service_runtime::RunningNatsService;
use ployz_nats::subjects::build_executor_log;
use tokio::sync::{Mutex, Notify, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::{Instant, timeout};

use super::command::{BuildExecutorAdmission, BuildExecutorCommand, BuildExecutorRunMode};
use super::external_service::{CompletionMode, start_executor_service};
use super::runtime::BuildExecutionError;
use super::watch_lifecycle::{
    WatchConnect, connect_once, connect_watch, credential_lifetime, executor_connect_config,
    executor_principal, resolve_workspace_root,
};
use crate::deploy::image_push::{probe_image_seed, push_validated_oci_layout};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    PloyzctlExecutionOutput, nats_connect_config, with_cluster_context_from_disk,
};

const SHUTDOWN_TIMEOUT: Duration = BUILD_TASK_DRAIN_TIMEOUT
    .saturating_add(BUILD_FORCE_CLEANUP_TIMEOUT)
    .saturating_add(Duration::from_secs(5));

struct ExecutorStartup {
    workspace: WorkspaceStartup,
    admission: BuildExecutorAdmission,
}

struct ConnectedExecutor {
    client: async_nats::Client,
    runtime: ExternalBuildRuntime,
    service: RunningNatsService,
    admission_opened: bool,
}

impl ConnectedExecutor {
    async fn start(
        identity: BuildExecutorIdentity,
        client: async_nats::Client,
        workspace_root: PathBuf,
        completion: CompletionMode,
        state: Arc<Mutex<RuntimeState>>,
        admission_generation: AdmissionGeneration,
        startup: ExecutorStartup,
    ) -> Result<Self, BuildExecutionError> {
        let ExecutorStartup {
            workspace,
            admission,
        } = startup;
        let readiness = probe_readiness().await?;
        let runtime = ExternalBuildRuntime::new(
            identity.clone(),
            client.clone(),
            workspace_root,
            admission,
            state,
        );
        if workspace == WorkspaceStartup::Recover
            && readiness.capability != BuildExecutorCapability::RuntimeUnavailable
        {
            runtime.recover_orphans().await?;
        }
        let service =
            start_executor_service(client.clone(), identity, runtime.clone(), completion).await?;
        let admission_opened = runtime.open_admission(admission_generation).await;
        if admission_opened {
            eprintln!("Build Executor {}", health_description(&readiness));
        }
        Ok(Self {
            client,
            runtime,
            service,
            admission_opened,
        })
    }

    async fn shutdown(self) -> Result<(), PloyzctlExecutionError> {
        let runtime_result = self
            .runtime
            .shutdown()
            .await
            .map_err(PloyzctlExecutionError::from);
        let service_result = self.service.shutdown().await.map_err(|error| {
            PloyzctlExecutionError::from(BuildExecutionError::ExecutorRuntime {
                message: error.to_string(),
            })
        });
        let client_result = self.client.drain().await.map_err(|error| {
            PloyzctlExecutionError::from(BuildExecutionError::ExecutorRuntime {
                message: format!("failed to drain NATS client: {error}"),
            })
        });
        runtime_result?;
        service_result?;
        client_result
    }
}

pub(crate) async fn run(
    command: BuildExecutorCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let identity = BuildExecutorIdentity {
        pool_id: command.pool_id.clone(),
        executor_id: command.executor_id.clone(),
    };
    let (connect, credential_expires_at) = executor_connect_config(config, &identity)?;
    let workspace_root = resolve_workspace_root(command.workspace_root.as_deref(), &identity)?;
    let admission = command.admission;

    match command.mode {
        BuildExecutorRunMode::Once { wait_timeout } => {
            let client = connect_once(
                &connect,
                credential_expires_at,
                config.nats_connect_timeout(),
            )
            .await?;
            run_connected_once(
                identity,
                client,
                workspace_root,
                admission,
                wait_timeout,
                credential_expires_at,
            )
            .await
        }
        BuildExecutorRunMode::Watch => {
            run_watch(
                identity,
                connect,
                workspace_root,
                admission,
                credential_expires_at,
                config.nats_connect_timeout(),
            )
            .await
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceStartup {
    Recover,
    Prepared,
}

pub(crate) async fn run_controlled(
    command: BuildExecutorCommand,
    config: PloyzctlRuntimeConfig,
    workspace_startup: WorkspaceStartup,
    credential_expires_at: ployz_core::nats_config::BuildExecutorCredentialExpiresAt,
    ready: Option<oneshot::Sender<()>>,
    mut shutdown: Option<oneshot::Receiver<()>>,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config)?;
    let identity = BuildExecutorIdentity {
        pool_id: command.pool_id.clone(),
        executor_id: command.executor_id.clone(),
    };
    let mut connect = nats_connect_config(&config)?;
    connect.principal = executor_principal(&identity);
    let client = connect_once(
        &connect,
        credential_expires_at,
        config.nats_connect_timeout(),
    )
    .await?;
    let workspace_root = resolve_workspace_root(command.workspace_root.as_deref(), &identity)?;
    let (terminal_tx, terminal_rx) = oneshot::channel();
    let state = Arc::new(Mutex::new(RuntimeState::new_connected()));
    let admission_generation = state
        .lock()
        .await
        .admission_generation()
        .expect("controlled mode initializes a connected generation");
    let session = ConnectedExecutor::start(
        identity,
        client,
        workspace_root,
        CompletionMode::once(terminal_tx),
        state,
        admission_generation,
        ExecutorStartup {
            workspace: workspace_startup,
            admission: command.admission,
        },
    )
    .await?;
    if !session.admission_opened {
        session.shutdown().await?;
        return Err(BuildExecutionError::ExecutorConnection {
            message: "Build Executor disconnected before admission opened".to_owned(),
        }
        .into());
    }
    if let Some(ready) = ready {
        let _ = ready.send(());
    }

    let wait = async {
        match command.mode {
            BuildExecutorRunMode::Once { wait_timeout } => {
                wait_for_once_terminal(terminal_rx, wait_timeout, credential_expires_at).await
            }
            BuildExecutorRunMode::Watch => wait_for_controlled_stop(credential_expires_at).await,
        }
    };
    tokio::pin!(wait);
    let wait_result = if let Some(shutdown) = &mut shutdown {
        tokio::select! {
            result = &mut wait => result,
            _ = shutdown => Ok(()),
        }
    } else {
        wait.await
    };
    let shutdown_result = session.shutdown().await;
    finish_executor_session(wait_result, shutdown_result)?;
    Ok(PloyzctlExecutionOutput::stdout(
        "Build Executor stopped.\n".to_owned(),
    ))
}

async fn wait_for_controlled_stop(
    credential_expires_at: ployz_core::nats_config::BuildExecutorCredentialExpiresAt,
) -> Result<(), BuildExecutionError> {
    let expiry = credential_lifetime(credential_expires_at, std::time::SystemTime::now()).map_err(
        |error| BuildExecutionError::ExecutorCredential {
            message: error.to_string(),
        },
    )?;
    tokio::select! {
        biased;
        () = tokio::time::sleep(expiry) => Err(BuildExecutionError::ExecutorCredential {
            message: format!(
                "Build Executor credential expired at Unix timestamp {}",
                credential_expires_at.unix_seconds()
            ),
        }),
        signal = tokio::signal::ctrl_c() => signal.map_err(|error| {
            BuildExecutionError::ExecutorSignal { message: error.to_string() }
        }),
    }
}

async fn run_connected_once(
    identity: BuildExecutorIdentity,
    client: async_nats::Client,
    workspace_root: PathBuf,
    admission: BuildExecutorAdmission,
    wait_timeout: Duration,
    credential_expires_at: ployz_core::nats_config::BuildExecutorCredentialExpiresAt,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let (terminal_tx, terminal_rx) = oneshot::channel();
    let state = Arc::new(Mutex::new(RuntimeState::new_connected()));
    let admission_generation = state
        .lock()
        .await
        .admission_generation()
        .expect("once mode initializes a connected generation");
    let session = ConnectedExecutor::start(
        identity,
        client,
        workspace_root,
        CompletionMode::once(terminal_tx),
        state,
        admission_generation,
        ExecutorStartup {
            workspace: WorkspaceStartup::Recover,
            admission,
        },
    )
    .await?;
    let wait_result =
        wait_for_once_terminal(terminal_rx, wait_timeout, credential_expires_at).await;
    let shutdown_result = session.shutdown().await;
    finish_executor_session(wait_result, shutdown_result)?;
    Ok(PloyzctlExecutionOutput::stdout(
        "Build Executor stopped.\n".to_owned(),
    ))
}

async fn run_watch(
    identity: BuildExecutorIdentity,
    connect: ployz_nats::connect::NatsConnectConfig,
    workspace_root: PathBuf,
    admission: BuildExecutorAdmission,
    credential_expires_at: ployz_core::nats_config::BuildExecutorCredentialExpiresAt,
    connect_timeout: Duration,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let mut session = match connect_watch(&connect, credential_expires_at, connect_timeout).await? {
        WatchConnect::Connected(session) => session,
        WatchConnect::Stopped => {
            return Ok(PloyzctlExecutionOutput::stdout(
                "Build Executor stopped.\n".to_owned(),
            ));
        }
    };
    let connected = ConnectedExecutor::start(
        identity,
        session.client.clone(),
        workspace_root,
        CompletionMode::Watch,
        session.runtime_state(),
        session.admission_generation(),
        ExecutorStartup {
            workspace: WorkspaceStartup::Recover,
            admission,
        },
    )
    .await?;
    let wait_result = session
        .wait(
            credential_expires_at,
            &connected.runtime,
            current_health_description,
        )
        .await;
    eprintln!("Build Executor stopping");
    let shutdown_result = connected.shutdown().await;
    finish_executor_session(wait_result, shutdown_result)?;
    eprintln!("Build Executor stopped");
    Ok(PloyzctlExecutionOutput::stdout(
        "Build Executor stopped.\n".to_owned(),
    ))
}

fn finish_executor_session(
    wait_result: Result<(), BuildExecutionError>,
    shutdown_result: Result<(), PloyzctlExecutionError>,
) -> Result<(), PloyzctlExecutionError> {
    shutdown_result?;
    wait_result?;
    Ok(())
}

async fn current_health_description() -> Result<String, BuildExecutionError> {
    probe_readiness()
        .await
        .map(|readiness| health_description(&readiness))
}

fn health_description(readiness: &BuildExecutorReadiness) -> String {
    let (state, capability) = match readiness.capability {
        BuildExecutorCapability::DockerfileAndRailpack => ("ready", "dockerfile_and_railpack"),
        BuildExecutorCapability::DockerfileOnly => ("ready", "dockerfile_only"),
        BuildExecutorCapability::RuntimeUnavailable => ("degraded", "runtime_unavailable"),
    };
    format!(
        "{} {}/{} {}",
        state,
        readiness.native_platform.os(),
        readiness.native_platform.architecture(),
        capability,
    )
}

async fn wait_for_once_terminal(
    terminal: oneshot::Receiver<()>,
    wait_timeout: Duration,
    credential_expires_at: ployz_core::nats_config::BuildExecutorCredentialExpiresAt,
) -> Result<(), BuildExecutionError> {
    let expiry = credential_lifetime(credential_expires_at, std::time::SystemTime::now()).map_err(
        |error| BuildExecutionError::ExecutorCredential {
            message: error.to_string(),
        },
    )?;
    wait_for_once_terminal_with_lifetime(terminal, wait_timeout, expiry, credential_expires_at)
        .await
}

async fn wait_for_once_terminal_with_lifetime(
    terminal: oneshot::Receiver<()>,
    wait_timeout: Duration,
    credential_lifetime: Duration,
    credential_expires_at: ployz_core::nats_config::BuildExecutorCredentialExpiresAt,
) -> Result<(), BuildExecutionError> {
    tokio::select! {
        biased;
        () = tokio::time::sleep(credential_lifetime) => Err(BuildExecutionError::ExecutorCredential {
            message: format!(
                "Build Executor credential expired at Unix timestamp {}",
                credential_expires_at.unix_seconds()
            ),
        }),
        result = terminal => result.map_err(|_| BuildExecutionError::ExecutorRuntime {
            message: "Build Executor terminal channel closed".to_owned(),
        }),
        () = tokio::time::sleep(wait_timeout) => {
            Err(BuildExecutionError::ExecutorIdleTimedOut { wait_timeout })
        }
    }
}

pub(super) async fn probe_readiness() -> Result<BuildExecutorReadiness, BuildExecutionError> {
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
    admission: &BuildExecutorAdmission,
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
    if let BuildExecutorAdmission::ExactOperation(expected) = admission
        && *expected != request.operation_id
    {
        return Err(BuildExecutorStartDomainError::OperationIdentityMismatch {
            expected: expected.clone(),
            actual: request.operation_id.clone(),
        });
    }
    let requested_timeout = Duration::from_millis(request.timeout_millis);
    if requested_timeout.is_zero() || requested_timeout > BUILD_MAX_EXECUTION_TIMEOUT {
        return Err(BuildExecutorStartDomainError::InvalidTimeout {
            timeout_millis: request.timeout_millis,
        });
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct ExternalBuildRuntime {
    identity: BuildExecutorIdentity,
    admission: BuildExecutorAdmission,
    client: async_nats::Client,
    executor: Arc<DockerBuildExecutor>,
    state: Arc<Mutex<RuntimeState>>,
    changed: Arc<Notify>,
}

pub(super) struct RuntimeState {
    connection: RuntimeConnection,
    admission: RuntimeAdmission,
    active: Option<ActiveBuild>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeConnection {
    AwaitingInitial,
    Connected(AdmissionGeneration),
    Disconnected(AdmissionGeneration),
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AdmissionGeneration(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeAdmission {
    Closed,
    Open,
    Terminal,
}

impl RuntimeState {
    pub(super) fn new_closed() -> Self {
        Self {
            connection: RuntimeConnection::AwaitingInitial,
            admission: RuntimeAdmission::Closed,
            active: None,
        }
    }

    fn new_connected() -> Self {
        Self {
            connection: RuntimeConnection::Connected(AdmissionGeneration(0)),
            admission: RuntimeAdmission::Closed,
            active: None,
        }
    }
    fn ensure_accepting(&self) -> Result<(), BuildExecutorStartDomainError> {
        if self.admission != RuntimeAdmission::Open {
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

    pub(super) fn close_admission(&mut self) {
        if self.admission != RuntimeAdmission::Terminal {
            self.admission = RuntimeAdmission::Closed;
        }
    }

    pub(super) fn terminate_admission(&mut self) {
        self.connection = RuntimeConnection::Terminal;
        self.admission = RuntimeAdmission::Terminal;
    }

    pub(super) fn record_connected(&mut self) {
        self.connection = match self.connection {
            RuntimeConnection::AwaitingInitial => {
                RuntimeConnection::Connected(AdmissionGeneration(0))
            }
            RuntimeConnection::Disconnected(generation) => RuntimeConnection::Connected(generation),
            RuntimeConnection::Connected(generation) => RuntimeConnection::Connected(generation),
            RuntimeConnection::Terminal => RuntimeConnection::Terminal,
        };
    }

    pub(super) fn record_disconnected(&mut self) {
        if self.connection == RuntimeConnection::Terminal {
            return;
        }
        let generation = match self.connection {
            RuntimeConnection::AwaitingInitial => 0,
            RuntimeConnection::Connected(AdmissionGeneration(generation))
            | RuntimeConnection::Disconnected(AdmissionGeneration(generation)) => generation,
            RuntimeConnection::Terminal => unreachable!("terminal handled above"),
        };
        let Some(generation) = generation.checked_add(1) else {
            self.terminate_admission();
            return;
        };
        self.connection = RuntimeConnection::Disconnected(AdmissionGeneration(generation));
        self.close_admission();
    }

    pub(super) fn admission_generation(&self) -> Option<AdmissionGeneration> {
        let RuntimeConnection::Connected(generation) = self.connection else {
            return None;
        };
        Some(generation)
    }

    fn open_admission(&mut self, generation: AdmissionGeneration) -> bool {
        if self.connection == RuntimeConnection::Connected(generation)
            && self.admission != RuntimeAdmission::Terminal
        {
            self.admission = RuntimeAdmission::Open;
            return true;
        }
        false
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
        admission: BuildExecutorAdmission,
        state: Arc<Mutex<RuntimeState>>,
    ) -> Self {
        Self {
            identity,
            admission,
            client,
            executor: Arc::new(DockerBuildExecutor::new(workspace_root)),
            state,
            changed: Arc::new(Notify::new()),
        }
    }

    async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        self.executor
            .recover_orphans()
            .await
            .map_err(|error| executor_error(error.to_string()))
    }

    pub(super) async fn close_admission(&self) {
        self.state.lock().await.close_admission();
    }

    pub(super) async fn open_admission(&self, generation: AdmissionGeneration) -> bool {
        self.state.lock().await.open_admission(generation)
    }

    pub(super) async fn start(
        &self,
        request: BuildExecutorStartRequest,
    ) -> Result<BuildExecutorStartOk, BuildExecutorStartDomainError> {
        validate_start_provenance_and_timeout(&self.identity, &self.admission, &request)?;
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
            verified_source: result.verified_source,
            toolchain: result.toolchain,
            log_summary,
        })
    }

    pub(super) async fn cancel(
        &self,
        request: BuildExecutorCancelRequest,
    ) -> Result<BuildExecutorCancelOk, BuildExecutorCancelDomainError> {
        let mut state = self.state.lock().await;
        cancel_active(&mut state, request)
    }

    async fn shutdown(&self) -> Result<(), BuildExecutionError> {
        let cleanup_target = {
            let mut state = self.state.lock().await;
            state.terminate_admission();
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
            return Ok(());
        }
        let Some((operation_id, platform, supervisor)) = cleanup_target else {
            return Ok(());
        };
        supervisor.abort();
        let cleanup = timeout(
            BUILD_FORCE_CLEANUP_TIMEOUT,
            self.executor.force_cleanup(&operation_id, &platform),
        )
        .await;
        self.remove_active(&operation_id).await;
        shutdown_cleanup_result(match cleanup {
            Ok(Ok(())) => BuildExecutorCleanupOutcome::Confirmed,
            Ok(Err(_)) | Err(_) => BuildExecutorCleanupOutcome::Unconfirmed,
        })
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

fn shutdown_cleanup_result(
    cleanup: BuildExecutorCleanupOutcome,
) -> Result<(), BuildExecutionError> {
    match cleanup {
        BuildExecutorCleanupOutcome::Confirmed => Ok(()),
        BuildExecutorCleanupOutcome::Unconfirmed => {
            Err(BuildExecutionError::ExecutorShutdownCleanupUnconfirmed)
        }
    }
}

struct ExternalBuildOutput {
    acceptance: BuildExecutorAcceptance,
    image: PlatformImage,
    verified_source: ployz_core::build::VerifiedBuildSource,
    toolchain: ployz_core::operation::BuildToolchainEvidence,
    log_summary: BuildLogSummary,
}

impl ExternalBuildOutput {
    fn into_start_ok(self) -> BuildExecutorStartOk {
        BuildExecutorStartOk {
            acceptance: self.acceptance,
            cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
            image: self.image,
            verified_source: self.verified_source,
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

pub(super) fn executor_error(message: impl Into<String>) -> BuildExecutionError {
    BuildExecutionError::ExecutorRuntime {
        message: message.into(),
    }
}

#[cfg(test)]
#[path = "external_runtime/tests.rs"]
mod tests;
