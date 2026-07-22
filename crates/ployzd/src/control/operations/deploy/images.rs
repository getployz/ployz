//! Pushed-image availability and mesh redistribution for deploy execution.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::deploy::{
    DeployCleanupAction, DeployPhasePlan, DeployPlan, DeployPlanStepRef, DeployRequest,
    DeployServicePlan, IMAGE_AVAILABILITY_SAFETY_MARGIN, ImageReference, ImageSource,
    RegistryCredential,
};
use ployz_core::ids::{MachineId, ServiceId};
use ployz_core::image::{
    ImageEnsureFailure, ImageEnsureRequest, ImageEnsureSource, ImageEnsureStatus,
    ImageRemoveDomainError, ImageRepository, ImageRpcDomainError,
};
use ployz_core::machine::runtime::{ManagedContainerIdentity, ManagedContainerKind};
use ployz_core::network::DataplaneMember;
use ployz_core::operation::{
    ArtifactUnavailableReason, DeployEvidence, DeployImageCleanup, DeployOperationFailure,
    FailureMessage,
};
use ployz_sdk_types::DeployPreviewImageFailure;

use crate::control::role_client::machine::{
    MachineClockTestimony, MachineImageEnsureError, MachineImageRemoveError,
    MachineImageResolveError,
};
use crate::roles::machine::protocol::MachineContainerRemoveRpcRequest;
use crate::roles::machine::protocol::MachineContainerResolveImageRpcRequest;

use super::deploy_plan;
use super::{
    DeployCleanupResult, DeployExecutionCommand, DeployExecutionError, DeployOperationRecorder,
    DeployServiceExecutionCommand, MachineContainerRuntime, MachineImageRemovalRuntime,
    cleanup_failure_message, deploy_step_id, record_evidence,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImagePreparationFailure {
    ResolutionFailed {
        service_id: ServiceId,
        machine_id: MachineId,
        image: ImageReference,
        message: FailureMessage,
    },
    SelectedPlatformUnavailable {
        service_id: ServiceId,
        machine_id: MachineId,
        target_platform: ployz_core::image::OciPlatform,
    },
    SeedUnavailable {
        service_id: ServiceId,
        seed: MachineId,
        message: FailureMessage,
    },
    AvailabilityExpired {
        service_id: ServiceId,
        seed: MachineId,
        target_platform: ployz_core::image::OciPlatform,
        expired_at: ployz_core::deploy::ImageAvailabilityExpiresAt,
    },
}

#[derive(Debug)]
pub(super) enum ImagePreparationError {
    Failure { failure: ImagePreparationFailure },
    ResolutionTimedOut { failure: ImagePreparationFailure },
    InternalInvariant { message: String },
}

impl From<ImagePreparationError> for DeployExecutionError {
    fn from(error: ImagePreparationError) -> Self {
        match error {
            ImagePreparationError::Failure { failure }
            | ImagePreparationError::ResolutionTimedOut { failure } => Self::Image {
                failure: Box::new(failure.into()),
            },
            ImagePreparationError::InternalInvariant { message } => {
                Self::InternalInvariant { message }
            }
        }
    }
}

impl From<ImagePreparationFailure> for DeployOperationFailure {
    fn from(failure: ImagePreparationFailure) -> Self {
        match failure {
            ImagePreparationFailure::ResolutionFailed {
                service_id,
                machine_id,
                image,
                message,
            } => Self::ImageResolutionFailed {
                service_id,
                machine_id,
                image,
                message,
            },
            ImagePreparationFailure::SelectedPlatformUnavailable {
                service_id,
                machine_id,
                target_platform,
            } => Self::PlatformImageUnavailable {
                service_id,
                machine_id,
                target_platform,
            },
            ImagePreparationFailure::SeedUnavailable {
                service_id,
                seed,
                message,
            } => Self::SeedUnavailable {
                service_id,
                seed,
                message,
            },
            ImagePreparationFailure::AvailabilityExpired {
                service_id,
                seed,
                target_platform,
                expired_at,
            } => Self::PlatformImageExpired {
                service_id,
                seed,
                target_platform,
                expired_at,
            },
        }
    }
}

impl From<ImagePreparationFailure> for DeployPreviewImageFailure {
    fn from(failure: ImagePreparationFailure) -> Self {
        match failure {
            ImagePreparationFailure::ResolutionFailed {
                service_id,
                machine_id,
                image,
                message,
            } => Self::ImageResolutionFailed {
                service_id,
                machine_id,
                image,
                message,
            },
            ImagePreparationFailure::SelectedPlatformUnavailable {
                service_id,
                machine_id,
                target_platform,
            } => Self::PlatformImageUnavailable {
                service_id,
                machine_id,
                target_platform,
            },
            ImagePreparationFailure::SeedUnavailable {
                service_id,
                seed,
                message,
            } => Self::SeedUnavailable {
                service_id,
                seed,
                message,
            },
            ImagePreparationFailure::AvailabilityExpired {
                service_id,
                seed,
                target_platform,
                expired_at,
            } => Self::PlatformImageExpired {
                service_id,
                seed,
                target_platform,
                expired_at,
            },
        }
    }
}

pub(crate) async fn execute_cleanup_actions<N>(
    operation_id: &ployz_core::ids::OperationId,
    step_timeout: std::time::Duration,
    machine_runtime: &mut N,
    actions: &[DeployCleanupAction],
) -> (Vec<DeployCleanupResult>, Vec<DeployImageCleanup>)
where
    N: MachineContainerRuntime + MachineImageRemovalRuntime,
{
    let mut cleanup = Vec::new();
    let mut evidence = Vec::new();
    let mut reclamations = std::collections::BTreeMap::<_, Vec<_>>::new();
    for action in actions {
        let target = action.target();
        let result = machine_runtime.remove_container(
            &target.machine_id,
            MachineContainerRemoveRpcRequest {
                operation_id: operation_id.clone(),
                container_id: target.container_id.clone(),
                expected_identity: target.identity.clone(),
            },
        );
        let result = tokio::time::timeout(step_timeout, result).await;
        match result {
            Ok(Ok(())) => {
                cleanup.push(DeployCleanupResult::Removed(target.clone()));
                match action {
                    DeployCleanupAction::RemoveContainer { .. } => {}
                    DeployCleanupAction::RemoveContainerAndReclaimImage {
                        image_identity, ..
                    } => {
                        reclamations
                            .entry((target.machine_id.clone(), image_identity.clone()))
                            .or_default()
                            .push(target.clone());
                    }
                    DeployCleanupAction::RemoveContainerWithInvalidImageIdentity {
                        observed_identity,
                        ..
                    } => {
                        evidence.push(DeployImageCleanup::MissingIdentity {
                            machine_id: target.machine_id.clone(),
                            service_id: target.identity.service_id.clone(),
                            container_id: target.container_id.clone(),
                            observed_identity: observed_identity.clone(),
                        });
                    }
                }
            }
            Ok(Err(error)) => cleanup.push(DeployCleanupResult::Failed {
                target: target.clone(),
                message: cleanup_failure_message(error),
            }),
            Err(_) => cleanup.push(DeployCleanupResult::Failed {
                target: target.clone(),
                message: deploy_failure_message(format!(
                    "container cleanup timed out after {} seconds",
                    step_timeout.as_secs()
                )),
            }),
        }
    }

    for ((machine_id, image_identity), targets) in reclamations {
        let request = ployz_core::image::ImageRemoveRequest {
            operation_id: operation_id.clone(),
            image_identity: image_identity.clone(),
        };
        let result = tokio::time::timeout(
            step_timeout,
            machine_runtime.remove_image(&machine_id, request),
        )
        .await;
        for target in targets {
            let item = match &result {
                Ok(Ok(ok)) => match ok.outcome {
                    ployz_core::image::ImageRemoveOutcome::Removed => DeployImageCleanup::Removed {
                        machine_id: machine_id.clone(),
                        service_id: target.identity.service_id.clone(),
                        image_identity: image_identity.clone(),
                    },
                    ployz_core::image::ImageRemoveOutcome::AlreadyAbsent => {
                        DeployImageCleanup::AlreadyAbsent {
                            machine_id: machine_id.clone(),
                            service_id: target.identity.service_id.clone(),
                            image_identity: image_identity.clone(),
                        }
                    }
                    ployz_core::image::ImageRemoveOutcome::RetainedInUse => {
                        DeployImageCleanup::RetainedInUse {
                            machine_id: machine_id.clone(),
                            service_id: target.identity.service_id.clone(),
                            image_identity: image_identity.clone(),
                        }
                    }
                },
                Ok(Err(error)) => DeployImageCleanup::Failed {
                    machine_id: machine_id.clone(),
                    service_id: target.identity.service_id.clone(),
                    image_identity: image_identity.clone(),
                    message: image_remove_failure_message(error.clone()),
                },
                Err(_) => DeployImageCleanup::Failed {
                    machine_id: machine_id.clone(),
                    service_id: target.identity.service_id.clone(),
                    image_identity: image_identity.clone(),
                    message: deploy_failure_message(format!(
                        "image reclamation timed out after {} seconds",
                        step_timeout.as_secs()
                    )),
                },
            };
            evidence.push(item);
        }
    }
    (cleanup, evidence)
}

fn image_remove_failure_message(error: MachineImageRemoveError) -> FailureMessage {
    match error {
        MachineImageRemoveError::Unavailable { reason, .. } => reason.failure_message(),
        MachineImageRemoveError::Domain { error, .. } => match error {
            ImageRemoveDomainError::InvalidRequest { message }
            | ImageRemoveDomainError::RemoveFailed { message } => message,
        },
    }
}

pub(super) fn dataplane_membership(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
) -> Vec<DataplaneMember> {
    let mut members = command.dataplane_members.clone();
    for service in command.services() {
        let ImageSource::PushedToSeed(receipt) = &service.service.image_source else {
            continue;
        };
        for (_, platform_image) in receipt.platforms() {
            if members
                .iter()
                .all(|member| member.machine_id != platform_image.seed)
            {
                members.push(DataplaneMember::default_for_machine(
                    platform_image.seed.clone(),
                ));
            }
        }
    }
    let mut machine_ids = plan.target_machines();
    machine_ids.extend(members.iter().map(|member| member.machine_id.clone()));
    machine_ids.sort();
    machine_ids.dedup();
    machine_ids
        .into_iter()
        .map(|machine_id| {
            members
                .iter()
                .find(|member| member.machine_id == machine_id)
                .cloned()
                .unwrap_or_else(|| DataplaneMember::default_for_machine(machine_id))
        })
        .collect()
}

pub(super) fn validate_pushed_platforms(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
) -> Result<(), Box<DeployExecutionError>> {
    for phase in &plan.phases {
        for service_plan in &phase.services {
            let Some(service) = command
                .services()
                .iter()
                .find(|service| service.service.service_id == service_plan.service_id)
            else {
                return Err(Box::new(DeployExecutionError::PlanInconsistent {
                    service_id: service_plan.service_id.clone(),
                }));
            };
            let ImageSource::PushedToSeed(receipt) = &service.service.image_source else {
                continue;
            };
            let Some(target_machines) = plan.service_target_machines(&service_plan.service_id)
            else {
                return Err(Box::new(DeployExecutionError::PlanInconsistent {
                    service_id: service_plan.service_id.clone(),
                }));
            };
            for machine_id in &target_machines {
                let target_platform = command
                    .target_platform(machine_id)
                    .map_err(|error| Box::new(error.into_execution_error()))?;
                if receipt.platform(target_platform).is_none() {
                    return Err(Box::new(DeployExecutionError::Image {
                        failure: Box::new(DeployOperationFailure::PlatformImageUnavailable {
                            service_id: service.service.service_id.clone(),
                            machine_id: machine_id.clone(),
                            target_platform: target_platform.clone(),
                        }),
                    }));
                }
            }
        }
    }
    Ok(())
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
    let targets = request
        .services
        .iter()
        .filter(|target| {
            matches!(target.image_source, ImageSource::Registry)
                && target.image.pinned_digest().is_none()
        })
        .map(|target| (target.service_id.clone(), target.image.clone()))
        .collect::<Vec<_>>();
    for (service_id, requested) in targets {
        let Some(service) = command
            .services()
            .iter()
            .find(|service| service.service.service_id == service_id)
        else {
            return Err(DeployExecutionError::PlanInconsistent { service_id });
        };
        let Some(machine_id) = registry_resolution_machine(&provisional_plan.phases, &service_id)
        else {
            return Err(DeployExecutionError::PlanInconsistent { service_id });
        };
        let resolved = resolve_registry_image(
            &service_id,
            &requested,
            &machine_id,
            service.registry_credential(),
            command.step_timeout(),
            machine_runtime,
        )
        .await
        .map_err(DeployExecutionError::from)?;
        request
            .replace_service_image(&service_id, resolved.clone())
            .map_err(|error| DeployExecutionError::InternalInvariant {
                message: error.to_string(),
            })?;
        record_evidence(
            command,
            recorder,
            DeployEvidence::ImageResolved {
                service_id,
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

pub(super) fn registry_resolution_machine(
    phases: &[DeployPhasePlan],
    service_id: &ServiceId,
) -> Option<MachineId> {
    phases
        .iter()
        .flat_map(|phase| &phase.services)
        .find(|plan| &plan.service_id == service_id)
        .and_then(|plan| plan.work.steps().next())
        .map(|step| step.machine_id().clone())
}

pub(super) async fn resolve_registry_image<N>(
    service_id: &ServiceId,
    requested: &ImageReference,
    machine_id: &MachineId,
    credential: Option<&RegistryCredential>,
    step_timeout: std::time::Duration,
    machine_runtime: &mut N,
) -> Result<ImageReference, ImagePreparationError>
where
    N: MachineContainerRuntime,
{
    let digest = tokio::time::timeout(
        step_timeout,
        machine_runtime.resolve_image(
            machine_id,
            MachineContainerResolveImageRpcRequest {
                reference: requested.clone(),
                credential: credential.cloned(),
            },
        ),
    )
    .await
    .map_err(|_| ImagePreparationError::ResolutionTimedOut {
        failure: ImagePreparationFailure::ResolutionFailed {
            service_id: service_id.clone(),
            machine_id: machine_id.clone(),
            image: requested.clone(),
            message: deploy_failure_message("image resolution timed out"),
        },
    })?;
    let digest = digest.map_err(|error| image_resolution_error(service_id, requested, error))?;
    requested.with_digest(&digest).map_err(|error| {
        image_resolution_failure(
            service_id,
            machine_id,
            requested,
            deploy_failure_message(error.to_string()),
        )
    })
}

fn image_resolution_error(
    service_id: &ServiceId,
    requested: &ployz_core::deploy::ImageReference,
    error: MachineImageResolveError,
) -> ImagePreparationError {
    match error {
        MachineImageResolveError::Rejected {
            machine_id,
            message,
        } => image_resolution_failure(service_id, &machine_id, requested, message),
        MachineImageResolveError::Unavailable { machine_id, reason } => {
            image_resolution_failure(service_id, &machine_id, requested, reason.failure_message())
        }
    }
}

fn image_resolution_failure(
    service_id: &ServiceId,
    machine_id: &ployz_core::ids::MachineId,
    requested: &ployz_core::deploy::ImageReference,
    message: FailureMessage,
) -> ImagePreparationError {
    ImagePreparationError::Failure {
        failure: ImagePreparationFailure::ResolutionFailed {
            service_id: service_id.clone(),
            machine_id: machine_id.clone(),
            image: requested.clone(),
            message,
        },
    }
}

pub(super) async fn ensure_images<R, N>(
    command: &DeployExecutionCommand,
    service_plans: &[DeployServicePlan],
    dataplane_members: &[DataplaneMember],
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    if command
        .services()
        .iter()
        .any(|service| matches!(&service.service.image_source, ImageSource::PushedToSeed(_)))
    {
        validate_pushed_image_availability(command, service_plans, current_unix_seconds()?)?;
    }
    for service in command.services() {
        let Some(service_plan) = service_plans
            .iter()
            .find(|plan| plan.service_id == service.service.service_id)
        else {
            continue;
        };
        let mut targets = BTreeMap::new();
        for step in service_plan.work.steps() {
            if let DeployPlanStepRef::RunContainer { machine_id, slot } = step {
                targets.entry(machine_id.clone()).or_insert(*slot);
            }
        }
        for (machine_id, slot) in targets {
            let target_platform = command
                .target_platform(&machine_id)
                .map_err(|error| error.into_execution_error())?;
            let owner = ManagedContainerIdentity {
                namespace_id: command.request.namespace_id.clone(),
                service_id: service.service.service_id.clone(),
                namespace_revision_entry_id: service.namespace_revision_entry_id(
                    &command.request.namespace_id,
                    &command.environment_revision_key,
                ),
                operation_id: command.operation_id.clone(),
                step_id: deploy_step_id(slot, &machine_id).map_err(DeployExecutionError::StepId)?,
                kind: ManagedContainerKind::Service,
            };
            let source = match &service.service.image_source {
                ImageSource::Registry => ImageEnsureSource::Registry {
                    reference: service.service.image.clone(),
                    credential: service.registry_credential().cloned(),
                },
                ImageSource::PushedToSeed(receipt) => {
                    let Some(platform_image) = receipt.platform(target_platform) else {
                        return Err(DeployExecutionError::Image {
                            failure: Box::new(DeployOperationFailure::PlatformImageUnavailable {
                                service_id: service.service.service_id.clone(),
                                machine_id: machine_id.clone(),
                                target_platform: target_platform.clone(),
                            }),
                        });
                    };
                    let Some(seed_host) = dataplane_members
                        .iter()
                        .find(|member| member.machine_id == platform_image.seed)
                        .map(|member| member.endpoint_subnet.host_address())
                    else {
                        return Err(DeployExecutionError::InvalidImagePull {
                            message: format!(
                                "image seed {} has no dataplane membership",
                                platform_image.seed.as_str()
                            ),
                        });
                    };
                    let repository = ImageRepository::for_service(
                        &command.request.namespace_id,
                        &service.service.service_id,
                    );
                    let seed_source = ImageEnsureSource::LocalSeed {
                        repository: repository.clone(),
                        manifest_digest: platform_image.manifest_digest.clone(),
                        image_id: platform_image.image_id.clone(),
                        platform: target_platform.clone(),
                    };
                    let seed_reference = drive_image_ensure(
                        machine_runtime,
                        &platform_image.seed,
                        owner.clone(),
                        seed_source,
                    )
                    .await
                    .map_err(|error| {
                        ensure_seed_drive_failure(
                            service,
                            &machine_id,
                            target_platform,
                            platform_image,
                            error,
                        )
                    })?;
                    if machine_id == platform_image.seed {
                        record_target_image_evidence(
                            command,
                            recorder,
                            service,
                            &machine_id,
                            target_platform.clone(),
                            seed_reference,
                        )
                        .await?;
                        continue;
                    }
                    ImageEnsureSource::MeshSeed {
                        seed_host,
                        repository,
                        manifest_digest: platform_image.manifest_digest.clone(),
                        image_id: platform_image.image_id.clone(),
                        platform: target_platform.clone(),
                    }
                }
            };
            let reference = drive_image_ensure(machine_runtime, &machine_id, owner, source)
                .await
                .map_err(|error| target_ensure_failure(command, service, &machine_id, error))?;
            record_target_image_evidence(
                command,
                recorder,
                service,
                &machine_id,
                target_platform.clone(),
                reference,
            )
            .await?;
        }
    }
    Ok(())
}

async fn record_target_image_evidence<R: DeployOperationRecorder>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    service: &DeployServiceExecutionCommand,
    machine_id: &MachineId,
    platform: ployz_core::image::OciPlatform,
    image: ImageReference,
) -> Result<(), DeployExecutionError> {
    record_evidence(
        command,
        recorder,
        DeployEvidence::ImageAvailabilityVerified {
            service_id: service.service.service_id.clone(),
            machine_id: machine_id.clone(),
            image,
            platform,
        },
    )
    .await
}

enum ImageEnsureDriveError {
    Call(MachineImageEnsureError),
    Failed(ImageEnsureFailure),
    Cancelled,
}

async fn drive_image_ensure<N: MachineContainerRuntime>(
    machine_runtime: &mut N,
    machine_id: &MachineId,
    owner: ManagedContainerIdentity,
    source: ImageEnsureSource,
) -> Result<ImageReference, ImageEnsureDriveError> {
    let mut status = call_image_ensure(
        machine_runtime,
        machine_id,
        ImageEnsureRequest::Start {
            owner: owner.clone(),
            source,
        },
    )
    .await
    .map_err(ImageEnsureDriveError::Call)?;
    loop {
        match status.status {
            ImageEnsureStatus::Completed { reference } => return Ok(reference),
            ImageEnsureStatus::Failed { failure } => {
                return Err(ImageEnsureDriveError::Failed(failure));
            }
            ImageEnsureStatus::Cancelled => return Err(ImageEnsureDriveError::Cancelled),
            ImageEnsureStatus::Accepted | ImageEnsureStatus::Running { .. } => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await
            }
        }
        status = match call_image_ensure(
            machine_runtime,
            machine_id,
            ImageEnsureRequest::Status {
                owner: owner.clone(),
            },
        )
        .await
        {
            Ok(status) => status,
            Err(error) => {
                let _ = machine_runtime
                    .ensure_image(
                        machine_id,
                        ImageEnsureRequest::Cancel {
                            owner: owner.clone(),
                        },
                    )
                    .await;
                return Err(ImageEnsureDriveError::Call(error));
            }
        };
    }
}

async fn call_image_ensure<N: MachineContainerRuntime>(
    machine_runtime: &mut N,
    machine_id: &MachineId,
    request: ImageEnsureRequest,
) -> Result<ployz_core::image::ImageEnsureOk, MachineImageEnsureError> {
    let mut last = None;
    for _ in 0..3 {
        match machine_runtime
            .ensure_image(machine_id, request.clone())
            .await
        {
            Ok(ok) => return Ok(ok),
            Err(error @ MachineImageEnsureError::Domain { .. }) => return Err(error),
            Err(error @ MachineImageEnsureError::Unavailable { .. }) => {
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    Err(last.expect("image ensure retry loop records an error"))
}

fn ensure_seed_drive_failure(
    service: &DeployServiceExecutionCommand,
    machine_id: &MachineId,
    target_platform: &ployz_core::image::OciPlatform,
    platform_image: &ployz_core::deploy::PlatformImage,
    error: ImageEnsureDriveError,
) -> DeployExecutionError {
    match error {
        ImageEnsureDriveError::Call(error) => {
            ensure_image_failure(service, machine_id, target_platform, platform_image, error)
        }
        ImageEnsureDriveError::Failed(ImageEnsureFailure::PullFailed { message }) => {
            DeployExecutionError::Image {
                failure: Box::new(DeployOperationFailure::SeedUnavailable {
                    service_id: service.service.service_id.clone(),
                    seed: platform_image.seed.clone(),
                    message,
                }),
            }
        }
        ImageEnsureDriveError::Failed(ImageEnsureFailure::Stalled { timeout_millis }) => {
            DeployExecutionError::Image {
                failure: Box::new(DeployOperationFailure::SeedUnavailable {
                    service_id: service.service.service_id.clone(),
                    seed: platform_image.seed.clone(),
                    message: deploy_failure_message(format!(
                        "image seed pull stalled after {timeout_millis}ms without verified progress"
                    )),
                }),
            }
        }
        ImageEnsureDriveError::Cancelled => DeployExecutionError::Image {
            failure: Box::new(DeployOperationFailure::SeedUnavailable {
                service_id: service.service.service_id.clone(),
                seed: platform_image.seed.clone(),
                message: deploy_failure_message("image seed ensure was cancelled"),
            }),
        },
    }
}

fn target_ensure_failure(
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    machine_id: &MachineId,
    error: ImageEnsureDriveError,
) -> DeployExecutionError {
    let reason = match error {
        ImageEnsureDriveError::Failed(ImageEnsureFailure::Stalled { timeout_millis }) => {
            ArtifactUnavailableReason::ImagePullStalled {
                machine_id: machine_id.clone(),
                timeout_millis,
            }
        }
        ImageEnsureDriveError::Failed(ImageEnsureFailure::PullFailed { message }) => {
            ArtifactUnavailableReason::ImagePullFailed {
                machine_id: machine_id.clone(),
                message,
            }
        }
        ImageEnsureDriveError::Cancelled => ArtifactUnavailableReason::ImagePullCancelled {
            machine_id: machine_id.clone(),
        },
        ImageEnsureDriveError::Call(MachineImageEnsureError::Unavailable { reason, .. }) => {
            ArtifactUnavailableReason::ImagePullFailed {
                machine_id: machine_id.clone(),
                message: reason.failure_message(),
            }
        }
        ImageEnsureDriveError::Call(MachineImageEnsureError::Domain { error, .. }) => {
            ArtifactUnavailableReason::ImagePullFailed {
                machine_id: machine_id.clone(),
                message: deploy_failure_message(format!("image ensure rejected: {error:?}")),
            }
        }
    };
    DeployExecutionError::Image {
        failure: Box::new(DeployOperationFailure::ArtifactUnavailable {
            service_id: service.service.service_id.clone(),
            namespace_revision_entry_id: service.namespace_revision_entry_id(
                &command.request.namespace_id,
                &command.environment_revision_key,
            ),
            reason,
        }),
    }
}

pub(super) fn validate_pushed_image_availability(
    command: &DeployExecutionCommand,
    service_plans: &[DeployServicePlan],
    now_unix_seconds: u64,
) -> Result<(), DeployExecutionError> {
    for service in command.services() {
        let Some(service_plan) = service_plans
            .iter()
            .find(|plan| plan.service_id == service.service.service_id)
        else {
            continue;
        };
        let ImageSource::PushedToSeed(receipt) = &service.service.image_source else {
            continue;
        };
        let target_machines = service_plan
            .work
            .steps()
            .map(|step| step.machine_id().clone())
            .collect::<Vec<_>>();
        validate_pushed_service_availability(
            &service.service.service_id,
            receipt,
            &target_machines,
            &command.machine_platforms,
            &command.seed_clock_testimony,
            now_unix_seconds,
        )
        .map_err(DeployExecutionError::from)?;
    }
    Ok(())
}

pub(super) fn validate_pushed_service_availability(
    service_id: &ployz_core::ids::ServiceId,
    receipt: &ployz_core::deploy::PushedImageReceipt,
    target_machines: &[ployz_core::ids::MachineId],
    machine_platforms: &BTreeMap<ployz_core::ids::MachineId, ployz_core::image::OciPlatform>,
    seed_clock_testimony: &BTreeMap<ployz_core::ids::MachineId, MachineClockTestimony>,
    now_unix_seconds: u64,
) -> Result<(), ImagePreparationError> {
    let mut target_platforms = BTreeMap::new();
    for machine_id in target_machines {
        let Some(target_platform) = machine_platforms.get(machine_id) else {
            return Err(ImagePreparationError::InternalInvariant {
                message: format!(
                    "placed target machine {} has no answered platform facts",
                    machine_id.as_str()
                ),
            });
        };
        target_platforms
            .entry(target_platform.clone())
            .or_insert_with(|| machine_id.clone());
    }
    for (target_platform, machine_id) in target_platforms {
        let Some(platform_image) = receipt.platform(&target_platform) else {
            return Err(ImagePreparationError::Failure {
                failure: ImagePreparationFailure::SelectedPlatformUnavailable {
                    service_id: service_id.clone(),
                    machine_id,
                    target_platform,
                },
            });
        };
        let failure = seed_clock_failure(
            service_id,
            platform_image,
            seed_clock_testimony.get(&platform_image.seed),
        )
        .or_else(|| {
            expired_platform_failure(
                service_id,
                &target_platform,
                platform_image,
                now_unix_seconds,
            )
        });
        if let Some(failure) = failure {
            return Err(ImagePreparationError::Failure { failure });
        }
    }
    Ok(())
}

fn seed_clock_failure(
    service_id: &ployz_core::ids::ServiceId,
    platform_image: &ployz_core::deploy::PlatformImage,
    testimony: Option<&MachineClockTestimony>,
) -> Option<ImagePreparationFailure> {
    let Some(testimony) = testimony else {
        return Some(ImagePreparationFailure::SeedUnavailable {
            service_id: service_id.clone(),
            seed: platform_image.seed.clone(),
            message: deploy_failure_message("fresh clock testimony from image seed is unavailable"),
        });
    };
    let maximum_seed_time = testimony.control_request_started_at_unix_ms.saturating_add(
        IMAGE_AVAILABILITY_SAFETY_MARGIN
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
    );
    (testimony.machine_observed_at_unix_ms > maximum_seed_time).then(|| {
        ImagePreparationFailure::SeedUnavailable {
            service_id: service_id.clone(),
            seed: platform_image.seed.clone(),
            message: deploy_failure_message(
                "image seed clock is more than 300 seconds ahead of Control",
            ),
        }
    })
}

pub(super) fn current_unix_seconds() -> Result<u64, DeployExecutionError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| DeployExecutionError::InternalInvariant {
            message: format!("system time is before the Unix epoch: {error}"),
        })
}

fn expired_platform_failure(
    service_id: &ployz_core::ids::ServiceId,
    target_platform: &ployz_core::image::OciPlatform,
    platform_image: &ployz_core::deploy::PlatformImage,
    now_unix_seconds: u64,
) -> Option<ImagePreparationFailure> {
    platform_image
        .availability_expires_at
        .is_expired_at(now_unix_seconds)
        .then(|| ImagePreparationFailure::AvailabilityExpired {
            service_id: service_id.clone(),
            seed: platform_image.seed.clone(),
            target_platform: target_platform.clone(),
            expired_at: platform_image.availability_expires_at,
        })
}

fn ensure_image_failure(
    service: &DeployServiceExecutionCommand,
    machine_id: &ployz_core::ids::MachineId,
    target_platform: &ployz_core::image::OciPlatform,
    platform_image: &ployz_core::deploy::PlatformImage,
    error: MachineImageEnsureError,
) -> DeployExecutionError {
    let seed = &platform_image.seed;
    let manifest_digest = &platform_image.manifest_digest;
    let failure = match error {
        MachineImageEnsureError::Domain {
            error: ImageRpcDomainError::ImageMissing { .. },
            ..
        } => DeployOperationFailure::ImageMissingOnSeed {
            service_id: service.service.service_id.clone(),
            seed: seed.clone(),
            manifest_digest: manifest_digest.clone(),
        },
        MachineImageEnsureError::Domain {
            error:
                ImageRpcDomainError::DigestMismatch { expected, actual }
                | ImageRpcDomainError::ConfigMismatch { expected, actual },
            ..
        } => DeployOperationFailure::ImageDigestMismatch {
            service_id: service.service.service_id.clone(),
            seed: seed.clone(),
            expected,
            actual,
        },
        MachineImageEnsureError::Domain {
            error: ImageRpcDomainError::PlatformMismatch { actual, .. },
            ..
        } => DeployOperationFailure::UnsupportedTargetPlatform {
            service_id: service.service.service_id.clone(),
            machine_id: machine_id.clone(),
            image_platform: actual,
            target_platform: target_platform.clone(),
        },
        MachineImageEnsureError::Unavailable { reason, .. } => {
            DeployOperationFailure::SeedUnavailable {
                service_id: service.service.service_id.clone(),
                seed: seed.clone(),
                message: reason.failure_message(),
            }
        }
        MachineImageEnsureError::Domain { error, .. } => DeployOperationFailure::SeedUnavailable {
            service_id: service.service.service_id.clone(),
            seed: seed.clone(),
            message: deploy_failure_message(format!("image seed rejected ensure: {error:?}")),
        },
    };
    DeployExecutionError::Image {
        failure: Box::new(failure),
    }
}

pub(super) fn machine_image_reference(
    namespace_id: &ployz_core::ids::NamespaceId,
    service: &DeployServiceExecutionCommand,
    machine_id: &ployz_core::ids::MachineId,
    target_platform: &ployz_core::image::OciPlatform,
    dataplane_members: &[DataplaneMember],
) -> Result<ImageReference, DeployExecutionError> {
    match &service.service.image_source {
        ImageSource::Registry => Ok(service.service.image.clone()),
        ImageSource::PushedToSeed(receipt) => {
            let Some(platform_image) = receipt.platform(target_platform) else {
                return Err(DeployExecutionError::Image {
                    failure: Box::new(DeployOperationFailure::PlatformImageUnavailable {
                        service_id: service.service.service_id.clone(),
                        machine_id: machine_id.clone(),
                        target_platform: target_platform.clone(),
                    }),
                });
            };
            let Some(seed_host) = dataplane_members
                .iter()
                .find(|member| member.machine_id == platform_image.seed)
                .map(|member| member.endpoint_subnet.host_address())
            else {
                return Err(DeployExecutionError::InvalidImagePull {
                    message: format!(
                        "image seed {} has no dataplane membership",
                        platform_image.seed.as_str()
                    ),
                });
            };
            ImageReference::try_new(
                ImageEnsureSource::MeshSeed {
                    seed_host,
                    repository: ImageRepository::for_service(
                        namespace_id,
                        &service.service.service_id,
                    ),
                    manifest_digest: platform_image.manifest_digest.clone(),
                    image_id: platform_image.image_id.clone(),
                    platform: target_platform.clone(),
                }
                .reference(),
            )
            .map_err(|error| DeployExecutionError::InvalidImagePull {
                message: error.to_string(),
            })
        }
    }
}

fn deploy_failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated deploy failure message is non-empty")
}

#[cfg(test)]
mod tests;
