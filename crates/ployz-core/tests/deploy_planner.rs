use ployz_core::deploy::{
    ContainerCommand, ContainerEntrypoint, ContainerHealthcheck, ContainerHealthcheckTest,
    ContainerMountPath, ContainerRuntimeSpec, DependencyCondition, DeployCleanupContainer,
    DeployPhasePlan, DeployPlan, DeployPlanError, DeployPlanStep, DeployPlanningInput,
    DeployPreparationInput, DeployRoute, DeployRouteTarget, DeployServicePlan,
    DeployServiceRequest, EnvName, EnvValue, ExistingServiceReplica, HealthcheckShellCommand,
    ImageReference, ImageSource, PreStartHook, PreStartHookStep, ReplicaCount, ReplicaSlot,
    ServiceDependency, ServiceEnvironment, ServiceVolumeMount, StopGracePeriod, VolumeName,
    auto_hostname_route_binding_commits, namespace_revision_id_for,
    namespace_route_binding_removals, namespace_serving_target_removals, plan_namespace_deploy,
    prepare_deploy, validate_deploy_route_bindings,
};
use ployz_core::ids::MachineId;
use ployz_core::image::OciDigest;
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
    container_id, machine_id, namespace_id, namespace_revision_entry_id, namespace_revision_id,
    route_hostname, service_id,
};
use std::collections::BTreeMap;

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
        "02e1f0da238ce3a680254e313d765001d47183b1f47b3fea6a9a72fccb6bcb31"
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
        "a3d07ffe3f681440764b1a81f42fd1d4258e3c5bcb3522fa44271e841510c87c"
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
        "292f3617119b4a663e9b4d68858def90de08bdf890b9130c6b52f832a94e42b5"
    );
}

#[test]
fn mutable_tag_repeats_as_same_namespace_revision_entry_identity() {
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
            .as_str(),
        "f7aa39e7122aee0273d2fb07c7b8e64568d052a7e05f02d9172a1fbeb65a9920"
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
fn pushed_image_identity_uses_config_digest_not_transfer_digest_or_seed() {
    let mut left = service_spec("svc_api", "api:latest", 1, None);
    left.image_source = ImageSource::PushedToSeed {
        seed: machine_id("machine_a"),
        manifest_digest: oci_digest('a'),
        image_id: oci_digest('c'),
    };
    let mut same_content = left.clone();
    same_content.image_source = ImageSource::PushedToSeed {
        seed: machine_id("machine_b"),
        manifest_digest: oci_digest('b'),
        image_id: oci_digest('c'),
    };
    let mut changed_content = same_content.clone();
    changed_content.image_source = ImageSource::PushedToSeed {
        seed: machine_id("machine_b"),
        manifest_digest: oci_digest('d'),
        image_id: oci_digest('e'),
    };

    assert_eq!(
        left.namespace_revision_entry_id(&namespace_id("default")),
        same_content.namespace_revision_entry_id(&namespace_id("default"))
    );
    assert_ne!(
        left.namespace_revision_entry_id(&namespace_id("default")),
        changed_content.namespace_revision_entry_id(&namespace_id("default"))
    );
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
    assert_eq!(
        plan_single_service(planning_input(
            3,
            [machine_id("machine_a"), machine_id("machine_b")]
        ))
        .expect("plan succeeds"),
        deploy_plan(
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
        plan_single_service(input).expect("plan succeeds"),
        deploy_plan(
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
        plan_single_service(input).expect("plan succeeds"),
        deploy_plan(
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
        plan_single_service(input).expect("existing reality satisfies target"),
        deploy_plan(
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
        cleanup_container("machine_b", "ctr_target_keep"),
        cleanup_container("machine_b", "ctr_target_extra"),
        cleanup_container("machine_b", "ctr_old"),
    ];

    assert_eq!(
        plan_single_service(input).expect("plan succeeds"),
        deploy_plan(
            vec![use_existing_step("machine_b", "ctr_target_extra", 1)],
            vec![
                cleanup_container("machine_b", "ctr_old"),
                cleanup_container("machine_b", "ctr_target_keep"),
            ],
        )
    );
}

#[test]
fn deploy_plan_requires_eligible_machine() {
    assert_eq!(
        plan_single_service(planning_input(1, [])),
        Err(DeployPlanError::NoEligibleMachines)
    );
}

#[test]
fn service_dependencies_reorder_namespace_plan_stably() {
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    worker.request.service_id = service_id("svc_worker");
    worker.request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    let api = planning_input(1, [machine_id("machine_a")]);

    let plan = plan_namespace_deploy(
        namespace_id("default"),
        namespace_revision_id("rev_1"),
        vec![worker, api],
        Vec::new(),
    )
    .expect("dependency plan succeeds");

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
    worker.request.service_id = service_id("svc_worker");
    worker.request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    let api = planning_input(1, [machine_id("machine_a")]);
    let mut database = planning_input(1, [machine_id("machine_a")]);
    database.request.service_id = service_id("svc_database");

    let plan = plan_namespace_deploy(
        namespace_id("default"),
        namespace_revision_id("rev_1"),
        vec![worker, database, api],
        Vec::new(),
    )
    .expect("dependency plan succeeds");

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
    api.request.service_id = service_id("svc_web");
    api.request.depends_on = vec![dependency("svc_api", DependencyCondition::Healthy)];

    assert_eq!(
        plan_namespace_deploy(
            namespace_id("default"),
            namespace_revision_id("rev_1"),
            vec![api, database],
            Vec::new(),
        ),
        Err(DeployPlanError::HealthyDependencyWithoutHealthcheck {
            service_id: service_id("svc_web"),
            dependency: service_id("svc_api"),
        })
    );
}

#[test]
fn unknown_service_dependency_fails_planning() {
    let mut api = planning_input(1, [machine_id("machine_a")]);
    api.request.depends_on = vec![dependency("svc_missing", DependencyCondition::Started)];

    assert_eq!(
        plan_namespace_deploy(
            namespace_id("default"),
            namespace_revision_id("rev_1"),
            vec![api],
            Vec::new(),
        ),
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
    api.request.depends_on = vec![dependency("svc_worker", DependencyCondition::Started)];
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    worker.request.service_id = service_id("svc_worker");
    worker.request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];

    assert_eq!(
        plan_namespace_deploy(
            namespace_id("default"),
            namespace_revision_id("rev_1"),
            vec![worker, api],
            Vec::new(),
        ),
        Err(DeployPlanError::ServiceDependencyCycle {
            service_ids: vec![service_id("svc_api"), service_id("svc_worker")],
        })
    );
}

#[test]
fn pre_start_hook_step_uses_first_run_container_machine() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    input.request.pre_start = Some(pre_start_hook());

    let plan = plan_single_service(input).expect("plan succeeds");
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
    input.request.pre_start = Some(pre_start_hook());
    input.existing_replicas = vec![existing_replica("machine_b", "ctr_existing")];

    let plan = plan_single_service(input).expect("existing reality satisfies target");
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
    input.request.runtime.volume_mounts = vec![volume_mount("postgres_data", "/var/lib/postgres")];

    assert_eq!(
        plan_single_service(input).expect("plan succeeds"),
        deploy_plan_with_volume_pins(
            vec![run_step("machine_a", 1), run_step("machine_a", 2)],
            vec![volume_pin("postgres_data", "machine_a")],
            Vec::new(),
        )
    );
}

#[test]
fn volume_backed_service_uses_existing_pin() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    input.request.runtime.volume_mounts = vec![volume_mount("postgres_data", "/var/lib/postgres")];
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    assert_eq!(
        plan_single_service(input).expect("plan succeeds"),
        deploy_plan(
            vec![run_step("machine_b", 1), run_step("machine_b", 2)],
            Vec::new()
        )
    );
}

#[test]
fn volume_backed_service_fails_when_existing_pin_is_not_eligible() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.request.runtime.volume_mounts = vec![volume_mount("postgres_data", "/var/lib/postgres")];
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    assert_eq!(
        plan_single_service(input),
        Err(DeployPlanError::NoEligibleMachines)
    );
}

#[test]
fn volume_backed_service_reuses_only_replicas_on_pinned_machine() {
    let mut input = planning_input(2, [machine_id("machine_a"), machine_id("machine_b")]);
    input.request.runtime.volume_mounts = vec![volume_mount("postgres_data", "/var/lib/postgres")];
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];
    input.existing_replicas = vec![
        existing_replica("machine_a", "ctr_off_pin"),
        existing_replica("machine_b", "ctr_pinned"),
    ];
    input.cleanup_candidates = vec![
        cleanup_container("machine_a", "ctr_off_pin"),
        cleanup_container("machine_b", "ctr_pinned"),
    ];

    assert_eq!(
        plan_single_service(input).expect("plan succeeds"),
        deploy_plan(
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
    first.request.runtime.volume_mounts = vec![volume_mount("data", "/data")];
    let mut second = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    second.request.service_id = service_id("svc_worker");
    second.request.runtime.volume_mounts = vec![
        volume_mount("data", "/data"),
        volume_mount("uploads", "/uploads"),
    ];
    second.volume_pins = vec![volume_pin("uploads", "machine_b")];

    assert_eq!(
        plan_namespace_deploy(
            namespace_id("default"),
            namespace_revision_id("rev_1"),
            vec![first, second],
            Vec::new(),
        ),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_worker"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}

#[test]
fn service_with_volumes_on_different_pinned_machines_fails_planning() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    input.request.runtime.volume_mounts = vec![
        volume_mount("postgres_data", "/var/lib/postgres"),
        volume_mount("uploads", "/srv/uploads"),
    ];
    input.volume_pins = vec![
        volume_pin("postgres_data", "machine_a"),
        volume_pin("uploads", "machine_b"),
    ];

    assert_eq!(
        plan_single_service(input),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_api"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}

#[test]
fn deploy_preparation_uses_active_revision_and_running_target_replicas() {
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: deploy_request(2),
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
                        "entry_1",
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
                        "entry_1",
                        ManagedContainerKind::Job,
                        ContainerRuntimeState::running_unroutable(),
                    ),
                    observed_container(
                        "machine_b",
                        "ctr_exited",
                        "svc_api",
                        "entry_1",
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::Exited,
                    ),
                ],
            )],
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(prepared.request, deploy_request(2));
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
            cleanup_container_with_entry("machine_b", "ctr_target", "entry_1"),
            cleanup_container_with_entry("machine_b", "ctr_old", "entry_old"),
            cleanup_container_with_entry("machine_b", "ctr_exited", "entry_1"),
        ]
    );
}

#[test]
fn deploy_preparation_evacuates_draining_machine_replicas() {
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: deploy_request(1),
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
        },
        route_binding_id_for,
    )
    .expect("deploy preparation");

    assert_eq!(prepared.existing_replicas, Vec::new());
    assert_eq!(
        prepared.cleanup_candidates,
        vec![cleanup_container_with_entry(
            "machine_b",
            "ctr_target",
            "entry_1"
        )]
    );

    let plan = plan_namespace_deploy(
        namespace_id("default"),
        namespace_revision_id("rev_1"),
        vec![DeployPlanningInput {
            request: prepared.request,
            eligible_machines: prepared.eligible_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
            volume_pins: Vec::new(),
        }],
        Vec::new(),
    )
    .expect("plan succeeds");
    assert_eq!(
        plan,
        deploy_plan(
            vec![run_step("machine_a", 1)],
            vec![cleanup_container_with_entry(
                "machine_b",
                "ctr_target",
                "entry_1"
            )],
        )
    );
}

#[test]
fn routed_deploy_preparation_reuses_matching_identity_regardless_of_endpoint_port() {
    let mut request = deploy_request(2);
    request.routes = vec![deploy_route("api.example.com", 8080)];

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request,
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
                        "entry_1",
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
                    ),
                    observed_container(
                        "machine_b",
                        "ctr_target",
                        "svc_api",
                        "entry_1",
                        ManagedContainerKind::Service,
                        ContainerRuntimeState::running_at(endpoint_ip("10.0.0.3")),
                    ),
                ],
            )],
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
    request.routes = vec![
        deploy_route("api.example.com", 8080),
        deploy_route("www.example.com", 8080),
    ];

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request,
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            draining_machines: Vec::new(),
            observed_machines: Vec::new(),
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
    request.routes = vec![deploy_route("api.example.com", 9090)];
    let mut existing = route_binding_state("api.example.com", "svc_api");
    existing.id = route_binding_id("route_existing");

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request,
            occupied_route_bindings: vec![existing],
            eligible_machines: Vec::new(),
            draining_machines: Vec::new(),
            observed_machines: Vec::new(),
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
        request.routes = vec![
            deploy_route("api.example.com", 8080),
            deploy_route("api.example.com", duplicate_port),
        ];

        let error = prepare_deploy(
            DeployPreparationInput {
                request,
                occupied_route_bindings: Vec::new(),
                eligible_machines: Vec::new(),
                draining_machines: Vec::new(),
                observed_machines: Vec::new(),
            },
            route_binding_id_for,
        )
        .expect_err("duplicate declared target must collide");

        assert!(matches!(
            error,
            ployz_core::deploy::RouteBindingCommitError::HostnameCollision { .. }
        ));
    }
}

#[test]
fn declared_route_reroute_rejects_other_owners_and_automatic_bindings() {
    let mut request = deploy_request(1);
    request.routes = vec![deploy_route("api.example.com", 9090)];

    let mut other_service = route_binding_state("api.example.com", "svc_worker");
    other_service.id = route_binding_id("route_other_service");
    let mut other_namespace = route_binding_state("api.example.com", "svc_api");
    other_namespace.id = route_binding_id("route_other_namespace");
    other_namespace.namespace_id = namespace_id("other");
    let mut automatic = route_binding_state("api.example.com", "svc_api");
    automatic.id = route_binding_id("route_automatic");
    automatic.origin = RouteBindingOrigin::Automatic;

    for occupied in [other_service, other_namespace, automatic] {
        let error = prepare_deploy(
            DeployPreparationInput {
                request: request.clone(),
                occupied_route_bindings: vec![occupied],
                eligible_machines: Vec::new(),
                draining_machines: Vec::new(),
                observed_machines: Vec::new(),
            },
            route_binding_id_for,
        )
        .expect_err("hostname owner must collide");

        assert!(matches!(
            error,
            ployz_core::deploy::RouteBindingCommitError::HostnameCollision { .. }
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
    request.routes = vec![deploy_route("api.example.com", 8080)];

    let prepared = prepare_deploy(
        DeployPreparationInput {
            request,
            occupied_route_bindings: Vec::new(),
            eligible_machines: Vec::new(),
            draining_machines: Vec::new(),
            observed_machines: Vec::new(),
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
    request.routes = vec![automatic_deploy_route("api", 8080)];

    let commits = auto_hostname_route_binding_commits(
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
    request.routes = vec![automatic_deploy_route("api", 9090)];
    let mut existing = route_binding_state("api.apps.example.com", "svc_api");
    existing.id = route_binding_id("route_existing");
    existing.origin = RouteBindingOrigin::Automatic;

    let commits = auto_hostname_route_binding_commits(
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
        request.routes = vec![
            automatic_deploy_route("api", 8080),
            automatic_deploy_route("api", duplicate_port),
        ];

        let error = auto_hostname_route_binding_commits(
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
    request.routes = vec![automatic_deploy_route("api", 8080)];
    let existing = route_binding_state("api.apps.example.com", "svc_api");

    let error = auto_hostname_route_binding_commits(
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
        "02e1f0da238ce3a680254e313d765001d47183b1f47b3fea6a9a72fccb6bcb31"
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
    let prepared = prepare_deploy(
        DeployPreparationInput {
            request: deploy_request(1),
            occupied_route_bindings: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            draining_machines: Vec::new(),
            observed_machines: vec![observed_machine("machine_a", [foreign])],
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
    DeployPlanningInput {
        request: deploy_request(replicas),
        eligible_machines: eligible_machines.into_iter().collect(),
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

fn deploy_request(replicas: u16) -> DeployServiceRequest {
    DeployServiceRequest {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        namespace_revision_id: namespace_revision_id("rev_1"),
        namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
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
fn plan_single_service(input: DeployPlanningInput) -> Result<DeployPlan, DeployPlanError> {
    plan_namespace_deploy(
        namespace_id("default"),
        namespace_revision_id("rev_1"),
        vec![input],
        Vec::new(),
    )
}

fn deploy_plan(
    steps: Vec<DeployPlanStep>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> DeployPlan {
    deploy_plan_with_volume_pins(steps, Vec::new(), cleanup_containers)
}

fn deploy_plan_with_volume_pins(
    steps: Vec<DeployPlanStep>,
    volume_pin_commits: Vec<VolumePinState>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_1"),
        phases: vec![DeployPhasePlan {
            services: vec![DeployServicePlan {
                service_id: service_id("svc_api"),
                steps,
                pre_start: None,
            }],
        }],
        volume_pin_commits,
        cleanup_containers,
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

fn volume_pin(volume_name: &str, machine_id: &str) -> VolumePinState {
    VolumePinState {
        namespace_id: namespace_id("default"),
        volume_name: VolumeName::try_new(volume_name).expect("valid volume name"),
        machine_id: self::machine_id(machine_id),
        kind: ployz_core::intent::VolumeKind::Plain,
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
