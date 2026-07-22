use std::time::Duration;

use ployz_core::build::{
    BuildAdapterKind, BuildExecutorAcceptance, BuildExecutorAssignment,
    BuildExecutorCleanupOutcome, BuildExecutorEvidence, BuildExecutorStartDomainError,
    BuildExecutorStartOk, BuildExecutorStartRequest, BuildExecutorStartResponse,
    BuildExecutorSuccessCleanupEvidence, BuildExecutorSuccessCleanupOutcome, BuildLogSummary,
    BuildPlatformExecutorAssignment, build_control_request_timeout,
};
use ployz_core::image::{
    ImageBlobCheckOk, ImageBlobCheckRequest, ImageRpcDomainError, OciPlatform,
};
use ployz_core::operation::{
    BuildEvidence, BuildOperationFailure, BuildPlatformFailure, FailureMessage,
};
use ployz_nats::service_runtime::{NatsJsonServiceRequestError, request_json};
use ployz_nats::subjects::{
    BuildExecutorServiceEndpoint, MachineServiceEndpoint, build_executor_log,
    build_executor_service,
};

use crate::control::role_client::machine::call_machine;
use crate::control::sequencer::AcceptedBuildExecution;

use super::driver::{BuildOperationDriver, PlatformOutcome, failure_message, record_failure};
use super::log_stream::{MachineCallOrLog, next_machine_call_or_log};
use super::platform_session::PlatformLogSession;

pub(super) async fn run_external_executor_session(
    driver: &BuildOperationDriver,
    accepted: &AcceptedBuildExecution,
    assignment: BuildPlatformExecutorAssignment,
) -> Result<PlatformOutcome, BuildOperationFailure> {
    let BuildPlatformExecutorAssignment { platform, executor } = assignment;
    let BuildExecutorAssignment::External {
        pool_id,
        executor_id,
        image_seed,
    } = &executor
    else {
        unreachable!("external executor session requires external assignment")
    };
    let id = &accepted.submission.operation_id;
    let evidence_executor = BuildExecutorEvidence::from_assignment(&executor);
    let logs = driver
        .client
        .subscribe(build_executor_log(pool_id, executor_id, id))
        .await
        .map_err(|error| {
            external_platform_failure(
                platform.clone(),
                &executor,
                BuildPlatformFailure::ExecutorUnavailable {
                    message: failure_message(error),
                },
            )
        })?;
    if !driver.active.claim_executor_start(id, &executor).await {
        return Ok(PlatformOutcome::CancelledBeforeStart);
    }
    let mut log_session = PlatformLogSession::new(
        driver.controllers.repository(),
        id,
        &platform,
        executor.clone(),
        logs,
    );
    if !image_seed_is_reachable(driver, image_seed).await {
        driver
            .active
            .release_executor_start_claim(id, &executor)
            .await;
        let failure = BuildPlatformFailure::ImageSeedUnavailable {
            image_seed: image_seed.clone(),
        };
        record_external_platform_failure(
            driver,
            id,
            platform.clone(),
            evidence_executor,
            failure.clone(),
        )
        .await?;
        return Ok(PlatformOutcome::Failed(external_platform_failure(
            platform, &executor, failure,
        )));
    }
    if !driver
        .active
        .executor_start_is_authorized(id, &executor)
        .await
    {
        driver
            .active
            .release_executor_start_claim(id, &executor)
            .await;
        return Ok(PlatformOutcome::CancelledBeforeStart);
    }
    let request = BuildExecutorStartRequest {
        operation_id: id.clone(),
        assignment: executor.clone(),
        source: accepted.source.clone(),
        adapter: accepted.submission.adapter.clone(),
        platform: platform.clone(),
        timeout_millis: execution_timeout_millis(driver.timeout),
    };
    let result =
        receive_external_result(driver, pool_id, executor_id, &request, &mut log_session).await?;
    let result = match result {
        ExternalExecutorResult::Response(response) => *response,
        ExternalExecutorResult::Transport(error) if external_request_timed_out(&error) => {
            log_session.drain(BuildLogSummary::none()).await?;
            return Ok(PlatformOutcome::TimedOut {
                executor,
                message: external_request_failure(error),
                cleanup: BuildExecutorCleanupOutcome::Unconfirmed,
            });
        }
        ExternalExecutorResult::Transport(error) => {
            let failure = BuildPlatformFailure::ExecutorUnavailable {
                message: external_request_failure(error),
            };
            record_external_platform_failure(
                driver,
                id,
                platform.clone(),
                evidence_executor,
                failure.clone(),
            )
            .await?;
            return Ok(PlatformOutcome::Failed(external_platform_failure(
                platform, &executor, failure,
            )));
        }
    };
    let summary = match result {
        Ok(ok) => ExternalBuildSummary::Completed(ok),
        Err(BuildExecutorStartDomainError::PlatformFailed {
            acceptance,
            failure,
            log_summary,
        }) => ExternalBuildSummary::Failed {
            acceptance: *acceptance,
            failure,
            log_summary,
        },
        Err(BuildExecutorStartDomainError::Cancelled {
            acceptance,
            cleanup,
            log_summary,
        }) => ExternalBuildSummary::Cancelled {
            acceptance: *acceptance,
            cleanup,
            log_summary,
        },
        Err(BuildExecutorStartDomainError::TimedOut {
            acceptance,
            message,
            cleanup,
            log_summary,
        }) => ExternalBuildSummary::TimedOut {
            acceptance: *acceptance,
            message,
            cleanup,
            log_summary,
        },
        Err(error) => {
            let failure = map_pre_acceptance_error(error);
            record_external_platform_failure(
                driver,
                id,
                platform.clone(),
                evidence_executor,
                failure.clone(),
            )
            .await?;
            return Ok(PlatformOutcome::Failed(external_platform_failure(
                platform, &executor, failure,
            )));
        }
    };
    let expected = BuildExecutorAcceptance::from_start_request(&request);
    if summary.acceptance() != &expected {
        let failure = BuildPlatformFailure::ExecutorUnavailable {
            message: failure_message(
                "external Build Executor returned mismatched acceptance provenance",
            ),
        };
        record_external_platform_failure(
            driver,
            id,
            platform.clone(),
            evidence_executor,
            failure.clone(),
        )
        .await?;
        return Ok(PlatformOutcome::Failed(external_platform_failure(
            platform, &executor, failure,
        )));
    }
    log_session.drain(summary.log_summary()).await?;
    let ExternalBuildSummary::Completed(ok) = summary else {
        return match summary {
            ExternalBuildSummary::Failed { failure, .. } => {
                record_external_platform_failure(
                    driver,
                    id,
                    platform.clone(),
                    evidence_executor,
                    failure.clone(),
                )
                .await?;
                Ok(PlatformOutcome::Failed(external_platform_failure(
                    platform, &executor, failure,
                )))
            }
            ExternalBuildSummary::Cancelled { cleanup, .. } => {
                Ok(PlatformOutcome::Cancelled { executor, cleanup })
            }
            ExternalBuildSummary::TimedOut {
                message, cleanup, ..
            } => Ok(PlatformOutcome::TimedOut {
                executor,
                message,
                cleanup,
            }),
            ExternalBuildSummary::Completed(_) => unreachable!(),
        };
    };
    if ok.completion.image.seed != *image_seed {
        let failure = BuildPlatformFailure::ExecutorUnavailable {
            message: failure_message(
                "external Build Executor returned an image from a different seed",
            ),
        };
        record_external_platform_failure(
            driver,
            id,
            platform.clone(),
            evidence_executor,
            failure.clone(),
        )
        .await?;
        return Ok(PlatformOutcome::Failed(external_platform_failure(
            platform, &executor, failure,
        )));
    }
    let BuildExecutorStartOk {
        acceptance: _,
        completion:
            ployz_core::build::BuildExecutorCompletion {
                cleanup:
                    BuildExecutorSuccessCleanupEvidence {
                        outcome: BuildExecutorSuccessCleanupOutcome::Confirmed,
                    },
                image,
                verified_source,
                toolchain,
                log_summary: _,
            },
    } = ok;
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::VerifiedSource {
                platform: platform.clone(),
                executor: evidence_executor.clone(),
                source: verified_source,
            },
        )
        .await
        .map_err(record_failure)?;
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::ToolchainVerified {
                platform: platform.clone(),
                executor: evidence_executor.clone(),
                toolchain,
            },
        )
        .await
        .map_err(record_failure)?;
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::PlatformCompleted {
                platform: platform.clone(),
                executor: evidence_executor,
                image: image.clone(),
            },
        )
        .await
        .map_err(record_failure)?;
    Ok(PlatformOutcome::Completed { platform, image })
}

async fn image_seed_is_reachable(
    driver: &BuildOperationDriver,
    image_seed: &ployz_core::ids::MachineId,
) -> bool {
    call_machine::<ImageBlobCheckOk, ImageRpcDomainError>(
        &driver.client,
        Duration::from_secs(3),
        image_seed,
        MachineServiceEndpoint::ImageBlobCheck,
        &ImageBlobCheckRequest {
            digests: Vec::new(),
        },
    )
    .await
    .is_ok_and(|answer| answer.present.is_empty())
}

async fn receive_external_result(
    driver: &BuildOperationDriver,
    pool_id: &ployz_core::build::BuildPoolId,
    executor_id: &ployz_core::build::BuildExecutorId,
    request: &BuildExecutorStartRequest,
    log_session: &mut PlatformLogSession<'_>,
) -> Result<ExternalExecutorResult, BuildOperationFailure> {
    let call = request_json::<_, BuildExecutorStartResponse>(
        &driver.client,
        build_executor_service(
            pool_id,
            executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        ),
        request,
        build_control_request_timeout(driver.timeout),
    );
    tokio::pin!(call);
    loop {
        let logs_open = log_session.logs_open();
        match next_machine_call_or_log(call.as_mut(), log_session.logs_mut(), logs_open).await {
            MachineCallOrLog::Call(Ok(BuildExecutorStartResponse::Ok(ok))) => {
                return Ok(ExternalExecutorResult::Response(Box::new(Ok(*ok))));
            }
            MachineCallOrLog::Call(Ok(BuildExecutorStartResponse::DomainError { error })) => {
                return Ok(ExternalExecutorResult::Response(Box::new(Err(error))));
            }
            MachineCallOrLog::Call(Err(error)) => {
                return Ok(ExternalExecutorResult::Transport(error));
            }
            MachineCallOrLog::LogsClosed => log_session.record_message(None).await?,
            MachineCallOrLog::Log(message) => log_session.record_message(message).await?,
        }
    }
}

fn external_request_timed_out(error: &NatsJsonServiceRequestError) -> bool {
    matches!(
        error,
        NatsJsonServiceRequestError::Request {
            failure: ployz_nats::service_runtime::NatsServiceRequestFailure::TimedOut,
        }
    )
}

fn external_request_failure(error: NatsJsonServiceRequestError) -> FailureMessage {
    failure_message(format!("external Build Executor request failed: {error}"))
}

async fn record_external_platform_failure(
    driver: &BuildOperationDriver,
    operation_id: &ployz_core::ids::OperationId,
    platform: OciPlatform,
    executor: BuildExecutorEvidence,
    failure: BuildPlatformFailure,
) -> Result<(), BuildOperationFailure> {
    driver
        .controllers
        .repository()
        .record_build_evidence(
            operation_id,
            BuildEvidence::PlatformFailed {
                platform,
                executor,
                failure,
            },
        )
        .await
        .map_err(record_failure)?;
    Ok(())
}

fn external_platform_failure(
    platform: OciPlatform,
    executor: &BuildExecutorAssignment,
    failure: BuildPlatformFailure,
) -> BuildOperationFailure {
    BuildOperationFailure::ExternalPlatformFailed {
        platform,
        executor: BuildExecutorEvidence::from_assignment(executor),
        failure,
    }
}

fn map_pre_acceptance_error(error: BuildExecutorStartDomainError) -> BuildPlatformFailure {
    match error {
        BuildExecutorStartDomainError::PlatformMismatch { expected, actual } => {
            BuildPlatformFailure::PlatformMismatch { expected, actual }
        }
        BuildExecutorStartDomainError::ImageSeedUnavailable { image_seed } => {
            BuildPlatformFailure::ImageSeedUnavailable { image_seed }
        }
        BuildExecutorStartDomainError::AssignmentMismatch { .. }
        | BuildExecutorStartDomainError::ExecutorIdentityMismatch { .. }
        | BuildExecutorStartDomainError::OperationIdentityMismatch { .. }
        | BuildExecutorStartDomainError::RuntimeUnavailable
        | BuildExecutorStartDomainError::RuntimeStopped
        | BuildExecutorStartDomainError::ToolchainUnavailable {
            adapter: BuildAdapterKind::Dockerfile | BuildAdapterKind::Railpack,
        }
        | BuildExecutorStartDomainError::InvalidTimeout { .. }
        | BuildExecutorStartDomainError::AlreadyRunning => {
            BuildPlatformFailure::ExecutorUnavailable {
                message: failure_message(
                    "external Build Executor rejected the assigned build before acceptance",
                ),
            }
        }
        BuildExecutorStartDomainError::PlatformFailed { failure, .. } => failure,
        BuildExecutorStartDomainError::Cancelled { .. } => {
            BuildPlatformFailure::ExecutorUnavailable {
                message: failure_message(
                    "external Build Executor cancelled before acceptance processing",
                ),
            }
        }
        BuildExecutorStartDomainError::TimedOut { message, .. } => {
            BuildPlatformFailure::ExecutorUnavailable { message }
        }
    }
}

fn execution_timeout_millis(timeout: Duration) -> u64 {
    timeout.as_millis().try_into().unwrap_or(u64::MAX)
}

enum ExternalBuildSummary {
    Completed(BuildExecutorStartOk),
    Failed {
        acceptance: BuildExecutorAcceptance,
        failure: BuildPlatformFailure,
        log_summary: BuildLogSummary,
    },
    Cancelled {
        acceptance: BuildExecutorAcceptance,
        cleanup: BuildExecutorCleanupOutcome,
        log_summary: BuildLogSummary,
    },
    TimedOut {
        acceptance: BuildExecutorAcceptance,
        message: FailureMessage,
        cleanup: BuildExecutorCleanupOutcome,
        log_summary: BuildLogSummary,
    },
}

enum ExternalExecutorResult {
    Response(Box<Result<BuildExecutorStartOk, BuildExecutorStartDomainError>>),
    Transport(NatsJsonServiceRequestError),
}

impl ExternalBuildSummary {
    fn acceptance(&self) -> &BuildExecutorAcceptance {
        match self {
            Self::Completed(ok) => &ok.acceptance,
            Self::Failed { acceptance, .. }
            | Self::Cancelled { acceptance, .. }
            | Self::TimedOut { acceptance, .. } => acceptance,
        }
    }

    fn log_summary(&self) -> BuildLogSummary {
        match self {
            Self::Completed(ok) => ok.completion.log_summary,
            Self::Failed { log_summary, .. }
            | Self::Cancelled { log_summary, .. }
            | Self::TimedOut { log_summary, .. } => *log_summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{BuildExecutorId, BuildPoolId, GitSource, VerifiedBuildSource};
    use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::image::OciDigest;
    use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildToolchainEvidence};
    use ployz_nats::service_runtime::NatsServiceRequestFailure;

    #[test]
    fn seed_loss_and_external_runtime_failures_keep_distinct_provenance() {
        let seed = ployz_core::ids::MachineId::try_new("seed-a").expect("seed");
        assert_eq!(
            map_pre_acceptance_error(BuildExecutorStartDomainError::ImageSeedUnavailable {
                image_seed: seed.clone(),
            }),
            BuildPlatformFailure::ImageSeedUnavailable { image_seed: seed }
        );
        assert!(matches!(
            map_pre_acceptance_error(BuildExecutorStartDomainError::RuntimeUnavailable),
            BuildPlatformFailure::ExecutorUnavailable { .. }
        ));
        assert!(matches!(
            map_pre_acceptance_error(BuildExecutorStartDomainError::OperationIdentityMismatch {
                expected: OperationId::try_new("expected-operation").expect("operation"),
                actual: OperationId::try_new("other-operation").expect("operation"),
            }),
            BuildPlatformFailure::ExecutorUnavailable { .. }
        ));
    }

    #[test]
    fn only_transport_timeout_uses_timed_out_outcome() {
        assert!(external_request_timed_out(
            &NatsJsonServiceRequestError::Request {
                failure: NatsServiceRequestFailure::TimedOut,
            }
        ));
        assert!(!external_request_timed_out(
            &NatsJsonServiceRequestError::Request {
                failure: NatsServiceRequestFailure::NoResponders,
            }
        ));
    }

    #[test]
    fn external_success_roundtrip_preserves_cleanup_and_subject_provenance() {
        let pool_id = BuildPoolId::try_new("pool-a").expect("pool");
        let executor_id = BuildExecutorId::try_new("executor-a").expect("executor");
        let operation_id = OperationId::try_new("build-1").expect("operation");
        let image_seed = MachineId::try_new("seed-a").expect("seed");
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let assignment = BuildExecutorAssignment::External {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
            image_seed: image_seed.clone(),
        };
        let source: ployz_core::build::BuildSource = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source")
        .into();
        let digest = OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let request = BuildExecutorStartRequest {
            operation_id: operation_id.clone(),
            assignment: assignment.clone(),
            source: source.clone(),
            adapter: ployz_core::build::BuildAdapter::Dockerfile {
                dockerfile: ployz_core::build::BuildContextPath::try_new("Dockerfile")
                    .expect("dockerfile"),
                target: None,
            },
            platform: platform.clone(),
            timeout_millis: 1_000,
        };
        let response = BuildExecutorStartResponse::Ok(Box::new(BuildExecutorStartOk {
            acceptance: BuildExecutorAcceptance::from_start_request(&request),
            completion: ployz_core::build::BuildExecutorCompletion {
                cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
                image: PlatformImage {
                    seed: image_seed,
                    manifest_digest: digest.clone(),
                    image_id: digest.clone(),
                    availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                        .expect("expiry"),
                },
                verified_source: VerifiedBuildSource::from_source(&source),
                toolchain: BuildToolchainEvidence {
                    buildkit_image: digest,
                    adapter: BuildAdapterToolchainEvidence::Dockerfile,
                },
                log_summary: BuildLogSummary::new(3, 0),
            },
        }));
        let encoded = serde_json::to_vec(&response).expect("encode response");
        let decoded: BuildExecutorStartResponse =
            serde_json::from_slice(&encoded).expect("decode response");
        let BuildExecutorStartResponse::Ok(ok) = decoded else {
            panic!("success response")
        };
        assert_eq!(ok.acceptance.assignment, assignment);
        assert_eq!(
            ok.completion.cleanup,
            BuildExecutorSuccessCleanupEvidence::confirmed()
        );
        assert_eq!(
            build_executor_service(
                &pool_id,
                &executor_id,
                BuildExecutorServiceEndpoint::BuildStart,
            ),
            "plz.v1.rpc.build_executor.command.pool-a.executor-a.build.start"
        );
        assert_eq!(
            build_executor_log(&pool_id, &executor_id, &operation_id),
            "plz.v1.signal.build_executor.pool-a.executor-a.build.operation.build-1.log"
        );
    }
}
