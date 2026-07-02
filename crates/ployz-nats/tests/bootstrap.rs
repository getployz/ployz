use ployz_nats::bootstrap::{
    BootstrapAssuranceError, BootstrapPlan, BootstrapRefusal, BootstrapResourceRefusal,
    NatsServerCapabilities, ResourceReplicas, assure_nats_resources,
};
use ployz_nats::schedules::NatsServerVersion;
use ployz_nats::streams::{DiscardPolicy, RetentionPolicy};

fn supported_single_core_plan() -> BootstrapPlan {
    BootstrapPlan::for_single_core(single_capabilities(NatsServerVersion::new(2, 14, 2), true))
        .expect("supported nats-server can be bootstrapped")
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

    assert!(
        plan.streams.iter().any(|spec| spec.name == "PLZ_OPS"),
        "missing stream PLZ_OPS"
    );
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
    assert_eq!(ops.discard, DiscardPolicy::Old);
}

#[tokio::test]
async fn bootstrap_assurance_creates_and_adopts_resources() {
    let server = ployz_test_support::nats::TestNats::start().await;
    let plan = supported_single_core_plan();

    assure_nats_resources(&server.jetstream, &plan)
        .await
        .expect("fresh resources are created");
    assure_nats_resources(&server.jetstream, &plan)
        .await
        .expect("existing resources are adopted");

    for bucket in ["KV_CORE", "KV_OPS", "KV_OBS"] {
        server
            .jetstream
            .get_key_value(bucket)
            .await
            .expect("bootstrap created KV bucket");
    }
    server
        .jetstream
        .get_stream("PLZ_OPS")
        .await
        .expect("bootstrap created PLZ_OPS");
    drop(server);
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
    drop(server);
}

#[tokio::test]
async fn bootstrap_refuses_existing_stream_shape_drift() {
    let server = ployz_test_support::nats::TestNats::start().await;
    server
        .jetstream
        .create_stream(async_nats::jetstream::stream::Config {
            name: "PLZ_OPS".to_owned(),
            subjects: vec!["wrong.>".to_owned()],
            retention: async_nats::jetstream::stream::RetentionPolicy::Limits,
            storage: async_nats::jetstream::stream::StorageType::File,
            discard: async_nats::jetstream::stream::DiscardPolicy::Old,
            ..Default::default()
        })
        .await
        .expect("drifted stream stores");
    let plan = supported_single_core_plan();
    let error = assure_nats_resources(&server.jetstream, &plan)
        .await
        .expect_err("drifted stream is refused");

    assert!(matches!(
        error,
        BootstrapAssuranceError::RefuseResource {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "subjects",
                ref expected,
                ref observed,
            },
            ..
        } if expected == "[\"plz.v1.op.>\"]" && observed == "[\"wrong.>\"]"
    ));
    drop(server);
}

#[tokio::test]
async fn bootstrap_refuses_existing_resource_policy_drift() {
    let server = ployz_test_support::nats::TestNats::start().await;
    server
        .jetstream
        .create_stream(async_nats::jetstream::stream::Config {
            name: "PLZ_OPS".to_owned(),
            subjects: vec!["plz.v1.op.>".to_owned()],
            retention: async_nats::jetstream::stream::RetentionPolicy::Limits,
            storage: async_nats::jetstream::stream::StorageType::File,
            discard: async_nats::jetstream::stream::DiscardPolicy::New,
            ..Default::default()
        })
        .await
        .expect("drifted stream stores");
    let plan = supported_single_core_plan();
    let error = assure_nats_resources(&server.jetstream, &plan)
        .await
        .expect_err("drifted stream is refused");

    assert!(matches!(
        error,
        BootstrapAssuranceError::RefuseResource {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "discard",
                ref expected,
                ref observed,
            },
            ..
        } if expected == "Old" && observed == "New"
    ));
    drop(server);
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
