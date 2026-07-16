//! Pushed-image availability and mesh redistribution for deploy execution.

use std::collections::BTreeMap;

use ployz_core::deploy::{
    DeployCleanupAction, DeployPlan, DeployPlanStep, DeployServicePlan, ImageSource,
    VolumeDeclaredDeployRequest,
};
use ployz_core::image::{
    ImageEnsureRequest, ImageRemoveDomainError, ImageRepository, ImageRpcDomainError,
};
use ployz_core::network::DataplaneMember;
use ployz_core::operation::{
    DeployEvidence, DeployImageCleanup, DeployOperationFailure, FailureMessage,
};

use crate::control::role_client::machine::{
    MachineImageEnsureError, MachineImageRemoveError, MachineImageResolveError,
};
use crate::roles::machine::protocol::MachineContainerRemoveRpcRequest;
use crate::roles::machine::protocol::{MachineContainerResolveImageRpcRequest, MachineImagePull};

use super::deploy_plan;
use super::{
    DeployCleanupResult, DeployExecutionCommand, DeployExecutionError, DeployOperationRecorder,
    DeployServiceExecutionCommand, MachineContainerRuntime, MachineImageRemovalRuntime,
    cleanup_failure_message, record_evidence,
};

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
            let target_machines = service_plan
                .pre_start
                .iter()
                .map(|pre_start| &pre_start.machine_id)
                .chain(service_plan.steps.iter().map(|step| match step {
                    DeployPlanStep::UseExistingContainer { machine_id, .. }
                    | DeployPlanStep::RunContainer { machine_id, .. } => machine_id,
                }));
            for machine_id in target_machines {
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
    request: &mut VolumeDeclaredDeployRequest,
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    let provisional_plan = deploy_plan(command)?;
    let targets = request
        .services()
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
        let Some(machine_id) = provisional_plan
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
            .find(|plan| plan.service_id == service_id)
            .and_then(|plan| plan.steps.first())
            .map(|step| match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id.clone(),
            })
        else {
            return Err(DeployExecutionError::PlanInconsistent { service_id });
        };
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
            service_id: service.service.service_id.clone(),
            machine_id: machine_id.clone(),
            image: requested.clone(),
            message,
        }),
    }
}

pub(super) async fn ensure_images<R, N>(
    command: &DeployExecutionCommand,
    service_plans: &[DeployServicePlan],
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
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
        let mut target_platforms = BTreeMap::new();
        for step in &service_plan.steps {
            let machine_id = match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id,
            };
            let target_platform = command
                .target_platform(machine_id)
                .map_err(|error| error.into_execution_error())?;
            target_platforms
                .entry(target_platform.clone())
                .or_insert_with(|| machine_id.clone());
        }
        for (target_platform, machine_id) in target_platforms {
            let Some(platform_image) = receipt.platform(&target_platform) else {
                return Err(DeployExecutionError::Image {
                    failure: Box::new(DeployOperationFailure::PlatformImageUnavailable {
                        service_id: service.service.service_id.clone(),
                        machine_id,
                        target_platform,
                    }),
                });
            };
            let request = ImageEnsureRequest {
                repository: ImageRepository::for_service(
                    command.request.namespace_id(),
                    &service.service.service_id,
                ),
                manifest_digest: platform_image.manifest_digest.clone(),
                image_id: platform_image.image_id.clone(),
                platform: target_platform.clone(),
            };
            tokio::time::timeout(
                command.step_timeout(),
                machine_runtime.ensure_image(&platform_image.seed, request),
            )
            .await
            .map_err(|_| DeployExecutionError::Image {
                failure: Box::new(DeployOperationFailure::SeedUnavailable {
                    service_id: service.service.service_id.clone(),
                    seed: platform_image.seed.clone(),
                    message: deploy_failure_message("image seed ensure timed out"),
                }),
            })?
            .map_err(|error| {
                ensure_image_failure(
                    service,
                    &machine_id,
                    &target_platform,
                    platform_image,
                    error,
                )
            })?;
            record_evidence(
                command,
                recorder,
                DeployEvidence::ImageAvailabilityVerified {
                    service_id: service.service.service_id.clone(),
                    seed: platform_image.seed.clone(),
                    platform: target_platform,
                    manifest_digest: platform_image.manifest_digest.clone(),
                },
            )
            .await?;
        }
    }
    Ok(())
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

pub(super) fn machine_image_pull(
    namespace_id: &ployz_core::ids::NamespaceId,
    service: &DeployServiceExecutionCommand,
    machine_id: &ployz_core::ids::MachineId,
    target_platform: &ployz_core::image::OciPlatform,
    dataplane_members: &[DataplaneMember],
) -> Result<MachineImagePull, DeployExecutionError> {
    match &service.service.image_source {
        ImageSource::Registry => Ok(MachineImagePull::Registry {
            reference: service.service.image.clone(),
            credential: service.registry_credential().cloned(),
        }),
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
            Ok(MachineImagePull::MeshSeed {
                seed_host,
                repository: ImageRepository::for_service(namespace_id, &service.service.service_id),
                manifest_digest: platform_image.manifest_digest.clone(),
            })
        }
    }
}

fn deploy_failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated deploy failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployPhasePlan, DeployRequest, DeployServiceSpec, ImageReference,
        PlatformImage, PushedImageReceipt, ReplicaCount, ReplicaSlot, VolumeDeclaredDeployRequest,
    };
    use ployz_core::ids::{MachineId, NamespaceId, NamespaceRevisionId, OperationId, ServiceId};
    use ployz_core::image::{OciDigest, OciPlatform};

    #[test]
    fn pushed_receipt_without_target_platform_is_a_typed_failure() {
        let service = pushed_service();
        let target_machine = machine_id("machine_arm");
        let arm64 = platform("arm64");

        let error = machine_image_pull(
            &NamespaceId::try_new("default").expect("namespace id"),
            &service,
            &target_machine,
            &arm64,
            &[],
        )
        .expect_err("missing platform must fail");

        assert!(matches!(
            error,
            DeployExecutionError::Image { failure }
                if *failure == DeployOperationFailure::PlatformImageUnavailable {
                    service_id: ServiceId::try_new("api").expect("service id"),
                    machine_id: target_machine,
                    target_platform: arm64,
                }
        ));
    }

    #[test]
    fn pushed_platforms_are_validated_across_all_phases_before_execution() {
        let service = pushed_service();
        let target_machine = machine_id("machine_arm");
        let target_platform = platform("arm64");
        let request = VolumeDeclaredDeployRequest::try_new(DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![service.service.clone()],
        })
        .expect("deploy request");
        let command = DeployExecutionCommand {
            operation_id: OperationId::try_new("op_platform_validation").expect("operation id"),
            request,
            services: vec![service],
            route_binding_removals: Vec::new(),
            serving_target_removals: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            storage_testimony: BTreeMap::new(),
            machine_platforms: [(target_machine.clone(), target_platform.clone())]
                .into_iter()
                .collect(),
            dataplane_members: Vec::new(),
            exact_certificate_routes: Vec::new(),
            ployz_automatic_hostnames: false,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            unusable_machines: Vec::new(),
            step_timeout: std::time::Duration::from_secs(1),
        };
        let plan = DeployPlan {
            namespace_id: command.request.namespace_id().clone(),
            namespace_revision_id: NamespaceRevisionId::try_new("revision_platform_validation")
                .expect("revision id"),
            phases: vec![
                DeployPhasePlan {
                    services: Vec::new(),
                },
                DeployPhasePlan {
                    services: vec![DeployServicePlan {
                        service_id: ServiceId::try_new("api").expect("service id"),
                        steps: vec![DeployPlanStep::RunContainer {
                            machine_id: target_machine.clone(),
                            slot: ReplicaSlot::try_new(1).expect("replica slot"),
                        }],
                        pre_start: None,
                    }],
                },
            ],
            volume_pin_commits: Vec::new(),
            volume_ensures: Vec::new(),
            cleanup_actions: Vec::new(),
        };

        let error = validate_pushed_platforms(&command, &plan)
            .expect_err("later-phase missing platform must fail prevalidation");

        assert!(matches!(
            *error,
            DeployExecutionError::Image { failure }
                if *failure == DeployOperationFailure::PlatformImageUnavailable {
                    service_id: ServiceId::try_new("api").expect("service id"),
                    machine_id: target_machine,
                    target_platform,
                }
        ));
    }

    fn pushed_service() -> DeployServiceExecutionCommand {
        let digest = |value: char| {
            OciDigest::try_new(format!("sha256:{}", value.to_string().repeat(64)))
                .expect("OCI digest")
        };
        DeployServiceExecutionCommand {
            service: DeployServiceSpec {
                service_id: ServiceId::try_new("api").expect("service id"),
                image: ImageReference::try_new(format!("local/api@{}", digest('a')))
                    .expect("image reference"),
                image_source: ImageSource::PushedToSeed(
                    PushedImageReceipt::try_new([(
                        platform("amd64"),
                        PlatformImage {
                            seed: machine_id("machine_seed"),
                            manifest_digest: digest('b'),
                            image_id: digest('c'),
                        },
                    )])
                    .expect("pushed receipt"),
                ),
                replicas: ReplicaCount::try_new(1).expect("replica count"),
                keep: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            },
            registry_credential: None,
            route_commits: Vec::new(),
            volume_pins: Vec::new(),
            eligible_machines: Vec::new(),
            existing_replicas: Vec::new(),
            cleanup_candidates: Vec::new(),
        }
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("machine id")
    }

    fn platform(architecture: &str) -> OciPlatform {
        OciPlatform::try_new("linux", architecture).expect("platform")
    }
}
