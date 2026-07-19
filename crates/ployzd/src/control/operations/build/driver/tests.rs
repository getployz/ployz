use std::pin::Pin;

use super::*;
use crate::control::operation_evidence::OperationRepository;
use crate::control::sequencer::{
    BuildSubmitCommand, MachineAddBootstrapConfig, OperationControllers,
};
use crate::control::store::CoreStore;
use crate::roles::machine::protocol::{
    MachineBuildLogFrame, MachineBuildStartRpcRequest, MachineBuildStartRpcResponse,
};
use crate::tasks::TaskRegistry;
use futures_util::StreamExt;
use ployz_core::build::{
    BuildAdapter, BuildCacheScope, BuildExecutorCancelOk, BuildExecutorCleanupOutcome,
    BuildExecutorStartOk, BuildExecutorSuccessCleanupEvidence, BuildPlatforms, BuildTarget,
    GitSource, VerifiedGitCommit,
};
use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::install::{
    DEFAULT_MACHINE_BOOTSTRAP_URL, InstallArtifactVersion, InstallSha256Digest, MachineBootstrapUrl,
};
use ployz_core::operation::{
    BuildAdapterToolchainEvidence, BuildEvidence, BuildLogChunk, BuildOperationState,
    BuildToolchainEvidence, OperationEvent, OperationEventReplayRequest,
};
use ployz_nats::subjects::machine_service;
use ployz_test_support::ids::{event_replay_limit, event_sequence, machine_id, operation_id};

fn cluster_evidence(machine_id: &MachineId) -> BuildExecutorEvidence {
    BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    })
}

fn executor_acceptance(
    operation_id: &OperationId,
    machine_id: &MachineId,
    platform: &OciPlatform,
) -> BuildExecutorAcceptance {
    BuildExecutorAcceptance {
        operation_id: operation_id.clone(),
        assignment: BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        },
        platform: platform.clone(),
    }
}

#[tokio::test]
async fn platform_rpc_failure_records_real_evidence_and_publishes_no_image_index() {
    let amd64_machine = machine_id("machine_amd64");
    let arm64_machine = machine_id("machine_arm64");
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        amd64_machine.clone(),
        arm64_machine.clone(),
    ])
    .await;
    let store = CoreStore::open_in_memory().await.expect("core store opens");
    let repository = OperationRepository::open(store, nats.controller.clone());
    let controllers = OperationControllers::new(
        repository.clone(),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    );
    let amd64 = OciPlatform::try_new("linux", "amd64").expect("amd64 platform");
    let arm64 = OciPlatform::try_new("linux", "arm64").expect("arm64 platform");
    let operation_id = operation_id("build_mixed_platform_failure");
    let source = GitSource::try_new(
        "https://example.com/repository.git",
        "0123456789abcdef0123456789abcdef01234567",
        "git",
        "private-token",
        None::<String>,
    )
    .expect("valid git source");
    let accepted = controllers
        .submit_build(BuildSubmitCommand {
            operation_id: operation_id.clone(),
            target: BuildTarget::Cluster,
            source: source.clone(),
            adapter: BuildAdapter::Railpack {
                cache_scope: BuildCacheScope::try_new("mixed-platform").expect("valid cache scope"),
            },
            platforms: BuildPlatforms::try_new([amd64.clone(), arm64.clone()])
                .expect("two platforms"),
        })
        .await
        .expect("build submits");
    repository
        .record_build_transition(&operation_id, BuildTransition::Placing)
        .await
        .expect("record placement");
    for (platform, machine_id) in [(&amd64, &amd64_machine), (&arm64, &arm64_machine)] {
        repository
            .record_build_evidence(
                &operation_id,
                BuildEvidence::PlatformPlaced {
                    platform: platform.clone(),
                    executor: cluster_evidence(machine_id),
                },
            )
            .await
            .expect("record platform placement");
    }
    repository
        .record_build_transition(&operation_id, BuildTransition::Building)
        .await
        .expect("record execution start");

    let completed_image = PlatformImage {
        seed: amd64_machine.clone(),
        manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
            .expect("manifest digest"),
        image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("image id"),
        availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
            .expect("availability expiry"),
    };
    let platform_failure = BuildPlatformFailure::AdapterFailed {
        message: FailureMessage::try_new("railpack exited with status 1").expect("failure message"),
    };
    let operation_failure = BuildOperationFailure::PlatformFailed {
        platform: arm64.clone(),
        machine_id: arm64_machine.clone(),
        failure: platform_failure.clone(),
    };
    let verified_commit = VerifiedGitCommit::from_source(&source);
    let toolchain = BuildToolchainEvidence {
        buildkit_image: OciDigest::try_new(format!("sha256:{}", "3".repeat(64)))
            .expect("buildkit digest"),
        adapter: BuildAdapterToolchainEvidence::Railpack {
            helper_version: InstallArtifactVersion::try_new("v0.31.0")
                .expect("railpack helper version"),
            helper_sha256: InstallSha256Digest::try_new("4".repeat(64))
                .expect("railpack helper digest"),
            frontend_image: OciDigest::try_new(format!("sha256:{}", "5".repeat(64)))
                .expect("railpack frontend digest"),
        },
    };

    let amd64_request = start_build_responder(
        nats.machine_client(&amd64_machine).await,
        amd64_machine.clone(),
        MachineBuildStartRpcResponse::Ok(MachineBuildStartRpcOk::from((
            amd64_machine.clone(),
            BuildExecutorStartOk {
                acceptance: executor_acceptance(&operation_id, &amd64_machine, &amd64),
                cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
                image: completed_image.clone(),
                verified_commit: verified_commit.clone(),
                toolchain: toolchain.clone(),
                log_summary: BuildLogSummary::none(),
            },
        ))),
    )
    .await;
    let arm64_request = start_build_responder(
        nats.machine_client(&arm64_machine).await,
        arm64_machine.clone(),
        MachineBuildStartRpcResponse::DomainError {
            machine_id: arm64_machine.clone(),
            error: MachineBuildStartDomainError::PlatformFailed {
                acceptance: Box::new(executor_acceptance(&operation_id, &arm64_machine, &arm64)),
                failure: platform_failure.clone(),
                log_summary: BuildLogSummary::none(),
            },
        },
    )
    .await;

    let tasks = TaskRegistry::default();
    let driver = BuildOperationDriver::new(
        nats.controller.clone(),
        NatsMachineFactsReader::new(nats.controller.clone()),
        NatsIntentReader::new(nats.controller.clone()),
        controllers,
        Duration::from_secs(30),
        tasks.spawner(),
    );
    driver
        .active
        .start(
            operation_id.clone(),
            accepted.submission.start_sequence,
            BuildTarget::Cluster,
        )
        .await;
    let outcomes = futures_util::future::join_all([
        driver.run_platform(
            &accepted,
            BuildPlatformExecutorAssignment {
                platform: amd64.clone(),
                executor: BuildExecutorAssignment::Cluster {
                    machine_id: amd64_machine.clone(),
                },
            },
        ),
        driver.run_platform(
            &accepted,
            BuildPlatformExecutorAssignment {
                platform: arm64.clone(),
                executor: BuildExecutorAssignment::Cluster {
                    machine_id: arm64_machine.clone(),
                },
            },
        ),
    ])
    .await;
    let result = driver
        .finalize_joined_outcomes(&operation_id, outcomes)
        .await;
    driver.record_run_result(&operation_id, result).await;

    let (amd64_request, arm64_request) = tokio::time::timeout(Duration::from_secs(2), async {
        (
            amd64_request.await.expect("amd64 responder completes"),
            arm64_request.await.expect("arm64 responder completes"),
        )
    })
    .await
    .expect("both build responders complete within the test deadline");
    assert_eq!(amd64_request.operation_id, operation_id);
    assert_eq!(amd64_request.source, source);
    assert_eq!(amd64_request.adapter, accepted.submission.adapter);
    assert_eq!(amd64_request.platform, amd64);
    assert_eq!(amd64_request.timeout_millis, 30_000);
    assert_eq!(arm64_request.operation_id, operation_id);
    assert_eq!(arm64_request.source, accepted.source);
    assert_eq!(arm64_request.adapter, accepted.submission.adapter);
    assert_eq!(arm64_request.platform, arm64);
    assert_eq!(arm64_request.timeout_millis, 30_000);

    let snapshot = repository
        .operation_status_snapshot(&operation_id)
        .await
        .expect("read build status")
        .expect("build status exists");
    let OperationStatus::Build { status } = &snapshot.status else {
        panic!("submitted build projects build status");
    };
    assert_eq!(
        status.state(),
        &BuildOperationState::Failed {
            failure: operation_failure,
        }
    );
    assert!(
        !serde_json::to_string(&snapshot)
            .expect("status serializes")
            .contains("index_digest")
    );

    let replay = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(20),
        })
        .await
        .expect("replay build evidence");
    assert!(replay.events.iter().any(|record| matches!(
        &record.event,
        OperationEvent::BuildCommitVerified {
            platform,
            executor,
            commit,
            ..
        }
            if platform == &amd64
                && executor == &cluster_evidence(&amd64_machine)
                && commit == &verified_commit
    )));
    assert!(replay.events.iter().any(|record| matches!(
        &record.event,
        OperationEvent::BuildPlatformToolchainVerified {
            platform,
            executor,
            toolchain: actual,
            ..
        } if platform == &amd64
            && executor == &cluster_evidence(&amd64_machine)
            && actual == &toolchain
    )));
    assert!(replay.events.iter().any(|record| matches!(
        &record.event,
        OperationEvent::BuildPlatformCompleted {
            platform,
            executor,
            image,
            ..
        }
            if platform == &amd64
                && executor == &cluster_evidence(&amd64_machine)
                && image == &completed_image
    )));
    assert!(replay.events.iter().any(|record| matches!(
        &record.event,
        OperationEvent::BuildPlatformFailed {
            platform,
            executor,
            failure,
            ..
        }
            if platform == &arm64
                && executor == &cluster_evidence(&arm64_machine)
                && failure == &platform_failure
    )));
    assert!(
        !replay
            .events
            .iter()
            .any(|record| matches!(record.event, OperationEvent::BuildCompleted { .. }))
    );
    assert!(
        !serde_json::to_string(&replay)
            .expect("replay serializes")
            .contains("index_digest")
    );
}

async fn start_build_responder(
    client: async_nats::Client,
    machine_id: MachineId,
    response: MachineBuildStartRpcResponse,
) -> tokio::task::JoinHandle<MachineBuildStartRpcRequest> {
    let mut requests = client
        .subscribe(machine_service(
            &machine_id,
            MachineServiceEndpoint::BuildStart,
        ))
        .await
        .expect("machine subscribes to build start");
    client
        .flush()
        .await
        .expect("build responder subscription flushes");
    tokio::spawn(async move {
        let message = requests.next().await.expect("build request arrives");
        let request = serde_json::from_slice(&message.payload).expect("build request decodes");
        let reply = message.reply.expect("build request carries reply subject");
        client
            .publish(
                reply,
                serde_json::to_vec(&response)
                    .expect("build response encodes")
                    .into(),
            )
            .await
            .expect("build response publishes");
        client.flush().await.expect("build response flushes");
        request
    })
}

#[tokio::test(start_paused = true)]
async fn placement_deadline_is_a_typed_control_failure() {
    let pending = std::future::pending::<Result<(), BuildOperationFailure>>();
    let result = within_placement_deadline(pending);
    tokio::pin!(result);

    tokio::time::advance(ployz_core::build::BUILD_MAX_PLACEMENT_TIMEOUT).await;

    assert!(matches!(
        result.await,
        Err(BuildOperationFailure::ControlUnavailable { message })
            if message.as_str() == "build placement exceeded its 180-second deadline"
    ));
}

#[test]
fn caller_budget_is_explicit_and_deterministic() {
    assert_eq!(
        execution_timeout_millis(Duration::from_millis(7_321)),
        7_321
    );
    assert_eq!(
        build_control_request_timeout(ployz_core::build::BUILD_MAX_EXECUTION_TIMEOUT),
        ployz_core::build::BUILD_MAX_MACHINE_RESPONSE_LIFETIME
            + ployz_core::build::BUILD_CONTROL_RESPONSE_MARGIN
    );
}

#[tokio::test]
async fn valid_log_is_observed_while_machine_call_is_pending() {
    let (sender, mut pending_call) = tokio::sync::oneshot::channel::<()>();
    let frame = MachineBuildLogFrame {
        operation_id: OperationId::try_new("build-1").expect("operation id"),
        assignment: BuildExecutorAssignment::Cluster {
            machine_id: MachineId::try_new("machine-a").expect("machine id"),
        },
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        sequence: 1,
        chunk: BuildLogChunk::try_new("live").expect("chunk"),
    };
    let mut logs = futures_util::stream::iter([frame.clone()]);

    assert!(matches!(
        next_machine_call_or_log(Pin::new(&mut pending_call), &mut logs, true).await,
        MachineCallOrLog::Log(Some(actual)) if actual == frame
    ));
    assert!(!sender.is_closed());
}

#[test]
fn retryable_cancel_delivery_results_use_one_absolute_deadline() {
    let now = tokio::time::Instant::now();
    let deadline = now + Duration::from_millis(60);
    assert_eq!(
        cancel_retry_delay(now, deadline),
        Some(Duration::from_millis(25))
    );
    assert_eq!(
        cancel_retry_delay(now + Duration::from_millis(50), deadline),
        Some(Duration::from_millis(10))
    );
    assert_eq!(cancel_retry_delay(deadline, deadline), None);
    assert_eq!(
        cancel_retry_delay(deadline + Duration::from_millis(1), deadline),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_cancel_attempt_retries_inside_one_absolute_deadline() {
    let machine_id = machine_id("machine-a");
    let attempts = std::cell::Cell::new(0);
    let observed_timeouts = std::cell::RefCell::new(Vec::new());
    let started_at = tokio::time::Instant::now();
    let deadline = started_at + BUILD_CANCEL_TIMEOUT;

    deliver_build_cancel_to_machine(&machine_id, deadline, |timeout| {
        observed_timeouts.borrow_mut().push(timeout);
        let attempt = attempts.get();
        attempts.set(attempt + 1);
        let machine_id = machine_id.clone();
        async move {
            if attempt == 0 {
                tokio::time::sleep(timeout).await;
                Err(MachineCallError::Unavailable(
                    MachineRuntimeUnavailableReason::RequestTimedOut,
                ))
            } else {
                Ok(MachineBuildCancelRpcOk::from((
                    machine_id.clone(),
                    BuildExecutorCancelOk {
                        assignment: BuildExecutorAssignment::Cluster { machine_id },
                        outcome: MachineBuildCancelOutcome::Requested,
                    },
                )))
            }
        }
    })
    .await;

    assert_eq!(attempts.get(), 2);
    assert_eq!(
        observed_timeouts.into_inner(),
        vec![BUILD_CANCEL_ATTEMPT_TIMEOUT, BUILD_CANCEL_ATTEMPT_TIMEOUT,]
    );
    assert_eq!(
        tokio::time::Instant::now().duration_since(started_at),
        BUILD_CANCEL_ATTEMPT_TIMEOUT + Duration::from_millis(25)
    );
    assert!(tokio::time::Instant::now() < deadline);
}

#[test]
fn cancel_delivery_retries_not_running_and_transient_unavailability() {
    let machine_id = machine_id("machine-a");
    let retryable = [
        Ok(MachineBuildCancelRpcOk::from((
            machine_id.clone(),
            BuildExecutorCancelOk {
                assignment: BuildExecutorAssignment::Cluster {
                    machine_id: machine_id.clone(),
                },
                outcome: MachineBuildCancelOutcome::NotRunning,
            },
        ))),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::NoResponders,
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::RequestTimedOut,
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::RequestFailed {
                message: "request failed".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::ServiceUnavailable {
                message: "service unavailable".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::ServiceTimedOut {
                message: "service timed out".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::ServiceInternal {
                message: "service internal".to_owned(),
            },
        )),
    ];

    assert!(
        retryable
            .iter()
            .all(|result| cancel_delivery_should_retry(&machine_id, result))
    );
}

#[test]
fn cancel_delivery_stops_on_requested_domain_failure_and_permanent_unavailability() {
    let machine_id = machine_id("machine-a");
    let permanent = [
        Ok(MachineBuildCancelRpcOk::from((
            machine_id.clone(),
            BuildExecutorCancelOk {
                assignment: BuildExecutorAssignment::Cluster {
                    machine_id: machine_id.clone(),
                },
                outcome: MachineBuildCancelOutcome::Requested,
            },
        ))),
        Err(MachineCallError::Domain(
            MachineBuildCancelDomainError::CancelFailed {
                message: FailureMessage::try_new("cancel failed").expect("failure message"),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::EncodeRequest {
                message: "encode failed".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::InvalidSubject,
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::MaxPayloadExceeded,
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::ServiceBadRequest {
                message: "bad request".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::ServiceConflict {
                message: "conflict".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::ServiceResponseTooLarge,
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::MalformedServiceError {
                message: "malformed error".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::DecodeResponse {
                message: "decode failed".to_owned(),
            },
        )),
        Err(MachineCallError::Unavailable(
            MachineRuntimeUnavailableReason::WrongResponder {
                actual_machine_id: machine_id.clone(),
            },
        )),
    ];

    assert!(
        permanent
            .iter()
            .all(|result| !cancel_delivery_should_retry(&machine_id, result))
    );
}

#[test]
fn already_running_is_stable_machine_unavailable_evidence() {
    let failure = machine_failure(
        OciPlatform::try_new("linux", "amd64").expect("platform"),
        MachineId::try_new("machine-a").expect("machine id"),
        MachineCallError::Domain(MachineBuildStartDomainError::AlreadyRunning),
    );

    assert!(matches!(
        failure,
        BuildOperationFailure::PlatformFailed {
            failure: BuildPlatformFailure::MachineUnavailable { message },
            ..
        } if message.as_str() == "machine reports this build is already running"
    ));
}

#[test]
fn terminal_acceptance_rejects_wrong_executor_provenance() {
    let operation_id = OperationId::try_new("build-1").expect("operation id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let expected = executor_acceptance(&operation_id, &machine_id, &platform);
    let mut actual = expected.clone();
    actual.assignment = BuildExecutorAssignment::External {
        pool_id: ployz_core::build::BuildPoolId::try_new("pool-a").expect("pool id"),
        executor_id: ployz_core::build::BuildExecutorId::try_new("executor-a")
            .expect("executor id"),
        image_seed: machine_id.clone(),
    };

    assert!(matches!(
        validate_executor_acceptance(&expected, &actual),
        Err(BuildPlatformFailure::MachineUnavailable { message })
            if message.as_str() == "build executor returned mismatched acceptance provenance"
    ));
}

#[test]
fn timeout_cleanup_is_completed_only_when_every_machine_confirms() {
    let confirmed = MachineId::try_new("confirmed").expect("machine id");
    let unconfirmed = MachineId::try_new("unconfirmed").expect("machine id");
    let message = FailureMessage::try_new("deadline exceeded").expect("message");

    assert_eq!(
        timeout_cleanup(&[(
            BuildExecutorAssignment::Cluster {
                machine_id: confirmed.clone(),
            },
            message.clone(),
            BuildExecutorCleanupOutcome::Confirmed,
        )]),
        BuildCleanupEvidence::Completed {
            machine_ids: vec![confirmed.clone()],
        }
    );
    assert_eq!(
        timeout_cleanup(&[
            (
                BuildExecutorAssignment::Cluster {
                    machine_id: confirmed,
                },
                message.clone(),
                BuildExecutorCleanupOutcome::Confirmed,
            ),
            (
                BuildExecutorAssignment::Cluster {
                    machine_id: unconfirmed.clone(),
                },
                message,
                BuildExecutorCleanupOutcome::Unconfirmed,
            ),
        ]),
        BuildCleanupEvidence::Unconfirmed {
            machine_ids: vec![unconfirmed],
        }
    );
}
