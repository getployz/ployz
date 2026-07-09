use ployz_core::deploy::{
    ContainerCommand, ContainerEntrypoint, ContainerRuntimeSpec, DeployCleanupContainer,
    DeployPlan, DeployPlanError, DeployPlanStep, DeployPlanningInput, DeployPreparationInput,
    DeployRoute, DeployServicePlan, DeployServiceRequest, EnvName, EnvValue,
    ExistingServiceReplica, ImageReference, ReplicaCount, ReplicaSlot, ServiceEnvironment,
    StopGracePeriod, namespace_route_binding_removals, namespace_serving_target_removals,
    plan_namespace_deploy, prepare_deploy,
};
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::RouteBindingState;
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, namespace_revision_id,
    service_id,
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
            public_port: 443,
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
        "4cb6de52f7609df479fe07a411d7312d82ff294b2ac1ee071de284a383be6d5e"
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
        "7798ddc1990c9db01fdc4258b5685b525c07fd0353d57aa735bf96cf7c4ef623"
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
        "27aec2fa8baea1fdb41a597a8ad0186198217e4a9fb449528b5afec365a13e91"
    );
}

#[test]
fn mutable_tag_repeats_as_same_namespace_revision_entry_identity() {
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None)
            .namespace_revision_entry_id(&namespace_id("default"))
            .as_str(),
        "f09905e06be8fda59795a12da5c2058ad0e72425b8cb143f138958787c166d96"
    );
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None)
            .namespace_revision_entry_id(&namespace_id("default")),
        service_spec(
            "svc_api",
            "nginx:latest",
            3,
            Some(SpecRoute {
                public_port: 443,
                endpoint_port: 8080
            })
        )
        .namespace_revision_entry_id(&namespace_id("default"))
    );
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
fn deploy_preparation_uses_active_revision_and_running_target_replicas() {
    let prepared = prepare_deploy(DeployPreparationInput {
        request: deploy_request(2),
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
    });

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
        ]
    );
}

#[test]
fn deploy_preparation_evacuates_draining_machine_replicas() {
    let prepared = prepare_deploy(DeployPreparationInput {
        request: deploy_request(1),
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
    });

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
    request.routes = vec![deploy_route("api.example.com", 443, 8080)];

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
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
    });

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
        deploy_route("api.example.com", 443, 8080),
        deploy_route("www.example.com", 443, 8080),
    ];

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        eligible_machines: vec![machine_id("machine_a")],
        draining_machines: Vec::new(),
        observed_machines: Vec::new(),
    });

    assert_eq!(
        prepared.route_commits,
        vec![
            RouteBindingState {
                namespace_id: namespace_id("default"),
                target: route_target("api.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            },
            RouteBindingState {
                namespace_id: namespace_id("default"),
                target: route_target("www.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            },
        ]
    );
}

#[test]
fn namespace_route_removals_detach_undeclared_targets_including_omitted_services() {
    // `admin` is owned by a declared service but no longer declared;
    // `orphan` is owned by a service the manifest omits entirely. Both are
    // detached: the manifest is the full desired route state.
    let removals = namespace_route_binding_removals(
        &namespace_id("default"),
        &[
            route_target("api.example.com", 443),
            route_target("www.example.com", 443),
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
            route_target("admin.example.com", 443),
            route_target("orphan.example.com", 443),
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
    request.routes = vec![deploy_route("api.example.com", 443, 8080)];

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        eligible_machines: Vec::new(),
        draining_machines: Vec::new(),
        observed_machines: Vec::new(),
    });

    assert_eq!(
        prepared.route_commits,
        vec![RouteBindingState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
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
        &[route_target("moved.example.com", 443)],
        &[route_binding_state("moved.example.com", "svc_api")],
    );

    assert!(removals.is_empty());
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
        "4cb6de52f7609df479fe07a411d7312d82ff294b2ac1ee071de284a383be6d5e"
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
    let prepared = prepare_deploy(DeployPreparationInput {
        request: deploy_request(1),
        eligible_machines: vec![machine_id("machine_a")],
        draining_machines: Vec::new(),
        observed_machines: vec![observed_machine("machine_a", [foreign])],
    });

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
    public_port: u16,
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
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        runtime: ContainerRuntimeSpec::image_defaults(),
        routes: route
            .map(|route| {
                vec![deploy_route(
                    "api.example.com",
                    route.public_port,
                    route.endpoint_port,
                )]
            })
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
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        runtime,
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

fn args_to_vec(args: impl IntoIterator<Item = &'static str>) -> Vec<String> {
    args.into_iter().map(str::to_owned).collect()
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
    }
}

fn route_binding_state(hostname: &str, service: &str) -> RouteBindingState {
    RouteBindingState {
        namespace_id: namespace_id("default"),
        target: route_target(hostname, 443),
        endpoint_port: route_port(8080),
        service_id: service_id(service),
    }
}

fn deploy_request(replicas: u16) -> DeployServiceRequest {
    DeployServiceRequest {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        namespace_revision_id: namespace_revision_id("rev_1"),
        namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        runtime: ContainerRuntimeSpec::image_defaults(),
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
    DeployPlan {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_1"),
        services: vec![DeployServicePlan {
            service_id: service_id("svc_api"),
            steps,
        }],
        cleanup_containers,
    }
}

fn deploy_route(hostname: &str, public_port: u16, endpoint_port: u16) -> DeployRoute {
    DeployRoute {
        target: route_target(hostname, public_port),
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

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget {
        hostname: RouteHostname::try_new(hostname).expect("valid route hostname"),
        port: route_port(port),
    }
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
