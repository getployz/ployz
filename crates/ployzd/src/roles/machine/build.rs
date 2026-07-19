use super::execution::build::{
    BuildExecutionError, BuildExecutionRequest, BuildExecutionResult, BuildLogProgress,
    DockerBuildExecutor,
};
use super::images::AvailableImageService;
use super::protocol::{
    BuildExecutorAcceptance, BuildExecutorAssignment, BuildLogSummary,
    MachineBuildCachePruneDomainError, MachineBuildCachePruneRpcOk,
    MachineBuildCachePruneRpcRequest, MachineBuildCachePruneRpcResponse,
    MachineBuildCancelDomainError, MachineBuildCancelOutcome, MachineBuildCancelRpcOk,
    MachineBuildCancelRpcRequest, MachineBuildCancelRpcResponse, MachineBuildCleanupOutcome,
    MachineBuildStartDomainError, MachineBuildStartRpcOk, MachineBuildStartRpcRequest,
    MachineBuildStartRpcResponse,
};
use super::response::{failure_message, machine_domain_error, machine_success};
use ployz_core::build::{
    BUILD_CACHE_PRUNE_MAX_EXECUTION_TIMEOUT, BUILD_FORCE_CLEANUP_TIMEOUT,
    BUILD_MAX_EXECUTION_TIMEOUT, BUILD_TASK_DRAIN_TIMEOUT, BuildExecutorCancelOk,
    BuildExecutorStartOk,
};
use ployz_core::deploy::PlatformImage;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::BuildPlatformFailure;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Notify, Semaphore, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;

#[derive(Clone)]
pub(crate) struct MachineBuildRuntime {
    machine_id: MachineId,
    effects: BuildEffects,
    lifecycle: Arc<BuildRuntimeLifecycle>,
    machine_slot: Arc<Semaphore>,
    local_platform: OciPlatform,
}

struct BuildRuntimeLifecycle {
    state: Mutex<BuildRuntimeState>,
    changed: Notify,
}

struct BuildRuntimeState {
    phase: BuildRuntimePhase,
    active: BTreeMap<OperationId, ActiveBuild>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BuildRuntimePhase {
    Accepting,
    ShuttingDown,
    Stopped,
}

struct ActiveBuild {
    platform: OciPlatform,
    cancel: watch::Sender<bool>,
    supervisor: AbortHandle,
    completion: Arc<BuildSupervisorCompletion>,
}

struct BuildSupervisorCompletion {
    finished: AtomicBool,
    changed: Notify,
}

impl BuildSupervisorCompletion {
    fn new() -> Self {
        Self {
            finished: AtomicBool::new(false),
            changed: Notify::new(),
        }
    }

    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
}

struct BuildSupervisorCompletionGuard(Arc<BuildSupervisorCompletion>);

impl Drop for BuildSupervisorCompletionGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}

#[derive(Clone)]
enum BuildEffects {
    Docker(Box<DockerBuildEffects>),
    #[cfg(test)]
    Test(Arc<tests::TestBuildEffects>),
}

#[derive(Clone)]
struct DockerBuildEffects {
    executor: DockerBuildExecutor,
    image_state: Option<AvailableImageService>,
}

impl MachineBuildRuntime {
    pub(crate) fn new(
        machine_id: MachineId,
        executor: DockerBuildExecutor,
        image_state: Option<AvailableImageService>,
    ) -> Result<Self, String> {
        Ok(Self {
            machine_id,
            effects: BuildEffects::Docker(Box::new(DockerBuildEffects {
                executor,
                image_state,
            })),
            lifecycle: Arc::new(BuildRuntimeLifecycle {
                state: Mutex::new(BuildRuntimeState {
                    phase: BuildRuntimePhase::Accepting,
                    active: BTreeMap::new(),
                }),
                changed: Notify::new(),
            }),
            machine_slot: Arc::new(Semaphore::new(1)),
            local_platform: local_platform()?,
        })
    }

    #[cfg(test)]
    fn new_with_test_effects(machine_id: MachineId, effects: Arc<tests::TestBuildEffects>) -> Self {
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

    #[cfg(test)]
    pub(crate) fn new_for_test(machine_id: MachineId) -> Self {
        Self::new_with_test_effects(machine_id, Arc::new(tests::TestBuildEffects::new(true)))
    }

    pub(crate) async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        match &self.effects {
            BuildEffects::Docker(effects) => effects.executor.recover_orphans().await,
            #[cfg(test)]
            BuildEffects::Test(_) => Ok(()),
        }
    }

    pub(crate) async fn prune_cache(
        &self,
    ) -> Result<ployz_core::operation::BuildCachePruneEvidence, BuildExecutionError> {
        let _slot = self
            .machine_slot
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| BuildExecutionError::Infrastructure {
                action: "acquire machine build slot",
                message: error.to_string(),
                log_summary: BuildLogSummary::none(),
            })?;
        if self.lifecycle.state.lock().await.phase != BuildRuntimePhase::Accepting {
            return Err(BuildExecutionError::Infrastructure {
                action: "prune build cache",
                message: "machine build runtime is shutting down".to_owned(),
                log_summary: BuildLogSummary::none(),
            });
        }
        match tokio::time::timeout(
            BUILD_CACHE_PRUNE_MAX_EXECUTION_TIMEOUT,
            self.effects.prune_cache(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(BuildExecutionError::Infrastructure {
                action: "prune build cache",
                message: format!(
                    "build cache prune timed out after {}s",
                    BUILD_CACHE_PRUNE_MAX_EXECUTION_TIMEOUT.as_secs()
                ),
                log_summary: BuildLogSummary::none(),
            }),
        }
    }

    pub(crate) async fn shutdown(&self) {
        let owns_shutdown = {
            let mut state = self.lifecycle.state.lock().await;
            match state.phase {
                BuildRuntimePhase::Accepting => {
                    state.phase = BuildRuntimePhase::ShuttingDown;
                    for build in state.active.values() {
                        let _ = build.cancel.send(true);
                    }
                    true
                }
                BuildRuntimePhase::ShuttingDown => false,
                BuildRuntimePhase::Stopped => return,
            }
        };
        self.lifecycle.changed.notify_waiters();
        if !owns_shutdown {
            self.wait_for_stopped().await;
            return;
        }

        let drained = tokio::time::timeout(
            BUILD_TASK_DRAIN_TIMEOUT,
            self.wait_for_active_builds_to_finish(),
        )
        .await
        .is_ok();
        let residual = if drained {
            Vec::new()
        } else {
            let state = self.lifecycle.state.lock().await;
            state
                .active
                .iter()
                .map(|(operation_id, build)| ResidualBuild {
                    operation_id: operation_id.clone(),
                    platform: build.platform.clone(),
                    supervisor: build.supervisor.clone(),
                    completion: build.completion.clone(),
                })
                .collect::<Vec<_>>()
        };
        for build in &residual {
            build.supervisor.abort();
        }
        let completion_wait =
            futures_util::future::join_all(residual.iter().map(|build| build.completion.wait()));
        let _ = tokio::time::timeout(BUILD_TASK_DRAIN_TIMEOUT, completion_wait).await;
        let _slot = self
            .machine_slot
            .clone()
            .acquire_owned()
            .await
            .expect("machine build slot remains open for the runtime lifetime");
        {
            let mut state = self.lifecycle.state.lock().await;
            state.active.clear();
        }
        self.lifecycle.changed.notify_waiters();
        futures_util::future::join_all(residual.iter().map(|build| {
            self.effects
                .force_cleanup(&build.operation_id, &build.platform)
        }))
        .await;
        {
            let mut state = self.lifecycle.state.lock().await;
            state.phase = BuildRuntimePhase::Stopped;
        }
        self.lifecycle.changed.notify_waiters();
    }

    async fn wait_for_active_builds_to_finish(&self) {
        loop {
            let changed = self.lifecycle.changed.notified();
            if self.lifecycle.state.lock().await.active.is_empty() {
                return;
            }
            changed.await;
        }
    }

    async fn wait_for_stopped(&self) {
        loop {
            let changed = self.lifecycle.changed.notified();
            if self.lifecycle.state.lock().await.phase == BuildRuntimePhase::Stopped {
                return;
            }
            changed.await;
        }
    }

    async fn start(
        &self,
        request: MachineBuildStartRpcRequest,
    ) -> Result<MachineBuildStartRpcOk, MachineBuildStartDomainError> {
        validate_start_provenance(&self.machine_id, &request)?;
        if self.lifecycle.state.lock().await.phase != BuildRuntimePhase::Accepting {
            return Err(build_runtime_stopped());
        }
        if request.platform != self.local_platform {
            return Err(MachineBuildStartDomainError::PlatformMismatch {
                expected: request.platform,
                actual: self.local_platform.clone(),
            });
        }
        let timeout = requested_build_timeout(request.timeout_millis)?;
        let acceptance = BuildExecutorAcceptance::from_start_request(&request);
        let (cancel, cancel_rx) = watch::channel(false);
        let runtime = self.clone();
        let operation_id = request.operation_id.clone();
        let registered_operation_id = operation_id.clone();
        let platform = request.platform.clone();
        let task_cancel = cancel.clone();
        let deadline = Instant::now() + timeout;
        let (result_tx, result_rx) = oneshot::channel();
        let (launch, launch_rx) = oneshot::channel();
        let completion = Arc::new(BuildSupervisorCompletion::new());
        let completion_guard = BuildSupervisorCompletionGuard(completion.clone());
        let task_operation_id = operation_id.clone();
        let task_acceptance = acceptance.clone();
        let supervisor = tokio::spawn(async move {
            let _completion = completion_guard;
            if launch_rx.await.is_err() {
                return;
            }
            let result = runtime
                .run_build(request, task_acceptance, task_cancel, cancel_rx, deadline)
                .await;
            runtime.remove_active(&task_operation_id).await;
            let _ = result_tx.send(result);
        });
        {
            let mut state = self.lifecycle.state.lock().await;
            if state.phase != BuildRuntimePhase::Accepting {
                supervisor.abort();
                return Err(build_runtime_stopped());
            }
            if state.active.contains_key(&operation_id) {
                supervisor.abort();
                return Err(MachineBuildStartDomainError::AlreadyRunning);
            }
            state.active.insert(
                operation_id,
                ActiveBuild {
                    platform,
                    cancel: cancel.clone(),
                    supervisor: supervisor.abort_handle(),
                    completion,
                },
            );
        }
        if launch.send(()).is_err() {
            supervisor.abort();
            self.remove_active(&registered_operation_id).await;
        }
        result_rx.await.unwrap_or_else(|_| {
            Err(MachineBuildStartDomainError::PlatformFailed {
                acceptance: Box::new(acceptance),
                failure: BuildPlatformFailure::MachineUnavailable {
                    message: failure_message(
                        "machine build task stopped before returning a result",
                    ),
                },
                log_summary: BuildLogSummary::none(),
            })
        })
    }

    async fn run_build(
        &self,
        request: MachineBuildStartRpcRequest,
        acceptance: BuildExecutorAcceptance,
        cancel: watch::Sender<bool>,
        mut cancel_rx: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<MachineBuildStartRpcOk, MachineBuildStartDomainError> {
        let slot = self.machine_slot.clone().acquire_owned();
        let _slot = tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => {
                return Err(MachineBuildStartDomainError::TimedOut {
                    acceptance: Box::new(acceptance),
                    message: failure_message("build timed out waiting for the machine build slot"),
                    cleanup: MachineBuildCleanupOutcome::Confirmed,
                    log_summary: BuildLogSummary::none(),
                });
            }
            changed = cancel_rx.changed() => {
                let _ = changed;
                return Err(MachineBuildStartDomainError::Cancelled {
                    acceptance: Box::new(acceptance),
                    cleanup: MachineBuildCleanupOutcome::Confirmed,
                    log_summary: BuildLogSummary::none(),
                });
            }
            permit = slot => permit.map_err(|_| MachineBuildStartDomainError::PlatformFailed {
                acceptance: Box::new(acceptance.clone()),
                failure: BuildPlatformFailure::MachineUnavailable {
                    message: failure_message("machine build slot closed"),
                },
                log_summary: BuildLogSummary::none(),
            })?,
        };
        let operation_id = request.operation_id.clone();
        let platform = request.platform.clone();
        let progress = BuildLogProgress::default();
        let task_effects = self.effects.clone();
        let machine_id = self.machine_id.clone();
        let task_progress = progress.clone();
        let task_cancel_rx = cancel_rx.clone();
        let completion = {
            let task = task_effects.execute_and_ingest(
                machine_id,
                request,
                acceptance.clone(),
                task_cancel_rx,
                task_progress,
                deadline,
            );
            tokio::pin!(task);
            let completion = tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => {
                    BuildTaskCompletion::TimedOut
                }
                changed = cancel_rx.changed() => {
                    let _ = changed;
                    BuildTaskCompletion::Cancelled
                }
                result = &mut task => BuildTaskCompletion::Finished(Box::new(result)),
            };
            if !matches!(completion, BuildTaskCompletion::Finished(_)) {
                let _ = cancel.send(true);
                let _ = tokio::time::timeout(BUILD_TASK_DRAIN_TIMEOUT, &mut task).await;
            }
            completion
        };
        let cleanup = self.effects.force_cleanup(&operation_id, &platform).await;
        let (final_log_sequence, omitted_log_bytes) = progress.summary();
        let log_summary = BuildLogSummary::new(final_log_sequence, omitted_log_bytes);
        match completion {
            BuildTaskCompletion::Cancelled => Err(MachineBuildStartDomainError::Cancelled {
                acceptance: Box::new(acceptance),
                cleanup,
                log_summary,
            }),
            BuildTaskCompletion::TimedOut => Err(MachineBuildStartDomainError::TimedOut {
                acceptance: Box::new(acceptance),
                message: failure_message(match cleanup {
                    MachineBuildCleanupOutcome::Confirmed => {
                        "build exceeded its operation deadline"
                    }
                    MachineBuildCleanupOutcome::Unconfirmed => {
                        "build exceeded its deadline and cleanup did not finish"
                    }
                }),
                cleanup,
                log_summary,
            }),
            BuildTaskCompletion::Finished(result) => {
                if cleanup == MachineBuildCleanupOutcome::Unconfirmed {
                    return Err(MachineBuildStartDomainError::PlatformFailed {
                        acceptance: Box::new(acceptance),
                        failure: BuildPlatformFailure::MachineUnavailable {
                            message: failure_message(
                                "build workspace cleanup did not finish successfully",
                            ),
                        },
                        log_summary,
                    });
                }
                (*result).map_err(|error| machine_build_error(error, cleanup, acceptance))
            }
        }
    }

    async fn cancel(&self, operation_id: &OperationId) -> MachineBuildCancelOutcome {
        let state = self.lifecycle.state.lock().await;
        let Some(build) = state.active.get(operation_id) else {
            return MachineBuildCancelOutcome::NotRunning;
        };
        let _ = build.cancel.send(true);
        MachineBuildCancelOutcome::Requested
    }

    async fn remove_active(&self, operation_id: &OperationId) {
        self.lifecycle
            .state
            .lock()
            .await
            .active
            .remove(operation_id);
        self.lifecycle.changed.notify_waiters();
    }
}

enum BuildTaskCompletion {
    Finished(Box<Result<MachineBuildStartRpcOk, BuildExecutionError>>),
    Cancelled,
    TimedOut,
}

struct ResidualBuild {
    operation_id: OperationId,
    platform: OciPlatform,
    supervisor: AbortHandle,
    completion: Arc<BuildSupervisorCompletion>,
}

impl BuildEffects {
    async fn prune_cache(
        &self,
    ) -> Result<ployz_core::operation::BuildCachePruneEvidence, BuildExecutionError> {
        match self {
            Self::Docker(effects) => effects.executor.prune_cache().await,
            #[cfg(test)]
            Self::Test(effects) => effects.prune_cache().await,
        }
    }

    async fn execute_and_ingest(
        &self,
        machine_id: MachineId,
        request: MachineBuildStartRpcRequest,
        acceptance: BuildExecutorAcceptance,
        cancel_rx: watch::Receiver<bool>,
        log_progress: BuildLogProgress,
        deadline: Instant,
    ) -> Result<MachineBuildStartRpcOk, BuildExecutionError> {
        match self {
            Self::Docker(effects) => {
                let operation_id = request.operation_id;
                let platform = request.platform;
                let result: BuildExecutionResult = effects
                    .executor
                    .execute(
                        BuildExecutionRequest::new(
                            &operation_id,
                            &request.source,
                            &request.adapter,
                            &platform,
                        ),
                        cancel_rx,
                        log_progress,
                        deadline,
                    )
                    .await?;
                let log_summary = result.log_summary;
                let Some(images) = &effects.image_state else {
                    return Err(BuildExecutionError::Platform {
                        failure: BuildPlatformFailure::MachineUnavailable {
                            message: failure_message(
                                "machine image content service is unavailable",
                            ),
                        },
                        log_summary,
                    });
                };
                let lease_expires_at =
                    images
                        .ingest_build_layout(&result.layout)
                        .await
                        .map_err(|message| BuildExecutionError::Platform {
                            failure: BuildPlatformFailure::ImagePushFailed {
                                message: failure_message(message),
                            },
                            log_summary,
                        })?;
                let availability_expires_at =
                    ployz_core::deploy::ImageAvailabilityExpiresAt::from_content_lease_expiry(
                        lease_expires_at,
                    )
                    .map_err(|error| BuildExecutionError::Platform {
                        failure: BuildPlatformFailure::ImagePushFailed {
                            message: failure_message(error.to_string()),
                        },
                        log_summary,
                    })?;
                Ok(MachineBuildStartRpcOk::from((
                    machine_id.clone(),
                    BuildExecutorStartOk {
                        acceptance,
                        image: PlatformImage {
                            seed: machine_id,
                            manifest_digest: result.layout.manifest_digest().clone(),
                            image_id: result.layout.image_id().clone(),
                            availability_expires_at,
                        },
                        verified_commit: result.verified_commit,
                        toolchain: result.toolchain,
                        log_summary,
                    },
                )))
            }
            #[cfg(test)]
            Self::Test(effects) => {
                let _ = deadline;
                effects.execute_and_ingest(log_progress, cancel_rx).await
            }
        }
    }

    async fn force_cleanup(
        &self,
        operation_id: &OperationId,
        platform: &OciPlatform,
    ) -> MachineBuildCleanupOutcome {
        let cleanup = async {
            match self {
                Self::Docker(effects) => {
                    effects.executor.force_cleanup(operation_id, platform).await
                }
                #[cfg(test)]
                Self::Test(effects) => effects.force_cleanup().await,
            }
        };
        match tokio::time::timeout(BUILD_FORCE_CLEANUP_TIMEOUT, cleanup).await {
            Ok(Ok(())) => MachineBuildCleanupOutcome::Confirmed,
            Ok(Err(_)) | Err(_) => MachineBuildCleanupOutcome::Unconfirmed,
        }
    }
}

fn requested_build_timeout(
    timeout_millis: u64,
) -> Result<std::time::Duration, MachineBuildStartDomainError> {
    let timeout = std::time::Duration::from_millis(timeout_millis);
    if timeout.is_zero() || timeout > BUILD_MAX_EXECUTION_TIMEOUT {
        return Err(MachineBuildStartDomainError::InvalidTimeout { timeout_millis });
    }
    Ok(timeout)
}

fn validate_start_provenance(
    machine_id: &MachineId,
    request: &MachineBuildStartRpcRequest,
) -> Result<(), MachineBuildStartDomainError> {
    let expected = BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    };
    if request.assignment != expected {
        return Err(MachineBuildStartDomainError::AssignmentMismatch {
            expected: Box::new(expected),
            actual: request.assignment.clone(),
        });
    }
    Ok(())
}

fn validate_cancel_provenance(
    machine_id: &MachineId,
    request: &MachineBuildCancelRpcRequest,
) -> Result<(), MachineBuildCancelDomainError> {
    let expected = BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    };
    if request.assignment != expected {
        return Err(MachineBuildCancelDomainError::AssignmentMismatch {
            expected: Box::new(expected),
            actual: request.assignment.clone(),
        });
    }
    Ok(())
}

fn machine_build_error(
    error: BuildExecutionError,
    cleanup: MachineBuildCleanupOutcome,
    acceptance: BuildExecutorAcceptance,
) -> MachineBuildStartDomainError {
    let log_summary = error.log_summary();
    match error {
        BuildExecutionError::Cancelled { .. } => MachineBuildStartDomainError::Cancelled {
            acceptance: Box::new(acceptance),
            cleanup,
            log_summary,
        },
        BuildExecutionError::TimedOut { .. } => MachineBuildStartDomainError::TimedOut {
            acceptance: Box::new(acceptance),
            message: failure_message("build exceeded its operation deadline"),
            cleanup,
            log_summary,
        },
        BuildExecutionError::Platform { failure, .. } => {
            MachineBuildStartDomainError::PlatformFailed {
                acceptance: Box::new(acceptance),
                failure,
                log_summary,
            }
        }
        BuildExecutionError::Infrastructure {
            action, message, ..
        } => MachineBuildStartDomainError::PlatformFailed {
            acceptance: Box::new(acceptance),
            failure: BuildPlatformFailure::MachineUnavailable {
                message: failure_message(format!("{action}: {message}")),
            },
            log_summary,
        },
    }
}

fn build_runtime_stopped() -> MachineBuildStartDomainError {
    MachineBuildStartDomainError::RuntimeStopped
}

fn local_platform() -> Result<OciPlatform, String> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        architecture => {
            return Err(format!(
                "unsupported build machine architecture {architecture}"
            ));
        }
    };
    OciPlatform::try_new(std::env::consts::OS, architecture).map_err(|error| error.to_string())
}

pub(crate) async fn handle_build_start(
    machine_id: MachineId,
    runtime: Option<MachineBuildRuntime>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineBuildStartRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(error) = validate_start_provenance(&machine_id, &request) {
        return machine_domain_error(MachineBuildStartRpcResponse::DomainError {
            machine_id,
            error,
        });
    }
    let Some(runtime) = runtime else {
        return machine_domain_error(MachineBuildStartRpcResponse::DomainError {
            machine_id,
            error: MachineBuildStartDomainError::RuntimeUnavailable,
        });
    };
    match runtime.start(request).await {
        Ok(value) => machine_success(MachineBuildStartRpcResponse::Ok(value)),
        Err(error) => {
            machine_domain_error(MachineBuildStartRpcResponse::DomainError { machine_id, error })
        }
    }
}

pub(crate) async fn handle_build_cancel(
    machine_id: MachineId,
    runtime: Option<MachineBuildRuntime>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineBuildCancelRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(error) = validate_cancel_provenance(&machine_id, &request) {
        return machine_domain_error(MachineBuildCancelRpcResponse::DomainError {
            machine_id,
            error,
        });
    }
    let Some(runtime) = runtime else {
        return machine_domain_error(MachineBuildCancelRpcResponse::DomainError {
            machine_id,
            error: super::protocol::MachineBuildCancelDomainError::CancelFailed {
                message: failure_message("machine build runtime is unavailable"),
            },
        });
    };
    let outcome = runtime.cancel(&request.operation_id).await;
    machine_success(MachineBuildCancelRpcResponse::Ok(
        MachineBuildCancelRpcOk::from((
            machine_id.clone(),
            BuildExecutorCancelOk {
                assignment: BuildExecutorAssignment::Cluster {
                    machine_id: machine_id.clone(),
                },
                outcome,
            },
        )),
    ))
}

pub(crate) async fn handle_build_cache_prune(
    machine_id: MachineId,
    runtime: Option<MachineBuildRuntime>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let _request = match decode_json_request::<MachineBuildCachePruneRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(runtime) = runtime else {
        return machine_domain_error(MachineBuildCachePruneRpcResponse::DomainError {
            machine_id,
            error: MachineBuildCachePruneDomainError::PruneFailed {
                message: failure_message("machine build runtime is unavailable"),
            },
        });
    };
    match runtime.prune_cache().await {
        Ok(evidence) => machine_success(MachineBuildCachePruneRpcResponse::Ok(
            MachineBuildCachePruneRpcOk {
                machine_id,
                evidence,
            },
        )),
        Err(error) => machine_domain_error(MachineBuildCachePruneRpcResponse::DomainError {
            machine_id,
            error: MachineBuildCachePruneDomainError::PruneFailed {
                message: failure_message(error.to_string()),
            },
        }),
    }
}

#[cfg(test)]
mod tests;
