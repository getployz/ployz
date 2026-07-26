use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::future::join_all;
use ployz_core::build::BuildPlatforms;
use ployz_core::deploy::{
    DeployPlacementPlan, DeployPlanError, DeployPlanningContext, DeployPlanningTarget,
    DeployPreviewImage, DeployPreviewTarget, EnvironmentRevisionKey, ImageSource,
    plan_namespace_deploy,
};
use ployz_core::operation::{FailureMessage, UnusableMachine};
use ployz_sdk_types::{DeployPreview, DeployPreviewError, DeployPreviewRequest};

use crate::control::intent::ingress_intent::{IngressProjectionStore, PloyzDnsTargetStore};
use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::OperationRepository;
use crate::control::role_client::machine::NatsMachineFactsReader;

use super::{
    DeployFactLoadError, MachineContainerRuntime,
    facts::{PlacementFactsGatherPolicy, load_planning_facts_from_nats},
    images::ImagePreparationError,
};

pub(crate) const DEPLOY_PREVIEW_FACT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const DEPLOY_PREVIEW_FACT_PHASE_TIMEOUT: Duration = Duration::from_millis(6_250);
pub(crate) const DEPLOY_PREVIEW_IMAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const DEPLOY_PREVIEW_TOTAL_TIMEOUT: Duration = Duration::from_millis(9_500);
pub(crate) const DEPLOY_PREVIEW_HANDLER_TIMEOUT: Duration = Duration::from_millis(9_750);

pub(crate) struct DeployPreviewStores<'a> {
    pub operation_repository: &'a OperationRepository,
    pub target_store: &'a PloyzDnsTargetStore,
    pub projection_store: &'a IngressProjectionStore,
}

pub async fn preview_deploy_from_nats(
    request: DeployPreviewRequest,
    environment_revision_key: &EnvironmentRevisionKey,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    machine_runtime: &mut (impl MachineContainerRuntime + Clone),
    stores: DeployPreviewStores<'_>,
    step_timeout: Duration,
) -> Result<DeployPreview, DeployPreviewError> {
    let fact_request_timeout = step_timeout.min(DEPLOY_PREVIEW_FACT_REQUEST_TIMEOUT);
    let image_request_timeout = step_timeout.min(DEPLOY_PREVIEW_IMAGE_REQUEST_TIMEOUT);
    let DeployPreviewRequest {
        mut target,
        registry_credentials,
    } = request;
    let planning_target = DeployPlanningTarget::try_from_preview(&target, environment_revision_key)
        .map_err(|error| invalid_target(error.to_string()))?;
    planning_target
        .validate_registry_credential_service_ids(registry_credentials.keys())
        .map_err(|error| invalid_target(error.to_string()))?;
    let facts = tokio::time::timeout(
        DEPLOY_PREVIEW_FACT_PHASE_TIMEOUT,
        load_planning_facts_from_nats(
            &planning_target,
            intent_reader,
            facts_reader,
            stores.target_store,
            stores.projection_store,
            fact_request_timeout,
            PlacementFactsGatherPolicy::OverallDeadline(fact_request_timeout),
        ),
    )
    .await
    .map_err(|_| DeployPreviewError::Unavailable {
        message: "deploy preview fact gathering timed out".to_owned(),
    })?
    .map_err(preview_fact_load_error)?;
    let reusable_interrupted_operation_ids =
        super::driver::reusable_interrupted_deploy_operation_ids(
            stores.operation_repository,
            planning_target.namespace_id(),
            &facts.observed_machines,
        )
        .await
        .map_err(|error| DeployPreviewError::Unavailable {
            message: error.to_string(),
        })?;
    let provisional_command = super::preparation::prepare_deploy_preview_command(
        planning_target,
        facts.clone(),
        &reusable_interrupted_operation_ids,
    );
    let provisional_plan = preview_plan(&provisional_command)?;
    resolve_preview_registry_images(
        &mut target,
        &provisional_plan,
        &provisional_command,
        &registry_credentials,
        image_request_timeout,
        machine_runtime,
    )
    .await?;
    let planning_target = DeployPlanningTarget::try_from_preview(&target, environment_revision_key)
        .map_err(|error| invalid_target(error.to_string()))?;
    let command = super::preparation::prepare_deploy_preview_command(
        planning_target,
        facts,
        &reusable_interrupted_operation_ids,
    );
    let plan = preview_plan(&command)?;

    let now_unix_seconds = super::images::current_unix_seconds()
        .map_err(|error| planning_failed(error.to_string(), command.unusable_machines.clone()))?;
    for service in command.target.services() {
        let Some((_, ImageSource::PushedToSeed(receipt))) = service.concrete_image() else {
            continue;
        };
        let target_machines = plan
            .service_target_machines(service.service_id())
            .ok_or_else(|| {
                planning_failed(
                    format!(
                        "preview plan omitted service {}",
                        service.service_id().as_str()
                    ),
                    command.unusable_machines.clone(),
                )
            })?;
        super::images::validate_pushed_service_availability(
            service.service_id(),
            receipt,
            &target_machines,
            &command.machine_platforms,
            &command.seed_clock_testimony,
            now_unix_seconds,
        )
        .map_err(|error| {
            preview_image_preparation_error(
                error,
                unusable_machines_for_service(&command, service.service_id()),
            )
        })?;
    }

    let build_platform_requirements = command
        .target
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
        unusable_machines_by_service: command.unusable_machines_by_service,
    })
}

fn preview_plan(
    command: &super::types::DeployPreviewPlanningCommand,
) -> Result<DeployPlacementPlan, DeployPreviewError> {
    plan_namespace_deploy(
        &command.target,
        command.planning_inputs.clone(),
        command.namespace_cleanup_candidates.clone(),
        DeployPlanningContext {
            storage_testimony: &command.storage_testimony,
            placement_load: &command.placement_load,
        },
    )
    .map_err(|error| preview_plan_error(error, command))
}

async fn resolve_preview_registry_images(
    target: &mut DeployPreviewTarget,
    plan: &DeployPlacementPlan,
    command: &super::types::DeployPreviewPlanningCommand,
    registry_credentials: &BTreeMap<
        ployz_core::ids::ServiceId,
        ployz_core::deploy::RegistryCredential,
    >,
    image_timeout: Duration,
    machine_runtime: &mut (impl MachineContainerRuntime + Clone),
) -> Result<(), DeployPreviewError> {
    let mut unresolved = target
        .services
        .iter()
        .filter_map(|service| match &service.image {
            DeployPreviewImage::Concrete {
                image,
                image_source: ImageSource::Registry,
            } if image.pinned_digest().is_none() => {
                Some((service.service_id.clone(), image.clone()))
            }
            DeployPreviewImage::Concrete { .. } | DeployPreviewImage::PendingBuild => None,
        })
        .collect::<Vec<_>>();
    unresolved.sort_by(|left, right| left.0.cmp(&right.0));
    let requests = unresolved
        .into_iter()
        .map(|(service_id, requested)| {
            let unusable_machines = unusable_machines_for_service(command, &service_id);
            let Some(machine_id) =
                super::images::registry_resolution_machine(&plan.phases, &service_id)
            else {
                return Err(planning_failed(
                    format!("preview plan omitted service {}", service_id.as_str()),
                    unusable_machines,
                ));
            };
            Ok((
                service_id.clone(),
                requested,
                machine_id,
                registry_credentials.get(&service_id).cloned(),
                unusable_machines,
            ))
        })
        .collect::<Result<Vec<_>, DeployPreviewError>>()?;
    let resolutions = join_all(requests.into_iter().map(
        |(service_id, requested, machine_id, credential, unusable_machines)| {
            let mut runtime = (*machine_runtime).clone();
            async move {
                let resolved = super::images::resolve_registry_image(
                    &service_id,
                    &requested,
                    &machine_id,
                    credential.as_ref(),
                    image_timeout,
                    &mut runtime,
                )
                .await
                .map_err(|error| preview_image_preparation_error(error, unusable_machines))?;
                Ok((service_id, resolved))
            }
        },
    ))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, DeployPreviewError>>()?;
    for (service_id, resolved) in resolutions {
        let service = target
            .services
            .iter_mut()
            .find(|service| service.service_id == service_id)
            .expect("validated preview target still contains its service");
        let DeployPreviewImage::Concrete { image, .. } = &mut service.image else {
            unreachable!("registry resolution target remains concrete")
        };
        *image = resolved;
    }
    Ok(())
}

fn unusable_machines_for_service(
    command: &super::types::DeployPreviewPlanningCommand,
    service_id: &ployz_core::ids::ServiceId,
) -> Vec<UnusableMachine> {
    command
        .unusable_machines_by_service
        .get(service_id)
        .cloned()
        .unwrap_or_else(|| command.unusable_machines.clone())
}

fn preview_image_preparation_error(
    error: ImagePreparationError,
    unusable_machines: Vec<UnusableMachine>,
) -> DeployPreviewError {
    match error {
        ImagePreparationError::Failure { failure } => DeployPreviewError::ImageUnavailable {
            failure: Box::new(failure.into()),
            unusable_machines,
        },
        ImagePreparationError::ResolutionTimedOut { .. } => DeployPreviewError::Unavailable {
            message: "deploy preview registry image resolution timed out".to_owned(),
        },
        ImagePreparationError::InternalInvariant { message } => planning_failed(
            format!("deploy execution invariant failed: {message}"),
            unusable_machines,
        ),
    }
}

fn preview_plan_error(
    error: DeployPlanError,
    command: &super::types::DeployPreviewPlanningCommand,
) -> DeployPreviewError {
    let unusable_machines = match &error {
        DeployPlanError::VolumeAdmissionOnMachine {
            service_id,
            machine_id,
            failure,
        } => {
            return DeployPreviewError::VolumeAdmissionFailed {
                service_id: service_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            };
        }
        DeployPlanError::NoEligibleMachines { service_id } => command
            .unusable_machines_by_service
            .get(service_id)
            .cloned()
            .unwrap_or_else(|| command.unusable_machines.clone()),
        DeployPlanError::UnknownService { .. }
        | DeployPlanError::PlacementModeMismatch { .. }
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

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn preview_deadlines_cover_sequential_phases() {
        assert!(DEPLOY_PREVIEW_FACT_REQUEST_TIMEOUT < DEPLOY_PREVIEW_FACT_PHASE_TIMEOUT);
        assert!(DEPLOY_PREVIEW_IMAGE_REQUEST_TIMEOUT < DEPLOY_PREVIEW_FACT_PHASE_TIMEOUT);
        assert!(
            DEPLOY_PREVIEW_FACT_PHASE_TIMEOUT + DEPLOY_PREVIEW_IMAGE_REQUEST_TIMEOUT
                < DEPLOY_PREVIEW_TOTAL_TIMEOUT
        );
        assert!(DEPLOY_PREVIEW_TOTAL_TIMEOUT < DEPLOY_PREVIEW_HANDLER_TIMEOUT);
    }
}
