use ployz_nats::schedules::{NatsServerVersion, ScheduleCapability};

#[test]
fn schedule_capability_starts_at_nats_2_12() {
    assert!(!ScheduleCapability::from_server_version(NatsServerVersion::new(2, 11, 9))
        .message_schedules_available);
    assert!(ScheduleCapability::from_server_version(NatsServerVersion::new(2, 12, 0))
        .message_schedules_available);
}

#[test]
fn extended_schedule_controls_start_at_nats_2_14() {
    assert!(!ScheduleCapability::from_server_version(NatsServerVersion::new(2, 13, 9))
        .extended_schedule_controls_available);
    assert!(ScheduleCapability::from_server_version(NatsServerVersion::new(2, 14, 0))
        .extended_schedule_controls_available);
}
