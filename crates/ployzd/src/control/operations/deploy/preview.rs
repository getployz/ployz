use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use ployz_core::build::BuildPlatforms;
use ployz_core::deploy::{VolumeDeclaredDeployRequest, validate_pushed_image_reference};
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::operation::FailureMessage;
use ployz_sdk_types::{DeployPreview, DeployPreviewError, DeployPreviewRequest};

use crate::control::intent::ingress_intent::{IngressProjectionStore, PloyzDnsTargetStore};
use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::NatsMachineFactsReader;

use super::{
    DeployFactLoadError, deploy_plan, load_deploy_execution_facts_from_nats,
    validate_pushed_platforms,
};

pub async fn preview_deploy_from_nats(
    request: DeployPreviewRequest,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    target_store: &PloyzDnsTargetStore,
    projection_store: &IngressProjectionStore,
    step_timeout: Duration,
) -> Result<DeployPreview, DeployPreviewError> {
    let (target, pending_builds) = request.target.into_planning_target();
    let target = VolumeDeclaredDeployRequest::try_new(target)
        .map_err(|error| invalid_target(error.to_string()))?;
    super::validate_deploy_service_names(&target)
        .map_err(|message| DeployPreviewError::InvalidTarget { message })?;
    let facts = load_deploy_execution_facts_from_nats(
        &target,
        intent_reader,
        facts_reader,
        target_store,
        projection_store,
        step_timeout,
    )
    .await
    .map_err(preview_fact_load_error)?;
    validate_concrete_pushed_images(&target, &pending_builds)?;
    let machine_platforms = facts.machine_platforms.clone();
    let command = super::preparation::prepare_deploy_execution_command_with_credentials(
        preview_operation_id(),
        target,
        facts,
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    let unusable_machines = command.unusable_machines.clone();
    let plan = deploy_plan(&command)
        .map_err(|error| planning_failed(error.to_string(), unusable_machines.clone()))?;
    validate_pushed_platforms(&command, &plan)
        .map_err(|error| planning_failed(error.to_string(), unusable_machines.clone()))?;
    let build_platform_requirements = pending_builds
        .into_iter()
        .map(|service_id| {
            let target_machines = plan.service_target_machines(&service_id).ok_or_else(|| {
                planning_failed(
                    format!("preview plan omitted service {}", service_id.as_str()),
                    unusable_machines.clone(),
                )
            })?;
            let platforms = target_machines
                .iter()
                .map(|machine_id| {
                    machine_platforms.get(machine_id).cloned().ok_or_else(|| {
                        planning_failed(
                            format!(
                                "planned machine {} did not report a platform",
                                machine_id.as_str()
                            ),
                            unusable_machines.clone(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let platforms = BuildPlatforms::try_new(platforms)
                .map_err(|error| planning_failed(error.to_string(), unusable_machines.clone()))?;
            Ok((service_id, platforms))
        })
        .collect::<Result<BTreeMap<_, _>, DeployPreviewError>>()?;

    Ok(DeployPreview {
        projection: plan.into(),
        build_platform_requirements,
        unusable_machines,
    })
}

fn validate_concrete_pushed_images(
    target: &VolumeDeclaredDeployRequest,
    pending_builds: &BTreeSet<ServiceId>,
) -> Result<(), DeployPreviewError> {
    for service in target.services() {
        if pending_builds.contains(&service.service_id) {
            continue;
        }
        validate_pushed_image_reference(&service.image, &service.image_source).map_err(
            |error| {
                invalid_target(format!(
                    "pushed image for service {} {error}",
                    service.service_id.as_str()
                ))
            },
        )?;
    }
    Ok(())
}

fn preview_operation_id() -> OperationId {
    OperationId::try_new("op_deploy_preview").expect("preview operation id is valid")
}

fn invalid_target(message: String) -> DeployPreviewError {
    DeployPreviewError::InvalidTarget {
        message: FailureMessage::try_new(message).expect("validation failure is non-empty"),
    }
}

fn planning_failed(
    message: String,
    unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
) -> DeployPreviewError {
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
