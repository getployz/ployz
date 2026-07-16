use ployz_core::deploy::{
    ContainerCommand, ContainerEntrypoint, ContainerHealthcheck, ContainerHealthcheckTest,
    ContainerMountPath, ContainerRuntimeSpec, DatasetName, DependencyCondition,
    DeployCleanupContainer, DeployPhasePlan, DeployPlan, DeployPlanError, DeployPlanStep,
    DeployPlanningInput as CoreDeployPlanningInput, DeployPreparationInput, DeployRoute,
    DeployRouteTarget, DeployServicePlan, DeployServiceSpec, EnvName, EnvValue,
    ExistingReplicaCreationGate, ExistingReplicaPolicy, ExistingServiceReplica,
    HealthcheckShellCommand, ImageReference, ImageSource, ObservedCleanupCandidate, PlatformImage,
    PreStartHook, PreStartHookStep, PushedImageReceipt, ReplicaCount, ReplicaSlot,
    ServiceDependency, ServiceEnvironment, ServiceVolumeMount, StopGracePeriod,
    VolumeAdmissionFailure, VolumeMaxSizeBytes, VolumeName, VolumeSpec, ZfsPoolName,
    auto_hostname_route_binding_commits, namespace_revision_id_for,
    namespace_route_binding_removals, namespace_serving_target_removals, plan_namespace_deploy,
    prepare_deploy, validate_deploy_route_bindings,
};
use ployz_core::ids::MachineId;
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::ingress::{AutomaticHostnameLabel, RouteBindingOrigin};
use ployz_core::intent::{RouteBindingState, VolumePinState};
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::operation::{RouteHostname, RoutePort, RouteTarget};
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, operation_id, route_hostname, service_id,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct DeployPlanningInput {
    service: DeployServiceSpec,
    volumes: BTreeMap<VolumeName, VolumeSpec>,
    eligible_machines: Vec<MachineId>,
    existing_replicas: Vec<ExistingServiceReplica>,
    cleanup_candidates: Vec<ObservedCleanupCandidate>,
    volume_pins: Vec<VolumePinState>,
    storage_testimony: BTreeMap<MachineId, Option<ployz_core::machine::StorageCapability>>,
}

#[test]
fn namespace_revision_entry_identity_is_stable_for_same_service_shape() {
    let left = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);
    let right = service_spec(
        "svc_api",
        "ghcr.io/acme/api:rev-1",
        3,
        Some(SpecRoute {
            endpoint_port: 8080,
        }),
    );

    assert_eq!(
        left.namespace_revision_entry_id(&namespace_id("default")),
        right.namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_eq!(
        left.namespace_revision_entry_id(&namespace_id("default"))
            .as_str(),
        "c0efb9e9ad7fc2843b0e4988e101df759a2d554498dd0797bf5866d1b37641a9"
    );
}

#[test]
fn namespace_revision_entry_identity_changes_for_service_or_image_change() {
    let base = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);

    assert_ne!(
        base.namespace_revision_entry_id(&namespace_id("default")),
        service_spec("svc_web", "ghcr.io/acme/api:rev-1", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_eq!(
        service_spec("svc_web", "ghcr.io/acme/api:rev-1", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
            .as_str(),
        "f33e503c27652276d057ba043e2d6a53426b7f83c0a07a3789faa03ff392202d"
    );
    assert_ne!(
        base.namespace_revision_entry_id(&namespace_id("default")),
        service_spec("svc_api", "ghcr.io/acme/api:rev-2", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_eq!(
        service_spec("svc_api", "ghcr.io/acme/api:rev-2", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
            .as_str(),
        "034347bbe51f5d3d20359e3576cff43b8ed3d1aab21f16551a3621a63e7bceec"
    );
}

#[test]
fn mutable_tag_repeats_as_same_namespace_revision_entry_identity() {
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
            .as_str(),
        "0ea3e7c5d9cade815538aa0467430568e61a92ff415f698ae220d2458e7e29b8"
    );
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None)
            .namespace_revision_entry_id(&namespace_id("default")),
        service_spec(
            "svc_api",
            "nginx:latest",
            3,
            Some(SpecRoute {
                endpoint_port: 8080
            })
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
}

#[test]
fn retention_policy_changes_neither_service_entry_nor_namespace_revision_identity() {
    let without_keep = deploy_request(1);
    let mut with_keep = without_keep.clone();
    with_keep.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(2));
    let namespace = namespace_id("default");

    assert_eq!(
        without_keep.namespace_revision_entry_id(&namespace),
        with_keep.namespace_revision_entry_id(&namespace)
    );
    assert_eq!(
        normalized_services(vec![without_keep], BTreeMap::new()).namespace_revision_id(),
        normalized_services(vec![with_keep], BTreeMap::new()).namespace_revision_id()
    );
}

#[test]
fn pushed_image_identity_uses_index_digest_across_platform_receipts() {
    let mut left = service_spec("svc_api", "api:latest", 1, None);
    left.image_source = pushed_image_source(
        [('b', "amd64", "machine_a"), ('c', "arm64", "machine_c")],
        4_102_444_800,
    );
    let mut same_content = left.clone();
    same_content.image_source = pushed_image_source(
        [('b', "amd64", "machine_b"), ('c', "arm64", "machine_d")],
        4_000_000_000,
    );
    let mut changed_content = same_content.clone();
    changed_content.image_source = pushed_image_source(
        [('d', "amd64", "machine_b"), ('c', "arm64", "machine_d")],
        4_000_000_000,
    );

    assert_eq!(
        left.namespace_revision_entry_id(&namespace_id("default")),
        same_content.namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_ne!(
        left.namespace_revision_entry_id(&namespace_id("default")),
        changed_content.namespace_revision_entry_id(&namespace_id("default"))
    );
}

fn pushed_image_source<const N: usize>(
    platforms: [(char, &str, &str); N],
    availability_expires_at: u64,
) -> ImageSource {
    let receipt =
        PushedImageReceipt::try_new(platforms.into_iter().map(|(digest, architecture, seed)| {
            (
                OciPlatform::try_new("linux", architecture).expect("platform"),
                PlatformImage {
                    seed: machine_id(seed),
                    manifest_digest: oci_digest(digest),
                    image_id: oci_digest(digest),
                    availability_expires_at:
                        ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(
                            availability_expires_at,
                        )
                        .expect("expiry"),
                },
            )
        }))
        .expect("pushed image receipt");
    ImageSource::PushedToSeed(receipt)
}

fn oci_digest(hex: char) -> OciDigest {
    OciDigest::try_new(format!("sha256:{}", hex.to_string().repeat(64))).expect("valid OCI digest")
}

#[test]
fn namespace_revision_entry_identity_changes_for_each_runtime_field() {
    let base = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);
    let base_id = base.namespace_revision_entry_id(&namespace_id("default"));

    assert_ne!(
        base_id,
        service_spec_with_runtime(
            "svc_api",
            "ghcr.io/acme/api:rev-1",
            runtime_with_command(["bundle", "exec"])
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_ne!(
        base_id,
        service_spec_with_runtime(
            "svc_api",
            "ghcr.io/acme/api:rev-1",
            runtime_with_entrypoint(ContainerEntrypoint::Argv(
                ContainerCommand::try_new(vec!["/init".to_owned()]).expect("non-empty argv")
            ))
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_ne!(
        base_id,
        service_spec_with_runtime(
            "svc_api",
            "ghcr.io/acme/api:rev-1",
            runtime_with_env([("LOG_LEVEL", "debug")])
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_ne!(
        base_id,
        service_spec_with_runtime(
            "svc_api",
            "ghcr.io/acme/api:rev-1",
            runtime_with_stop_grace(30)
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_ne!(
        base_id,
        service_spec_with_runtime(
            "svc_api",
            "ghcr.io/acme/api:rev-1",
            runtime_with_volume_mount("postgres_data", "/var/lib/postgresql/data")
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
}

#[test]
fn namespace_revision_entry_identity_is_stable_for_environment_order() {
    let left = service_spec_with_runtime(
        "svc_api",
        "ghcr.io/acme/api:rev-1",
        runtime_with_env([("BETA", "2"), ("ALPHA", "1")]),
    );
    let right = service_spec_with_runtime(
        "svc_api",
        "ghcr.io/acme/api:rev-1",
        runtime_with_env([("ALPHA", "1"), ("BETA", "2")]),
    );

    assert_eq!(
        left.namespace_revision_entry_id(&namespace_id("default")),
        right.namespace_revision_entry_id(&namespace_id("default"))
    );
}

#[test]
fn namespace_revision_entry_identity_frames_environment_values() {
    let env_injected = service_spec_with_runtime(
        "svc_api",
        "ghcr.io/acme/api:rev-1",
        runtime_with_env([("PAYLOAD", "value\nimage=ghcr.io/acme/api:rev-2")]),
    );
    let image_changed = service_spec_with_runtime(
        "svc_api",
        "ghcr.io/acme/api:rev-2",
        runtime_with_env([("PAYLOAD", "value")]),
    );

    assert_ne!(
        env_injected.namespace_revision_entry_id(&namespace_id("default")),
        image_changed.namespace_revision_entry_id(&namespace_id("default"))
    );
}

#[test]
fn new_service_plan_runs_replicas_across_eligible_machines() {
    let input = planning_input(3, [machine_id("machine_a"), machine_id("machine_b")]);
    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![
                run_step("machine_a", 1),
                run_step("machine_b", 2),
                run_step("machine_a", 3),
            ],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_reuses_running_target_entry_containers() {
    let mut input = planning_input(3, [machine_id("machine_a"), machine_id("machine_b")]);
    input.existing_replicas = vec![existing_replica("machine_b", "ctr_existing")];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![
                use_existing_step("machine_b", "ctr_existing", 1),
                run_step("machine_a", 2),
                run_step("machine_b", 3),
            ],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_counts_duplicate_observations_once() {
    let mut input = planning_input(2, [machine_id("machine_a")]);
    input.existing_replicas = vec![
        existing_replica("machine_b", "ctr_existing"),
        existing_replica("machine_b", "ctr_existing"),
    ];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![
                use_existing_step("machine_b", "ctr_existing", 1),
                run_step("machine_a", 2),
            ],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_does_not_require_eligible_machines_when_reality_already_satisfies_replicas() {
    let mut input = planning_input(1, []);
    input.existing_replicas = vec![existing_replica("machine_b", "ctr_existing")];

    assert_eq!(
        plan_single_service(&input).expect("existing reality satisfies target"),
        deploy_plan(
            &input,
            vec![use_existing_step("machine_b", "ctr_existing", 1)],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_cleans_up_unselected_service_containers_after_success() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.existing_replicas = vec![
        existing_replica("machine_b", "ctr_target_keep"),
        existing_replica("machine_b", "ctr_target_extra"),
    ];
    input.cleanup_candidates = vec![
        cleanup_container_observed("machine_b", "ctr_target_keep", true, None),
        cleanup_container_observed("machine_b", "ctr_target_extra", true, None),
        cleanup_container_observed("machine_b", "ctr_old", true, None),
    ];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![use_existing_step("machine_b", "ctr_target_extra", 1)],
            vec![
                cleanup_container("machine_b", "ctr_old"),
                cleanup_container("machine_b", "ctr_target_keep"),
            ],
        )
    );
}

#[test]
fn service_keep_retains_newest_stopped_superseded_containers_without_counting_running_cleanup() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.service.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(1));
    input.cleanup_candidates = vec![
        cleanup_container_observed("machine_a", "ctr_stopped_old", false, Some(10)),
        cleanup_container_observed("machine_a", "ctr_running_scale_down", true, Some(30)),
        cleanup_container_observed("machine_a", "ctr_stopped_new", false, Some(20)),
    ];

    let plan = plan_single_service(&input).expect("retention plan succeeds");

    assert_eq!(
        plan.cleanup_actions
            .iter()
            .map(|action| action.target().clone())
            .collect::<Vec<_>>(),
        vec![
            cleanup_container("machine_a", "ctr_running_scale_down"),
            cleanup_container("machine_a", "ctr_stopped_old"),
        ]
    );
    assert!(plan.cleanup_actions.iter().all(|action| matches!(
        action,
        ployz_core::deploy::DeployCleanupAction::RemoveContainerAndReclaimImage { .. }
    )));
}

#[test]
fn service_keep_treats_missing_created_time_as_newest_retained_evidence() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.service.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(1));
    input.cleanup_candidates = vec![
        cleanup_container_observed("machine_a", "ctr_known", false, Some(20)),
        cleanup_container_observed("machine_a", "ctr_unknown", false, None),
    ];

    let plan = plan_single_service(&input).expect("retention plan succeeds");

    assert_eq!(
        plan.cleanup_actions
            .iter()
            .map(|action| action.target().clone())
            .collect::<Vec<_>>(),
        vec![cleanup_container("machine_a", "ctr_known")]
    );
    assert_eq!(plan.cleanup_actions.len(), 1);
}

#[test]
fn service_keep_zero_retains_no_stopped_superseded_containers() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.service.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(0));
    input.cleanup_candidates = vec![cleanup_container_observed(
        "machine_a",
        "ctr_stopped",
        false,
        Some(10),
    )];

    let plan = plan_single_service(&input).expect("retention plan succeeds");

    assert_eq!(
        plan.cleanup_actions
            .iter()
            .map(|action| action.target().clone())
            .collect::<Vec<_>>(),
        input
            .cleanup_candidates
            .iter()
            .map(|candidate| candidate.target.clone())
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        plan.cleanup_actions.as_slice(),
        [ployz_core::deploy::DeployCleanupAction::RemoveContainerAndReclaimImage { target, image_identity }]
            if target.container_id == container_id("ctr_stopped")
                && image_identity == &OciDigest::sha256(b"ctr_stopped")
    ));
}

#[test]
fn service_keep_breaks_created_time_ties_deterministically_after_excluding_selected_reuse() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.service.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(1));
    input.existing_replicas = vec![existing_replica("machine_a", "ctr_selected")];
    input.cleanup_candidates = vec![
        cleanup_container_observed("machine_b", "ctr_b", false, Some(10)),
        cleanup_container_observed("machine_a", "ctr_a", false, Some(10)),
        cleanup_container_observed("machine_a", "ctr_selected", true, Some(30)),
    ];

    let plan = plan_single_service(&input).expect("retention plan succeeds");

    assert_eq!(
        plan.cleanup_actions
            .iter()
            .map(|action| action.target().clone())
            .collect::<Vec<_>>(),
        vec![cleanup_container("machine_b", "ctr_b")]
    );
}

#[test]
fn deploy_plan_requires_eligible_machine() {
    assert_eq!(
        plan_single_service(&planning_input(1, [])),
        Err(DeployPlanError::NoEligibleMachines)
    );
}

#[test]
fn namespace_planner_rejects_a_service_id_outside_the_validated_request() {
    let input = CoreDeployPlanningInput {
        service_id: service_id("svc_foreign"),
        eligible_machines: vec![machine_id("machine_a")],
        existing_replicas: Vec::new(),
        cleanup_candidates: Vec::new(),
        volume_pins: Vec::new(),
    };
    let request = normalized_services(vec![deploy_request(1)], BTreeMap::new());

    assert_eq!(
        plan_namespace_deploy(
            &request,
            vec![input],
            Vec::new(),
            ployz_core::deploy::DeployPlanningContext {
                storage_testimony: &BTreeMap::new(),
            },
        ),
        Err(DeployPlanError::UnknownService {
            service_id: service_id("svc_foreign"),
        })
    );
}

#[test]
fn deploy_preparation_rejects_a_service_id_outside_the_validated_request() {
    let request = normalized_services(vec![deploy_request(1)], BTreeMap::new());

    assert_eq!(
        prepare_deploy(
            DeployPreparationInput {
                request: &request,
                service_id: service_id("svc_foreign"),
                occupied_route_bindings: Vec::new(),
                eligible_machines: Vec::new(),
                draining_machines: Vec::new(),
                observed_machines: Vec::new(),
                existing_replica_policy: ExistingReplicaPolicy::ExcludeUnpromoted,
            },
            route_binding_id_for,
        ),
        Err(ployz_core::deploy::DeployPreparationError::UnknownService {
            service_id: service_id("svc_foreign"),
        })
    );
}

#[test]
fn service_dependencies_reorder_namespace_plan_stably() {
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    });
    let api = planning_input(1, [machine_id("machine_a")]);

    let plan = plan_inputs(vec![worker, api], Vec::new()).expect("dependency plan succeeds");

    assert_eq!(
        plan.phases
            .iter()
            .flat_map(|phase| &phase.services)
            .map(|service| service.service_id.clone())
            .collect::<Vec<_>>(),
        vec![service_id("svc_api"), service_id("svc_worker")]
    );
}

#[test]
fn namespace_plan_groups_all_simultaneously_eligible_services_into_deterministic_phases() {
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    });
    let api = planning_input(1, [machine_id("machine_a")]);
    let mut database = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut database.service, |request| {
        request.service_id = service_id("svc_database");
    });

    let plan =
        plan_inputs(vec![worker, database, api], Vec::new()).expect("dependency plan succeeds");

    assert_eq!(
        plan.phases
            .iter()
            .map(|phase| {
                phase
                    .services
                    .iter()
                    .map(|service| service.service_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![
            vec![service_id("svc_api"), service_id("svc_database")],
            vec![service_id("svc_worker")],
        ]
    );
}

#[test]
fn healthy_dependency_requires_an_executable_healthcheck() {
    let database = planning_input(1, [machine_id("machine_a")]);
    let mut api = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut api.service, |request| {
        request.service_id = service_id("svc_web");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Healthy)];
    });

    assert_eq!(
        plan_inputs(vec![api, database], Vec::new(),),
        Err(DeployPlanError::HealthyDependencyWithoutHealthcheck {
            service_id: service_id("svc_web"),
            dependency: service_id("svc_api"),
        })
    );
}

#[test]
fn unknown_service_dependency_fails_planning() {
    let mut api = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut api.service, |request| {
        request.depends_on = vec![dependency("svc_missing", DependencyCondition::Started)];
    });

    assert_eq!(
        plan_inputs(vec![api], Vec::new(),),
        Err(DeployPlanError::UnknownServiceDependency {
            service_id: service_id("svc_api"),
            dependency: service_id("svc_missing"),
        })
    );
}

#[test]
fn namespace_revision_identity_includes_hooks_and_dependencies() {
    let base = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);
    let mut changed = base.clone();
    changed.pre_start = Some(pre_start_hook());
    changed.depends_on = vec![dependency("svc_database", DependencyCondition::Started)];

    assert_ne!(
        namespace_revision_id_for(&namespace_id("default"), &[base]),
        namespace_revision_id_for(&namespace_id("default"), &[changed])
    );
}

#[test]
fn service_dependency_cycle_reports_sorted_unplaced_services() {
    let mut api = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut api.service, |request| {
        request.depends_on = vec![dependency("svc_worker", DependencyCondition::Started)];
    });
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    });

    assert_eq!(
        plan_inputs(vec![worker, api], Vec::new(),),
        Err(DeployPlanError::ServiceDependencyCycle {
            service_ids: vec![service_id("svc_api"), service_id("svc_worker")],
        })
    );
}

#[test]
fn pre_start_hook_step_uses_first_run_container_machine() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    update_request(&mut input.service, |request| {
        request.pre_start = Some(pre_start_hook());
    });

    let plan = plan_single_service(&input).expect("plan succeeds");
    let [phase] = plan.phases.as_slice() else {
        panic!("plan contains one phase");
    };
    let [service] = phase.services.as_slice() else {
        panic!("plan contains one service");
    };

    assert_eq!(
        service.pre_start,
        Some(PreStartHookStep {
            machine_id: machine_id("machine_a"),
        })
    );
}

#[test]
fn pre_start_hook_step_is_absent_when_all_containers_are_reused() {
    let mut input = planning_input(1, []);
    update_request(&mut input.service, |request| {
        request.pre_start = Some(pre_start_hook());
    });
    input.existing_replicas = vec![existing_replica("machine_b", "ctr_existing")];

    let plan = plan_single_service(&input).expect("existing reality satisfies target");
    let [phase] = plan.phases.as_slice() else {
        panic!("plan contains one phase");
    };
    let [service] = phase.services.as_slice() else {
        panic!("plan contains one service");
    };

    assert_eq!(service.pre_start, None);
}

#[test]
fn volume_backed_service_pins_to_first_eligible_machine() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan_with_volume_pins(
            &input,
            vec![run_step("machine_a", 1), run_step("machine_a", 2)],
            vec![volume_pin("postgres_data", "machine_a")],
            Vec::new(),
        )
    );
}

#[test]
fn duplicate_plain_volume_mounts_emit_one_pin_commit() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("data", "/data"),
            volume_mount("data", "/var/lib/data"),
        ],
    );

    assert_eq!(
        plan_single_service(&input).expect("duplicate mounts share one placement pin"),
        deploy_plan_with_volume_pins(
            &input,
            vec![run_step("machine_a", 1)],
            vec![volume_pin("data", "machine_a")],
            Vec::new(),
        )
    );
}

#[test]
fn new_plain_volume_pin_commits_are_sorted_by_volume_name() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("uploads", "/uploads"),
            volume_mount("data", "/data"),
        ],
    );

    assert_eq!(
        plan_single_service(&input).expect("new volume pins are deterministic"),
        deploy_plan_with_volume_pins(
            &input,
            vec![run_step("machine_a", 1)],
            vec![
                volume_pin("data", "machine_a"),
                volume_pin("uploads", "machine_a"),
            ],
            Vec::new(),
        )
    );
}

#[test]
fn provisioned_volume_without_a_dataset_backed_pin_fails_before_plain_pin_commit() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_volume_mounts(
        &mut input,
        vec![(
            volume_mount("data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
            },
        )],
    );

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::ProvisionedVolumeRequiresProvisioning {
            service_id: service_id("svc_api"),
            volume_name: VolumeName::try_new("data").expect("volume name"),
        })
    );
}

#[test]
fn provisioned_volume_with_a_dataset_backed_pin_can_be_placed() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_volume_mounts(
        &mut input,
        vec![(
            volume_mount("data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
            },
        )],
    );
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];

    assert_eq!(
        plan_single_service(&input).expect("dataset-backed pin is placeable"),
        deploy_plan(&input, vec![run_step("machine_a", 1)], Vec::new())
    );
}

#[test]
fn plain_declaration_rejects_a_provisioned_pin() {
    let mut input = planning_input(1, [machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmission {
            failure: VolumeAdmissionFailure::KindConversion {
                declaration: VolumeSpec::Plain,
                pin_kind: ployz_core::intent::VolumeKind::Provisioned { .. },
                ..
            },
            ..
        })
    ));
}

#[test]
fn provisioned_declaration_rejects_a_plain_pin() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_volume_mounts(
        &mut input,
        vec![(
            volume_mount("data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
            },
        )],
    );
    input.volume_pins = vec![volume_pin("data", "machine_a")];

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmission {
            failure: VolumeAdmissionFailure::KindConversion {
                declaration: VolumeSpec::Provisioned { .. },
                pin_kind: ployz_core::intent::VolumeKind::Plain,
                ..
            },
            ..
        })
    ));
}

#[test]
fn duplicate_pins_for_one_volume_fail_as_ambiguous_before_placement() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![
        volume_pin("data", "machine_a"),
        volume_pin("data", "machine_b"),
    ];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmission {
            service_id: service_id("svc_api"),
            failure: VolumeAdmissionFailure::AmbiguousPins {
                volume_name: VolumeName::try_new("data").expect("volume name"),
                pin_count: 2,
            },
        })
    );
}

#[test]
fn provisioned_declaration_requires_provisioning_to_grow_a_pinned_maximum() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_volume_mounts(
        &mut input,
        vec![(
            volume_mount("data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(2048).expect("non-zero size"),
            },
        )],
    );
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];
    input.storage_testimony = BTreeMap::from([(
        machine_id("machine_a"),
        Some(ready_storage_with_quota("data", 1024)),
    )]);

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::ProvisionedVolumeRequiresProvisioning {
            service_id: service_id("svc_api"),
            volume_name: VolumeName::try_new("data").expect("volume name"),
        })
    );
}

#[test]
fn provisioned_declaration_rejects_shrinking_a_pinned_maximum() {
    let mut input = planning_input(1, [machine_id("machine_b")]);
    declare_volume_mounts(
        &mut input,
        vec![(
            volume_mount("data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(512).expect("non-zero size"),
            },
        )],
    );
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmission {
            service_id: service_id("svc_api"),
            failure: VolumeAdmissionFailure::QuotaShrink {
                volume_name: VolumeName::try_new("data").expect("volume name"),
                declared_max_size_bytes: VolumeMaxSizeBytes::try_new(512).expect("non-zero size"),
                pinned_max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
            },
        })
    );
}

#[test]
fn volume_backed_service_uses_existing_pin() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![run_step("machine_b", 1), run_step("machine_b", 2)],
            Vec::new()
        )
    );
}

#[test]
fn volume_backed_service_fails_when_existing_pin_is_not_eligible() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::NoEligibleMachines)
    );
}

#[test]
fn volume_backed_service_reuses_only_replicas_on_pinned_machine() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];
    input.existing_replicas = vec![
        existing_replica("machine_a", "ctr_off_pin"),
        existing_replica("machine_b", "ctr_pinned"),
    ];
    input.cleanup_candidates = vec![
        cleanup_container_observed("machine_a", "ctr_off_pin", true, None),
        cleanup_container_observed("machine_b", "ctr_pinned", true, None),
    ];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![
                use_existing_step("machine_b", "ctr_pinned", 1),
                run_step("machine_b", 2),
            ],
            vec![cleanup_container("machine_a", "ctr_off_pin")],
        )
    );
}

#[test]
fn namespace_volume_pin_commits_are_visible_to_later_service_plans() {
    let mut first = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut first, vec![volume_mount("data", "/data")]);
    let mut second = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    update_request(&mut second.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_plain_volume_mounts(
        &mut second,
        vec![
            volume_mount("data", "/data"),
            volume_mount("uploads", "/uploads"),
        ],
    );
    second.volume_pins = vec![volume_pin("uploads", "machine_b")];

    assert_eq!(
        plan_inputs(vec![first, second], Vec::new(),),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_worker"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}

#[test]
fn multi_service_redeploy_canonicalizes_identical_pin_fan_out() {
    let pin = volume_pin("data", "machine_a");
    let mut api = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(&mut api, vec![volume_mount("data", "/data")]);
    api.volume_pins = vec![pin.clone()];
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_plain_volume_mounts(&mut worker, vec![volume_mount("data", "/work")]);
    worker.volume_pins = vec![pin];

    let plan = plan_inputs(vec![api, worker], Vec::new())
        .expect("identical snapshots from each service are one durable pin");

    assert!(plan.volume_pin_commits.is_empty());
    assert_eq!(plan.target_machines(), vec![machine_id("machine_a")]);
}

#[test]
fn other_namespace_pin_does_not_assign_an_unpinned_current_namespace_volume() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![VolumePinState::plain(
        namespace_id("other_namespace"),
        VolumeName::try_new("data").expect("volume name"),
        machine_id("machine_b"),
    )];

    let plan = plan_single_service(&input).expect("other namespace pin is irrelevant");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_a")]);
    assert_eq!(
        plan.volume_pin_commits,
        vec![volume_pin("data", "machine_a")]
    );
}

#[test]
fn multi_service_provisioned_births_share_one_machine_capacity_snapshot() {
    let mut api = planning_input(1, [machine_id("machine_a")]);
    declare_volume_mounts(
        &mut api,
        vec![(
            volume_mount("api_data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(600).expect("non-zero size"),
            },
        )],
    );
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_volume_mounts(
        &mut worker,
        vec![(
            volume_mount("worker_data", "/work"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(600).expect("non-zero size"),
            },
        )],
    );
    let testimony = BTreeMap::from([(
        machine_id("machine_a"),
        Some(ready_storage_with_capacity(1_000, Vec::new())),
    )]);
    api.storage_testimony = testimony.clone();
    worker.storage_testimony = testimony;

    assert!(matches!(
        plan_inputs(vec![api, worker], Vec::new()),
        Err(DeployPlanError::VolumeAdmission {
            failure: VolumeAdmissionFailure::CapacityExceeded {
                available_bytes: 1_000,
                requested_total_bytes: 1_200,
            },
            ..
        })
    ));
}

#[test]
fn admitted_provisioned_birth_never_reaches_container_planning() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_volume_mounts(
        &mut input,
        vec![(
            volume_mount("data", "/data"),
            VolumeSpec::Provisioned {
                max_size_bytes: VolumeMaxSizeBytes::try_new(512).expect("non-zero size"),
            },
        )],
    );

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::ProvisionedVolumeRequiresProvisioning { .. })
    ));
}

#[test]
fn service_with_volumes_on_different_pinned_machines_fails_planning() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("postgres_data", "/var/lib/postgres"),
            volume_mount("uploads", "/srv/uploads"),
        ],
    );
    input.volume_pins = vec![
        volume_pin("postgres_data", "machine_a"),
        volume_pin("uploads", "machine_b"),
    ];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_api"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}

#[test]
fn deploy_preparation_uses_active_revision_and_running_target_replicas() {
    let request = deploy_request(2);
    let entry_id = request.namespace_revision_entry_id(&namespace_id("default"));
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            draining_machines: Vec::new(),
            observed_machines: vec![observed_machine(
                "machine_b",
                [
                    observed_container(
                        "machine_b",
                        "ctr_target",
                        "svc_api",
                        entry_id.as_str(),
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::running_unroutable(),
                    ),
                    observed_container(
                        "machine_b",
                        "ctr_old",
                        "svc_api",
                        "entry_old",
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::running_unroutable(),
                    ),
                    observed_container(
                        "machine_b",
                        "ctr_job",
                        "svc_api",
                        entry_id.as_str(),
                        ManagedContainerKind::Job,
                        ContainerRuntimeState::running_unroutable(),
                    ),
                    observed_container(
                        "machine_b",
                        "ctr_exited",
                        "svc_api",
                        entry_id.as_str(),
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::Exited,
                    ),
                ],
            )],
            existing_replica_policy: ExistingReplicaPolicy::Promoted {
                interrupted_operation_ids: BTreeSet::new(),
            },
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(prepared.service, deploy_request(2));
    assert_eq!(
        prepared.eligible_machines,
        vec![machine_id("machine_a"), machine_id("machine_b")]
    );
    assert_eq!(
        prepared.existing_replicas,
        vec![existing_replica("machine_b", "ctr_target")]
    );
    assert_eq!(
        prepared.cleanup_candidates,
        vec![
            observed_cleanup_candidate("machine_b", "ctr_target", entry_id.as_str(), true, None),
            observed_cleanup_candidate("machine_b", "ctr_old", "entry_old", true, None),
            observed_cleanup_candidate("machine_b", "ctr_exited", entry_id.as_str(), false, None),
        ]
    );
}

#[test]
fn deploy_preparation_marks_interrupted_replicas_for_creation_gating() {
    let request = deploy_request(2);
    let entry_id = request.namespace_revision_entry_id(&namespace_id("default"));
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            draining_machines: Vec::new(),
            observed_machines: vec![observed_machine(
                "machine_b",
                [observed_container(
                    "machine_b",
                    "ctr_retained",
                    "svc_api",
                    entry_id.as_str(),
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                )],
            )],
            existing_replica_policy: ExistingReplicaPolicy::RecoverInterrupted {
                operation_ids: BTreeSet::from([operation_id("op_existing")]),
            },
        },
        route_binding_id_for,
    )
    .expect("interrupted deploy preparation");

    assert_eq!(
        prepared.existing_replicas,
        vec![ExistingServiceReplica {
            machine_id: machine_id("machine_b"),
            container_id: container_id("ctr_retained"),
            creation_gate: ExistingReplicaCreationGate::RequiredAfterInterruption,
        }]
    );
}

#[test]
fn promoted_entry_still_gates_replica_from_interrupted_provenance() {
    let request = deploy_request(2);
    let entry_id = request.namespace_revision_entry_id(&namespace_id("default"));
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());
    let mut completed = observed_container(
        "machine_a",
        "ctr_completed",
        "svc_api",
        entry_id.as_str(),
        ManagedContainerKind::Service,
        ContainerRuntimeState::running_unroutable(),
    );
    completed.identity.operation_id = operation_id("op_completed");
    let mut interrupted = observed_container(
        "machine_b",
        "ctr_interrupted",
        "svc_api",
        entry_id.as_str(),
        ManagedContainerKind::Service,
        ContainerRuntimeState::running_unroutable(),
    );
    interrupted.identity.operation_id = operation_id("op_interrupted");
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id,
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            draining_machines: Vec::new(),
            observed_machines: vec![
                observed_machine("machine_a", [completed]),
                observed_machine("machine_b", [interrupted]),
            ],
            existing_replica_policy: ExistingReplicaPolicy::Promoted {
                interrupted_operation_ids: BTreeSet::from([operation_id("op_interrupted")]),
            },
        },
        route_binding_id_for,
    )
    .expect("promoted deploy preparation");

    assert_eq!(
        prepared.existing_replicas,
        vec![
            ExistingServiceReplica {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_completed"),
                creation_gate: ExistingReplicaCreationGate::AlreadyPassed,
            },
            ExistingServiceReplica {
                machine_id: machine_id("machine_b"),
                container_id: container_id("ctr_interrupted"),
                creation_gate: ExistingReplicaCreationGate::RequiredAfterInterruption,
            },
        ]
    );
}

#[test]
fn deploy_preparation_evacuates_draining_machine_replicas() {
    let request = deploy_request(1);
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            draining_machines: vec![machine_id("machine_b")],
            observed_machines: vec![observed_machine(
                "machine_b",
                [observed_container(
                    "machine_b",
                    "ctr_target",
                    "svc_api",
                    "entry_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                )],
            )],
            existing_replica_policy: ExistingReplicaPolicy::Promoted {
                interrupted_operation_ids: BTreeSet::new(),
            },
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(prepared.existing_replicas, Vec::new());
    assert_eq!(
        prepared.cleanup_candidates,
        vec![observed_cleanup_candidate(
            "machine_b",
            "ctr_target",
            "entry_1",
            true,
            None,
        )]
    );

    let input = DeployPlanningInput {
        service: prepared.service,
        volumes: BTreeMap::new(),
        eligible_machines: prepared.eligible_machines,
        existing_replicas: prepared.existing_replicas,
        cleanup_candidates: prepared.cleanup_candidates,
        volume_pins: Vec::new(),
        storage_testimony: BTreeMap::from([(machine_id("machine_a"), Some(ready_storage()))]),
    };
    let expected = deploy_plan(
        &input,
        vec![run_step("machine_a", 1)],
        vec![cleanup_container_with_entry(
            "machine_b",
            "ctr_target",
            "entry_1",
        )],
    );
    let plan = plan_inputs(vec![input], Vec::new()).expect("plan succeeds");
    assert_eq!(plan, expected);
}

#[test]
fn routed_deploy_preparation_reuses_matching_identity_regardless_of_endpoint_port() {
    let mut request = deploy_request(2);
    set_routes(&mut request, vec![deploy_route("api.example.com", 8080)]);
    let entry_id = request.namespace_revision_entry_id(&namespace_id("default"));
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            draining_machines: Vec::new(),
            observed_machines: vec![observed_machine(
                "machine_b",
                [
                    observed_container(
                        "machine_b",
                        "ctr_wrong_port",
                        "svc_api",
                        entry_id.as_str(),
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
                    ),
                    observed_container(
                        "machine_b",
                        "ctr_target",
                        "svc_api",
                        entry_id.as_str(),
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::running_at(endpoint_ip("10.0.0.3")),
                    ),
                ],
            )],
            existing_replica_policy: ExistingReplicaPolicy::Promoted {
                interrupted_operation_ids: BTreeSet::new(),
            },
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(
        prepared.existing_replicas,
        vec![
            existing_replica("machine_b", "ctr_wrong_port"),
            existing_replica("machine_b", "ctr_target"),
        ]
    );
}

#[test]
fn deploy_preparation_commits_multiple_routes_per_service() {
    let mut request = deploy_request(1);
    set_routes(
        &mut request,
        vec![
            deploy_route("api.example.com", 8080),
            deploy_route("www.example.com", 8080),
        ],
    );
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            draining_machines: Vec::new(),
            observed_machines: Vec::new(),
            existing_replica_policy: ExistingReplicaPolicy::ExcludeUnpromoted,
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(
        prepared.route_commits,
        vec![
            RouteBindingState {
                id: route_binding_id("route_api_example_com"),
                namespace_id: namespace_id("default"),
                target: route_target("api.example.com"),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                origin: RouteBindingOrigin::Declared,
            },
            RouteBindingState {
                id: route_binding_id("route_www_example_com"),
                namespace_id: namespace_id("default"),
                target: route_target("www.example.com"),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                origin: RouteBindingOrigin::Declared,
            },
        ]
    );
}

#[test]
fn declared_route_reroute_reuses_the_binding_identity_and_updates_endpoint_port() {
    let mut request = deploy_request(1);
    set_routes(&mut request, vec![deploy_route("api.example.com", 9090)]);
    let mut existing = route_binding_state("api.example.com", "svc_api");
    existing.id = route_binding_id("route_existing");
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: vec![existing],
            eligible_machines: Vec::new(),
            draining_machines: Vec::new(),
            observed_machines: Vec::new(),
            existing_replica_policy: ExistingReplicaPolicy::ExcludeUnpromoted,
        },
        route_binding_id_for,
    )
    .expect("declared route reuse");
    let [commit] = prepared.route_commits.as_slice() else {
        panic!("one declared route commit")
    };

    assert_eq!(commit.id, route_binding_id("route_existing"));
    assert_eq!(commit.endpoint_port, route_port(9090));
}

#[test]
fn declared_route_rejects_duplicate_target_regardless_of_endpoint_port() {
    for duplicate_port in [8080, 9090] {
        let mut request = deploy_request(1);
        set_routes(
            &mut request,
            vec![
                deploy_route("api.example.com", 8080),
                deploy_route("api.example.com", duplicate_port),
            ],
        );
        let normalized = normalized_services(vec![request.clone()], BTreeMap::new());

        let error = prepare_deploy(
            DeployPreparationInput {
                request: &normalized,
                service_id: request.service_id.clone(),
                occupied_route_bindings: Vec::new(),
                eligible_machines: Vec::new(),
                draining_machines: Vec::new(),
                observed_machines: Vec::new(),
                existing_replica_policy: ExistingReplicaPolicy::ExcludeUnpromoted,
            },
            route_binding_id_for,
        )
        .expect_err("duplicate declared target must collide");

        assert!(matches!(
            error,
            ployz_core::deploy::DeployPreparationError::Route(
                ployz_core::deploy::RouteBindingCommitError::HostnameCollision { .. }
            )
        ));
    }
}

#[test]
fn declared_route_reroute_rejects_other_owners_and_automatic_bindings() {
    let mut request = deploy_request(1);
    set_routes(&mut request, vec![deploy_route("api.example.com", 9090)]);

    let mut other_service = route_binding_state("api.example.com", "svc_worker");
    other_service.id = route_binding_id("route_other_service");
    let mut other_namespace = route_binding_state("api.example.com", "svc_api");
    other_namespace.id = route_binding_id("route_other_namespace");
    other_namespace.namespace_id = namespace_id("other");
    let mut automatic = route_binding_state("api.example.com", "svc_api");
    automatic.id = route_binding_id("route_automatic");
    automatic.origin = RouteBindingOrigin::Automatic;

    for occupied in [other_service, other_namespace, automatic] {
        let normalized = normalized_services(vec![request.clone()], BTreeMap::new());
        let error = prepare_deploy(
            DeployPreparationInput {
                request: &normalized,
                service_id: request.service_id.clone(),
                occupied_route_bindings: vec![occupied],
                eligible_machines: Vec::new(),
                draining_machines: Vec::new(),
                observed_machines: Vec::new(),
                existing_replica_policy: ExistingReplicaPolicy::ExcludeUnpromoted,
            },
            route_binding_id_for,
        )
        .expect_err("hostname owner must collide");

        assert!(matches!(
            error,
            ployz_core::deploy::DeployPreparationError::Route(
                ployz_core::deploy::RouteBindingCommitError::HostnameCollision { .. }
            )
        ));
    }
}

#[test]
fn namespace_route_removals_detach_undeclared_targets_including_omitted_services() {
    // `admin` is owned by a declared service but no longer declared;
    // `orphan` is owned by a service the manifest omits entirely. Both are
    // detached: the manifest is the full desired route state.
    let removals = namespace_route_binding_removals(
        &namespace_id("default"),
        &[
            route_target("api.example.com"),
            route_target("www.example.com"),
        ],
        &[
            route_binding_state("admin.example.com", "svc_api"),
            route_binding_state("orphan.example.com", "svc_omitted"),
            route_binding_state("api.example.com", "svc_api"),
        ],
    );

    assert_eq!(
        removals,
        vec![
            route_target("admin.example.com"),
            route_target("orphan.example.com"),
        ]
    );
}

#[test]
fn namespace_route_removals_ignore_other_namespaces() {
    let mut foreign = route_binding_state("other.example.com", "svc_api");
    foreign.namespace_id = namespace_id("other");

    assert!(namespace_route_binding_removals(&namespace_id("default"), &[], &[foreign]).is_empty());
}

#[test]
fn namespace_serving_removals_unpublish_omitted_services_only() {
    let removals = namespace_serving_target_removals(
        &namespace_id("default"),
        &[service_id("svc_api")],
        &[
            serving_target_entry("svc_api", "entry_1"),
            serving_target_entry("svc_omitted", "entry_old"),
        ],
    );

    assert_eq!(
        removals,
        vec![serving_target_entry("svc_omitted", "entry_old")]
    );
}

#[test]
fn deploy_preparation_updates_endpoint_port_without_container_plan_changes() {
    let mut request = deploy_request(1);
    set_routes(&mut request, vec![deploy_route("api.example.com", 8080)]);
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: Vec::new(),
            draining_machines: Vec::new(),
            observed_machines: Vec::new(),
            existing_replica_policy: ExistingReplicaPolicy::ExcludeUnpromoted,
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(
        prepared.route_commits,
        vec![RouteBindingState {
            id: route_binding_id("route_api_example_com"),
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com"),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            origin: RouteBindingOrigin::Declared,
        }]
    );
    assert!(prepared.existing_replicas.is_empty());
    assert!(prepared.cleanup_candidates.is_empty());
}

#[test]
fn namespace_route_removals_keep_targets_reassigned_to_another_service() {
    // `moved.example.com` was owned by `svc_api` before this deploy and is
    // now declared by a sibling service in the same manifest. Removal is a
    // namespace-level decision, so the moved target is not detached.
    let removals = namespace_route_binding_removals(
        &namespace_id("default"),
        &[route_target("moved.example.com")],
        &[route_binding_state("moved.example.com", "svc_api")],
    );

    assert!(removals.is_empty());
}

#[test]
fn automatic_route_commit_uses_the_exact_requested_label() {
    let mut request = deploy_request(1);
    set_routes(&mut request, vec![automatic_deploy_route("api", 8080)]);

    let commits = auto_hostname_route_binding_commits(
        &namespace_id("default"),
        &request,
        Some(&route_hostname("lease.up.ployz.app")),
        &[],
        route_binding_id_for,
    )
    .expect("automatic binding");
    let [commit] = commits.as_slice() else {
        panic!("one automatic binding")
    };

    assert_eq!(commit.target, route_target("api.lease.up.ployz.app"));
}

#[test]
fn automatic_route_reroute_reuses_the_binding_identity_and_updates_endpoint_port() {
    let mut request = deploy_request(1);
    set_routes(&mut request, vec![automatic_deploy_route("api", 9090)]);
    let mut existing = route_binding_state("api.apps.example.com", "svc_api");
    existing.id = route_binding_id("route_existing");
    existing.origin = RouteBindingOrigin::Automatic;

    let commits = auto_hostname_route_binding_commits(
        &namespace_id("default"),
        &request,
        Some(&route_hostname("apps.example.com")),
        &[existing],
        route_binding_id_for,
    )
    .expect("reused binding");
    let [commit] = commits.as_slice() else {
        panic!("one reused binding")
    };

    assert_eq!(commit.id, route_binding_id("route_existing"));
    assert_eq!(commit.endpoint_port, route_port(9090));
}

#[test]
fn automatic_route_rejects_duplicate_target_regardless_of_endpoint_port() {
    for duplicate_port in [8080, 9090] {
        let mut request = deploy_request(1);
        set_routes(
            &mut request,
            vec![
                automatic_deploy_route("api", 8080),
                automatic_deploy_route("api", duplicate_port),
            ],
        );

        let error = auto_hostname_route_binding_commits(
            &namespace_id("default"),
            &request,
            Some(&route_hostname("apps.example.com")),
            &[],
            route_binding_id_for,
        )
        .expect_err("duplicate automatic target must collide");

        assert!(matches!(
            error,
            ployz_core::deploy::AutoHostnameRouteBindingError::HostnameCollision { .. }
        ));
    }
}

#[test]
fn deploy_route_validation_rejects_duplicate_service_ids() {
    let mut first = service_spec("svc_api", "registry.example/api:rev-1", 1, None);
    first.routes = vec![automatic_deploy_route("api", 8080)];
    let mut second = first.clone();
    second.routes = vec![automatic_deploy_route("api", 9090)];
    let request = ployz_core::deploy::DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![first, second],
    };
    let request = ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(request)
        .expect("deploy request normalizes");

    let error = validate_deploy_route_bindings(
        &request,
        Some(&route_hostname("apps.example.com")),
        &[],
        route_binding_id_for,
    )
    .expect_err("duplicate service ids must be rejected");

    assert!(matches!(
        error,
        ployz_core::deploy::DeployRouteBindingValidationError::DuplicateServiceId {
            service_id: duplicate_service_id
        } if duplicate_service_id == service_id("svc_api")
    ));
}

#[test]
fn automatic_route_rejects_a_declared_hostname_collision() {
    let mut request = deploy_request(1);
    set_routes(&mut request, vec![automatic_deploy_route("api", 8080)]);
    let existing = route_binding_state("api.apps.example.com", "svc_api");

    let error = auto_hostname_route_binding_commits(
        &namespace_id("default"),
        &request,
        Some(&route_hostname("apps.example.com")),
        &[existing],
        route_binding_id_for,
    )
    .expect_err("declared collision");

    assert!(matches!(
        error,
        ployz_core::deploy::AutoHostnameRouteBindingError::HostnameCollision { .. }
    ));
}

#[test]
fn deploy_route_validation_reuses_identical_automatic_binding() {
    let mut service = service_spec("svc_api", "registry.example/api:rev-1", 1, None);
    service.routes = vec![automatic_deploy_route("api", 8080)];
    let request = ployz_core::deploy::DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![service],
    };
    let request = ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(request)
        .expect("deploy request normalizes");
    let mut existing = route_binding_state("api.apps.example.com", "svc_api");
    existing.id = route_binding_id("route_existing");
    existing.origin = RouteBindingOrigin::Automatic;

    let commits = validate_deploy_route_bindings(
        &request,
        Some(&route_hostname("apps.example.com")),
        &[existing],
        route_binding_id_for,
    )
    .expect("identical route is valid");

    let [commit] = commits.as_slice() else {
        panic!("expected one route binding commit");
    };
    assert_eq!(commit.id, route_binding_id("route_existing"));
}

#[test]
fn namespace_revision_entry_id_pins_the_versioned_encoding() {
    // Golden pin: this digest covers the encoding version tag, service id,
    // and image reference. It must only change through a deliberate
    // encoding version bump (ADR 0022) - an unintended change here means
    // every running container would be replaced after upgrade.
    let entry_id = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None)
        .namespace_revision_entry_id(&namespace_id("default"));

    assert_eq!(
        entry_id.as_str(),
        "c0efb9e9ad7fc2843b0e4988e101df759a2d554498dd0797bf5866d1b37641a9"
    );
}

#[test]
fn deploy_preparation_ignores_same_service_id_in_other_namespace() {
    // Another namespace's running container with the same service id is
    // neither a reusable replica nor a cleanup candidate.
    let mut foreign = observed_container(
        "machine_a",
        "ctr_foreign",
        "svc_api",
        "entry_1",
        ManagedContainerKind::Service,
        ContainerRuntimeState::running_unroutable(),
    );
    foreign.identity.namespace_id = namespace_id("other");
    let request = deploy_request(1);
    let normalized = normalized_services(vec![request.clone()], BTreeMap::new());
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: &normalized,
            service_id: request.service_id.clone(),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            draining_machines: Vec::new(),
            observed_machines: vec![observed_machine("machine_a", [foreign])],
            existing_replica_policy: ExistingReplicaPolicy::Promoted {
                interrupted_operation_ids: BTreeSet::new(),
            },
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert!(prepared.existing_replicas.is_empty());
    assert!(prepared.cleanup_candidates.is_empty());
}

#[test]
fn namespace_revision_entry_id_differs_across_namespaces() {
    // Two namespaces deploying the same service name and image must never
    // share an entry identity: the id travels through labels and gateway
    // matching, where a collision would serve one namespace's traffic from
    // another namespace's containers (ADR 0022).
    let spec = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);

    assert_ne!(
        spec.namespace_revision_entry_id(&namespace_id("team-a")),
        spec.namespace_revision_entry_id(&namespace_id("team-b"))
    );
}

struct SpecRoute {
    endpoint_port: u16,
}

fn service_spec(
    service: &str,
    image: &str,
    replicas: u16,
    route: Option<SpecRoute>,
) -> ployz_core::deploy::DeployServiceSpec {
    ployz_core::deploy::DeployServiceSpec {
        keep: None,
        service_id: service_id(service),
        image: ImageReference::try_new(image).expect("valid image"),
        image_source: ployz_core::deploy::ImageSource::Registry,
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        runtime: ContainerRuntimeSpec::image_defaults(),
        pre_start: None,
        depends_on: Vec::new(),
        routes: route
            .map(|route| vec![deploy_route("api.example.com", route.endpoint_port)])
            .unwrap_or_default(),
    }
}

fn service_spec_with_runtime(
    service: &str,
    image: &str,
    runtime: ContainerRuntimeSpec,
) -> ployz_core::deploy::DeployServiceSpec {
    ployz_core::deploy::DeployServiceSpec {
        keep: None,
        service_id: service_id(service),
        image: ImageReference::try_new(image).expect("valid image"),
        image_source: ployz_core::deploy::ImageSource::Registry,
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        runtime,
        pre_start: None,
        depends_on: Vec::new(),
        routes: Vec::new(),
    }
}

fn runtime_with_command(args: impl IntoIterator<Item = &'static str>) -> ContainerRuntimeSpec {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(ContainerCommand::try_new(args_to_vec(args)).expect("valid command"));
    runtime
}

fn runtime_with_entrypoint(entrypoint: ContainerEntrypoint) -> ContainerRuntimeSpec {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.entrypoint = Some(entrypoint);
    runtime
}

fn runtime_with_env(
    items: impl IntoIterator<Item = (&'static str, &'static str)>,
) -> ContainerRuntimeSpec {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    let environment = items
        .into_iter()
        .map(|(name, value)| {
            (
                EnvName::try_new(name).expect("valid env name"),
                EnvValue::try_new(value).expect("valid env value"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    runtime.environment = ServiceEnvironment::from(environment);
    runtime
}

fn runtime_with_stop_grace(seconds: u32) -> ContainerRuntimeSpec {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.stop_grace_period = StopGracePeriod::from(seconds);
    runtime
}

fn runtime_with_volume_mount(volume_name: &str, target: &str) -> ContainerRuntimeSpec {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.volume_mounts = vec![volume_mount(volume_name, target)];
    runtime
}

fn args_to_vec(args: impl IntoIterator<Item = &'static str>) -> Vec<String> {
    args.into_iter().map(str::to_owned).collect()
}

fn pre_start_hook() -> PreStartHook {
    PreStartHook {
        command: ContainerCommand::try_new(vec!["true".to_owned()]).expect("valid command"),
    }
}

fn planning_input(
    replicas: u16,
    eligible_machines: impl IntoIterator<Item = MachineId>,
) -> DeployPlanningInput {
    let eligible_machines = eligible_machines.into_iter().collect::<Vec<_>>();
    DeployPlanningInput {
        service: deploy_request(replicas),
        volumes: BTreeMap::new(),
        storage_testimony: eligible_machines
            .iter()
            .map(|machine_id| (machine_id.clone(), Some(ready_storage())))
            .collect(),
        eligible_machines,
        existing_replicas: Vec::new(),
        cleanup_candidates: Vec::new(),
        volume_pins: Vec::new(),
    }
}

fn route_binding_state(hostname: &str, service: &str) -> RouteBindingState {
    RouteBindingState {
        id: route_binding_id_for(&route_target(hostname)),
        namespace_id: namespace_id("default"),
        target: route_target(hostname),
        endpoint_port: route_port(8080),
        service_id: service_id(service),
        origin: RouteBindingOrigin::Declared,
    }
}

fn deploy_request(replicas: u16) -> DeployServiceSpec {
    DeployServiceSpec {
        keep: None,
        service_id: service_id("svc_api"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
        image_source: ployz_core::deploy::ImageSource::Registry,
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        runtime: ContainerRuntimeSpec::image_defaults(),
        pre_start: None,
        depends_on: Vec::new(),
        routes: Vec::new(),
    }
}

/// Plans one service through the namespace planner, the only production
/// entry point.
fn plan_single_service(input: &DeployPlanningInput) -> Result<DeployPlan, DeployPlanError> {
    plan_inputs(vec![input.clone()], Vec::new())
}

fn plan_inputs(
    inputs: Vec<DeployPlanningInput>,
    cleanup: Vec<DeployCleanupContainer>,
) -> Result<DeployPlan, DeployPlanError> {
    let mut volumes = BTreeMap::new();
    for input in &inputs {
        volumes.extend(input.volumes.clone());
    }
    let request = ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(
        ployz_core::deploy::DeployRequest {
            namespace_id: namespace_id("default"),
            origin: None,
            volumes,
            services: inputs.iter().map(|input| input.service.clone()).collect(),
        },
    )
    .expect("planner fixture normalizes");
    let storage_testimony = inputs
        .iter()
        .flat_map(|input| input.storage_testimony.clone())
        .collect::<BTreeMap<_, _>>();
    plan_namespace_deploy(
        &request,
        inputs
            .into_iter()
            .map(|input| CoreDeployPlanningInput {
                service_id: input.service.service_id,
                eligible_machines: input.eligible_machines,
                existing_replicas: input.existing_replicas,
                cleanup_candidates: input.cleanup_candidates,
                volume_pins: input.volume_pins,
            })
            .collect(),
        cleanup,
        ployz_core::deploy::DeployPlanningContext {
            storage_testimony: &storage_testimony,
        },
    )
}

fn normalized_services(
    services: Vec<DeployServiceSpec>,
    volumes: BTreeMap<VolumeName, VolumeSpec>,
) -> ployz_core::deploy::VolumeDeclaredDeployRequest {
    ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(ployz_core::deploy::DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes,
        services,
    })
    .expect("planner fixture normalizes")
}

fn deploy_plan(
    input: &DeployPlanningInput,
    steps: Vec<DeployPlanStep>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> DeployPlan {
    deploy_plan_with_volume_pins(input, steps, Vec::new(), cleanup_containers)
}

fn deploy_plan_with_volume_pins(
    input: &DeployPlanningInput,
    steps: Vec<DeployPlanStep>,
    volume_pin_commits: Vec<VolumePinState>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id_for(
            &namespace_id("default"),
            std::slice::from_ref(&input.service),
        ),
        phases: vec![DeployPhasePlan {
            services: vec![DeployServicePlan {
                service_id: service_id("svc_api"),
                steps,
                pre_start: None,
            }],
        }],
        volume_pin_commits,
        cleanup_actions: cleanup_containers
            .into_iter()
            .map(|target| ployz_core::deploy::DeployCleanupAction::RemoveContainer { target })
            .collect(),
    }
}

fn dependency(service: &str, condition: DependencyCondition) -> ServiceDependency {
    ServiceDependency {
        service_id: service_id(service),
        condition,
    }
}

fn volume_mount(volume_name: &str, target: &str) -> ServiceVolumeMount {
    ServiceVolumeMount {
        volume_name: VolumeName::try_new(volume_name).expect("valid volume name"),
        target: ContainerMountPath::try_new(target).expect("valid mount target"),
    }
}

fn declare_plain_volume_mounts(input: &mut DeployPlanningInput, mounts: Vec<ServiceVolumeMount>) {
    declare_volume_mounts(
        input,
        mounts
            .into_iter()
            .map(|mount| (mount, VolumeSpec::Plain))
            .collect(),
    );
}

fn declare_volume_mounts(
    input: &mut DeployPlanningInput,
    mounts: Vec<(ServiceVolumeMount, VolumeSpec)>,
) {
    input.service.runtime.volume_mounts = mounts.iter().map(|(mount, _)| mount.clone()).collect();
    input.volumes = mounts
        .into_iter()
        .map(|(mount, spec)| (mount.volume_name, spec))
        .collect();
}

fn update_request(request: &mut DeployServiceSpec, update: impl FnOnce(&mut DeployServiceSpec)) {
    update(request);
}

fn set_routes(request: &mut DeployServiceSpec, routes: Vec<DeployRoute>) {
    update_request(request, |request| request.routes = routes);
}

fn volume_pin(volume_name: &str, machine_id: &str) -> VolumePinState {
    VolumePinState::plain(
        namespace_id("default"),
        VolumeName::try_new(volume_name).expect("valid volume name"),
        self::machine_id(machine_id),
    )
}

fn provisioned_volume_pin(volume_name: &str, machine_id: &str) -> VolumePinState {
    let volume_name = VolumeName::try_new(volume_name).expect("valid volume name");
    VolumePinState::try_new(
        namespace_id("default"),
        volume_name.clone(),
        self::machine_id(machine_id),
        ployz_core::intent::VolumeKind::Provisioned {
            dataset: DatasetName::for_volume(
                &ZfsPoolName::try_new("tank").expect("pool name"),
                &namespace_id("default"),
                &volume_name,
            )
            .expect("dataset name"),
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
        },
    )
    .expect("dataset identity matches pin")
}

fn ready_storage() -> ployz_core::machine::StorageCapability {
    ready_storage_with_capacity(1024 * 1024, Vec::new())
}

fn ready_storage_with_capacity(
    free_bytes: u64,
    child_quotas: Vec<ployz_core::machine::DatasetQuotaFact>,
) -> ployz_core::machine::StorageCapability {
    ployz_core::machine::StorageCapability::Ready {
        pool: ZfsPoolName::try_new("tank").expect("pool name"),
        capacity: ployz_core::machine::PoolCapacityFacts {
            total_bytes: free_bytes,
            provisioned_used_bytes: 0,
            free_bytes,
            child_quotas,
        },
    }
}

fn ready_storage_with_quota(
    volume_name: &str,
    quota_bytes: u64,
) -> ployz_core::machine::StorageCapability {
    let pool = ZfsPoolName::try_new("tank").expect("pool name");
    let volume_name = VolumeName::try_new(volume_name).expect("valid volume name");
    ployz_core::machine::StorageCapability::Ready {
        pool: pool.clone(),
        capacity: ployz_core::machine::PoolCapacityFacts {
            total_bytes: 1024 * 1024,
            provisioned_used_bytes: 0,
            free_bytes: 1024 * 1024,
            child_quotas: vec![ployz_core::machine::DatasetQuotaFact {
                dataset: DatasetName::for_volume(&pool, &namespace_id("default"), &volume_name)
                    .expect("dataset name"),
                quota_bytes,
            }],
        },
    }
}

fn deploy_route(hostname: &str, endpoint_port: u16) -> DeployRoute {
    DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: route_hostname(hostname),
        },
        endpoint_port: route_port(endpoint_port),
    }
}

fn automatic_deploy_route(label: &str, endpoint_port: u16) -> DeployRoute {
    DeployRoute {
        target: DeployRouteTarget::AutoHostname {
            label: AutomaticHostnameLabel::try_new(label).expect("valid automatic label"),
        },
        endpoint_port: route_port(endpoint_port),
    }
}

fn use_existing_step(machine: &str, container: &str, slot: u16) -> DeployPlanStep {
    DeployPlanStep::UseExistingContainer {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        slot: ReplicaSlot::try_new(slot).expect("valid replica slot"),
    }
}

fn run_step(machine: &str, slot: u16) -> DeployPlanStep {
    DeployPlanStep::RunContainer {
        machine_id: machine_id(machine),
        slot: ReplicaSlot::try_new(slot).expect("valid replica slot"),
    }
}

fn route_target(hostname: &str) -> RouteTarget {
    RouteTarget::new(RouteHostname::try_new(hostname).expect("valid route hostname"))
}

fn route_binding_id(value: &str) -> ployz_core::ids::RouteBindingId {
    ployz_core::ids::RouteBindingId::try_new(value).expect("valid route binding id")
}

fn route_binding_id_for(target: &RouteTarget) -> ployz_core::ids::RouteBindingId {
    route_binding_id(&format!(
        "route_{}",
        target.hostname.as_str().replace(['.', '-'], "_")
    ))
}

fn route_port(port: u16) -> RoutePort {
    RoutePort::try_new(port).expect("valid route port")
}

fn endpoint_ip(ip: &str) -> std::net::IpAddr {
    ip.parse().expect("valid container endpoint ip")
}

fn existing_replica(machine: &str, container: &str) -> ExistingServiceReplica {
    ExistingServiceReplica {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        creation_gate: ExistingReplicaCreationGate::AlreadyPassed,
    }
}

fn cleanup_container(machine: &str, container: &str) -> DeployCleanupContainer {
    cleanup_container_with_entry(machine, container, "entry_1")
}

fn cleanup_container_with_entry(
    machine: &str,
    container: &str,
    namespace_revision_entry: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        identity: containers::identity("svc_api")
            .entry(namespace_revision_entry)
            .operation("op_existing")
            .step(container)
            .build(),
    }
}

fn cleanup_container_observed(
    machine: &str,
    container: &str,
    running: bool,
    created_at_unix_seconds: Option<i64>,
) -> ObservedCleanupCandidate {
    let target = cleanup_container(machine, container);
    ObservedCleanupCandidate {
        state: if running {
            ployz_core::machine::runtime::ContainerRuntimeState::running_unroutable()
        } else {
            ployz_core::machine::runtime::ContainerRuntimeState::Exited
        },
        created_at_unix_seconds,
        observed_image_identity: Some(OciDigest::sha256(container.as_bytes()).to_string()),
        target,
    }
}

fn observed_cleanup_candidate(
    machine: &str,
    container: &str,
    entry: &str,
    running: bool,
    created_at_unix_seconds: Option<i64>,
) -> ObservedCleanupCandidate {
    let mut candidate =
        cleanup_container_observed(machine, container, running, created_at_unix_seconds);
    candidate.target = cleanup_container_with_entry(machine, container, entry);
    candidate.observed_image_identity = None;
    candidate
}

fn observed_machine(
    machine: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(machine), containers)
        .expect("valid observation snapshot")
}

#[test]
fn only_probing_healthchecks_report_docker_health() {
    let healthcheck = |test: ContainerHealthcheckTest| ContainerHealthcheck {
        test,
        interval: None,
        timeout: None,
        retries: None,
        start_period: None,
    };

    assert!(
        healthcheck(ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("true").expect("valid healthcheck command")
        ))
        .reports_docker_health()
    );
    assert!(
        healthcheck(ContainerHealthcheckTest::Exec(
            ContainerCommand::try_new(vec!["true".to_owned()]).expect("valid healthcheck argv")
        ))
        .reports_docker_health()
    );
    assert!(!healthcheck(ContainerHealthcheckTest::Disable).reports_docker_health());
    assert!(!healthcheck(ContainerHealthcheckTest::Inherit).reports_docker_health());
}

fn observed_container(
    machine: &str,
    container: &str,
    service: &str,
    revision: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    containers::observation(machine, container)
        .with(
            containers::identity(service)
                .entry(revision)
                .operation("op_existing")
                .step(container)
                .kind(kind),
        )
        .state(state)
        .build()
}
