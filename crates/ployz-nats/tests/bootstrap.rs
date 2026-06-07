use ployz_core::ha::CoreTopology;
use ployz_core::ids::NodeId;
use ployz_nats::bootstrap::{
    BootstrapPlan, BootstrapRefusal, BootstrapResourceAction, BootstrapResourceRefusal,
    ExistingResources, JetStreamPeerSet, JetStreamPeerSetError, NatsServerCapabilities,
    ReplicationPromotionAction,
};
use ployz_nats::replication::ReplicationFactor;
use ployz_nats::schedules::NatsServerVersion;
use ployz_nats::streams::RetentionPolicy;

fn supported_single_node_plan() -> BootstrapPlan {
    let topology = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single topology");

    BootstrapPlan::for_core_topology(
        single_capabilities("core_1", NatsServerVersion::new(2, 14, 2), true),
        &topology,
    )
    .expect("supported nats-server can be bootstrapped")
}

fn supported_ha_plan() -> BootstrapPlan {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three core topology");
    BootstrapPlan::for_core_topology(
        NatsServerCapabilities::clustered(
            NatsServerVersion::new(2, 14, 2),
            true,
            jetstream_peers(["core_1", "core_2", "core_3"]),
        ),
        &topology,
    )
    .expect("supported nats-server can bootstrap HA resources")
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
    all_resources_have_action(&diff, BootstrapResourceAction::Create)
}

fn all_resources_are_adopted(plan: &BootstrapPlan) -> bool {
    let existing = existing_from_plan(plan);
    let diff = plan.diff_against(&existing);
    all_resources_have_action(&diff, BootstrapResourceAction::Adopt)
}

fn all_resources_have_action(
    diff: &ployz_nats::bootstrap::BootstrapDiff,
    action: BootstrapResourceAction,
) -> bool {
    diff.all_resources()
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
fn high_availability_bootstrap_uses_r3_resources() {
    let plan = supported_ha_plan();

    assert!(
        plan.kv_buckets
            .iter()
            .all(|bucket| bucket.replicas == ReplicationFactor::Three)
    );
    assert!(
        plan.streams
            .iter()
            .all(|stream| stream.replicas == ReplicationFactor::Three)
    );
    assert!(
        plan.object_buckets
            .iter()
            .all(|bucket| bucket.replicas == ReplicationFactor::Three)
    );
}

#[test]
fn bootstrap_plan_tracks_core_topology() {
    let single_capabilities = single_capabilities("core_1", NatsServerVersion::new(2, 14, 2), true);
    let ha_capabilities = NatsServerCapabilities::clustered(
        NatsServerVersion::new(2, 14, 2),
        true,
        jetstream_peers(["core_1", "core_2", "core_3"]),
    );
    let single = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single core");
    let ha = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three cores");

    assert!(
        BootstrapPlan::for_core_topology(single_capabilities.clone(), &single)
            .expect("single plan")
            .kv_buckets
            .iter()
            .all(|bucket| bucket.replicas == ReplicationFactor::One)
    );
    assert_eq!(
        CoreTopology::from_nodes(vec![node_id("core_1"), node_id("core_2")]),
        Err(ployz_core::ha::CoreTopologyError::TransitionalCoreCount { count: 2 })
    );
    assert!(
        BootstrapPlan::for_core_topology(ha_capabilities, &ha)
            .expect("ha plan")
            .streams
            .iter()
            .all(|stream| stream.replicas == ReplicationFactor::Three)
    );
}

#[test]
fn high_availability_bootstrap_requires_three_clustered_jetstream_servers() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three core topology");

    assert_eq!(
        BootstrapPlan::for_core_topology(
            single_capabilities("core_1", NatsServerVersion::new(2, 14, 2), true),
            &topology,
        ),
        Err(BootstrapRefusal::InsufficientJetStreamServers {
            required: 3,
            actual: 1,
        })
    );
    assert_eq!(
        BootstrapPlan::for_core_topology(
            NatsServerCapabilities::clustered(
                NatsServerVersion::new(2, 14, 2),
                true,
                jetstream_peers(["core_1", "core_2"]),
            ),
            &topology,
        ),
        Err(BootstrapRefusal::InsufficientJetStreamServers {
            required: 3,
            actual: 2,
        })
    );
}

#[test]
fn topology_bootstrap_requires_single_server_to_match_single_core() {
    let topology = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single topology");

    assert_eq!(
        BootstrapPlan::for_core_topology(
            single_capabilities("other_core", NatsServerVersion::new(2, 14, 2), true),
            &topology,
        ),
        Err(BootstrapRefusal::JetStreamPeersDoNotCoverCoreTopology)
    );
}

#[test]
fn topology_bootstrap_requires_jetstream_peers_to_cover_core_nodes() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three core topology");
    let wrong_peers = NatsServerCapabilities::clustered(
        NatsServerVersion::new(2, 14, 2),
        true,
        jetstream_peers(["core_1", "core_2", "other_core"]),
    );

    assert_eq!(
        BootstrapPlan::for_core_topology(wrong_peers, &topology),
        Err(BootstrapRefusal::JetStreamPeersDoNotCoverCoreTopology)
    );
}

#[test]
fn topology_bootstrap_rejects_extra_jetstream_peers() {
    let topology = CoreTopology::from_nodes(vec![
        node_id("core_1"),
        node_id("core_2"),
        node_id("core_3"),
    ])
    .expect("three core topology");
    let extra_peers = NatsServerCapabilities::clustered(
        NatsServerVersion::new(2, 14, 2),
        true,
        jetstream_peers(["core_1", "core_2", "core_3", "other_core"]),
    );

    assert_eq!(
        BootstrapPlan::for_core_topology(extra_peers, &topology),
        Err(BootstrapRefusal::JetStreamPeersDoNotCoverCoreTopology)
    );
}

#[test]
fn jetstream_peer_set_has_set_semantics() {
    let peers = JetStreamPeerSet::from_nodes(vec![
        node_id("core_3"),
        node_id("core_1"),
        node_id("core_2"),
    ])
    .expect("peer set");

    assert_eq!(
        peers.nodes(),
        &[node_id("core_1"), node_id("core_2"), node_id("core_3")]
    );
    assert_eq!(
        JetStreamPeerSet::from_nodes(vec![node_id("core_1"), node_id("core_1")]),
        Err(JetStreamPeerSetError::DuplicateNode {
            node_id: node_id("core_1"),
        })
    );
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
    let topology = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single topology");
    let with_schedules = BootstrapPlan::for_core_topology(
        single_capabilities("core_1", NatsServerVersion::new(2, 12, 0), true),
        &topology,
    )
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
fn ha_bootstrap_refuses_existing_single_node_replication() {
    let ha_plan = supported_ha_plan();
    let single_node_existing = existing_from_plan(&supported_single_node_plan());

    let diff = ha_plan.diff_against(&single_node_existing);
    let core = diff.kv_bucket("KV_CORE").expect("KV_CORE diff exists");

    assert_eq!(
        core.action,
        BootstrapResourceAction::Refuse {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "replicas",
                expected: "3".to_owned(),
                observed: "1".to_owned(),
            },
        }
    );
}

#[test]
fn ha_promotion_diff_marks_r1_resources_for_replication_upgrade() {
    let ha_plan = supported_ha_plan();
    let single_node_existing = existing_from_plan(&supported_single_node_plan());

    let plan = ha_plan.replication_promotion_plan_against(&single_node_existing);

    assert!(plan.steps.iter().all(|step| step.action
        == ReplicationPromotionAction::UpgradeReplicas {
            from: ReplicationFactor::One,
            to: ReplicationFactor::Three,
        }));
}

#[test]
fn ha_promotion_diff_refuses_stream_shape_drift_before_replication_upgrade() {
    let ha_plan = supported_ha_plan();
    let mut single_node_existing = existing_from_plan(&supported_single_node_plan());
    single_node_existing
        .streams
        .iter_mut()
        .find(|stream| stream.name == "PLZ_OPS")
        .expect("observed PLZ_OPS stream exists")
        .subjects = vec!["wrong.>".to_owned()];

    let plan = ha_plan.replication_promotion_plan_against(&single_node_existing);

    assert_eq!(
        plan.stream("PLZ_OPS")
            .expect("PLZ_OPS promotion step exists")
            .action,
        ReplicationPromotionAction::Refuse {
            reason: BootstrapResourceRefusal::ConfigurationDrift {
                field: "subjects",
                expected: "[\"plz.v1.op.>\"]".to_owned(),
                observed: "[\"wrong.>\"]".to_owned(),
            },
        }
    );
}

#[test]
fn bootstrap_creates_missing_resource_by_name() {
    let plan = supported_single_node_plan();
    let mut existing = existing_from_plan(&plan);
    existing.streams.retain(|stream| stream.name != "PLZ_OPS");

    let diff = plan.diff_against(&existing);
    let ops = diff.stream("PLZ_OPS").expect("PLZ_OPS diff exists");

    assert_eq!(ops.action, BootstrapResourceAction::Create);
}

#[test]
fn bootstrap_refuses_without_jetstream() {
    let topology = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single topology");
    let capabilities = single_capabilities("core_1", NatsServerVersion::new(2, 14, 2), false);

    assert_eq!(
        BootstrapPlan::for_core_topology(capabilities, &topology),
        Err(BootstrapRefusal::JetStreamDisabled)
    );
}

#[test]
fn bootstrap_refuses_old_nats_server() {
    let topology = CoreTopology::from_nodes(vec![node_id("core_1")]).expect("single topology");
    let capabilities = single_capabilities("core_1", NatsServerVersion::new(2, 11, 9), true);

    assert_eq!(
        BootstrapPlan::for_core_topology(capabilities, &topology),
        Err(BootstrapRefusal::UnsupportedServerVersion {
            minimum: NatsServerVersion::new(2, 12, 0),
            actual: NatsServerVersion::new(2, 11, 9),
        })
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn jetstream_peers<const N: usize>(nodes: [&str; N]) -> JetStreamPeerSet {
    JetStreamPeerSet::from_nodes(nodes.into_iter().map(node_id).collect()).expect("valid peers")
}

fn single_capabilities(
    node: &str,
    version: NatsServerVersion,
    jetstream_enabled: bool,
) -> NatsServerCapabilities {
    NatsServerCapabilities::new(version, jetstream_enabled, node_id(node))
}
