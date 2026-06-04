use ployz_nats::bootstrap::{
    BootstrapAction, BootstrapPlan, BootstrapRefusal, BootstrapResourceRefusal, ExistingResources,
    NatsServerCapabilities,
};
use ployz_nats::schedules::NatsServerVersion;
use ployz_nats::streams::RetentionPolicy;

fn supported_single_node_plan() -> BootstrapPlan {
    BootstrapPlan::single_node(NatsServerCapabilities::new(
        NatsServerVersion::new(2, 14, 2),
        true,
    ))
    .expect("supported nats-server can be bootstrapped")
}

fn existing_from_plan(plan: &BootstrapPlan) -> ExistingResources {
    ExistingResources {
        kv_buckets: plan.kv_buckets.clone(),
        streams: plan.streams.clone(),
        object_buckets: plan.object_buckets.clone(),
    }
}

fn all_resources_are_created(plan: &BootstrapPlan) -> bool {
    let diff = plan.diff_against(&ExistingResources::default());
    all_resources_have_action(&diff, BootstrapAction::Create)
}

fn all_resources_are_adopted(plan: &BootstrapPlan) -> bool {
    let existing = existing_from_plan(plan);
    let diff = plan.diff_against(&existing);
    all_resources_have_action(&diff, BootstrapAction::Adopt)
}

fn all_resources_have_action(
    diff: &ployz_nats::bootstrap::BootstrapDiff,
    action: BootstrapAction,
) -> bool {
    diff.kv_buckets
        .iter()
        .chain(diff.streams.iter())
        .chain(diff.object_buckets.iter())
        .all(|resource| resource.action == action)
}

#[test]
fn single_node_bootstrap_contains_required_resources() {
    let plan = supported_single_node_plan();

    for bucket in ["KV_CORE", "KV_OPS", "KV_OBS", "KV_LOCKS"] {
        assert!(
            plan.kv_buckets.iter().any(|spec| spec.name == bucket),
            "missing KV bucket {bucket}"
        );
    }

    for stream in [
        "PLZ_OPS",
        "PLZ_JOBS",
        "PLZ_AUDIT",
        "PLZ_OBS_TRANSITIONS",
        "PLZ_SCHEDULES",
    ] {
        assert!(
            plan.streams.iter().any(|spec| spec.name == stream),
            "missing stream {stream}"
        );
    }

    for bucket in ["PLZ_BUNDLES", "PLZ_DIAGNOSTICS", "PLZ_CERTS", "PLZ_BACKUPS"] {
        assert!(
            plan.object_buckets.iter().any(|spec| spec.name == bucket),
            "missing object bucket {bucket}"
        );
    }
}

#[test]
fn operation_stream_is_retained_history_not_work_queue() {
    let plan = supported_single_node_plan();
    let ops = plan
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("PLZ_OPS stream exists");

    assert_eq!(ops.retention, RetentionPolicy::Limits);
    assert_eq!(ops.subjects, vec!["plz.v1.op.>"]);
}

#[test]
fn jobs_stream_is_separate_from_operation_history() {
    let plan = supported_single_node_plan();
    let jobs = plan
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_JOBS")
        .expect("PLZ_JOBS stream exists");

    assert_eq!(jobs.retention, RetentionPolicy::Limits);
    assert_eq!(jobs.subjects, vec!["plz.v1.job.>"]);
}

#[test]
fn schedule_stream_tracks_server_capability() {
    let with_schedules = BootstrapPlan::single_node(NatsServerCapabilities::new(
        NatsServerVersion::new(2, 12, 0),
        true,
    ))
    .expect("minimum supported nats-server can be bootstrapped");
    let schedule_stream = with_schedules
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_SCHEDULES")
        .expect("schedule stream exists");

    assert!(schedule_stream.allow_message_schedules);
}

#[test]
fn fresh_bootstrap_creates_all_resources() {
    let plan = supported_single_node_plan();

    assert!(all_resources_are_created(&plan));
}

#[test]
fn reboot_bootstrap_adopts_existing_resources() {
    let plan = supported_single_node_plan();

    assert!(all_resources_are_adopted(&plan));
}

#[test]
fn bootstrap_refuses_existing_resource_drift() {
    let plan = supported_single_node_plan();
    let mut existing = existing_from_plan(&plan);
    existing
        .streams
        .iter_mut()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("observed PLZ_OPS stream exists")
        .subjects = vec!["wrong.>".to_owned()];

    let diff = plan.diff_against(&existing);
    let ops = diff
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("PLZ_OPS diff exists");

    assert_eq!(
        ops.action,
        BootstrapAction::Refuse {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "subjects",
                expected: "[\"plz.v1.op.>\"]".to_owned(),
                observed: "[\"wrong.>\"]".to_owned(),
            }
        }
    );
}

#[test]
fn bootstrap_creates_missing_resource_by_name() {
    let plan = supported_single_node_plan();
    let mut existing = existing_from_plan(&plan);
    existing.streams.retain(|stream| stream.name != "PLZ_OPS");

    let diff = plan.diff_against(&existing);
    let ops = diff
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("PLZ_OPS diff exists");

    assert_eq!(ops.action, BootstrapAction::Create);
}

#[test]
fn bootstrap_refuses_without_jetstream() {
    let capabilities = NatsServerCapabilities::new(NatsServerVersion::new(2, 14, 2), false);

    assert_eq!(
        BootstrapPlan::single_node(capabilities),
        Err(BootstrapRefusal::JetStreamDisabled)
    );
}

#[test]
fn bootstrap_refuses_old_nats_server() {
    let capabilities = NatsServerCapabilities::new(NatsServerVersion::new(2, 11, 9), true);

    assert_eq!(
        BootstrapPlan::single_node(capabilities),
        Err(BootstrapRefusal::UnsupportedServerVersion {
            minimum: NatsServerVersion::new(2, 12, 0),
            actual: NatsServerVersion::new(2, 11, 9),
        })
    );
}
