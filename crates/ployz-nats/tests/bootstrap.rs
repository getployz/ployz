use ployz_nats::bootstrap::BootstrapPlan;
use ployz_nats::schedules::NatsServerVersion;
use ployz_nats::streams::RetentionPolicy;

#[test]
fn single_node_bootstrap_contains_required_resources() {
    let plan = BootstrapPlan::single_node(NatsServerVersion::new(2, 14, 2));

    for bucket in ["KV_CORE", "KV_OPS", "KV_OBS", "KV_LOCKS"] {
        assert!(plan.bucket_named(bucket), "missing KV bucket {bucket}");
    }

    for stream in [
        "PLZ_OPS",
        "PLZ_AUDIT",
        "PLZ_OBS_TRANSITIONS",
        "PLZ_SCHEDULES",
    ] {
        assert!(plan.stream_named(stream), "missing stream {stream}");
    }

    for bucket in ["PLZ_BUNDLES", "PLZ_DIAGNOSTICS", "PLZ_CERTS", "PLZ_BACKUPS"] {
        assert!(plan.object_bucket_named(bucket), "missing object bucket {bucket}");
    }
}

#[test]
fn operation_stream_is_retained_history_not_work_queue() {
    let plan = BootstrapPlan::single_node(NatsServerVersion::new(2, 14, 2));
    let ops = plan
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("PLZ_OPS stream exists");

    assert_eq!(ops.retention, RetentionPolicy::Limits);
    assert!(ops.subjects.contains(&"plz.op.>".to_owned()));
    assert!(ops.subjects.contains(&"plz.job.>".to_owned()));
}

#[test]
fn schedule_stream_tracks_server_capability() {
    let without_schedules = BootstrapPlan::single_node(NatsServerVersion::new(2, 11, 0));
    let old_schedule_stream = without_schedules
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_SCHEDULES")
        .expect("schedule stream exists");
    assert!(!old_schedule_stream.allow_message_schedules);

    let with_schedules = BootstrapPlan::single_node(NatsServerVersion::new(2, 12, 0));
    let schedule_stream = with_schedules
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_SCHEDULES")
        .expect("schedule stream exists");
    assert!(schedule_stream.allow_message_schedules);
}
