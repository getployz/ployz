use std::pin::Pin;

use super::*;
use crate::roles::machine::protocol::MachineBuildLogFrame;
use ployz_core::image::OciPlatform;
use ployz_core::operation::BuildLogChunk;

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
fn not_running_retries_use_one_absolute_deadline() {
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
