//! Image acquisition planning and phase-local coordination.

use super::*;

pub(in crate::control::operations::deploy) async fn ensure_images<R, N>(
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
    let mut seed_jobs = Vec::<ImageEnsureJob>::new();
    let mut target_jobs = Vec::<ImageEnsureJob>::new();
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
                    platform: target_platform.clone(),
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
                    let seed_index = seed_jobs
                        .iter()
                        .position(|job| {
                            job.machine_id == platform_image.seed
                                && job.service.service.service_id == service.service.service_id
                                && job.source == seed_source
                        })
                        .unwrap_or_else(|| {
                            seed_jobs.push(ImageEnsureJob {
                                machine_id: platform_image.seed.clone(),
                                owner: owner.clone(),
                                source: seed_source,
                                service: service.clone(),
                                target_machine: machine_id.clone(),
                                platform: target_platform.clone(),
                                seed: Some(platform_image.clone()),
                                evidence_targets: Vec::new(),
                            });
                            seed_jobs.len() - 1
                        });
                    if machine_id == platform_image.seed {
                        seed_jobs
                            .get_mut(seed_index)
                            .expect("seed job index was just found or inserted")
                            .evidence_targets
                            .push(machine_id.clone());
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
            push_unique_target_job(
                &mut target_jobs,
                ImageEnsureJob {
                    machine_id: machine_id.clone(),
                    owner,
                    source,
                    service: service.clone(),
                    target_machine: machine_id.clone(),
                    platform: target_platform.clone(),
                    seed: None,
                    evidence_targets: vec![machine_id],
                },
            );
        }
    }
    let seed_references = run_image_ensure_wave(machine_runtime, &seed_jobs)
        .await
        .map_err(|(index, error)| {
            let job = seed_jobs
                .get(index)
                .expect("image ensure failure index belongs to the seed wave");
            ensure_seed_drive_failure(
                &job.service,
                &job.target_machine,
                &job.platform,
                job.seed.as_ref().expect("seed job has platform image"),
                error,
            )
        })?;
    for (job, reference) in seed_jobs.iter().zip(seed_references) {
        for target in &job.evidence_targets {
            record_target_image_evidence(
                command,
                recorder,
                &job.service,
                target,
                job.platform.clone(),
                reference.clone(),
            )
            .await?;
        }
    }
    let target_references = run_image_ensure_wave(machine_runtime, &target_jobs)
        .await
        .map_err(|(index, error)| {
            let job = target_jobs
                .get(index)
                .expect("image ensure failure index belongs to the target wave");
            target_ensure_failure(command, &job.service, &job.target_machine, error)
        })?;
    for (job, reference) in target_jobs.iter().zip(target_references) {
        for target in &job.evidence_targets {
            record_target_image_evidence(
                command,
                recorder,
                &job.service,
                target,
                job.platform.clone(),
                reference.clone(),
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) struct ImageEnsureJob {
    pub(super) machine_id: MachineId,
    pub(super) owner: ManagedContainerIdentity,
    pub(super) source: ImageEnsureSource,
    pub(super) service: DeployServiceExecutionCommand,
    pub(super) target_machine: MachineId,
    pub(super) platform: ployz_core::image::OciPlatform,
    pub(super) seed: Option<ployz_core::deploy::PlatformImage>,
    pub(super) evidence_targets: Vec<MachineId>,
}

pub(super) fn push_unique_target_job(jobs: &mut Vec<ImageEnsureJob>, candidate: ImageEnsureJob) {
    if !jobs.iter().any(|job| {
        job.machine_id == candidate.machine_id
            && job.owner == candidate.owner
            && job.source == candidate.source
    }) {
        jobs.push(candidate);
    }
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

#[derive(Debug)]
pub(super) enum ImageEnsureDriveError {
    Call(MachineImageEnsureError),
    Failed(ImageEnsureFailure),
    Cancelled,
}

pub(super) async fn run_image_ensure_wave<N: MachineContainerRuntime>(
    machine_runtime: &mut N,
    jobs: &[ImageEnsureJob],
) -> Result<Vec<ImageReference>, (usize, ImageEnsureDriveError)> {
    let runtime = machine_runtime.image_ensure_runtime();
    run_image_ensure_wave_with_runtime(runtime, jobs).await
}

pub(super) async fn run_image_ensure_wave_with_runtime<N: MachineImageEnsureRuntime>(
    runtime: N,
    jobs: &[ImageEnsureJob],
) -> Result<Vec<ImageReference>, (usize, ImageEnsureDriveError)> {
    let start_outcomes = futures_util::future::join_all(jobs.iter().map(|job| {
        let runtime = runtime.clone();
        let machine_id = job.machine_id.clone();
        let request = ImageEnsureRequest::Start {
            owner: job.owner.clone(),
            source: job.source.clone(),
        };
        async move { call_image_ensure(&runtime, &machine_id, request).await }
    }))
    .await;
    if let Some((index, error)) = start_outcomes
        .iter()
        .enumerate()
        .find_map(|(index, outcome)| outcome.as_ref().err().map(|error| (index, error.clone())))
    {
        let statuses = start_outcomes
            .into_iter()
            .map(|outcome| outcome.ok().map(|ok| ok.ensure_status))
            .collect::<Vec<_>>();
        cancel_unfinished_jobs(&runtime, jobs, &statuses).await;
        return Err((index, ImageEnsureDriveError::Call(error)));
    }
    let mut statuses = start_outcomes
        .into_iter()
        .map(|outcome| outcome.expect("all starts succeeded").ensure_status)
        .collect::<Vec<_>>();
    loop {
        for (index, status) in statuses.iter().enumerate() {
            if let Some(error) = image_ensure_status_error(status) {
                let statuses = statuses.into_iter().map(Some).collect::<Vec<_>>();
                cancel_unfinished_jobs(&runtime, jobs, &statuses).await;
                return Err((index, error));
            }
        }
        let mut polls = FuturesUnordered::new();
        for (index, job) in jobs.iter().enumerate() {
            if !matches!(
                statuses
                    .get(index)
                    .expect("each image ensure job has a status"),
                ImageEnsureStatus::Completed { .. }
            ) {
                let runtime = runtime.clone();
                let machine_id = job.machine_id.clone();
                let request = ImageEnsureRequest::Status {
                    owner: job.owner.clone(),
                };
                polls.push(async move {
                    (
                        index,
                        call_image_ensure(&runtime, &machine_id, request).await,
                    )
                });
            }
        }
        while let Some((index, outcome)) = polls.next().await {
            let status = statuses
                .get_mut(index)
                .expect("poll result index belongs to the image ensure wave");
            *status = match outcome {
                Ok(status) => status.ensure_status,
                Err(error) => {
                    drop(polls);
                    let statuses = statuses.into_iter().map(Some).collect::<Vec<_>>();
                    cancel_unfinished_jobs(&runtime, jobs, &statuses).await;
                    return Err((index, ImageEnsureDriveError::Call(error)));
                }
            };
            if let Some(error) = image_ensure_status_error(status) {
                drop(polls);
                let statuses = statuses.into_iter().map(Some).collect::<Vec<_>>();
                cancel_unfinished_jobs(&runtime, jobs, &statuses).await;
                return Err((index, error));
            }
        }
        if statuses
            .iter()
            .all(|status| matches!(status, ImageEnsureStatus::Completed { .. }))
        {
            return Ok(statuses
                .into_iter()
                .map(|status| match status {
                    ImageEnsureStatus::Completed { reference } => reference,
                    ImageEnsureStatus::Accepted
                    | ImageEnsureStatus::Running { .. }
                    | ImageEnsureStatus::Failed { .. }
                    | ImageEnsureStatus::Cancelled => {
                        unreachable!("all image ensures completed")
                    }
                })
                .collect());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn image_ensure_status_error(status: &ImageEnsureStatus) -> Option<ImageEnsureDriveError> {
    match status {
        ImageEnsureStatus::Failed { failure } => {
            Some(ImageEnsureDriveError::Failed(failure.clone()))
        }
        ImageEnsureStatus::Cancelled => Some(ImageEnsureDriveError::Cancelled),
        ImageEnsureStatus::Accepted
        | ImageEnsureStatus::Running { .. }
        | ImageEnsureStatus::Completed { .. } => None,
    }
}

async fn cancel_unfinished_jobs<N: MachineImageEnsureRuntime>(
    runtime: &N,
    jobs: &[ImageEnsureJob],
    statuses: &[Option<ImageEnsureStatus>],
) {
    futures_util::future::join_all(
        jobs.iter()
            .zip(statuses)
            .filter(|(_, status)| {
                !matches!(
                    status,
                    Some(
                        ImageEnsureStatus::Completed { .. }
                            | ImageEnsureStatus::Failed { .. }
                            | ImageEnsureStatus::Cancelled
                    )
                )
            })
            .map(|(job, _)| {
                let runtime = runtime.clone();
                let machine_id = job.machine_id.clone();
                let request = ImageEnsureRequest::Cancel {
                    owner: job.owner.clone(),
                };
                async move {
                    let _ = runtime.ensure_image(&machine_id, request).await;
                }
            }),
    )
    .await;
}

async fn call_image_ensure<N: MachineImageEnsureRuntime>(
    runtime: &N,
    machine_id: &MachineId,
    request: ImageEnsureRequest,
) -> Result<ployz_core::image::ImageEnsureOk, MachineImageEnsureError> {
    let mut last = None;
    for _ in 0..3 {
        match runtime.ensure_image(machine_id, request.clone()).await {
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
