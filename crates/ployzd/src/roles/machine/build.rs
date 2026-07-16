use super::execution::build::{
    BuildExecutionError, BuildExecutionRequest, BuildExecutionResult, BuildLogProgress,
    DockerBuildExecutor,
};
use super::images::AvailableImageService;
use super::protocol::{
    BuildLogSummary, MachineBuildCancelOutcome, MachineBuildCancelRpcOk,
    MachineBuildCancelRpcRequest, MachineBuildCancelRpcResponse, MachineBuildCleanupOutcome,
    MachineBuildStartDomainError, MachineBuildStartRpcOk, MachineBuildStartRpcRequest,
    MachineBuildStartRpcResponse,
};
use super::response::{failure_message, machine_domain_error, machine_success};
use ployz_core::build::{
    BUILD_FORCE_CLEANUP_TIMEOUT, BUILD_MAX_EXECUTION_TIMEOUT, BUILD_TASK_DRAIN_TIMEOUT,
};
use ployz_core::deploy::PlatformImage;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::BuildPlatformFailure;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore, watch};
use tokio::time::Instant;

#[derive(Clone)]
pub(crate) struct MachineBuildRuntime {
    machine_id: MachineId,
    effects: BuildEffects,
    active: Arc<Mutex<BTreeMap<OperationId, watch::Sender<bool>>>>,
    machine_slot: Arc<Semaphore>,
    local_platform: OciPlatform,
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
            active: Arc::new(Mutex::new(BTreeMap::new())),
            machine_slot: Arc::new(Semaphore::new(1)),
            local_platform: local_platform()?,
        })
    }

    pub(crate) async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        match &self.effects {
            BuildEffects::Docker(effects) => effects.executor.recover_orphans().await,
            #[cfg(test)]
            BuildEffects::Test(_) => Ok(()),
        }
    }

    async fn start(
        &self,
        request: MachineBuildStartRpcRequest,
    ) -> Result<MachineBuildStartRpcOk, MachineBuildStartDomainError> {
        if request.platform != self.local_platform {
            return Err(MachineBuildStartDomainError::PlatformFailed {
                failure: BuildPlatformFailure::PlatformMismatch {
                    expected: request.platform,
                    actual: self.local_platform.clone(),
                },
                log_summary: BuildLogSummary::none(),
            });
        }
        let timeout = requested_build_timeout(request.timeout_millis)?;
        let (cancel, cancel_rx) = watch::channel(false);
        {
            let mut active = self.active.lock().await;
            if active.contains_key(&request.operation_id) {
                return Err(MachineBuildStartDomainError::AlreadyRunning);
            }
            active.insert(request.operation_id.clone(), cancel.clone());
        }
        let runtime = self.clone();
        let operation_id = request.operation_id.clone();
        let deadline = Instant::now() + timeout;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = runtime
                .run_build(request, cancel, cancel_rx, deadline)
                .await;
            runtime.active.lock().await.remove(&operation_id);
            let _ = result_tx.send(result);
        });
        result_rx.await.unwrap_or_else(|_| {
            Err(MachineBuildStartDomainError::PlatformFailed {
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
        cancel: watch::Sender<bool>,
        mut cancel_rx: watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<MachineBuildStartRpcOk, MachineBuildStartDomainError> {
        let slot = self.machine_slot.clone().acquire_owned();
        let _slot = tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => {
                return Err(MachineBuildStartDomainError::TimedOut {
                    message: failure_message("build timed out waiting for the machine build slot"),
                    cleanup: MachineBuildCleanupOutcome::Confirmed,
                    log_summary: BuildLogSummary::none(),
                });
            }
            changed = cancel_rx.changed() => {
                let _ = changed;
                return Err(MachineBuildStartDomainError::Cancelled {
                    cleanup: MachineBuildCleanupOutcome::Confirmed,
                    log_summary: BuildLogSummary::none(),
                });
            }
            permit = slot => permit.map_err(|_| MachineBuildStartDomainError::PlatformFailed {
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
        let task = tokio::spawn(async move {
            task_effects
                .execute_and_ingest(machine_id, request, task_cancel_rx, task_progress, deadline)
                .await
        });
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
            result = &mut task => BuildTaskCompletion::Finished(Box::new(join_build(result))),
        };
        if !matches!(completion, BuildTaskCompletion::Finished(_)) {
            let _ = cancel.send(true);
            if tokio::time::timeout(BUILD_TASK_DRAIN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        let cleanup = self.effects.force_cleanup(&operation_id, &platform).await;
        let (final_log_sequence, omitted_log_bytes) = progress.summary();
        let log_summary = BuildLogSummary::new(final_log_sequence, omitted_log_bytes);
        match completion {
            BuildTaskCompletion::Cancelled => Err(MachineBuildStartDomainError::Cancelled {
                cleanup,
                log_summary,
            }),
            BuildTaskCompletion::TimedOut => Err(MachineBuildStartDomainError::TimedOut {
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
                        failure: BuildPlatformFailure::MachineUnavailable {
                            message: failure_message(
                                "build workspace cleanup did not finish successfully",
                            ),
                        },
                        log_summary,
                    });
                }
                (*result).map_err(|error| machine_build_error(error, cleanup))
            }
        }
    }

    async fn cancel(&self, operation_id: &OperationId) -> MachineBuildCancelOutcome {
        let active = self.active.lock().await;
        let Some(cancel) = active.get(operation_id) else {
            return MachineBuildCancelOutcome::NotRunning;
        };
        let _ = cancel.send(true);
        MachineBuildCancelOutcome::Requested
    }
}

enum BuildTaskCompletion {
    Finished(Box<Result<MachineBuildStartRpcOk, BuildExecutionError>>),
    Cancelled,
    TimedOut,
}

impl BuildEffects {
    async fn execute_and_ingest(
        &self,
        machine_id: MachineId,
        request: MachineBuildStartRpcRequest,
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
                images
                    .ingest_build_layout(&result.layout)
                    .await
                    .map_err(|message| BuildExecutionError::Platform {
                        failure: BuildPlatformFailure::ImagePushFailed {
                            message: failure_message(message),
                        },
                        log_summary,
                    })?;
                Ok(MachineBuildStartRpcOk {
                    machine_id: machine_id.clone(),
                    image: PlatformImage {
                        seed: machine_id,
                        manifest_digest: result.layout.manifest_digest().clone(),
                        image_id: result.layout.image_id().clone(),
                    },
                    verified_commit: result.verified_commit,
                    toolchain: result.toolchain,
                    log_summary,
                })
            }
            #[cfg(test)]
            Self::Test(effects) => {
                let _ = deadline;
                effects.execute_and_ingest(log_progress).await
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
        return Err(MachineBuildStartDomainError::TimedOut {
            message: failure_message("build timeout must be between 1ms and 30m"),
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary::none(),
        });
    }
    Ok(timeout)
}

fn join_build(
    result: Result<Result<MachineBuildStartRpcOk, BuildExecutionError>, tokio::task::JoinError>,
) -> Result<MachineBuildStartRpcOk, BuildExecutionError> {
    result.map_err(|error| BuildExecutionError::Infrastructure {
        action: "join machine build task",
        message: error.to_string(),
        log_summary: BuildLogSummary::none(),
    })?
}

fn machine_build_error(
    error: BuildExecutionError,
    cleanup: MachineBuildCleanupOutcome,
) -> MachineBuildStartDomainError {
    let log_summary = error.log_summary();
    match error {
        BuildExecutionError::Cancelled { .. } => MachineBuildStartDomainError::Cancelled {
            cleanup,
            log_summary,
        },
        BuildExecutionError::TimedOut { .. } => MachineBuildStartDomainError::TimedOut {
            message: failure_message("build exceeded its operation deadline"),
            cleanup,
            log_summary,
        },
        BuildExecutionError::Platform { failure, .. } => {
            MachineBuildStartDomainError::PlatformFailed {
                failure,
                log_summary,
            }
        }
        BuildExecutionError::Infrastructure {
            action, message, ..
        } => MachineBuildStartDomainError::PlatformFailed {
            failure: BuildPlatformFailure::MachineUnavailable {
                message: failure_message(format!("{action}: {message}")),
            },
            log_summary,
        },
    }
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
    let Some(runtime) = runtime else {
        return machine_domain_error(MachineBuildStartRpcResponse::DomainError {
            machine_id,
            error: MachineBuildStartDomainError::PlatformFailed {
                failure: BuildPlatformFailure::MachineUnavailable {
                    message: failure_message("machine build runtime is unavailable"),
                },
                log_summary: BuildLogSummary::none(),
            },
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
    let Some(runtime) = runtime else {
        return machine_domain_error(MachineBuildCancelRpcResponse::DomainError {
            machine_id,
            error: super::protocol::MachineBuildCancelDomainError::CancelFailed {
                message: failure_message("machine build runtime is unavailable"),
            },
        });
    };
    let outcome = runtime.cancel(&request.operation_id).await;
    machine_success(MachineBuildCancelRpcResponse::Ok(MachineBuildCancelRpcOk {
        machine_id,
        outcome,
    }))
}

#[cfg(test)]
mod tests;
