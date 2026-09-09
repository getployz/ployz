//! Execute a captured remote plan in dependency order. A consumer gets the
//! completed dependency's immutable image served by its actual Build Machine.

use super::*;
use ployz_build::{
    Progress, Stage, TargetEvidence, WorkEvidence,
    remote::{Definition, Outcome},
};
use ployz_core::MachineId;
use tokio_util::sync::CancellationToken;

impl CapturedBuild {
    /// Execute this capture on one resolved Machine over the authenticated
    /// Ployz stream. No local Docker or local image store is consulted.
    pub async fn execute_remote(
        self,
        client: &crate::connect::Client,
        machine_id: ployz_core::MachineId,
        cancellation: tokio_util::sync::CancellationToken,
        progress: impl Fn(ployz_build::Progress),
    ) -> ployz_build::remote::Outcome {
        if self.options.output == Output::Load {
            let locations = self
                .targets
                .iter()
                .map(|target| (target.name.clone(), machine_id))
                .collect();
            return match self
                .execute_remote_steps(client, &locations, cancellation, progress)
                .await
            {
                Ok(images) => ployz_build::remote::Outcome::Images {
                    machine_id,
                    images: images.into_iter().map(|service| service.built).collect(),
                },
                Err(outcome) => outcome,
            };
        }
        let definition = ployz_build::remote::Definition {
            retained_tags: Vec::new(),
            image_contexts: Default::default(),
            targets: self.targets,
            output: self.options.output,
            no_cache: self.options.no_cache,
            pull: self.options.pull,
        };
        super::super::remote_build::execute(
            self.inputs,
            definition,
            client,
            machine_id,
            cancellation,
            progress,
        )
        .await
        .into_outcome()
    }

    /// Execute remotely and bind each completed output to its captured Service.
    /// # Errors
    /// Failed, unknown, or image-less outcomes cannot be used by Deploy.
    pub async fn execute_remote_images(
        self,
        client: &crate::connect::Client,
        machine_id: ployz_core::MachineId,
        cancellation: tokio_util::sync::CancellationToken,
        progress: impl Fn(ployz_build::Progress),
    ) -> Result<Vec<BuiltService>, ComposeError> {
        let locations = self
            .targets
            .iter()
            .map(|target| (target.name.clone(), machine_id))
            .collect();
        self.execute_on_machines(client, &locations, cancellation, progress)
            .await
    }

    /// Build on already-resolved Machines, independently of application placement.
    /// Every target must have a location before any source is submitted.
    /// Keys are raw Compose Build names, not DNS-label Deploy Service names.
    /// # Errors
    /// Reports failed/unknown work and leaves dependent/remaining targets unattempted.
    pub async fn execute_on_machines(
        self,
        client: &crate::connect::Client,
        locations: &BTreeMap<String, MachineId>,
        cancellation: CancellationToken,
        progress: impl Fn(Progress),
    ) -> Result<Vec<BuiltService>, ComposeError> {
        self.execute_remote_steps(client, locations, cancellation, progress)
            .await
            .map_err(remote_error)
    }

    pub(super) async fn execute_remote_steps(
        self,
        client: &crate::connect::Client,
        locations: &BTreeMap<String, MachineId>,
        cancellation: CancellationToken,
        progress: impl Fn(Progress),
    ) -> Result<Vec<BuiltService>, Outcome> {
        let mut work = WorkEvidence::new(&self.targets);
        let failed = |stage, message| Outcome::Failed {
            stage,
            message,
            work: Default::default(),
        };
        if self.options.output != Output::Load {
            return Err(failed(
                Stage::Preparation,
                "per-Machine execution requires loaded image results".into(),
            )
            .with_work(work));
        }
        if self
            .targets
            .iter()
            .any(|target| !locations.contains_key(&target.name))
        {
            return Err(failed(
                Stage::Admission,
                "every Build must have a selected Machine before execution".into(),
            )
            .with_work(work));
        }
        let mut completed: Vec<BuiltService> = Vec::new();
        for (service, target) in self.plan.iter().zip(&self.targets) {
            if cancellation.is_cancelled() {
                return Err(failed(
                    Stage::Admission,
                    "Build cancelled before the next target was submitted".into(),
                )
                .with_work(work));
            }
            let machine_id = *locations.get(&target.name).expect("all locations checked");
            let build = super::BuildSpec {
                raw: service.build.clone(),
            };
            let mut contexts = BTreeMap::new();
            for dependency in build.additional_services() {
                let Some(image) = completed.iter().find(|image| image.name == dependency) else {
                    return Err(failed(
                        Stage::Preparation,
                        format!("Service context {dependency} has no completed Build output"),
                    )
                    .with_work(work));
                };
                let BuildLocation::Machine(source) = image.location else {
                    unreachable!("remote steps return Machine images")
                };
                let serving = crate::image::serve_build_image(
                    &mut client.clone(),
                    &image.built,
                    source,
                    &cancellation,
                )
                .await;
                let source = serving.map_err(|error| {
                    failed(
                        Stage::Preparation,
                        format!("Service context {dependency}: {error}"),
                    )
                    .with_work(work.clone())
                })?;
                contexts.insert(
                    dependency.to_owned(),
                    ployz_build::ImageContext {
                        reference: image.built.repository_reference().map_err(|error| {
                            failed(Stage::Preparation, error.to_string()).with_work(work.clone())
                        })?,
                        platforms: image.built.platforms.clone(),
                        source,
                    },
                );
            }
            let inputs = self.inputs.for_service(&target.name).map_err(|error| {
                failed(Stage::Preparation, error.to_string()).with_work(work.clone())
            })?;
            let outcome = super::super::remote_build::execute(
                inputs,
                Definition {
                    retained_tags: self
                        .retained_tags
                        .get(&target.name)
                        .cloned()
                        .into_iter()
                        .collect(),
                    targets: vec![target.clone()],
                    image_contexts: contexts,
                    output: Output::Load,
                    no_cache: self.options.no_cache,
                    pull: self.options.pull,
                },
                client,
                machine_id,
                cancellation.clone(),
                &progress,
            )
            .await;
            match outcome {
                super::super::remote_build::Completion::Images { images, stream, .. } => {
                    let image = images
                        .into_iter()
                        .next()
                        .expect("remote adapter validated the count");
                    work.0
                        .insert(target.name.clone(), TargetEvidence::Image(image.clone()));
                    completed.push(BuiltService {
                        name: service.name.clone(),
                        image: service.image.clone(),
                        machines: service.machines.clone(),
                        location: BuildLocation::Machine(machine_id),
                        built: image,
                        _retention: Some(BuildRetention::Remote { _stream: stream }),
                    });
                }
                super::super::remote_build::Completion::Report(
                    outcome @ (Outcome::Failed { .. } | Outcome::Unknown { .. }),
                ) => {
                    if let Outcome::Failed { work: observed, .. }
                    | Outcome::Unknown { work: observed, .. } = &outcome
                        && let Some(evidence) = observed.0.get(&target.name)
                    {
                        work.0.insert(target.name.clone(), evidence.clone());
                    }
                    return Err(outcome.with_work(work));
                }
                super::super::remote_build::Completion::Report(
                    Outcome::Validated { .. } | Outcome::Published { .. } | Outcome::Images { .. },
                ) => {
                    unreachable!("adapter validated output disposition")
                }
            }
        }
        Ok(completed)
    }
}

pub(super) fn remote_error(outcome: Outcome) -> ComposeError {
    match outcome {
        Outcome::Failed {
            stage,
            message,
            work,
        } => invalid_build(&format!(
            "Build failed during {stage:?}: {message}; target evidence: {work:?}"
        )),
        Outcome::Unknown {
            stage,
            message,
            work,
        } => invalid_build(&format!(
            "Build outcome unknown during {stage:?}: {message}; target evidence: {work:?}"
        )),
        Outcome::Images { .. } | Outcome::Validated { .. } | Outcome::Published { .. } => {
            invalid_build("Build produced no image available for Direct Image Transfer")
        }
    }
}
