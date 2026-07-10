//! Pushed-image availability and mesh redistribution for deploy execution.

use ployz_core::dataplane::{DataplaneMember, DataplanePrepareRequest};
use ployz_core::deploy::{DeployPlan, DeployPlanStep, DeployRequest, ImageSource};
use ployz_core::image::{ImageEnsureRequest, ImageRepository, ImageRpcDomainError, OciDigest};
use ployz_core::ops::{DeployEvidence, DeployOperationFailure, FailureMessage};

use crate::roles::machine::client::{MachineImageEnsureError, MachineImageResolveError};
use crate::roles::machine::protocol::{MachineContainerResolveImageRpcRequest, MachineImagePull};

use super::deploy_plan;
use super::{
    DeployExecutionCommand, DeployExecutionError, DeployOperationRecorder,
    DeployServiceExecutionCommand, MachineContainerRuntime, record_evidence,
};

pub(super) fn dataplane_prepare_request(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
) -> DataplanePrepareRequest {
    let mut members = command.dataplane_members.clone();
    for seed in command.services().iter().filter_map(|service| {
        let ImageSource::PushedToSeed { seed, .. } = &service.request.image_source else {
            return None;
        };
        Some(seed)
    }) {
        if members.iter().all(|member| member.machine_id != *seed) {
            members.push(DataplaneMember::default_for_machine(seed.clone()));
        }
    }
    DataplanePrepareRequest::for_deploy_plan(command.operation_id.clone(), plan, &members)
}

pub(super) async fn resolve_registry_images<R, N>(
    command: &DeployExecutionCommand,
    request: &mut DeployRequest,
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    let provisional_plan = deploy_plan(command)?;
    for target in &mut request.services {
        if !matches!(target.image_source, ImageSource::Registry) {
            continue;
        }
        if target.image.pinned_digest().is_some() {
            continue;
        }
        let Some(service) = command
            .services()
            .iter()
            .find(|service| service.request.service_id == target.service_id)
        else {
            return Err(DeployExecutionError::PlanInconsistent {
                service_id: target.service_id.clone(),
            });
        };
        let Some(machine_id) = provisional_plan
            .services
            .iter()
            .find(|plan| plan.service_id == target.service_id)
            .and_then(|plan| plan.steps.first())
            .map(|step| match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id.clone(),
            })
        else {
            return Err(DeployExecutionError::PlanInconsistent {
                service_id: target.service_id.clone(),
            });
        };
        let requested = target.image.clone();
        let digest = tokio::time::timeout(
            command.step_timeout(),
            machine_runtime.resolve_image(
                &machine_id,
                MachineContainerResolveImageRpcRequest {
                    reference: requested.clone(),
                    credential: service.registry_credential().cloned(),
                },
            ),
        )
        .await
        .map_err(|_| {
            image_resolution_failure(
                service,
                &machine_id,
                &requested,
                deploy_failure_message("image resolution timed out"),
            )
        })?
        .map_err(|error| image_resolution_error(service, &requested, error))?;
        let resolved = requested.with_digest(&digest).map_err(|error| {
            image_resolution_failure(
                service,
                &machine_id,
                &requested,
                deploy_failure_message(error.to_string()),
            )
        })?;
        target.image = resolved.clone();
        record_evidence(
            command,
            recorder,
            DeployEvidence::ImageResolved {
                service_id: target.service_id.clone(),
                machine_id,
                requested,
                resolved,
                credential_supplied: service.registry_credential().is_some(),
            },
        )
        .await?;
    }
    Ok(())
}

fn image_resolution_error(
    service: &DeployServiceExecutionCommand,
    requested: &ployz_core::deploy::ImageReference,
    error: MachineImageResolveError,
) -> DeployExecutionError {
    match error {
        MachineImageResolveError::Rejected {
            machine_id,
            message,
        } => image_resolution_failure(service, &machine_id, requested, message),
        MachineImageResolveError::Unavailable { machine_id, reason } => {
            image_resolution_failure(service, &machine_id, requested, reason.failure_message())
        }
    }
}

fn image_resolution_failure(
    service: &DeployServiceExecutionCommand,
    machine_id: &ployz_core::ids::MachineId,
    requested: &ployz_core::deploy::ImageReference,
    message: FailureMessage,
) -> DeployExecutionError {
    DeployExecutionError::Image {
        failure: Box::new(DeployOperationFailure::ImageResolutionFailed {
            service_id: service.request.service_id.clone(),
            machine_id: machine_id.clone(),
            image: requested.clone(),
            message,
        }),
    }
}

pub(super) async fn ensure_images<R, N>(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    for service in command.services() {
        let ImageSource::PushedToSeed {
            seed,
            manifest_digest,
            image_id,
        } = &service.request.image_source
        else {
            continue;
        };
        let request = ImageEnsureRequest {
            repository: ImageRepository::for_service(
                &service.request.namespace_id,
                &service.request.service_id,
            ),
            manifest_digest: manifest_digest.clone(),
            image_id: image_id.clone(),
        };
        let ensured = tokio::time::timeout(
            command.step_timeout(),
            machine_runtime.ensure_image(seed, request),
        )
        .await
        .map_err(|_| DeployExecutionError::Image {
            failure: Box::new(DeployOperationFailure::SeedUnavailable {
                service_id: service.request.service_id.clone(),
                seed: seed.clone(),
                message: deploy_failure_message("image seed ensure timed out"),
            }),
        })?
        .map_err(|error| ensure_image_failure(service, seed, manifest_digest, error))?;
        let Some(service_plan) = plan
            .services
            .iter()
            .find(|plan| plan.service_id == service.request.service_id)
        else {
            continue;
        };
        for step in &service_plan.steps {
            let machine_id = match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id,
            };
            let Some(target_platform) = command.machine_platform(machine_id) else {
                return Err(DeployExecutionError::InvalidImagePull {
                    message: format!(
                        "target machine {} did not report a platform",
                        machine_id.as_str()
                    ),
                });
            };
            if ensured.platform != *target_platform {
                return Err(DeployExecutionError::Image {
                    failure: Box::new(DeployOperationFailure::UnsupportedTargetPlatform {
                        service_id: service.request.service_id.clone(),
                        machine_id: machine_id.clone(),
                        image_platform: ensured.platform.clone(),
                        target_platform: target_platform.clone(),
                    }),
                });
            }
        }
        record_evidence(
            command,
            recorder,
            DeployEvidence::ImageAvailabilityVerified {
                service_id: service.request.service_id.clone(),
                seed: seed.clone(),
                manifest_digest: manifest_digest.clone(),
            },
        )
        .await?;
    }
    Ok(())
}

fn ensure_image_failure(
    service: &DeployServiceExecutionCommand,
    seed: &ployz_core::ids::MachineId,
    manifest_digest: &OciDigest,
    error: MachineImageEnsureError,
) -> DeployExecutionError {
    let failure = match error {
        MachineImageEnsureError::Domain {
            error: ImageRpcDomainError::ImageMissing { .. },
            ..
        } => DeployOperationFailure::ImageMissingOnSeed {
            service_id: service.request.service_id.clone(),
            seed: seed.clone(),
            manifest_digest: manifest_digest.clone(),
        },
        MachineImageEnsureError::Domain {
            error:
                ImageRpcDomainError::DigestMismatch { expected, actual }
                | ImageRpcDomainError::ConfigMismatch { expected, actual },
            ..
        } => DeployOperationFailure::ImageDigestMismatch {
            service_id: service.request.service_id.clone(),
            seed: seed.clone(),
            expected,
            actual,
        },
        MachineImageEnsureError::Unavailable { reason, .. } => {
            DeployOperationFailure::SeedUnavailable {
                service_id: service.request.service_id.clone(),
                seed: seed.clone(),
                message: reason.failure_message(),
            }
        }
        MachineImageEnsureError::Domain { error, .. } => DeployOperationFailure::SeedUnavailable {
            service_id: service.request.service_id.clone(),
            seed: seed.clone(),
            message: deploy_failure_message(format!("image seed rejected ensure: {error:?}")),
        },
    };
    DeployExecutionError::Image {
        failure: Box::new(failure),
    }
}

pub(super) fn machine_image_pull(
    service: &DeployServiceExecutionCommand,
    dataplane_members: &[DataplaneMember],
) -> Result<MachineImagePull, DeployExecutionError> {
    match &service.request.image_source {
        ImageSource::Registry => Ok(MachineImagePull::Registry {
            reference: service.request.image.clone(),
            credential: service.registry_credential().cloned(),
        }),
        ImageSource::PushedToSeed {
            seed,
            manifest_digest,
            image_id: _,
        } => {
            let Some(seed_host) = dataplane_members
                .iter()
                .find(|member| member.machine_id == *seed)
                .map(|member| member.endpoint_subnet.host_address())
            else {
                return Err(DeployExecutionError::InvalidImagePull {
                    message: format!("image seed {} has no dataplane membership", seed.as_str()),
                });
            };
            Ok(MachineImagePull::MeshSeed {
                seed_host,
                repository: ImageRepository::for_service(
                    &service.request.namespace_id,
                    &service.request.service_id,
                ),
                manifest_digest: manifest_digest.clone(),
            })
        }
    }
}

fn deploy_failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated deploy failure message is non-empty")
}
