use super::*;
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployPlan, DeployPlanStep, DeployRequest, DeployRoute,
    DeployRouteTarget, DeployServicePlan, DeployServiceSpec, ImageReference, ReplicaCount,
    ReplicaSlot,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, ServiceId,
};
use ployz_core::ops::{
    ArtifactUnavailableReason, DeployCompletionOutcome, DeployOperationFailure, DeployRunningStage,
    EventSequence, FailureMessage, HealthCheckFailure, OperationEvent, OperatorHint,
    PreStartHookFailure, RetainedArtifact, RouteCutoverFailureReason, RouteHostname, RoutePort,
};

fn operation_id() -> OperationId {
    OperationId::try_new("op_317").expect("valid operation id")
}

fn namespace_id() -> NamespaceId {
    NamespaceId::try_new("prod").expect("valid namespace id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn machine_id(value: &str) -> MachineId {
    MachineId::try_new(value).expect("valid machine id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

fn service(name: &str, image: &str, replicas: u16, routes: Vec<DeployRoute>) -> DeployServiceSpec {
    DeployServiceSpec {
        service_id: service_id(name),
        image: ImageReference::try_new(image).expect("valid image reference"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        runtime: ContainerRuntimeSpec::image_defaults(),
        pre_start: None,
        depends_on: Vec::new(),
        routes,
    }
}

fn target() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id(),
        services: vec![
            service(
                "web",
                "ghcr.io/acme/web:1",
                2,
                vec![DeployRoute {
                    target: DeployRouteTarget::Hostname {
                        hostname: RouteHostname::try_new("app.example.com")
                            .expect("valid hostname"),
                        port: route_port(443),
                    },
                    endpoint_port: route_port(8080),
                }],
            ),
            service(
                "worker",
                "ghcr.io/acme/worker:1",
                1,
                vec![DeployRoute {
                    target: DeployRouteTarget::AutoHostname {
                        port: route_port(443),
                    },
                    endpoint_port: route_port(9000),
                }],
            ),
        ],
    }
}

fn single_service_target() -> DeployRequest {
    let mut target = target();
    target.services.truncate(1);
    target
}

fn plan() -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id(),
        namespace_revision_id: NamespaceRevisionId::try_new("revision_317")
            .expect("valid namespace revision id"),
        services: vec![
            DeployServicePlan {
                service_id: service_id("web"),
                pre_start: None,
                steps: vec![
                    DeployPlanStep::RunContainer {
                        machine_id: machine_id("hetzner-1"),
                        slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
                    },
                    DeployPlanStep::RunContainer {
                        machine_id: machine_id("hetzner-2"),
                        slot: ReplicaSlot::try_new(2).expect("valid replica slot"),
                    },
                ],
            },
            DeployServicePlan {
                service_id: service_id("worker"),
                pre_start: None,
                steps: vec![DeployPlanStep::UseExistingContainer {
                    machine_id: machine_id("hetzner-2"),
                    container_id: container_id("worker-existing"),
                    slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
                }],
            },
        ],
        volume_pin_commits: Vec::new(),
        cleanup_containers: Vec::new(),
    }
}

fn replay(sequence: u64, event: OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: EventSequence::try_new(sequence).expect("valid event sequence"),
        event,
    }
}

fn happy_events() -> Vec<ReplayedOperationEvent> {
    let operation_id = operation_id();
    vec![
        replay(
            1,
            OperationEvent::DeploySubmitted {
                operation_id: operation_id.clone(),
                target: target(),
            },
        ),
        replay(
            2,
            OperationEvent::DeployPlanningStarted {
                operation_id: operation_id.clone(),
            },
        ),
        replay(
            3,
            OperationEvent::DeployPlanCreated {
                operation_id: operation_id.clone(),
                plan: plan(),
            },
        ),
        replay(
            4,
            OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: DeployRunningStage::PreparingDataplane,
            },
        ),
        replay(
            5,
            OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: DeployRunningStage::StartingContainers,
            },
        ),
        replay(
            6,
            OperationEvent::DeployContainerStarted {
                operation_id: operation_id.clone(),
                machine_id: machine_id("hetzner-1"),
                container_id: container_id("web-1"),
            },
        ),
        replay(
            7,
            OperationEvent::DeployContainerStarted {
                operation_id: operation_id.clone(),
                machine_id: machine_id("hetzner-2"),
                container_id: container_id("web-2"),
            },
        ),
        replay(
            8,
            OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: DeployRunningStage::WaitingForHealth,
            },
        ),
        replay(
            9,
            OperationEvent::DeployHealthCheckStarted {
                operation_id: operation_id.clone(),
            },
        ),
        replay(
            10,
            OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: DeployRunningStage::RouteCutover,
            },
        ),
        replay(
            11,
            OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: DeployRunningStage::ServingTargetCommit,
            },
        ),
        replay(
            12,
            OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: DeployRunningStage::RemovingSupersededContainers,
            },
        ),
        replay(
            13,
            OperationEvent::DeployCompleted {
                operation_id,
                outcome: DeployCompletionOutcome::Completed,
            },
        ),
    ]
}

fn happy_tree() -> DeployTree {
    let mut tree = DeployTree::new();
    tree.ingest_page(&happy_events());
    tree
}

#[test]
fn happy_path_renders_plain_milestones_and_final_tree() {
    let tree = happy_tree();

    assert_eq!(
        render_plain_lines(&tree),
        concat!(
            "deploy op_317: planning — 2 services\n",
            "deploy op_317: images — ghcr.io/acme/web:1 planned\n",
            "deploy op_317: images — ghcr.io/acme/worker:1 planned\n",
            "deploy op_317: worker — worker.1 already at target on hetzner-2\n",
            "deploy op_317: web — web.1 running on hetzner-1\n",
            "deploy op_317: web — web.2 running on hetzner-2\n",
            "deploy op_317: web — web.1 healthy on hetzner-1\n",
            "deploy op_317: web — web.2 healthy on hetzner-2\n",
            "deploy op_317: routes — app.example.com → web:8080\n",
            "deploy op_317: routes — worker → public URL (auto)\n",
            "deploy op_317: succeeded\n",
        )
    );

    assert_eq!(
        render_frame(&tree),
        concat!(
            "Deploy op_317 started — namespace prod, 2 services\n",
            "\n",
            "  images\n",
            "    ✓ ghcr.io/acme/web:1\n",
            "    ✓ ghcr.io/acme/worker:1\n",
            "  web\n",
            "    ✓ web.1 on hetzner-1 — healthy\n",
            "    ✓ web.2 on hetzner-2 — healthy\n",
            "  worker\n",
            "    ✓ no changes — already at target (worker.1 on hetzner-2)\n",
            "  routes\n",
            "    ✓ app.example.com → web:8080\n",
            "    ✓ worker → public URL (auto)\n",
        )
    );
    assert_eq!(render_success(&tree), "Deploy succeeded.\n");
}

#[test]
fn final_render_has_the_grounded_three_level_tree_and_success_line() {
    let tree = happy_tree();
    let output = render_frame(&tree) + &render_success(&tree);

    assert!(output.contains("Deploy op_317 started — namespace prod, 2 services\n\n"));
    assert!(output.contains("  images\n"));
    assert!(output.contains("  web\n    ✓ web.1 on hetzner-1 — healthy\n"));
    assert!(output.contains("  routes\n"));
    assert!(output.ends_with("Deploy succeeded.\n"));
    assert!(!output.contains("http"));
}

#[test]
fn starting_container_count_advances_plan_lines_without_attribution() {
    let events = happy_events();
    let first_started = events.get(..6).expect("happy path has a first start");
    let second_started = events.get(6..7).expect("happy path has a second start");
    let mut tree = DeployTree::new();
    tree.ingest_page(first_started);

    let first_frame = render_frame(&tree);
    assert!(first_frame.contains("✓ web.1 on hetzner-1 — created"));
    assert!(first_frame.contains("web.2 on hetzner-2 — creating"));
    assert!(
        first_frame.contains("  routes    queued\n"),
        "a group whose work has not begun collapses to its named title line"
    );
    assert_eq!(
        first_frame
            .chars()
            .filter(|character| SPINNER_FRAMES.contains(character))
            .count(),
        1
    );

    tree.ingest_page(second_started);
    let second_frame = render_frame(&tree);
    assert!(second_frame.contains("✓ web.1 on hetzner-1 — created"));
    assert!(second_frame.contains("✓ web.2 on hetzner-2 — created"));
    assert!(!second_frame.contains("web.2 on hetzner-2 — running"));
}

#[test]
fn partial_completion_stays_distinct_from_success() {
    let operation_id = operation_id();
    let mut tree = DeployTree::new();
    tree.ingest_page(&[
        replay(
            1,
            OperationEvent::DeploySubmitted {
                operation_id: operation_id.clone(),
                target: single_service_target(),
            },
        ),
        replay(
            2,
            OperationEvent::DeployCompleted {
                operation_id,
                outcome: DeployCompletionOutcome::PartiallyCompletedWithWarnings,
            },
        ),
    ]);

    assert!(
        render_plain_lines(&tree).ends_with("deploy op_317: partially completed with warnings\n")
    );
    assert_eq!(
        render_success(&tree),
        "Deploy partially completed with warnings.\n"
    );
}

#[test]
fn early_artifact_failure_is_minimal() {
    let operation_id = operation_id();
    let mut tree = DeployTree::new();
    tree.ingest_page(&[
        replay(
            1,
            OperationEvent::DeploySubmitted {
                operation_id: operation_id.clone(),
                target: target(),
            },
        ),
        replay(
            2,
            OperationEvent::DeployFailed {
                operation_id,
                failure: DeployOperationFailure::ArtifactUnavailable {
                    service_id: service_id("worker"),
                    namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_worker")
                        .expect("valid namespace revision entry id"),
                    reason: ArtifactUnavailableReason::BundleUnreadable {
                        message: FailureMessage::try_new("registry denied the pull")
                            .expect("valid failure message"),
                    },
                },
            },
        ),
    ]);

    let output = render_failure_block(&tree);
    assert!(output.starts_with("Deploy failed — image-resolve-pull-failed, service worker.\n"));
    assert!(output.contains(
        "  ✗ image ghcr.io/acme/worker:1 could not be resolved: registry denied the pull\n"
    ));
    assert!(output.contains("Nothing changed: the failure happened before any container work."));
    assert!(output.contains("timeline:  ployz ops status op_317"));
    assert!(!output.contains("logs:"));
    assert!(!output.contains("rollback:"));
    assert!(!output.contains("Serving is unchanged."));
}

/// A cutover-or-later failure with nothing retained must not claim "nothing
/// changed" (containers ran) nor "serving is unchanged" (the route may have
/// moved): it makes no safety claim and keeps the recovery hints.
#[test]
fn route_cutover_failure_makes_no_safety_claim() {
    let operation_id = operation_id();
    let mut tree = DeployTree::new();
    tree.ingest_page(&[
        replay(
            1,
            OperationEvent::DeploySubmitted {
                operation_id: operation_id.clone(),
                target: single_service_target(),
            },
        ),
        replay(
            2,
            OperationEvent::DeployFailed {
                operation_id,
                failure: DeployOperationFailure::RouteCutoverFailed {
                    route: ployz_core::ops::RouteTarget {
                        hostname: RouteHostname::try_new("app.example.com")
                            .expect("valid hostname"),
                        port: route_port(443),
                    },
                    reason: RouteCutoverFailureReason::RouteRejected {
                        message: FailureMessage::try_new("hostname already bound")
                            .expect("valid failure message"),
                    },
                    retained_artifacts: Vec::new(),
                },
            },
        ),
    ]);

    let output = render_failure_block(&tree);
    assert!(output.starts_with("Deploy failed — route-cutover-failed, service web.\n"));
    assert!(output.contains("  ✗ route app.example.com:443 rejected: hostname already bound\n"));
    assert!(!output.contains("Nothing changed"));
    assert!(!output.contains("Serving is unchanged."));
    assert!(!output.contains("logs:"));
    assert!(output.contains("timeline:  ployz ops status op_317"));
    assert!(output.contains("rollback:  ployz deploy rollback -n prod"));
}

#[test]
fn deep_health_failure_keeps_container_evidence_and_hints() {
    let operation_id = operation_id();
    let mut tree = DeployTree::new();
    tree.ingest_page(&[
        replay(
            1,
            OperationEvent::DeploySubmitted {
                operation_id: operation_id.clone(),
                target: single_service_target(),
            },
        ),
        replay(
            2,
            OperationEvent::DeployFailed {
                operation_id,
                failure: DeployOperationFailure::HealthCheckFailed {
                    health_check: HealthCheckFailure::ProbeFailed {
                        machine_id: machine_id("hetzner-1"),
                        container_id: container_id("web-1"),
                        message: FailureMessage::try_new("HTTP probe returned status 503")
                            .expect("valid failure message"),
                        log_hint: OperatorHint::try_new("inspect the service logs")
                            .expect("valid operator hint"),
                    },
                    retained_artifacts: vec![RetainedArtifact::StartedContainer {
                        machine_id: machine_id("hetzner-1"),
                        container_id: container_id("web-1"),
                        log_hint: OperatorHint::try_new("ployz logs web --failed")
                            .expect("valid operator hint"),
                    }],
                },
            },
        ),
    ]);

    let output = render_failure_block(&tree);
    assert!(output.starts_with("Deploy failed — health-gate-failed, service web on hetzner-1.\n"));
    assert!(output.contains("  ✗ health check failed: HTTP probe returned status 503\n"));
    assert!(output.contains("    failed container web-1 retained on hetzner-1\n"));
    assert!(output.contains("Serving is unchanged."));
    assert!(output.contains("logs:      ployz logs web -n prod --failed"));
    assert!(output.contains("timeline:  ployz ops status op_317"));
    assert!(output.contains("rollback:  ployz deploy rollback -n prod"));
}

#[test]
fn pre_start_failure_keeps_hook_evidence_and_serving_safety() {
    let operation_id = operation_id();
    let mut tree = DeployTree::new();
    tree.ingest_page(&[
        replay(
            1,
            OperationEvent::DeploySubmitted {
                operation_id: operation_id.clone(),
                target: single_service_target(),
            },
        ),
        replay(
            2,
            OperationEvent::DeployFailed {
                operation_id,
                failure: DeployOperationFailure::PreStartHookFailed {
                    machine_id: machine_id("hetzner-1"),
                    failure: PreStartHookFailure::Exited {
                        container_id: container_id("web-hook-1"),
                        exit_code: 17,
                        message: FailureMessage::try_new("database migration rejected")
                            .expect("valid failure message"),
                        log_hint: OperatorHint::try_new("ployzctl logs web-hook-1")
                            .expect("valid operator hint"),
                    },
                    retained_artifacts: vec![RetainedArtifact::StartedContainer {
                        machine_id: machine_id("hetzner-1"),
                        container_id: container_id("web-hook-1"),
                        log_hint: OperatorHint::try_new("ployzctl logs web-hook-1")
                            .expect("valid operator hint"),
                    }],
                },
            },
        ),
    ]);

    let output = render_failure_block(&tree);
    assert!(
        output.starts_with("Deploy failed — pre-start-hook-failed, service web on hetzner-1.\n")
    );
    assert!(
        output.contains("  ✗ pre-start hook exited with code 17: database migration rejected\n")
    );
    assert!(output.contains("    failed container web-hook-1 retained on hetzner-1\n"));
    assert!(output.contains("Serving is unchanged."));
    assert!(output.contains("logs:      ployz logs web -n prod --failed"));
}
