use super::images::AvailableImageService;
use super::protocol::{
    BuildExecutorAcceptance, BuildExecutorAssignment, BuildLogSummary,
    MachineBuildCachePruneDomainError, MachineBuildCachePruneRpcOk,
    MachineBuildCachePruneRpcRequest, MachineBuildCachePruneRpcResponse,
    MachineBuildCancelDomainError, MachineBuildCancelOutcome, MachineBuildCancelRpcOk,
    MachineBuildCancelRpcRequest, MachineBuildCancelRpcResponse, MachineBuildCleanupOutcome,
    MachineBuildStartDomainError, MachineBuildStartRpcOk, MachineBuildStartRpcRequest,
    MachineBuildStartRpcResponse, MachineBuildStatusDomainError, MachineBuildStatusRpcOk,
    MachineBuildStatusRpcRequest, MachineBuildStatusRpcResponse,
};
use super::response::{failure_message, machine_domain_error, machine_success};
use ployz_build_executor::{
    BuildExecutionError, BuildExecutionRequest, BuildExecutionResult, BuildExecutionSupervision,
    BuildLogDestination, BuildLogProgress, DockerBuildExecutor,
};
use ployz_core::build::{
    BUILD_CACHE_PRUNE_MAX_EXECUTION_TIMEOUT, BUILD_FORCE_CLEANUP_TIMEOUT,
    BUILD_MAX_EXECUTION_TIMEOUT, BUILD_TASK_DRAIN_TIMEOUT, BuildExecutorCancelOk,
    BuildExecutorCompletion, BuildExecutorStartOk, BuildExecutorState, BuildExecutorStatus,
    BuildExecutorStatusFailure, BuildExecutorSuccessCleanupEvidence, VerifiedBuildSource,
};
use ployz_core::deploy::PlatformImage;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::BuildPlatformFailure;
use ployz_nats::service_runtime::NatsClient;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use ployz_nats::subjects::machine_build_log;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Notify, Semaphore, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;

mod effects;
mod status;
use effects::{BuildEffects, DockerBuildEffects};
use status::{BuildStatusRecord, BuildStatusRepository};

#[derive(Clone)]
pub(crate) struct MachineBuildRuntime {
    machine_id: MachineId,
    effects: BuildEffects,
    lifecycle: Arc<BuildRuntimeLifecycle>,
    machine_slot: Arc<Semaphore>,
    local_platform: OciPlatform,
    status: BuildStatusRepository,
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
    acceptance: BuildExecutorAcceptance,
    progress: BuildLogProgress,
    evidence_error: Option<String>,
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

struct MachineBuildLogRoute {
    subject: String,
    assignment: BuildExecutorAssignment,
}

fn machine_build_log_route(
    machine_id: &MachineId,
    operation_id: &OperationId,
) -> MachineBuildLogRoute {
    MachineBuildLogRoute {
        subject: machine_build_log(machine_id, operation_id),
        assignment: BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        },
    }
}

impl MachineBuildRuntime {
    pub(crate) fn new_with_status_path(
        machine_id: MachineId,
        log_client: NatsClient,
        executor: DockerBuildExecutor,
        image_state: Option<AvailableImageService>,
        status_path: std::path::PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            machine_id,
            effects: BuildEffects::Docker(Box::new(DockerBuildEffects {
                executor,
                log_client,
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
            status: BuildStatusRepository::new(status_path),
        })
    }

    #[cfg(test)]
    fn new_for_test(machine_id: MachineId, effects: Arc<tests::TestBuildEffects>) -> Self {
        let status_path = tempfile::tempdir()
            .expect("build status directory")
            .keep()
            .join("status");
        Self::new_for_test_with_status_path(machine_id, effects, status_path)
    }

    #[cfg(test)]
    fn new_for_test_with_status_path(
        machine_id: MachineId,
        effects: Arc<tests::TestBuildEffects>,
        status_path: std::path::PathBuf,
    ) -> Self {
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
            status: BuildStatusRepository::new(status_path),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_default_test_effects(machine_id: MachineId) -> Self {
        Self::new_for_test(machine_id, Arc::new(tests::TestBuildEffects::new(true)))
    }

    pub(crate) async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        match &self.effects {
            BuildEffects::Docker(effects) => effects.executor.recover_orphans().await,
            #[cfg(test)]
            BuildEffects::Test(effects) => effects.recover_orphans().await,
        }?;
        self.recover_status_records()
            .map_err(|message| BuildExecutionError::Infrastructure {
                action: "recover machine build status",
                message,
                log_summary: BuildLogSummary::none(),
            })
    }

    fn recover_status_records(&self) -> Result<(), String> {
        for record in self.status.records()? {
            if matches!(record.status.state, BuildExecutorState::Running { .. }) {
                let log_summary = record.status.log_summary();
                let failed = BuildExecutorStatus {
                    acceptance: record.status.acceptance.clone(),
                    state: BuildExecutorState::Failed {
                        failure: BuildExecutorStatusFailure::PlatformFailed {
                            failure: BuildPlatformFailure::MachineUnavailable {
                                message: failure_message(
                                    "machine restarted before the accepted build completed",
                                ),
                            },
                        },
                        cleanup: MachineBuildCleanupOutcome::Confirmed,
                        log_summary,
                    },
                };
                self.status.write(&BuildStatusRecord::new(failed))?;
            }
        }
        Ok(())
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
                    acceptance: build.acceptance.clone(),
                    progress: build.progress.clone(),
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
        let cleanup = futures_util::future::join_all(residual.iter().map(|build| {
            self.effects
                .force_cleanup(&build.operation_id, &build.platform)
        }))
        .await;
        for (build, cleanup) in residual.iter().zip(cleanup) {
            let (final_log_sequence, omitted_log_bytes) = build.progress.summary();
            let _ = self
                .status
                .write(&BuildStatusRecord::new(BuildExecutorStatus {
                    acceptance: build.acceptance.clone(),
                    state: BuildExecutorState::Cancelled {
                        cleanup,
                        log_summary: BuildLogSummary::new(final_log_sequence, omitted_log_bytes),
                    },
                }));
        }
        {
            let mut state = self.lifecycle.state.lock().await;
            state.active.clear();
        }
        self.lifecycle.changed.notify_waiters();
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
        match self.status.read(&request.operation_id) {
            Ok(Some(record)) if record.status.acceptance == acceptance => {
                return Ok(MachineBuildStartRpcOk::from((
                    self.machine_id.clone(),
                    acceptance,
                )));
            }
            Ok(Some(_)) | Err(_) => return Err(MachineBuildStartDomainError::AlreadyRunning),
            Ok(None) => {}
        }
        let (cancel, cancel_rx) = watch::channel(false);
        let runtime = self.clone();
        let operation_id = request.operation_id.clone();
        let platform = request.platform.clone();
        let task_cancel = cancel.clone();
        let (launch, launch_rx) = oneshot::channel();
        let completion = Arc::new(BuildSupervisorCompletion::new());
        let completion_guard = BuildSupervisorCompletionGuard(completion.clone());
        let task_operation_id = operation_id.clone();
        let task_acceptance = acceptance.clone();
        let progress = BuildLogProgress::default();
        let task_progress = progress.clone();
        let supervisor = tokio::spawn(async move {
            let _completion = completion_guard;
            if launch_rx.await.is_err() {
                return;
            }
            let status = runtime
                .run_build(
                    request,
                    task_acceptance.clone(),
                    task_cancel,
                    cancel_rx,
                    timeout,
                    task_progress,
                )
                .await;
            if let Err(message) = runtime.status.write(&BuildStatusRecord::new(status)) {
                if matches!(
                    runtime.status.read(&task_operation_id),
                    Ok(Some(BuildStatusRecord { status, .. }))
                        if status.acceptance == task_acceptance
                            && status.is_terminal()
                ) {
                    runtime.remove_active(&task_operation_id).await;
                    return;
                }
                // The in-memory reservation remains poisoned until restart; the
                // existing running evidence prevents a relaunch after restart.
                if let Some(active) = runtime
                    .lifecycle
                    .state
                    .lock()
                    .await
                    .active
                    .get_mut(&task_operation_id)
                {
                    active.evidence_error = Some(message);
                }
                return;
            }
            runtime.remove_active(&task_operation_id).await;
        });
        {
            let mut state = self.lifecycle.state.lock().await;
            if state.phase != BuildRuntimePhase::Accepting {
                supervisor.abort();
                return Err(build_runtime_stopped());
            }
            if state.active.contains_key(&operation_id) {
                supervisor.abort();
                let active = state
                    .active
                    .get(&operation_id)
                    .expect("checked active build");
                if active.acceptance == acceptance {
                    return Ok(MachineBuildStartRpcOk::from((
                        self.machine_id.clone(),
                        acceptance,
                    )));
                }
                return Err(MachineBuildStartDomainError::AlreadyRunning);
            }
            let running = BuildExecutorStatus {
                acceptance: acceptance.clone(),
                state: BuildExecutorState::Running {
                    log_summary: BuildLogSummary::none(),
                },
            };
            self.status
                .write(&BuildStatusRecord::new(running))
                .map_err(|_| MachineBuildStartDomainError::RuntimeUnavailable)?;
            state.active.insert(
                operation_id,
                ActiveBuild {
                    platform,
                    cancel: cancel.clone(),
                    supervisor: supervisor.abort_handle(),
                    completion,
                    acceptance: acceptance.clone(),
                    progress,
                    evidence_error: None,
                },
            );
        }
        if launch.send(()).is_err() {
            supervisor.abort();
            return Err(MachineBuildStartDomainError::RuntimeUnavailable);
        }
        Ok(MachineBuildStartRpcOk::from((
            self.machine_id.clone(),
            acceptance,
        )))
    }

    async fn run_build(
        &self,
        request: MachineBuildStartRpcRequest,
        acceptance: BuildExecutorAcceptance,
        cancel: watch::Sender<bool>,
        mut cancel_rx: watch::Receiver<bool>,
        timeout: std::time::Duration,
        progress: BuildLogProgress,
    ) -> BuildExecutorStatus {
        let mut deadline = Instant::now() + timeout;
        let mut activity = progress.subscribe();
        let slot = self.machine_slot.clone().acquire_owned();
        let _slot = tokio::select! {
            biased;
            permit = slot => match permit {
                Ok(permit) => permit,
                Err(_) => return failed_status(
                    acceptance,
                    BuildPlatformFailure::MachineUnavailable {
                        message: failure_message("machine build slot closed"),
                    },
                    MachineBuildCleanupOutcome::Confirmed,
                    BuildLogSummary::none(),
                ),
            },
            () = tokio::time::sleep_until(deadline) => {
                return BuildExecutorStatus {
                    acceptance,
                    state: BuildExecutorState::Failed {
                        failure: BuildExecutorStatusFailure::Stalled {
                            message: failure_message("build stalled waiting for the machine build slot"),
                        },
                        cleanup: MachineBuildCleanupOutcome::Confirmed,
                        log_summary: BuildLogSummary::none(),
                    },
                };
            }
            changed = cancel_rx.changed() => {
                let _ = changed;
                return BuildExecutorStatus {
                    acceptance,
                    state: BuildExecutorState::Cancelled {
                        cleanup: MachineBuildCleanupOutcome::Confirmed,
                        log_summary: BuildLogSummary::none(),
                    },
                };
            },
        };
        let operation_id = request.operation_id.clone();
        let platform = request.platform.clone();
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
            );
            tokio::pin!(task);
            let completion = loop {
                tokio::select! {
                    biased;
                    result = &mut task => break BuildTaskCompletion::Finished(Box::new(result)),
                    changed = cancel_rx.changed() => {
                        let _ = changed;
                        break BuildTaskCompletion::Cancelled;
                    }
                    changed = activity.changed() => {
                        if changed.is_ok() {
                            deadline = Instant::now() + timeout;
                            continue;
                        }
                    }
                    () = tokio::time::sleep_until(deadline) => {
                        break BuildTaskCompletion::TimedOut;
                    }
                }
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
            BuildTaskCompletion::Cancelled => BuildExecutorStatus {
                acceptance,
                state: BuildExecutorState::Cancelled {
                    cleanup,
                    log_summary,
                },
            },
            BuildTaskCompletion::TimedOut => BuildExecutorStatus {
                acceptance,
                state: BuildExecutorState::Failed {
                    failure: BuildExecutorStatusFailure::Stalled {
                        message: failure_message(match cleanup {
                            MachineBuildCleanupOutcome::Confirmed => {
                                "build made no verified progress before its stall budget elapsed"
                            }
                            MachineBuildCleanupOutcome::Unconfirmed => {
                                "build stalled and cleanup did not finish"
                            }
                        }),
                    },
                    cleanup,
                    log_summary,
                },
            },
            BuildTaskCompletion::Finished(result) => {
                finish_machine_build_status(*result, cleanup, acceptance, log_summary)
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

    async fn status(
        &self,
        acceptance: &BuildExecutorAcceptance,
    ) -> Result<BuildExecutorStatus, MachineBuildStatusDomainError> {
        let state = self.lifecycle.state.lock().await;
        if let Some(active) = state.active.get(&acceptance.operation_id) {
            if active.acceptance != *acceptance {
                return Err(MachineBuildStatusDomainError::NotFound {
                    acceptance: acceptance.clone(),
                });
            }
            if let Some(message) = &active.evidence_error {
                return Err(MachineBuildStatusDomainError::EvidenceUnavailable {
                    acceptance: acceptance.clone(),
                    message: failure_message(message.clone()),
                });
            }
            let (final_log_sequence, omitted_log_bytes) = active.progress.summary();
            return Ok(BuildExecutorStatus {
                acceptance: active.acceptance.clone(),
                state: BuildExecutorState::Running {
                    log_summary: BuildLogSummary::new(final_log_sequence, omitted_log_bytes),
                },
            });
        }
        drop(state);
        match self.status.read(&acceptance.operation_id) {
            Ok(Some(record)) if record.status.acceptance == *acceptance => Ok(record.status),
            Ok(Some(_)) | Ok(None) => Err(MachineBuildStatusDomainError::NotFound {
                acceptance: acceptance.clone(),
            }),
            Err(message) => Err(MachineBuildStatusDomainError::EvidenceUnavailable {
                acceptance: acceptance.clone(),
                message: failure_message(message),
            }),
        }
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
    Finished(Box<Result<MachineBuildOutput, BuildExecutionError>>),
    Cancelled,
    TimedOut,
}

struct MachineBuildOutput {
    acceptance: BuildExecutorAcceptance,
    image: PlatformImage,
    verified_source: VerifiedBuildSource,
    toolchain: ployz_core::operation::BuildToolchainEvidence,
    log_summary: BuildLogSummary,
}

impl MachineBuildOutput {
    fn into_result(self) -> BuildExecutorStartOk {
        BuildExecutorStartOk {
            acceptance: self.acceptance,
            completion: BuildExecutorCompletion {
                cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
                image: self.image,
                verified_source: self.verified_source,
                toolchain: self.toolchain,
                log_summary: self.log_summary,
            },
        }
    }
}

fn finish_machine_build_status(
    result: Result<MachineBuildOutput, BuildExecutionError>,
    cleanup: MachineBuildCleanupOutcome,
    acceptance: BuildExecutorAcceptance,
    log_summary: BuildLogSummary,
) -> BuildExecutorStatus {
    if cleanup == MachineBuildCleanupOutcome::Unconfirmed {
        return failed_status(
            acceptance,
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("build workspace cleanup did not finish successfully"),
            },
            cleanup,
            log_summary,
        );
    }
    match result {
        Ok(output) => BuildExecutorStatus::from_start_ok(output.into_result()),
        Err(BuildExecutionError::Cancelled { .. }) => BuildExecutorStatus {
            acceptance,
            state: BuildExecutorState::Cancelled {
                cleanup,
                log_summary,
            },
        },
        Err(BuildExecutionError::TimedOut { .. }) => BuildExecutorStatus {
            acceptance,
            state: BuildExecutorState::Failed {
                failure: BuildExecutorStatusFailure::Stalled {
                    message: failure_message("build made no verified progress"),
                },
                cleanup,
                log_summary,
            },
        },
        Err(BuildExecutionError::Platform { failure, .. }) => {
            failed_status(acceptance, failure, cleanup, log_summary)
        }
        Err(BuildExecutionError::Infrastructure {
            action, message, ..
        }) => failed_status(
            acceptance,
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message(format!("{action}: {message}")),
            },
            cleanup,
            log_summary,
        ),
    }
}

fn failed_status(
    acceptance: BuildExecutorAcceptance,
    failure: BuildPlatformFailure,
    cleanup: MachineBuildCleanupOutcome,
    log_summary: BuildLogSummary,
) -> BuildExecutorStatus {
    BuildExecutorStatus {
        acceptance,
        state: BuildExecutorState::Failed {
            failure: BuildExecutorStatusFailure::PlatformFailed { failure },
            cleanup,
            log_summary,
        },
    }
}

struct ResidualBuild {
    operation_id: OperationId,
    platform: OciPlatform,
    supervisor: AbortHandle,
    completion: Arc<BuildSupervisorCompletion>,
    acceptance: BuildExecutorAcceptance,
    progress: BuildLogProgress,
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
            actual: Box::new(request.assignment.clone()),
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

fn build_runtime_stopped() -> MachineBuildStartDomainError {
    MachineBuildStartDomainError::RuntimeStopped
}

fn local_platform() -> Result<OciPlatform, String> {
    ployz_build_executor::native_oci_platform()
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

pub(crate) async fn handle_build_status(
    machine_id: MachineId,
    runtime: Option<MachineBuildRuntime>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineBuildStatusRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let expected = BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    };
    if request.acceptance.assignment != expected {
        return machine_domain_error(MachineBuildStatusRpcResponse::DomainError {
            machine_id,
            error: MachineBuildStatusDomainError::NotFound {
                acceptance: request.acceptance,
            },
        });
    }
    let Some(runtime) = runtime else {
        return machine_domain_error(MachineBuildStatusRpcResponse::DomainError {
            machine_id,
            error: MachineBuildStatusDomainError::EvidenceUnavailable {
                acceptance: request.acceptance,
                message: failure_message("machine build runtime is unavailable"),
            },
        });
    };
    match runtime.status(&request.acceptance).await {
        Ok(status) => machine_success(MachineBuildStatusRpcResponse::Ok(
            MachineBuildStatusRpcOk::from((machine_id, status)),
        )),
        Err(error) => {
            machine_domain_error(MachineBuildStatusRpcResponse::DomainError { machine_id, error })
        }
    }
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
