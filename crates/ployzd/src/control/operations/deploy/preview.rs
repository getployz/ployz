use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use ployz_core::build::BuildPlatforms;
use ployz_core::deploy::{
    DeployPlanError, DeployPlanningContext, DeployPlanningTarget, DeployPreviewImage, ImageSource,
    plan_namespace_deploy,
};
use ployz_core::machine::MachineUsabilityReason;
use ployz_core::operation::{FailureMessage, UnusableMachine};
use ployz_sdk_types::{DeployPreview, DeployPreviewError, DeployPreviewRequest};

use crate::control::intent::ingress_intent::{IngressProjectionStore, PloyzDnsTargetStore};
use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::NatsMachineFactsReader;

use super::{DeployExecutionFacts, DeployFactLoadError, load_deploy_preview_facts_from_nats};

pub async fn preview_deploy_from_nats(
    request: DeployPreviewRequest,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    target_store: &PloyzDnsTargetStore,
    projection_store: &IngressProjectionStore,
    step_timeout: Duration,
) -> Result<DeployPreview, DeployPreviewError> {
    let target = request.target;
    DeployPlanningTarget::try_from_preview(&target)
        .map_err(|error| invalid_target(error.to_string()))?;
    let facts = load_deploy_preview_facts_from_nats(
        &target,
        intent_reader,
        facts_reader,
        target_store,
        projection_store,
        step_timeout,
    )
    .await
    .map_err(preview_fact_load_error)?;
    validate_pushed_image_seeds(&target, &facts)?;
    let command = super::preparation::prepare_deploy_preview_command(target, facts);
    let planning_target = DeployPlanningTarget::try_from_preview(&command.target)
        .expect("deploy preview command contains a validated planning target");
    let plan = plan_namespace_deploy(
        &planning_target,
        command.planning_inputs.clone(),
        command.namespace_cleanup_candidates.clone(),
        DeployPlanningContext {
            storage_testimony: &command.storage_testimony,
        },
    )
    .map_err(|error| preview_plan_error(error, &command))?;

    let now_unix_seconds = super::images::current_unix_seconds()
        .map_err(|error| planning_failed(error.to_string(), command.unusable_machines.clone()))?;
    for service in &command.target.services {
        let DeployPreviewImage::Concrete {
            image_source: ImageSource::PushedToSeed(receipt),
            ..
        } = &service.image
        else {
            continue;
        };
        let target_machines = plan
            .service_target_machines(&service.service_id)
            .ok_or_else(|| {
                planning_failed(
                    format!(
                        "preview plan omitted service {}",
                        service.service_id.as_str()
                    ),
                    command.unusable_machines.clone(),
                )
            })?;
        super::images::validate_pushed_service_availability(
            &service.service_id,
            receipt,
            &target_machines,
            &command.machine_platforms,
            &command.seed_clock_testimony,
            now_unix_seconds,
        )
        .map_err(|error| {
            let unusable_machines = command
                .unusable_machines_by_service
                .get(&service.service_id)
                .cloned()
                .unwrap_or_else(|| command.unusable_machines.clone());
            match error {
                super::DeployExecutionError::Image { failure } => {
                    DeployPreviewError::ImageUnavailable {
                        failure,
                        unusable_machines,
                    }
                }
                error @ (super::DeployExecutionError::Plan(_)
                | super::DeployExecutionError::PlanInconsistent { .. }
                | super::DeployExecutionError::StepId(_)
                | super::DeployExecutionError::InvalidImagePull { .. }
                | super::DeployExecutionError::InternalInvariant { .. }
                | super::DeployExecutionError::StepTimedOut { .. }
                | super::DeployExecutionError::RecordTransition(_)
                | super::DeployExecutionError::RecordEvidence(_)
                | super::DeployExecutionError::RunContainer(_)
                | super::DeployExecutionError::EnsureVolume(_)
                | super::DeployExecutionError::PreStartHook(_)
                | super::DeployExecutionError::PreStartHookExited { .. }
                | super::DeployExecutionError::WaitHealthy(_)
                | super::DeployExecutionError::ProvisionCertificate { .. }
                | super::DeployExecutionError::CommitNamespaceState(_)
                | super::DeployExecutionError::Failed { .. }) => {
                    planning_failed(error.to_string(), unusable_machines)
                }
            }
        })?;
    }

    let build_platform_requirements = planning_target
        .services()
        .iter()
        .filter(|service| service.concrete_image().is_none())
        .map(|service| service.service_id())
        .map(|service_id| {
            let target_machines = plan.service_target_machines(service_id).ok_or_else(|| {
                planning_failed(
                    format!("preview plan omitted service {}", service_id.as_str()),
                    command.unusable_machines.clone(),
                )
            })?;
            let platforms = target_machines
                .iter()
                .map(|machine_id| {
                    command
                        .machine_platforms
                        .get(machine_id)
                        .cloned()
                        .ok_or_else(|| {
                            planning_failed(
                                format!(
                                    "planned machine {} did not report a platform",
                                    machine_id.as_str()
                                ),
                                command.unusable_machines.clone(),
                            )
                        })
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            let platforms = BuildPlatforms::try_new(platforms).map_err(|error| {
                planning_failed(error.to_string(), command.unusable_machines.clone())
            })?;
            Ok((service_id.clone(), platforms))
        })
        .collect::<Result<BTreeMap<_, _>, DeployPreviewError>>()?;

    Ok(DeployPreview {
        projection: ployz_core::deploy::DeployPreviewProjection::from_plan(
            plan,
            command.route_binding_additions,
            command.route_binding_removals,
            command.serving_target_commits,
            command.serving_target_removals,
        ),
        build_platform_requirements,
        unusable_machines: command.unusable_machines,
    })
}

fn validate_pushed_image_seeds(
    target: &ployz_core::deploy::DeployPreviewTarget,
    facts: &DeployExecutionFacts,
) -> Result<(), DeployPreviewError> {
    for service in &target.services {
        let DeployPreviewImage::Concrete {
            image_source: ImageSource::PushedToSeed(receipt),
            ..
        } = &service.image
        else {
            continue;
        };
        for (_, platform_image) in receipt.platforms() {
            let seed = &platform_image.seed;
            if !facts
                .dataplane_members
                .iter()
                .any(|member| &member.machine_id == seed)
            {
                return Err(invalid_target(format!(
                    "pushed image seed {} is not in the active roster",
                    seed.as_str()
                )));
            }
            if facts.unusable_machines.iter().any(|unusable| {
                &unusable.machine_id == seed
                    && matches!(&unusable.reason, MachineUsabilityReason::Draining)
            }) {
                return Err(invalid_target(format!(
                    "pushed image seed {} is not in the active lifecycle",
                    seed.as_str()
                )));
            }
        }
    }
    Ok(())
}

fn preview_plan_error(
    error: DeployPlanError,
    command: &super::types::DeployPreviewPlanningCommand,
) -> DeployPreviewError {
    let unusable_machines = match &error {
        DeployPlanError::NoEligibleMachines { service_id } => command
            .unusable_machines_by_service
            .get(service_id)
            .cloned()
            .unwrap_or_else(|| command.unusable_machines.clone()),
        DeployPlanError::UnknownService { .. }
        | DeployPlanError::UnknownServiceDependency { .. }
        | DeployPlanError::ServiceDependencyCycle { .. }
        | DeployPlanError::HealthyDependencyWithoutHealthcheck { .. }
        | DeployPlanError::ConflictingVolumePins { .. }
        | DeployPlanError::VolumeAdmission { .. } => command.unusable_machines.clone(),
    };
    planning_failed(error.to_string(), unusable_machines)
}

fn invalid_target(message: String) -> DeployPreviewError {
    DeployPreviewError::InvalidTarget {
        message: FailureMessage::try_new(message).expect("validation failure is non-empty"),
    }
}

fn planning_failed(message: String, unusable_machines: Vec<UnusableMachine>) -> DeployPreviewError {
    DeployPreviewError::PlanningFailed {
        message: FailureMessage::try_new(message).expect("planning failure is non-empty"),
        unusable_machines,
    }
}

fn preview_fact_load_error(error: DeployFactLoadError) -> DeployPreviewError {
    match error {
        DeployFactLoadError::InvalidStoredTarget { .. }
        | DeployFactLoadError::InvalidRouteBindings { .. } => invalid_target(error.to_string()),
        DeployFactLoadError::IntentRead { .. }
        | DeployFactLoadError::IngressState { .. }
        | DeployFactLoadError::IngressUnavailable { .. } => DeployPreviewError::Unavailable {
            message: error.to_string(),
        },
    }
}
