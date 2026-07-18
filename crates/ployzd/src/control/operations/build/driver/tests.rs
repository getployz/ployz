use std::pin::Pin;

use super::*;
use crate::control::operation_evidence::OperationRepository;
use crate::control::sequencer::{
    BuildSubmitCommand, MachineAddBootstrapConfig, OperationControllers,
};
use crate::control::store::CoreStore;
use crate::roles::machine::protocol::MachineBuildLogFrame;
use crate::tasks::TaskRegistry;
use ployz_core::build::{BuildAdapter, BuildCacheScope, BuildPlatforms, GitSource};
use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::install::{DEFAULT_MACHINE_BOOTSTRAP_URL, MachineBootstrapUrl};
use ployz_core::operation::{
    BuildLogChunk, BuildOperationState, OperationEvent, OperationEventReplayRequest,
};
use ployz_test_support::ids::{event_replay_limit, event_sequence, machine_id, operation_id};

#[tokio::test]
async fn mixed_platform_failure_publishes_no_image_index() {
    let nats = ployz_test_support::nats::TestNats::start().await;
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
    let accepted = controllers
        .submit_build(BuildSubmitCommand {
            operation_id: operation_id.clone(),
            source: GitSource::try_new(
                "https://example.com/repository.git",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "private-token",
                None::<String>,
            )
            .expect("valid git source"),
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
    repository
        .record_build_transition(&operation_id, BuildTransition::Building)
        .await
        .expect("record execution start");

    let amd64_machine = machine_id("machine_amd64");
    let arm64_machine = machine_id("machine_arm64");
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
    repository
        .record_build_evidence(
            &operation_id,
            BuildEvidence::PlatformCompleted {
                platform: amd64.clone(),
                machine_id: amd64_machine.clone(),
                image: completed_image.clone(),
            },
        )
        .await
        .expect("record completed platform evidence");
    repository
        .record_build_evidence(
            &operation_id,
            BuildEvidence::PlatformFailed {
                platform: arm64.clone(),
                machine_id: arm64_machine.clone(),
                failure: platform_failure.clone(),
            },
        )
        .await
        .expect("record failed platform evidence");

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
        .start(operation_id.clone(), accepted.submission.start_sequence)
        .await;
    let result = driver
        .finalize_joined_outcomes(
            &operation_id,
            vec![
                Ok(PlatformOutcome::Completed {
                    platform: amd64.clone(),
                    image: completed_image.clone(),
                }),
                Ok(PlatformOutcome::Failed(operation_failure.clone())),
            ],
        )
        .await;
    driver.record_run_result(&operation_id, result).await;

    let snapshot = repository
        .operation_status_snapshot(&operation_id)
        .await
        .expect("read build status")
        .expect("build status exists");
    let OperationStatus::Build { state, .. } = &snapshot.status else {
        panic!("submitted build projects build status");
    };
    assert_eq!(
        state,
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
            operation_id,
            start_sequence: event_sequence(1),
            limit: event_replay_limit(20),
        })
        .await
        .expect("replay build evidence");
    assert!(replay.events.iter().any(|record| matches!(
        &record.event,
        OperationEvent::BuildPlatformCompleted { platform, machine_id, image, .. }
            if platform == &amd64
                && machine_id == &amd64_machine
                && image == &completed_image
    )));
    assert!(replay.events.iter().any(|record| matches!(
        &record.event,
        OperationEvent::BuildPlatformFailed { platform, machine_id, failure, .. }
            if platform == &arm64
                && machine_id == &arm64_machine
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
        machine_id: MachineId::try_new("machine-a").expect("machine id"),
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

    deliver_build_cancel_to_machine(deadline, |timeout| {
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
                Ok(MachineBuildCancelRpcOk {
                    machine_id,
                    outcome: MachineBuildCancelOutcome::Requested,
                })
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
        Ok(MachineBuildCancelRpcOk {
            machine_id,
            outcome: MachineBuildCancelOutcome::NotRunning,
        }),
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

    assert!(retryable.iter().all(cancel_delivery_should_retry));
}

#[test]
fn cancel_delivery_stops_on_requested_domain_failure_and_permanent_unavailability() {
    let machine_id = machine_id("machine-a");
    let permanent = [
        Ok(MachineBuildCancelRpcOk {
            machine_id: machine_id.clone(),
            outcome: MachineBuildCancelOutcome::Requested,
        }),
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
                actual_machine_id: machine_id,
            },
        )),
    ];

    assert!(
        permanent
            .iter()
            .all(|result| !cancel_delivery_should_retry(result))
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
fn timeout_cleanup_is_completed_only_when_every_machine_confirms() {
    let confirmed = MachineId::try_new("confirmed").expect("machine id");
    let unconfirmed = MachineId::try_new("unconfirmed").expect("machine id");
    let message = FailureMessage::try_new("deadline exceeded").expect("message");

    assert_eq!(
        timeout_cleanup(&[(
            confirmed.clone(),
            message.clone(),
            MachineBuildCleanupOutcome::Confirmed,
        )]),
        BuildCleanupEvidence::Completed {
            machine_ids: vec![confirmed.clone()],
        }
    );
    assert_eq!(
        timeout_cleanup(&[
            (
                confirmed,
                message.clone(),
                MachineBuildCleanupOutcome::Confirmed,
            ),
            (
                unconfirmed.clone(),
                message,
                MachineBuildCleanupOutcome::Unconfirmed,
            ),
        ]),
        BuildCleanupEvidence::Unconfirmed {
            machine_ids: vec![unconfirmed],
        }
    );
}
