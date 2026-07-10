use std::time::Duration;

use ployz_core::dataplane::{DataplaneProviderFailure, PloyzNativeMeshComponent};
use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanStep, DeployRequest, DeployRoute,
    DeployRouteTarget,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    ArtifactUnavailableReason, CancellationReason, ControlPlaneCommitScope, DeployCleanupFailure,
    DeployCompletionOutcome, DeployOperationFailure, DeployRunningStage, HealthCheckFailure,
    OperationEvent, OperationKind, ReplayedOperationEvent, RouteCutoverFailureReason,
};
use ployz_core::state::MachineUsabilityReason;

use crate::commands::ops::{
    deploy_failure_containers, deploy_failure_machines, deploy_failure_service,
};

use self::failure::{artifact_unavailable_reason, failure_cause};

mod failure;

const SPINNER_FRAMES: [char; 8] = ['⣷', '⣯', '⣟', '⡿', '⢿', '⣻', '⣽', '⣾'];

pub(crate) struct DeployTree {
    deploy: Option<ObservedDeploy>,
    plain_lines: Vec<String>,
    spinner_frame: usize,
}

struct ObservedDeploy {
    operation_id: OperationId,
    target: DeployRequest,
    work: DeployWork,
    result: DeployResult,
}

enum DeployWork {
    Planning,
    Planned {
        plan: DeployPlan,
        stage: PlannedStage,
        started_containers: usize,
        cleanup: Option<(Vec<DeployCleanupContainer>, Vec<DeployCleanupFailure>)>,
    },
}

enum PlannedStage {
    Queued,
    Running(DeployRunningStage),
}

enum DeployResult {
    Active,
    Completed {
        outcome: DeployCompletionOutcome,
        elapsed: Duration,
    },
    Failed {
        failure: DeployOperationFailure,
        elapsed: Duration,
    },
    Cancelled {
        reason: CancellationReason,
        elapsed: Duration,
    },
}

impl DeployTree {
    pub(crate) fn new() -> Self {
        Self {
            deploy: None,
            plain_lines: Vec::new(),
            spinner_frame: 0,
        }
    }

    pub(crate) fn ingest_page(&mut self, events: &[ReplayedOperationEvent], elapsed: Duration) {
        for replayed in events {
            self.ingest(&replayed.event, elapsed);
        }
    }

    pub(crate) fn tick_spinner(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
    }

    fn ingest(&mut self, event: &OperationEvent, elapsed: Duration) {
        match event {
            OperationEvent::DeploySubmitted {
                operation_id,
                target,
            } => {
                self.deploy = Some(ObservedDeploy {
                    operation_id: operation_id.clone(),
                    target: target.clone(),
                    work: DeployWork::Planning,
                    result: DeployResult::Active,
                });
                self.plain_lines.push(format!(
                    "deploy {}: planning — {} services",
                    operation_id.as_str(),
                    target.services.len()
                ));
            }
            OperationEvent::DeployPlanningStarted { operation_id: _ } => {}
            OperationEvent::DeployPlanCreated { operation_id, plan } => {
                if let Some(deploy) = &self.deploy {
                    let target = &deploy.target;
                    for image in distinct_images(target) {
                        self.plain_lines.push(format!(
                            "deploy {}: images — {} resolved",
                            operation_id.as_str(),
                            image
                        ));
                    }
                }
                for service in &plan.services {
                    for step in &service.steps {
                        if let DeployPlanStep::UseExistingContainer {
                            machine_id, slot, ..
                        } = step
                        {
                            self.plain_lines.push(format!(
                                "deploy {}: {} — {}.{} already at target on {}",
                                operation_id.as_str(),
                                service.service_id.as_str(),
                                service.service_id.as_str(),
                                slot.get(),
                                machine_id.as_str()
                            ));
                        }
                    }
                }
                if let Some(deploy) = &mut self.deploy {
                    deploy.work = DeployWork::Planned {
                        plan: plan.clone(),
                        stage: PlannedStage::Queued,
                        started_containers: 0,
                        cleanup: None,
                    };
                }
            }
            OperationEvent::DeployRunning {
                operation_id,
                stage,
            } => {
                if matches!(stage, DeployRunningStage::RouteCutover) {
                    self.push_healthy_lines(operation_id);
                }
                if let Some(ObservedDeploy {
                    work: DeployWork::Planned { stage: current, .. },
                    ..
                }) = &mut self.deploy
                {
                    *current = PlannedStage::Running(*stage);
                }
            }
            OperationEvent::DeployDataplanePrepared {
                operation_id: _,
                report: _,
            } => {}
            OperationEvent::DeployContainerStarted {
                operation_id,
                machine_id: _,
                container_id: _,
            } => {
                let started_containers = self.started_containers();
                let line =
                    self.nth_run_step(started_containers)
                        .map(|(service_id, machine_id, slot)| {
                            format!(
                                "deploy {}: {} — {}.{} running on {}",
                                operation_id.as_str(),
                                service_id,
                                service_id,
                                slot,
                                machine_id
                            )
                        });
                if let Some(ObservedDeploy {
                    work:
                        DeployWork::Planned {
                            started_containers, ..
                        },
                    ..
                }) = &mut self.deploy
                {
                    *started_containers += 1;
                }
                if let Some(line) = line {
                    self.plain_lines.push(line);
                }
            }
            OperationEvent::DeployHealthCheckStarted { operation_id: _ } => {}
            OperationEvent::DeployCleanupFinished {
                operation_id,
                removed,
                failed,
            } => {
                for container in removed {
                    self.plain_lines.push(format!(
                        "deploy {}: cleanup — {} removed from {}",
                        operation_id.as_str(),
                        container.container_id.as_str(),
                        container.machine_id.as_str()
                    ));
                }
                for failure in failed {
                    self.plain_lines.push(format!(
                        "deploy {}: cleanup — {} failed on {}: {}",
                        operation_id.as_str(),
                        failure.target.container_id.as_str(),
                        failure.target.machine_id.as_str(),
                        failure.message.as_str()
                    ));
                }
                if let Some(ObservedDeploy {
                    work: DeployWork::Planned { cleanup, .. },
                    ..
                }) = &mut self.deploy
                {
                    *cleanup = Some((removed.clone(), failed.clone()));
                }
            }
            OperationEvent::DeployCompleted {
                operation_id,
                outcome,
            } => {
                match outcome {
                    DeployCompletionOutcome::Completed
                    | DeployCompletionOutcome::CompletedWithWarnings => {
                        self.push_route_lines(operation_id);
                    }
                    DeployCompletionOutcome::PartiallyCompleted
                    | DeployCompletionOutcome::PartiallyCompletedWithWarnings => {}
                }
                self.plain_lines
                    .push(render_plain_completion(operation_id, *outcome, elapsed));
                if let Some(deploy) = &mut self.deploy {
                    deploy.result = DeployResult::Completed {
                        outcome: *outcome,
                        elapsed,
                    };
                }
            }
            OperationEvent::DeployFailed {
                operation_id: _,
                failure,
            } => {
                if let Some(deploy) = &mut self.deploy {
                    deploy.result = DeployResult::Failed {
                        failure: failure.clone(),
                        elapsed,
                    };
                }
            }
            OperationEvent::Cancelled {
                operation_id,
                kind,
                reason,
            } => match kind {
                OperationKind::Deploy => {
                    self.plain_lines.push(format!(
                        "deploy {}: cancelled — {}",
                        operation_id.as_str(),
                        reason.as_str()
                    ));
                    if let Some(deploy) = &mut self.deploy {
                        deploy.result = DeployResult::Cancelled {
                            reason: reason.clone(),
                            elapsed,
                        };
                    }
                }
                OperationKind::Cert
                | OperationKind::MachineAdd
                | OperationKind::MachineUpdate
                | OperationKind::MachineLifecycle
                | OperationKind::CoreReplace
                | OperationKind::ServiceRestart
                | OperationKind::NamespaceRemove => {}
            },
            OperationEvent::CertRenewalSubmitted { .. }
            | OperationEvent::CertChallengePublished { .. }
            | OperationEvent::CertValidationStarted { .. }
            | OperationEvent::CertCompleted { .. }
            | OperationEvent::CertFailed { .. }
            | OperationEvent::MachineAddSubmitted { .. }
            | OperationEvent::MachineAddJoined { .. }
            | OperationEvent::MachineAddCredentialProvisioned { .. }
            | OperationEvent::MachineAddCompleted { .. }
            | OperationEvent::MachineAddFailed { .. }
            | OperationEvent::MachineUpdateSubmitted { .. }
            | OperationEvent::MachineUpdateRunning { .. }
            | OperationEvent::MachineUpdateCompleted { .. }
            | OperationEvent::MachineUpdateFailed { .. }
            | OperationEvent::MachineLifecycleSubmitted { .. }
            | OperationEvent::MachineLifecycleCompleted { .. }
            | OperationEvent::MachineLifecycleFailed { .. }
            | OperationEvent::CoreReplaceSubmitted { .. }
            | OperationEvent::CoreReplaceCompleted { .. }
            | OperationEvent::CoreReplaceFailed { .. }
            | OperationEvent::ServiceRestartSubmitted { .. }
            | OperationEvent::ServiceRestartRunning { .. }
            | OperationEvent::ServiceRestartContainerRestarted { .. }
            | OperationEvent::ServiceRestartCompleted { .. }
            | OperationEvent::ServiceRestartFailed { .. }
            | OperationEvent::NamespaceRemoveSubmitted { .. }
            | OperationEvent::NamespaceRemoveRunning { .. }
            | OperationEvent::NamespaceRemoveRouteBindingRemoved { .. }
            | OperationEvent::NamespaceRemoveContainerRemoved { .. }
            | OperationEvent::NamespaceRemoveCompleted { .. }
            | OperationEvent::NamespaceRemoveFailed { .. } => {}
        }
    }

    fn nth_run_step(&self, wanted: usize) -> Option<(&str, &str, u16)> {
        let plan = self.plan()?;
        let mut index = 0;
        for service in &plan.services {
            for step in &service.steps {
                match step {
                    DeployPlanStep::RunContainer { machine_id, slot } if index == wanted => {
                        return Some((
                            service.service_id.as_str(),
                            machine_id.as_str(),
                            slot.get(),
                        ));
                    }
                    DeployPlanStep::RunContainer { .. } => index += 1,
                    DeployPlanStep::UseExistingContainer { .. } => {}
                }
            }
        }
        None
    }

    fn push_healthy_lines(&mut self, operation_id: &OperationId) {
        let Some(plan) = self.plan() else {
            return;
        };
        let lines = plan
            .services
            .iter()
            .flat_map(|service| {
                service.steps.iter().filter_map(|step| match step {
                    DeployPlanStep::RunContainer { machine_id, slot } => Some(format!(
                        "deploy {}: {} — {}.{} healthy on {}",
                        operation_id.as_str(),
                        service.service_id.as_str(),
                        service.service_id.as_str(),
                        slot.get(),
                        machine_id.as_str()
                    )),
                    DeployPlanStep::UseExistingContainer { .. } => None,
                })
            })
            .collect::<Vec<_>>();
        self.plain_lines.extend(lines);
    }

    fn push_route_lines(&mut self, operation_id: &OperationId) {
        let Some(deploy) = &self.deploy else {
            return;
        };
        let lines = deploy_routes(&deploy.target)
            .map(|(service_id, route)| {
                format!(
                    "deploy {}: routes — {}",
                    operation_id.as_str(),
                    route_text(service_id, route)
                )
            })
            .collect::<Vec<_>>();
        self.plain_lines.extend(lines);
    }

    fn plan(&self) -> Option<&DeployPlan> {
        let deploy = self.deploy.as_ref()?;
        match &deploy.work {
            DeployWork::Planning => None,
            DeployWork::Planned { plan, .. } => Some(plan),
        }
    }

    fn stage(&self) -> Option<DeployRunningStage> {
        let deploy = self.deploy.as_ref()?;
        match &deploy.work {
            DeployWork::Planning => None,
            DeployWork::Planned {
                stage: PlannedStage::Queued,
                ..
            } => None,
            DeployWork::Planned {
                stage: PlannedStage::Running(stage),
                ..
            } => Some(*stage),
        }
    }

    fn started_containers(&self) -> usize {
        let Some(deploy) = &self.deploy else {
            return 0;
        };
        match &deploy.work {
            DeployWork::Planning => 0,
            DeployWork::Planned {
                started_containers, ..
            } => *started_containers,
        }
    }

    fn cleanup(&self) -> Option<&(Vec<DeployCleanupContainer>, Vec<DeployCleanupFailure>)> {
        let deploy = self.deploy.as_ref()?;
        match &deploy.work {
            DeployWork::Planning => None,
            DeployWork::Planned { cleanup, .. } => cleanup.as_ref(),
        }
    }

    fn is_active(&self) -> bool {
        self.deploy
            .as_ref()
            .is_some_and(|deploy| matches!(deploy.result, DeployResult::Active))
    }
}

/// One child line of a stage group. `queued` marks work that has not begun;
/// a group whose every child is queued collapses to its title line, per the
/// pinned rule that pending stages sit collapsed and named.
struct TreeLine {
    text: String,
    queued: bool,
}

impl TreeLine {
    fn engaged(text: String) -> Self {
        Self {
            text,
            queued: false,
        }
    }
}

pub(crate) fn render_frame(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    let operation_id = &deploy.operation_id;
    let target = &deploy.target;

    let mut active_marked = false;
    let mut groups = vec![(
        "images".to_owned(),
        render_image_lines(tree, target, &mut active_marked)
            .into_iter()
            .map(TreeLine::engaged)
            .collect::<Vec<_>>(),
    )];
    if let Some(plan) = tree.plan() {
        let mut run_index = 0;
        for service in &plan.services {
            let mut lines = Vec::new();
            for step in &service.steps {
                lines.push(render_service_step(
                    tree,
                    service.service_id.as_str(),
                    step,
                    run_index,
                    &mut active_marked,
                ));
                if matches!(step, DeployPlanStep::RunContainer { .. }) {
                    run_index += 1;
                }
            }
            groups.push((service.service_id.as_str().to_owned(), lines));
        }
    }

    let routes = deploy_routes(target)
        .map(|(service_id, route)| render_route_line(tree, service_id, route, &mut active_marked))
        .collect::<Vec<_>>();
    if !routes.is_empty() {
        groups.push(("routes".to_owned(), routes));
    }
    if let Some((removed, failed)) = tree.cleanup()
        && (!removed.is_empty() || !failed.is_empty())
    {
        groups.push((
            "cleanup".to_owned(),
            render_cleanup_lines(removed, failed)
                .into_iter()
                .map(TreeLine::engaged)
                .collect(),
        ));
    }

    let mut lines = vec![
        format!(
            "Deploy {} started — namespace {}, {} services",
            operation_id.as_str(),
            target.namespace_id.as_str(),
            target.services.len()
        ),
        String::new(),
    ];
    for (title, children) in groups {
        let collapsed = !children.is_empty() && children.iter().all(|child| child.queued);
        if collapsed {
            lines.push(format!("  {title}    queued"));
            continue;
        }
        lines.push(format!("  {title}"));
        for child in children {
            lines.push(format!("    {}", child.text));
        }
    }
    lines.join("\n") + "\n"
}

pub(crate) fn render_plain_lines(tree: &DeployTree) -> String {
    if tree.plain_lines.is_empty() {
        String::new()
    } else {
        tree.plain_lines.join("\n") + "\n"
    }
}

pub(crate) fn render_success(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    match &deploy.result {
        DeployResult::Completed { outcome, elapsed } => format!(
            "Deploy {} in {}s.\n",
            completion_text(*outcome),
            elapsed.as_secs()
        ),
        DeployResult::Active | DeployResult::Failed { .. } | DeployResult::Cancelled { .. } => {
            String::new()
        }
    }
}

pub(crate) fn render_terminal(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    match &deploy.result {
        DeployResult::Active => String::new(),
        DeployResult::Completed { .. } => render_success(tree),
        DeployResult::Failed { .. } => render_failure_block(tree),
        DeployResult::Cancelled { reason, elapsed } => format!(
            "Deploy cancelled in {}s — {}.\n",
            elapsed.as_secs(),
            reason.as_str()
        ),
    }
}

pub(crate) fn render_failure_block(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    let DeployResult::Failed { failure, elapsed } = &deploy.result else {
        return String::new();
    };
    let operation_id = deploy.operation_id.as_str();
    let target_service = match deploy.target.services.as_slice() {
        [service] => Some(&service.service_id),
        [] | [_, _, ..] => None,
    };
    let service = deploy_failure_service(failure, target_service);
    let cause = failure_cause(tree, failure);
    let retained = !failure.retained_artifacts().is_empty();

    if retained {
        let machines = deploy_failure_machines(failure);
        let machine = machines
            .first()
            .map_or("unknown", |machine| machine.as_str());
        let retained_containers = deploy_failure_containers(failure);
        let retained_machine = retained_containers
            .last()
            .map_or(machine, |container| container.machine_id.as_str());
        let namespace = deploy.target.namespace_id.as_str();
        let logs_hint = if service == "unknown" {
            String::new()
        } else {
            format!("  logs:      ployz logs {service} -n {namespace} --failed\n")
        };
        format!(
            "Deploy failed in {}s — {}, service {} on {}.\n\n  ✗ {}\n    failed container retained on {}\n\n  Serving is unchanged.\n\n{}  timeline:  ployz ops status {}\n  rollback:  ployz deploy rollback -n {}\n",
            elapsed.as_secs(),
            failure.failure_class().as_str(),
            service,
            machine,
            cause,
            retained_machine,
            logs_hint,
            operation_id,
            namespace
        )
    } else {
        format!(
            "Deploy failed in {}s — {}, service {}.\n\n  ✗ {}\n\n  Nothing changed: the failure happened before any container work.\n\n  timeline:  ployz ops status {}\n",
            elapsed.as_secs(),
            failure.failure_class().as_str(),
            service,
            cause,
            operation_id
        )
    }
}

fn distinct_images(target: &DeployRequest) -> Vec<&str> {
    let mut images = Vec::new();
    for service in &target.services {
        if !images.contains(&service.image.as_str()) {
            images.push(service.image.as_str());
        }
    }
    images
}

fn deploy_routes(target: &DeployRequest) -> impl Iterator<Item = (&str, &DeployRoute)> {
    target.services.iter().flat_map(|service| {
        service
            .routes
            .iter()
            .map(|route| (service.service_id.as_str(), route))
    })
}

fn route_text(service_id: &str, route: &DeployRoute) -> String {
    match &route.target {
        DeployRouteTarget::Hostname { hostname, port: _ } => format!(
            "{} → {}:{}",
            hostname.as_str(),
            service_id,
            route.endpoint_port.get()
        ),
        DeployRouteTarget::AutoHostname { port: _ } => {
            // Auto-hostname declarations carry no minted hostname. The renderer
            // can name it when operation evidence carries the bound route.
            format!("{service_id} → public URL (auto)")
        }
    }
}

fn render_image_lines(
    tree: &DeployTree,
    target: &DeployRequest,
    active_marked: &mut bool,
) -> Vec<String> {
    let failed_service = tree.failure().and_then(|failure| match failure {
        DeployOperationFailure::ArtifactUnavailable { service_id, .. } => Some(service_id),
        DeployOperationFailure::NoUsableMachines { .. }
        | DeployOperationFailure::PlanningFailed { .. }
        | DeployOperationFailure::DataplaneUnavailable { .. }
        | DeployOperationFailure::DataplanePrepareTimedOut { .. }
        | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
        | DeployOperationFailure::RuntimeUnavailable { .. }
        | DeployOperationFailure::ContainerStartFailed { .. }
        | DeployOperationFailure::HealthCheckFailed { .. }
        | DeployOperationFailure::ControlPlaneCommitFailed { .. }
        | DeployOperationFailure::RouteCutoverFailed { .. } => None,
    });
    distinct_images(target)
        .into_iter()
        .map(|image| {
            let failed_here = failed_service.is_some_and(|service_id| {
                target.services.iter().any(|service| {
                    &service.service_id == service_id && service.image.as_str() == image
                })
            });
            if failed_here {
                let reason = tree.failure().map_or_else(String::new, |failure| {
                    let DeployOperationFailure::ArtifactUnavailable { reason, .. } = failure else {
                        return String::new();
                    };
                    artifact_unavailable_reason(reason)
                });
                format!("✗ {image} — {reason}")
            } else if tree.plan().is_some() {
                format!("✓ {image}")
            } else if tree.is_active() && !*active_marked {
                *active_marked = true;
                format!("{} {image}", tree.spinner())
            } else {
                format!("· {image} — waiting on images")
            }
        })
        .collect()
}

fn render_service_step(
    tree: &DeployTree,
    service_id: &str,
    step: &DeployPlanStep,
    run_index: usize,
    active_marked: &mut bool,
) -> TreeLine {
    match step {
        DeployPlanStep::UseExistingContainer {
            machine_id,
            container_id: _,
            slot,
        } => TreeLine::engaged(format!(
            "✓ no changes — already at target ({}.{} on {})",
            service_id,
            slot.get(),
            machine_id.as_str()
        )),
        DeployPlanStep::RunContainer { machine_id, slot } => {
            let name = format!("{}.{} on {}", service_id, slot.get(), machine_id.as_str());
            if tree.is_complete_success()
                || tree.stage().is_some_and(|stage| {
                    stage_rank(stage) >= stage_rank(DeployRunningStage::RouteCutover)
                })
            {
                return TreeLine::engaged(format!("✓ {name} — healthy"));
            }
            if tree.stage().is_some_and(|stage| {
                stage_rank(stage) >= stage_rank(DeployRunningStage::WaitingForHealth)
            }) {
                return if !*active_marked && tree.is_active() {
                    *active_marked = true;
                    TreeLine::engaged(format!(
                        "{} {name} — running, waiting for health",
                        tree.spinner()
                    ))
                } else {
                    TreeLine::engaged(format!("· {name} — running, waiting for health"))
                };
            }
            if matches!(tree.stage(), Some(DeployRunningStage::StartingContainers)) {
                if run_index < tree.started_containers() {
                    return TreeLine::engaged(format!("✓ {name} — created"));
                }
                if !*active_marked && tree.is_active() {
                    *active_marked = true;
                    return TreeLine::engaged(format!("{} {name} — creating", tree.spinner()));
                }
                return TreeLine {
                    text: format!("· {name} — queued"),
                    queued: true,
                };
            }
            if !*active_marked && tree.is_active() {
                *active_marked = true;
                let step_text =
                    if matches!(tree.stage(), Some(DeployRunningStage::PreparingDataplane)) {
                        "preparing dataplane"
                    } else {
                        "queued"
                    };
                TreeLine::engaged(format!("{} {name} — {step_text}", tree.spinner()))
            } else {
                TreeLine {
                    text: format!("· {name} — queued"),
                    queued: true,
                }
            }
        }
    }
}

fn render_route_line(
    tree: &DeployTree,
    service_id: &str,
    route: &DeployRoute,
    active_marked: &mut bool,
) -> TreeLine {
    let text = route_text(service_id, route);
    if tree.is_complete_success() {
        return TreeLine::engaged(format!("✓ {text}"));
    }
    if tree.route_failed(route) {
        let reason = tree
            .failure()
            .map(|failure| failure_cause(tree, failure))
            .unwrap_or_else(|| "route cutover failed".to_owned());
        return TreeLine::engaged(format!("✗ {text} — {reason}"));
    }
    if tree
        .stage()
        .is_some_and(|stage| stage_rank(stage) >= stage_rank(DeployRunningStage::RouteCutover))
        && tree.is_active()
        && !*active_marked
    {
        *active_marked = true;
        let step_text = if matches!(tree.stage(), Some(DeployRunningStage::RouteCutover)) {
            "cutting over"
        } else {
            "committing"
        };
        TreeLine::engaged(format!("{} {text} — {step_text}", tree.spinner()))
    } else {
        TreeLine {
            text: format!("· {text} — queued"),
            queued: true,
        }
    }
}

fn render_cleanup_lines(
    removed: &[DeployCleanupContainer],
    failed: &[DeployCleanupFailure],
) -> Vec<String> {
    removed
        .iter()
        .map(|container| {
            format!(
                "✓ {} on {} — removed",
                container.container_id.as_str(),
                container.machine_id.as_str()
            )
        })
        .chain(failed.iter().map(|failure| {
            format!(
                "✗ {} on {} — {}",
                failure.target.container_id.as_str(),
                failure.target.machine_id.as_str(),
                failure.message.as_str()
            )
        }))
        .collect()
}

impl DeployTree {
    fn spinner(&self) -> char {
        SPINNER_FRAMES
            .get(self.spinner_frame)
            .copied()
            .unwrap_or('⣷')
    }

    fn failure(&self) -> Option<&DeployOperationFailure> {
        let deploy = self.deploy.as_ref()?;
        match &deploy.result {
            DeployResult::Failed { failure, .. } => Some(failure),
            DeployResult::Active
            | DeployResult::Completed { .. }
            | DeployResult::Cancelled { .. } => None,
        }
    }

    fn is_complete_success(&self) -> bool {
        self.deploy.as_ref().is_some_and(|deploy| {
            matches!(
                deploy.result,
                DeployResult::Completed {
                    outcome: DeployCompletionOutcome::Completed
                        | DeployCompletionOutcome::CompletedWithWarnings,
                    ..
                }
            )
        })
    }

    fn route_failed(&self, route: &DeployRoute) -> bool {
        let Some(DeployOperationFailure::RouteCutoverFailed { route: failed, .. }) = self.failure()
        else {
            return false;
        };
        route.target.concrete_target().as_ref() == Some(failed)
    }
}

const fn stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::PreparingDataplane => 0,
        DeployRunningStage::StartingContainers => 1,
        DeployRunningStage::WaitingForHealth => 2,
        DeployRunningStage::RouteCutover => 3,
        DeployRunningStage::ServingTargetCommit => 4,
        DeployRunningStage::RemovingSupersededContainers => 5,
    }
}

fn render_plain_completion(
    operation_id: &OperationId,
    outcome: DeployCompletionOutcome,
    elapsed: Duration,
) -> String {
    format!(
        "deploy {}: {} in {}s",
        operation_id.as_str(),
        completion_text(outcome),
        elapsed.as_secs()
    )
}

const fn completion_text(outcome: DeployCompletionOutcome) -> &'static str {
    match outcome {
        DeployCompletionOutcome::Completed => "succeeded",
        DeployCompletionOutcome::CompletedWithWarnings => "succeeded with warnings",
        DeployCompletionOutcome::PartiallyCompleted => "partially completed",
        DeployCompletionOutcome::PartiallyCompletedWithWarnings => {
            "partially completed with warnings"
        }
    }
}

#[cfg(test)]
mod tests;
