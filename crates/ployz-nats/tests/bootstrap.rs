use ployz_nats::bootstrap::{
    BootstrapPlan, BootstrapRefusal, BootstrapResourceAction, BootstrapResourceRefusal,
    ExistingResources, NatsServerCapabilities, ResourceReplicas, assure_nats_resources,
};
use ployz_nats::schedules::NatsServerVersion;
use ployz_nats::streams::{DiscardPolicy, RetentionPolicy};

fn supported_single_core_plan() -> BootstrapPlan {
    BootstrapPlan::for_single_core(single_capabilities(NatsServerVersion::new(2, 14, 2), true))
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
    diff.all_resources()
        .all(|resource| resource.action == BootstrapResourceAction::Create)
}

fn all_resources_are_adopted(plan: &BootstrapPlan) -> bool {
    let diff = plan.diff_against(&existing_from_plan(plan));
    diff.all_resources()
        .all(|resource| resource.action == BootstrapResourceAction::Adopt)
}

#[test]
fn single_core_bootstrap_contains_required_resources() {
    let plan = supported_single_core_plan();

    for bucket in ["KV_CORE", "KV_OPS", "KV_OBS"] {
        assert!(
            plan.kv_buckets.iter().any(|spec| spec.name == bucket),
            "missing KV bucket {bucket}"
        );
    }

    let stream = "PLZ_OPS";
    assert!(
        plan.streams.iter().any(|spec| spec.name == stream),
        "missing stream {stream}"
    );

    assert!(plan.object_buckets.is_empty());
}

#[test]
fn single_core_bootstrap_uses_r1_resources() {
    let plan = supported_single_core_plan();

    assert!(
        plan.kv_buckets
            .iter()
            .all(|bucket| bucket.replicas() == ResourceReplicas::SINGLE_CORE)
    );
    assert!(
        plan.streams
            .iter()
            .all(|stream| stream.replicas() == ResourceReplicas::SINGLE_CORE)
    );
    assert!(
        plan.object_buckets
            .iter()
            .all(|bucket| bucket.replicas() == ResourceReplicas::SINGLE_CORE)
    );
}

#[test]
fn operation_stream_is_retained_history_not_work_queue() {
    let plan = supported_single_core_plan();
    let ops = plan
        .streams
        .iter()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("PLZ_OPS stream exists");

    assert_eq!(ops.retention, RetentionPolicy::Limits);
    assert_eq!(ops.subjects, vec!["plz.v1.op.>"]);
}

#[test]
fn fresh_bootstrap_creates_all_resources() {
    assert!(all_resources_are_created(&supported_single_core_plan()));
}

#[test]
fn reboot_bootstrap_adopts_existing_resources() {
    assert!(all_resources_are_adopted(&supported_single_core_plan()));
}

#[tokio::test]
async fn bootstrap_assurance_adopts_resources_created_by_parallel_startup() {
    let server = ployz_test_support::nats::TestNats::start().await;
    let jetstream = server.jetstream.clone();
    let plan = supported_single_core_plan();

    let (first, second, third) = tokio::join!(
        assure_nats_resources(&jetstream, &plan),
        assure_nats_resources(&jetstream, &plan),
        assure_nats_resources(&jetstream, &plan),
    );

    assert!(first.is_ok(), "{first:?}");
    assert!(second.is_ok(), "{second:?}");
    assert!(third.is_ok(), "{third:?}");
    jetstream
        .get_stream("PLZ_OPS")
        .await
        .expect("bootstrap created PLZ_OPS");
    drop(server);
}

#[test]
fn bootstrap_refuses_existing_stream_shape_drift() {
    let plan = supported_single_core_plan();
    let mut existing = existing_from_plan(&plan);
    existing
        .streams
        .iter_mut()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("observed PLZ_OPS stream exists")
        .subjects = vec!["wrong.>".to_owned()];

    let diff = plan.diff_against(&existing);
    let ops = diff.stream("PLZ_OPS").expect("PLZ_OPS diff exists");

    assert_eq!(
        ops.action,
        BootstrapResourceAction::Refuse {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "subjects",
                expected: "[\"plz.v1.op.>\"]".to_owned(),
                observed: "[\"wrong.>\"]".to_owned(),
            }
        }
    );
}

#[test]
fn bootstrap_refuses_existing_resource_policy_drift() {
    let plan = supported_single_core_plan();
    let mut existing = existing_from_plan(&plan);
    let ops = existing
        .streams
        .iter_mut()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("observed PLZ_OPS stream exists");
    ops.discard = DiscardPolicy::New;

    let diff = plan.diff_against(&existing);
    let ops = diff.stream("PLZ_OPS").expect("PLZ_OPS diff exists");

    assert_eq!(
        ops.action,
        BootstrapResourceAction::Refuse {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "discard",
                expected: "Old".to_owned(),
                observed: "New".to_owned(),
            }
        }
    );
}

#[test]
fn bootstrap_creates_missing_resource_by_name() {
    let plan = supported_single_core_plan();
    let mut existing = existing_from_plan(&plan);
    existing.streams.retain(|stream| stream.name != "PLZ_OPS");

    let diff = plan.diff_against(&existing);
    let ops = diff.stream("PLZ_OPS").expect("PLZ_OPS diff exists");

    assert_eq!(ops.action, BootstrapResourceAction::Create);
}

#[test]
fn bootstrap_refuses_without_jetstream() {
    let capabilities = single_capabilities(NatsServerVersion::new(2, 14, 2), false);

    assert_eq!(
        BootstrapPlan::for_single_core(capabilities),
        Err(BootstrapRefusal::JetStreamDisabled)
    );
}

#[test]
fn bootstrap_refuses_old_nats_server() {
    let capabilities = single_capabilities(NatsServerVersion::new(2, 11, 9), true);

    assert_eq!(
        BootstrapPlan::for_single_core(capabilities),
        Err(BootstrapRefusal::UnsupportedServerVersion {
            minimum: NatsServerVersion::new(2, 12, 0),
            actual: NatsServerVersion::new(2, 11, 9),
        })
    );
}

fn single_capabilities(
    version: NatsServerVersion,
    jetstream_enabled: bool,
) -> NatsServerCapabilities {
    NatsServerCapabilities::new(version, jetstream_enabled)
}
