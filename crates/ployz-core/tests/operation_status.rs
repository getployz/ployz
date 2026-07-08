use ployz_core::ops::{NextEventSequenceError, OperationStatus};
use ployz_test_support::ids::{event_sequence, namespace_id, operation_id, service_id};

#[test]
fn next_event_sequence_reports_overflow() {
    let status = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        namespace_id("default"),
        service_id("svc_api"),
        event_sequence(u64::MAX),
    );

    assert_eq!(
        status.next_event_sequence(),
        Err(NextEventSequenceError::Overflow {
            last_event_sequence: event_sequence(u64::MAX),
        })
    );
}

#[test]
fn next_event_sequence_increments_normal_sequence() {
    let status = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        namespace_id("default"),
        service_id("svc_api"),
        event_sequence(41),
    );

    assert_eq!(status.next_event_sequence(), Ok(event_sequence(42)));
}
