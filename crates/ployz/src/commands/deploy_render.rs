use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanStep, DeployRequest, DeployRoute,
    DeployRouteTarget, ImageSource,
};
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    CancellationReason, DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure,
    DeployRunningStage, OperationEvent, OperationKind, ReplayedOperationEvent,
};

use super::deploy_failure::{
    DeployFailureView, FailureSafety, artifact_unavailable_reason, failure_cause,
};

const SPINNER_FRAMES: [char; 8] = ['⣷', '⣯', '⣟', '⡿', '⢿', '⣻', '⣽', '⣾'];

pub(crate) struct DeployTree {
    deploy: Option<ObservedDeploy>,
    plain_lines: Vec<String>,
    spinner_frame: usize,
}

struct ObservedDeploy {
    operation_id: OperationId,
    target: DeployRequest,
    verified_image_services: Vec<ServiceId>,
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
    Completed { outcome: DeployCompletionOutcome },
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl DeployTree {
    pub(crate) fn new() -> Self {
        Self {
            deploy: None,
            plain_lines: Vec::new(),
            spinner_frame: 0,
        }
    }

    pub(crate) fn ingest_page(&mut self, events: &[ReplayedOperationEvent]) {
        for replayed in events {
            self.ingest(&replayed.event);
        }
    }

    pub(crate) fn tick_spinner(&mut self) {
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
    }

    fn ingest(&mut self, event: &OperationEvent) {
        match event {
            OperationEvent::DeploySubmitted {
                operation_id,
                target,
                ..
            } => {
                self.deploy = Some(ObservedDeploy {
                    operation_id: operation_id.clone(),
                    target: target.clone(),
                    verified_image_services: Vec::new(),
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
            OperationEvent::DeployImageResolved {
                operation_id,
                service_id,
                requested,
                resolved,
                ..
            } => {
                if let Some(deploy) = &mut self.deploy
                    && let Some(service) = deploy
                        .target
                        .services
                        .iter_mut()
                        .find(|service| service.service_id == *service_id)
                {
                    service.image = resolved.clone();
                }
                self.plain_lines.push(format!(
                    "deploy {}: image {} — {} → {}",
                    operation_id.as_str(),
                    service_id.as_str(),
                    requested.as_str(),
                    resolved.as_str()
                ));
            }
            OperationEvent::DeployPlanCreated { operation_id, plan } => {
                if let Some(deploy) = &self.deploy {
                    let target = &deploy.target;
                    for image in distinct_images(target) {
                        self.plain_lines.push(format!(
                            "deploy {}: images — {} planned",
                            operation_id.as_str(),
                            image
                        ));
                    }
                }
                for service in plan.phases.iter().flat_map(|phase| &phase.services) {
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
            OperationEvent::DeployImageAvailabilityVerified {
                operation_id,
                service_id,
                seed,
                manifest_digest: _,
            } => {
                let image = self.requested_image(service_id).map(str::to_owned);
                if let Some(deploy) = &mut self.deploy
                    && !deploy.verified_image_services.contains(service_id)
                {
                    deploy.verified_image_services.push(service_id.clone());
                }
                if let Some(image) = image {
                    self.plain_lines.push(format!(
                        "deploy {}: images — {} available from {}",
                        operation_id.as_str(),
                        image,
                        seed.as_str()
                    ));
                }
            }
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
            OperationEvent::DeployPhaseStarted { .. }
            | OperationEvent::DeployPhaseFinished { .. } => {}
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
                self.plain_lines.push(format!(
                    "deploy {}: {}",
                    operation_id.as_str(),
                    completion_text(*outcome)
                ));
                if let Some(deploy) = &mut self.deploy {
                    deploy.result = DeployResult::Completed { outcome: *outcome };
                }
            }
            OperationEvent::DeployFailed {
                operation_id: _,
                failure,
            } => {
                if let Some(deploy) = &mut self.deploy {
                    deploy.result = DeployResult::Failed {
                        failure: failure.clone(),
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
                        };
                    }
                }
                OperationKind::Cert
                | OperationKind::IngressConfigure
                | OperationKind::IngressRefresh
                | OperationKind::ManagedDnsReconcile
                | OperationKind::MachineAdd
                | OperationKind::MachineUpdate
                | OperationKind::MachineLifecycle
                | OperationKind::CoreReplace
                | OperationKind::CredentialGrant
                | OperationKind::NetworkRepair
                | OperationKind::ServiceRestart
                | OperationKind::NamespaceRemove
                | OperationKind::VolumeRemove => {}
            },
            OperationEvent::ManagedDnsReconcileSubmitted { .. }
            | OperationEvent::ManagedDnsReconcileCompleted { .. }
            | OperationEvent::ManagedDnsReconcileFailed { .. }
            | OperationEvent::CertProvisionSubmitted { .. }
            | OperationEvent::CertChallengePublished { .. }
            | OperationEvent::CertValidationStarted { .. }
            | OperationEvent::CertWarning { .. }
            | OperationEvent::CertCompleted { .. }
            | OperationEvent::CertFailed { .. }
            | OperationEvent::IngressRefreshSubmitted { .. }
            | OperationEvent::IngressRefreshCompleted { .. }
            | OperationEvent::IngressRefreshFailed { .. }
            | OperationEvent::IngressConfigureSubmitted { .. }
            | OperationEvent::IngressConfigureCompleted { .. }
            | OperationEvent::IngressConfigureFailed { .. }
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
            | OperationEvent::CredentialGrantSubmitted { .. }
            | OperationEvent::CredentialGrantCompleted { .. }
            | OperationEvent::CredentialGrantFailed { .. }
            | OperationEvent::NetworkRepairSubmitted { .. }
            | OperationEvent::NetworkRepairRunning { .. }
            | OperationEvent::NetworkRepairDataplaneConverged { .. }
            | OperationEvent::NetworkRepairMachineFactsRefreshed { .. }
            | OperationEvent::NetworkRepairDnsRefreshConfirmed { .. }
            | OperationEvent::NetworkRepairCompleted { .. }
            | OperationEvent::NetworkRepairFailed { .. }
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
            | OperationEvent::NamespaceRemoveFailed { .. }
            | OperationEvent::VolumeRemoveSubmitted { .. }
            | OperationEvent::VolumeRemoveRunning { .. }
            | OperationEvent::VolumeRemoveCompleted { .. }
            | OperationEvent::VolumeRemoveFailed { .. } => {}
        }
    }

    /// The `wanted`-th `RunContainer` step in plan order. Container-started
    /// events carry no service or slot identity, so attribution is by count:
    /// the core sequencer starts containers one at a time in plan-step order,
    /// and that ordering invariant is what makes the positional match honest.
    fn nth_run_step(&self, wanted: usize) -> Option<(&str, &str, u16)> {
        let plan = self.plan()?;
        let mut index = 0;
        for service in plan.phases.iter().flat_map(|phase| &phase.services) {
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
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
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

    pub(super) fn requested_image(&self, service_id: &ployz_core::ids::ServiceId) -> Option<&str> {
        self.deploy
            .as_ref()?
            .target
            .services
            .iter()
            .find(|service| &service.service_id == service_id)
            .map(|service| service.image.as_str())
    }

    fn image_is_ready(&self, image: &str) -> bool {
        let Some(deploy) = &self.deploy else {
            return false;
        };
        deploy
            .target
            .services
            .iter()
            .filter(|service| service.image.as_str() == image)
            .all(|service| match &service.image_source {
                ImageSource::Registry => true,
                ImageSource::PushedToSeed { .. } => {
                    deploy.verified_image_services.contains(&service.service_id)
                }
            })
    }
}

/// One child line of a stage group.
///
/// `Settled` lines carry their own `✓`/`✗` marker and never hold the
/// spinner. `Pending` lines are spinner candidates: while the deploy is
/// active, exactly one pending line per frame — the first in display order —
/// renders as the spinner plus its `active` text; every other pending line
/// renders as `·` plus its `idle` text. [`PendingPhase::Queued`] marks work
/// that has not begun; a group whose every child is queued and that does not
/// hold the spinner collapses to its named title line, per the pinned rule
/// that pending stages sit collapsed and named.
enum TreeLine {
    Settled {
        text: String,
    },
    Pending {
        active: String,
        idle: String,
        phase: PendingPhase,
    },
}

enum PendingPhase {
    Engaged,
    Queued,
}

pub(crate) fn render_frame(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    let operation_id = &deploy.operation_id;
    let target = &deploy.target;

    let mut groups = vec![("images".to_owned(), render_image_lines(tree, target))];
    if let Some(plan) = tree.plan() {
        let mut run_index = 0;
        for service in plan.phases.iter().flat_map(|phase| &phase.services) {
            let mut lines = Vec::new();
            for step in &service.steps {
                lines.push(render_service_step(
                    tree,
                    service.service_id.as_str(),
                    step,
                    run_index,
                ));
                if matches!(step, DeployPlanStep::RunContainer { .. }) {
                    run_index += 1;
                }
            }
            groups.push((service.service_id.as_str().to_owned(), lines));
        }
    }

    let routes = deploy_routes(target)
        .map(|(service_id, route)| render_route_line(tree, target, service_id, route))
        .collect::<Vec<_>>();
    if !routes.is_empty() {
        groups.push(("routes".to_owned(), routes));
    }
    if let Some((removed, failed)) = tree.cleanup()
        && (!removed.is_empty() || !failed.is_empty())
    {
        groups.push(("cleanup".to_owned(), render_cleanup_lines(removed, failed)));
    }

    // The spinner belongs to the first pending line in display order, and
    // only while the deploy is active; its group stays expanded even when
    // fully queued so the active step is never hidden.
    let spinner_group = if tree.is_active() {
        groups.iter().position(|(_, children)| {
            children
                .iter()
                .any(|child| matches!(child, TreeLine::Pending { .. }))
        })
    } else {
        None
    };

    let mut lines = vec![
        format!(
            "Deploy {} started — namespace {}, {} services",
            operation_id.as_str(),
            target.namespace_id.as_str(),
            target.services.len()
        ),
        String::new(),
    ];
    for (group_index, (title, children)) in groups.into_iter().enumerate() {
        let holds_spinner = spinner_group == Some(group_index);
        let collapsed = !holds_spinner
            && !children.is_empty()
            && children.iter().all(|child| match child {
                TreeLine::Settled { .. } => false,
                TreeLine::Pending { phase, .. } => matches!(phase, PendingPhase::Queued),
            });
        if collapsed {
            lines.push(format!("  {title}    queued"));
            continue;
        }
        lines.push(format!("  {title}"));
        let mut spinner_assigned = false;
        for child in children {
            lines.push(match child {
                TreeLine::Settled { text } => format!("    {text}"),
                TreeLine::Pending { active, .. } if holds_spinner && !spinner_assigned => {
                    spinner_assigned = true;
                    format!("    {} {active}", tree.spinner())
                }
                TreeLine::Pending { idle, .. } => format!("    · {idle}"),
            });
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

fn render_success(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    match &deploy.result {
        DeployResult::Completed { outcome } => {
            format!("Deploy {}.\n", completion_text(*outcome))
        }
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
        DeployResult::Cancelled { reason } => {
            format!("Deploy cancelled — {}.\n", reason.as_str())
        }
    }
}

pub(crate) fn render_failure_block(tree: &DeployTree) -> String {
    let Some(deploy) = &tree.deploy else {
        return String::new();
    };
    let DeployResult::Failed { failure } = &deploy.result else {
        return String::new();
    };
    let target_service = match deploy.target.services.as_slice() {
        [service] => Some(&service.service_id),
        [] | [_, _, ..] => None,
    };
    let failure_view = DeployFailureView::new(failure, target_service);
    let service = failure_view.service();
    let cause = failure_cause(&deploy.target, failure);
    let safety = failure_view.safety();
    let machines = failure_view.machines();
    let retained_containers = failure_view.containers();

    let mut block = format!(
        "Deploy failed — {}, service {}{}.\n",
        failure.failure_class().as_str(),
        service,
        machines
            .first()
            .map_or_else(String::new, |machine| format!(" on {}", machine.as_str())),
    );
    block.push_str(&format!("\n  ✗ {cause}\n"));
    if let Some(container) = retained_containers.last() {
        block.push_str(&format!(
            "    failed container {} retained on {}\n",
            container.container_id.as_str(),
            container.machine_id.as_str()
        ));
    }
    match safety {
        FailureSafety::NothingChanged => {
            block.push_str("\n  Nothing changed: the failure happened before any container work.\n")
        }
        FailureSafety::ServingUnchanged => block.push_str("\n  Serving is unchanged.\n"),
        FailureSafety::NoClaim => {}
    }
    block.push('\n');
    if !retained_containers.is_empty() && service != "unknown" {
        block.push_str(&format!(
            "  logs:      ployz logs {service} -n {} --failed\n",
            deploy.target.namespace_id.as_str()
        ));
    }
    block.push_str(&format!(
        "  timeline:  ployz ops status {}\n",
        deploy.operation_id.as_str()
    ));
    if !matches!(safety, FailureSafety::NothingChanged) {
        block.push_str(&format!(
            "  rollback:  ployz deploy rollback -n {}\n",
            deploy.target.namespace_id.as_str()
        ));
    }
    block
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
        DeployRouteTarget::Hostname { hostname } => format!(
            "{} → {}:{}",
            hostname.as_str(),
            service_id,
            route.endpoint_port.get()
        ),
        DeployRouteTarget::AutoHostname { label } => {
            // Auto-hostname declarations carry no minted hostname. The renderer
            // can name it when operation evidence carries the bound route.
            format!("{service_id} → public URL (auto:{})", label.as_str())
        }
    }
}

fn render_image_lines(tree: &DeployTree, target: &DeployRequest) -> Vec<TreeLine> {
    let failed_service = tree
        .failure()
        .and_then(|failure| DeployFailureView::new(failure, None).image_failure_service());
    distinct_images(target)
        .into_iter()
        .map(|image| {
            let failed_here = failed_service.is_some_and(|service_id| {
                target.services.iter().any(|service| {
                    &service.service_id == service_id && service.image.as_str() == image
                })
            });
            if failed_here {
                let reason = tree
                    .failure()
                    .map_or_else(String::new, |failure| match failure {
                        DeployOperationFailure::ImageResolutionFailed { .. } => {
                            failure_cause(target, failure)
                        }
                        DeployOperationFailure::ArtifactUnavailable { reason, .. } => {
                            artifact_unavailable_reason(reason)
                        }
                        DeployOperationFailure::ImageMissingOnSeed { .. }
                        | DeployOperationFailure::ImageDigestMismatch { .. }
                        | DeployOperationFailure::SeedUnavailable { .. }
                        | DeployOperationFailure::UnsupportedTargetPlatform { .. } => {
                            failure_cause(target, failure)
                        }
                        DeployOperationFailure::NoUsableMachines { .. }
                        | DeployOperationFailure::PlanningFailed { .. }
                        | DeployOperationFailure::RuntimeUnavailable { .. }
                        | DeployOperationFailure::ContainerStartFailed { .. }
                        | DeployOperationFailure::PreStartHookFailed { .. }
                        | DeployOperationFailure::HealthCheckFailed { .. }
                        | DeployOperationFailure::CertificateProvisionFailed { .. }
                        | DeployOperationFailure::CertificateProvisionTimedOut { .. }
                        | DeployOperationFailure::ControlPlaneCommitFailed { .. }
                        | DeployOperationFailure::RouteCutoverFailed { .. } => String::new(),
                    });
                TreeLine::Settled {
                    text: format!("✗ {image} — {reason}"),
                }
            } else if tree.plan().is_some() && tree.image_is_ready(image) {
                TreeLine::Settled {
                    text: format!("✓ {image}"),
                }
            } else {
                let ensuring = matches!(tree.stage(), Some(DeployRunningStage::EnsuringImages));
                TreeLine::Pending {
                    active: if ensuring {
                        format!("{image} — redistributing")
                    } else {
                        image.to_owned()
                    },
                    idle: format!("{image} — waiting on images"),
                    phase: if ensuring {
                        PendingPhase::Engaged
                    } else if tree.plan().is_some() {
                        PendingPhase::Queued
                    } else {
                        PendingPhase::Engaged
                    },
                }
            }
        })
        .collect()
}

fn render_service_step(
    tree: &DeployTree,
    service_id: &str,
    step: &DeployPlanStep,
    run_index: usize,
) -> TreeLine {
    match step {
        DeployPlanStep::UseExistingContainer {
            machine_id,
            container_id: _,
            slot,
        } => TreeLine::Settled {
            text: format!(
                "✓ no changes — already at target ({}.{} on {})",
                service_id,
                slot.get(),
                machine_id.as_str()
            ),
        },
        DeployPlanStep::RunContainer { machine_id, slot } => {
            let name = format!("{}.{} on {}", service_id, slot.get(), machine_id.as_str());
            if tree.is_complete_success()
                || tree.stage().is_some_and(|stage| {
                    stage_rank(stage) >= stage_rank(DeployRunningStage::EnsuringCertificates)
                })
            {
                return TreeLine::Settled {
                    text: format!("✓ {name} — healthy"),
                };
            }
            if tree.stage().is_some_and(|stage| {
                stage_rank(stage) >= stage_rank(DeployRunningStage::WaitingForHealth)
            }) {
                let text = format!("{name} — running, waiting for health");
                return TreeLine::Pending {
                    active: text.clone(),
                    idle: text,
                    phase: PendingPhase::Engaged,
                };
            }
            if matches!(tree.stage(), Some(DeployRunningStage::StartingContainers)) {
                if run_index < tree.started_containers() {
                    return TreeLine::Settled {
                        text: format!("✓ {name} — created"),
                    };
                }
                return TreeLine::Pending {
                    active: format!("{name} — creating"),
                    idle: format!("{name} — queued"),
                    phase: PendingPhase::Queued,
                };
            }
            let step_text = match tree.stage() {
                Some(DeployRunningStage::EnsuringImages) => "ensuring images",
                Some(DeployRunningStage::StartingContainers)
                | Some(DeployRunningStage::WaitingForHealth)
                | Some(DeployRunningStage::EnsuringCertificates)
                | Some(DeployRunningStage::RouteCutover)
                | Some(DeployRunningStage::ServingTargetCommit)
                | Some(DeployRunningStage::RemovingSupersededContainers)
                | None => "queued",
            };
            TreeLine::Pending {
                active: format!("{name} — {step_text}"),
                idle: format!("{name} — queued"),
                phase: PendingPhase::Queued,
            }
        }
    }
}

fn render_route_line(
    tree: &DeployTree,
    target: &DeployRequest,
    service_id: &str,
    route: &DeployRoute,
) -> TreeLine {
    let text = route_text(service_id, route);
    if tree.is_complete_success() {
        return TreeLine::Settled {
            text: format!("✓ {text}"),
        };
    }
    if tree.route_failed(route) {
        let reason = tree
            .failure()
            .map(|failure| failure_cause(target, failure))
            .unwrap_or_else(|| "route cutover failed".to_owned());
        return TreeLine::Settled {
            text: format!("✗ {text} — {reason}"),
        };
    }
    let step_text = match tree.stage() {
        Some(DeployRunningStage::EnsuringCertificates) => "ensuring certificate",
        Some(DeployRunningStage::RouteCutover) => "cutting over",
        Some(DeployRunningStage::ServingTargetCommit)
        | Some(DeployRunningStage::RemovingSupersededContainers) => "committing",
        Some(DeployRunningStage::EnsuringImages)
        | Some(DeployRunningStage::StartingContainers)
        | Some(DeployRunningStage::WaitingForHealth)
        | None => "queued",
    };
    TreeLine::Pending {
        active: format!("{text} — {step_text}"),
        idle: format!("{text} — queued"),
        phase: PendingPhase::Queued,
    }
}

fn render_cleanup_lines(
    removed: &[DeployCleanupContainer],
    failed: &[DeployCleanupFailure],
) -> Vec<TreeLine> {
    removed
        .iter()
        .map(|container| TreeLine::Settled {
            text: format!(
                "✓ {} on {} — removed",
                container.container_id.as_str(),
                container.machine_id.as_str()
            ),
        })
        .chain(failed.iter().map(|failure| TreeLine::Settled {
            text: format!(
                "✗ {} on {} — {}",
                failure.target.container_id.as_str(),
                failure.target.machine_id.as_str(),
                failure.message.as_str()
            ),
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
        let Some(failure) = self.failure() else {
            return false;
        };
        let Some(failed) = DeployFailureView::new(failure, None).failed_route() else {
            return false;
        };
        route.target.concrete_target().as_ref() == Some(failed)
    }
}

const fn stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::EnsuringImages => 0,
        DeployRunningStage::StartingContainers => 1,
        DeployRunningStage::WaitingForHealth => 2,
        DeployRunningStage::EnsuringCertificates => 3,
        DeployRunningStage::RouteCutover => 4,
        DeployRunningStage::ServingTargetCommit => 5,
        DeployRunningStage::RemovingSupersededContainers => 6,
    }
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
