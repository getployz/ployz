use super::execution::build::{BuildExecutionError, BuildExecutionResult, DockerBuildExecutor};
use super::images::AvailableImageService;
use super::protocol::{
    MachineBuildCancelOutcome, MachineBuildCancelRpcOk, MachineBuildCancelRpcRequest,
    MachineBuildCancelRpcResponse, MachineBuildStartDomainError, MachineBuildStartRpcOk,
    MachineBuildStartRpcRequest, MachineBuildStartRpcResponse,
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
    executor: DockerBuildExecutor,
    image_state: Option<AvailableImageService>,
    active: Arc<Mutex<BTreeMap<OperationId, watch::Sender<bool>>>>,
    machine_slot: Arc<Semaphore>,
    local_platform: OciPlatform,
}

impl MachineBuildRuntime {
    pub(crate) fn new(
        machine_id: MachineId,
        executor: DockerBuildExecutor,
        image_state: Option<AvailableImageService>,
    ) -> Result<Self, String> {
        Ok(Self {
            machine_id,
            executor,
            image_state,
            active: Arc::new(Mutex::new(BTreeMap::new())),
            machine_slot: Arc::new(Semaphore::new(1)),
            local_platform: local_platform()?,
        })
    }

    pub(crate) async fn recover_orphans(&self) -> Result<(), BuildExecutionError> {
        self.executor.recover_orphans().await
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
                final_log_sequence: 0,
                omitted_log_bytes: 0,
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
                final_log_sequence: 0,
                omitted_log_bytes: 0,
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
            permit = slot => permit.map_err(|_| MachineBuildStartDomainError::PlatformFailed {
                failure: BuildPlatformFailure::MachineUnavailable {
                    message: failure_message("machine build slot closed"),
                },
                final_log_sequence: 0,
                omitted_log_bytes: 0,
            })?,
            changed = cancel_rx.changed() => {
                let _ = changed;
                return Err(MachineBuildStartDomainError::Cancelled {
                    final_log_sequence: 0,
                    omitted_log_bytes: 0,
                });
            }
            () = tokio::time::sleep_until(deadline) => {
                return Err(MachineBuildStartDomainError::TimedOut {
                    message: failure_message("build timed out waiting for the machine build slot"),
                    final_log_sequence: 0,
                    omitted_log_bytes: 0,
                });
            }
        };
        let executor = self.executor.clone();
        let cleanup_executor = self.executor.clone();
        let operation_id = request.operation_id.clone();
        let cleanup_operation_id = operation_id.clone();
        let source = request.source;
        let adapter = request.adapter;
        let platform = request.platform;
        let cleanup_platform = platform.clone();
        let task = tokio::spawn(async move {
            executor
                .execute(&operation_id, &source, &adapter, &platform, cancel_rx)
                .await
        });
        tokio::pin!(task);
        let result = tokio::select! {
            result = &mut task => join_build(result),
            () = tokio::time::sleep_until(deadline) => {
                let _ = cancel.send(true);
                match tokio::time::timeout(BUILD_TASK_DRAIN_TIMEOUT, &mut task).await {
                    Ok(result) => {
                        let cleanup = join_build(result);
                        let (final_log_sequence, omitted_log_bytes) = cleanup
                            .as_ref()
                            .err()
                            .map(BuildExecutionError::log_summary)
                            .unwrap_or((0, 0));
                        let cleanup = tokio::time::timeout(
                            BUILD_FORCE_CLEANUP_TIMEOUT,
                            cleanup_executor
                                .force_cleanup(&cleanup_operation_id, &cleanup_platform),
                        )
                        .await;
                        let message = match cleanup {
                            Ok(Ok(())) => "build exceeded its operation deadline",
                            Ok(Err(_)) | Err(_) => {
                                "build exceeded its deadline and cleanup did not finish"
                            }
                        };
                        return Err(MachineBuildStartDomainError::TimedOut {
                            message: failure_message(message),
                            final_log_sequence,
                            omitted_log_bytes,
                        });
                    }
                    Err(_) => {
                        task.abort();
                        let cleanup = tokio::time::timeout(
                            BUILD_FORCE_CLEANUP_TIMEOUT,
                            cleanup_executor
                                .force_cleanup(&cleanup_operation_id, &cleanup_platform),
                        )
                        .await;
                        let message = match cleanup {
                            Ok(Ok(())) => "build cleanup required task termination",
                            Ok(Err(_)) | Err(_) => {
                                "build cleanup did not finish before its deadline"
                            }
                        };
                        return Err(MachineBuildStartDomainError::TimedOut {
                            message: failure_message(message),
                            final_log_sequence: 0,
                            omitted_log_bytes: 0,
                        });
                    }
                }
            }
        };
        let BuildExecutionResult {
            layout,
            verified_commit,
            toolchain,
            final_log_sequence,
            omitted_log_bytes,
        } = result.map_err(machine_build_error)?;
        let ingest = match &self.image_state {
            Some(images) => images
                .ingest_build_layout(&layout)
                .await
                .map_err(|message| MachineBuildStartDomainError::PlatformFailed {
                    failure: BuildPlatformFailure::ImagePushFailed {
                        message: failure_message(message),
                    },
                    final_log_sequence,
                    omitted_log_bytes,
                }),
            None => Err(MachineBuildStartDomainError::PlatformFailed {
                failure: BuildPlatformFailure::MachineUnavailable {
                    message: failure_message("machine image content service is unavailable"),
                },
                final_log_sequence,
                omitted_log_bytes,
            }),
        };
        let cleanup = tokio::time::timeout(
            BUILD_FORCE_CLEANUP_TIMEOUT,
            self.executor
                .force_cleanup(&cleanup_operation_id, &cleanup_platform),
        )
        .await;
        match cleanup {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(MachineBuildStartDomainError::PlatformFailed {
                    failure: BuildPlatformFailure::MachineUnavailable {
                        message: failure_message(format!(
                            "build workspace cleanup failed: {error}"
                        )),
                    },
                    final_log_sequence,
                    omitted_log_bytes,
                });
            }
            Err(_) => {
                return Err(MachineBuildStartDomainError::PlatformFailed {
                    failure: BuildPlatformFailure::MachineUnavailable {
                        message: failure_message("build workspace cleanup timed out"),
                    },
                    final_log_sequence,
                    omitted_log_bytes,
                });
            }
        }
        ingest?;
        Ok(MachineBuildStartRpcOk {
            machine_id: self.machine_id.clone(),
            image: PlatformImage {
                seed: self.machine_id.clone(),
                manifest_digest: layout.manifest_digest().clone(),
                image_id: layout.image_id().clone(),
            },
            verified_commit,
            toolchain,
            final_log_sequence,
            omitted_log_bytes,
        })
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

fn requested_build_timeout(
    timeout_millis: u64,
) -> Result<std::time::Duration, MachineBuildStartDomainError> {
    let timeout = std::time::Duration::from_millis(timeout_millis);
    if timeout.is_zero() || timeout > BUILD_MAX_EXECUTION_TIMEOUT {
        return Err(MachineBuildStartDomainError::TimedOut {
            message: failure_message("build timeout must be between 1ms and 30m"),
            final_log_sequence: 0,
            omitted_log_bytes: 0,
        });
    }
    Ok(timeout)
}

fn join_build(
    result: Result<Result<BuildExecutionResult, BuildExecutionError>, tokio::task::JoinError>,
) -> Result<BuildExecutionResult, BuildExecutionError> {
    result.map_err(|error| BuildExecutionError::Infrastructure {
        action: "join machine build task",
        message: error.to_string(),
        final_log_sequence: 0,
        omitted_log_bytes: 0,
    })?
}

fn machine_build_error(error: BuildExecutionError) -> MachineBuildStartDomainError {
    let (final_log_sequence, omitted_log_bytes) = error.log_summary();
    match error {
        BuildExecutionError::Cancelled { .. } => MachineBuildStartDomainError::Cancelled {
            final_log_sequence,
            omitted_log_bytes,
        },
        BuildExecutionError::Platform { failure, .. } => {
            MachineBuildStartDomainError::PlatformFailed {
                failure,
                final_log_sequence,
                omitted_log_bytes,
            }
        }
        BuildExecutionError::Infrastructure {
            action, message, ..
        } => MachineBuildStartDomainError::PlatformFailed {
            failure: BuildPlatformFailure::MachineUnavailable {
                message: failure_message(format!("{action}: {message}")),
            },
            final_log_sequence,
            omitted_log_bytes,
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
                final_log_sequence: 0,
                omitted_log_bytes: 0,
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
mod tests {
    use super::*;
    use ployz_core::machine::rpc::MachineRpcResponse;

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
}
