//! Shared BuildKit execution for one captured Build.
//!
//! A caller captures a Build's inputs into a private directory holding a
//! Compose file and the sources it points at, then executes that capture
//! here: execute, verify the image, clean up. Builder lifecycle, subprocess
//! orchestration, image import, and cleanup stay private so no caller has to
//! restate them.
//!
//! BuildKit owns build options and their validation; this crate hands it the
//! captured Compose file and adds only the rules BuildKit cannot enforce: one
//! bounded attempt, and one image bound to the content that attempt produced.

mod builder;
mod execution;
mod received_recipe;
pub mod remote;
mod upload;

pub use execution::{
    Admission, Cancellation, HostPolicy, Progress, Stage, TargetEvidence, WorkEvidence,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use builder::Builder;

/// Pinned BuildKit release. Every Ployz Build runs this version.
pub const BUILDKIT_IMAGE: &str = "moby/buildkit:v0.26.2";

/// Longest one Build Attempt may run, from owning the builder through image
/// verification. Waiting for another local build is queueing, not attempt time.
pub const EXECUTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Budget for releasing an attempt's resources once it is over.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(60);

/// The Ployz builder for this user. Its cache volume outlives the container.
///
/// One builder per user keeps two people sharing a Docker daemon from
/// removing each other's build container.
#[must_use]
pub fn builder_name() -> String {
    format!("ployz-{}", rustix::process::getuid().as_raw())
}

/// One Build's captured inputs, ready to execute.
///
/// Paths point into the caller's private capture; nothing here is read from
/// the original sources again.
pub struct Request<'a> {
    /// Captured Compose file describing every target of this Build.
    pub compose_file: &'a Path,
    /// Private directory the build runs in, so no stray file can join it.
    pub working_dir: &'a Path,
    /// Values captured for this command. Later shell state is never consulted.
    pub environment: &'a BTreeMap<String, String>,
    /// Docker CLI to execute, defaulting to `docker` on the search path.
    pub docker: Option<&'a Path>,
    /// Images to build, named as the captured Compose file names them.
    pub targets: &'a [Target],
    /// Effective `KEY=VALUE` build-argument overrides for every target.
    pub build_args: &'a [String],
    /// What this attempt does with what it builds.
    pub output: Output,
    /// Build without reusing retained cache.
    pub no_cache: bool,
    /// Always attempt to pull a newer version of a referenced image.
    pub pull: bool,
}

/// One image to produce, named by the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Target {
    /// Caller's name for this image, as the captured Compose file names it.
    pub name: String,
    /// Platform the capture asks for, which the completed image must carry.
    /// `None` accepts whichever platform the builder produces.
    pub platform: Option<String>,
}

/// What an attempt does with its result. These outcomes are exclusive: an
/// attempt cannot both validate and produce an image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Output {
    /// Load completed images into the execution host's Docker image store.
    #[default]
    Load,
    /// Publish to the registry each tag names, retaining no local image.
    Registry,
    /// Validate the recipe without producing an image.
    Validate,
}

/// A completed image in the execution host's Docker image store, identified by
/// the content this attempt observed there.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuiltImage {
    /// Digest-pinned reference. Binds later use to this attempt's content, so
    /// a later Build moving a shared tag cannot substitute its image.
    pub reference: String,
    /// Tags this attempt applied, as the execution host recorded them.
    pub tags: Vec<String>,
    /// The platform actually present, not the one requested.
    pub platform: String,
}

/// Why a Build Attempt did not produce the image it was asked for.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BuildError {
    /// The first phase proven to have failed, retained through cleanup.
    #[error("{source}")]
    AtStage {
        stage: Stage,
        source: Box<BuildError>,
    },
    /// Another attempt owns the retained builder state.
    #[error("the Machine builder is busy; retry after the active Build finishes")]
    Busy,
    /// Cancellation was requested and termination was confirmed.
    #[error("the Build was cancelled and terminated")]
    Cancelled,
    /// The execution host cannot run Builds at all.
    #[error("{0}")]
    Prerequisite(String),
    /// The request itself cannot be executed as given.
    #[error("{0}")]
    Request(String),
    /// One Docker command failed. BuildKit's own diagnosis is the diagnostic.
    #[error("{action} failed: {diagnostic}")]
    Docker {
        action: &'static str,
        diagnostic: String,
    },
    /// The attempt exceeded its bounded execution time and was terminated.
    #[error("the build exceeded its {0}s execution timeout and was terminated")]
    TimedOut(u64),
    /// The attempt stopped, but its termination could not be observed.
    #[error("the build was terminated but its builder did not stop: {0}")]
    UncertainTermination(String),
    /// The build finished, but its result is not the image it claims.
    #[error("{0}")]
    Result(String),
}

impl BuildError {
    #[must_use]
    /// Earliest phase proven to have failed.
    pub fn stage(&self) -> Stage {
        match self {
            Self::AtStage { stage, .. } => *stage,
            Self::Busy => Stage::Admission,
            Self::Prerequisite(_) | Self::Request(_) => Stage::Preparation,
            Self::Result(_) => Stage::Output,
            Self::UncertainTermination(_) => Stage::Cleanup,
            Self::Cancelled | Self::TimedOut(_) | Self::Docker { .. } => Stage::Building,
        }
    }
    #[must_use]
    /// Whether any owned process has unconfirmed termination.
    pub fn is_unknown(&self) -> bool {
        match self {
            Self::AtStage { source, .. } => source.is_unknown(),
            Self::UncertainTermination(_) => true,
            Self::Busy
            | Self::Cancelled
            | Self::Prerequisite(_)
            | Self::Request(_)
            | Self::Docker { .. }
            | Self::TimedOut(_)
            | Self::Result(_) => false,
        }
    }
    fn with_later_failure(self, later: Self) -> Self {
        if later.is_unknown() && !self.is_unknown() {
            Self::UncertainTermination(format!("{self}; {later}")).at(self.stage())
        } else {
            self
        }
    }
    fn at(self, stage: Stage) -> Self {
        Self::AtStage {
            stage,
            source: Box::new(self),
        }
    }
}

/// Execute one captured Build against local Docker.
///
/// Requires Docker with Buildx and the containerd image store. It does not
/// require a Ployz daemon. Every Docker phase of the attempt runs inside
/// [`EXECUTION_TIMEOUT`], with a separate bounded budget for cleanup.
///
/// Returns the completed images in the order of `targets`. Validation and
/// registry publication retain no local image, so they return none.
///
/// # Errors
/// Returns a prerequisite error when Docker cannot serve the Build, a request
/// error when the targets cannot be told apart, the failing command's own
/// diagnosis, a timeout, an uncertain outcome when termination could not be
/// observed, or a result error when the completed image is not the content it
/// claims.
pub fn execute(request: &Request<'_>) -> Result<Vec<BuiltImage>, BuildError> {
    execute_admitted(request, Admission::wait()?, &|event| {
        if let Progress::Output(bytes) = event {
            use std::io::Write as _;
            let _ = std::io::stderr().write_all(&bytes);
        }
    })
}

/// Execute using admission acquired before source upload. The same deadline
/// and exclusive builder ownership cover upload, execution, and cleanup.
///
/// # Errors
/// Returns the earliest proven failure, or uncertain termination when owned
/// resources cannot be confirmed stopped. Uncertainty blocks competing work.
pub fn execute_admitted(
    request: &Request<'_>,
    admission: Admission,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Vec<BuiltImage>, BuildError> {
    let planned = plan(request.targets)?;
    if planned.is_empty() {
        return Ok(Vec::new());
    }
    admission.check()?;
    let docker = Docker {
        program: request.docker.unwrap_or_else(|| Path::new("docker")),
        environment: request.environment,
        working_dir: request.working_dir,
        deadline: admission.deadline,
        cancellation: Some(&admission.cancellation),
        progress: Some(progress),
    };
    let metadata = request.working_dir.join("build-metadata.json");
    progress(Progress::Stage(Stage::Preparation));
    docker
        .run("check Buildx", &["buildx", "version"], Streams::Captured)
        .map_err(|error| error.at(Stage::Preparation))?;
    let builder =
        Builder::acquire(&docker, admission.lock).map_err(|error| error.at(Stage::Preparation))?;
    let result = (|| {
        let native = builder
            .native_platform(request.targets)
            .map_err(|error| error.at(Stage::Preparation))?;
        // A successful per-target push is publication evidence. A failed batch
        // cannot tell us which of its registry exports completed.
        let batch_size = if request.output == Output::Registry {
            1
        } else {
            planned.len()
        };
        for batch in planned.chunks(batch_size) {
            let mut arguments = bake_arguments(request, batch, &metadata);
            for target in &planned {
                arguments.push("--set".into());
                arguments.push(format!(
                    "{}.platform={}",
                    target.bake,
                    target.target.platform.as_deref().unwrap_or(&native)
                ));
            }
            progress(Progress::Stage(Stage::Building));
            if let Err(error) = builder.run(&arguments, || {
                for target in batch {
                    progress(Progress::Target {
                        name: target.target.name.clone(),
                        outcome: TargetEvidence::Unknown,
                    });
                }
            }) {
                // Bake may have imported a prefix before a later target failed.
                // Only verify after termination is known; keep the original failure.
                if request.output == Output::Load
                    && !error.is_unknown()
                    && metadata.is_file()
                    && let Err(verification) =
                        built_images(&docker.releasing(), &metadata, batch, progress)
                {
                    return Err(error.at(Stage::Building).with_later_failure(verification));
                }
                return Err(error.at(Stage::Building));
            }
            progress(Progress::Stage(Stage::Output));
            match request.output {
                Output::Load => {
                    return built_images(&docker, &metadata, batch, progress)
                        .map_err(|error| error.at(Stage::Output));
                }
                Output::Registry | Output::Validate => {
                    for target in batch {
                        progress(Progress::Target {
                            name: target.target.name.clone(),
                            outcome: if request.output == Output::Validate {
                                TargetEvidence::Validated
                            } else {
                                TargetEvidence::Published
                            },
                        });
                    }
                }
            }
        }
        Ok(Vec::new())
    })();
    progress(Progress::Stage(Stage::Cleanup));
    builder.finish(result)
}

/// One target with the Buildx name it will carry, derived once.
#[derive(Debug)]
struct Planned<'a> {
    target: &'a Target,
    bake: String,
}

/// Name each target for Buildx, refusing names that would share one result.
fn plan(targets: &[Target]) -> Result<Vec<Planned<'_>>, BuildError> {
    let mut names = BTreeSet::new();
    targets
        .iter()
        .map(|target| {
            // Compose names may contain a dot; Buildx target names may not.
            let bake = target.name.replace('.', "_");
            if names.insert(bake.clone()) {
                Ok(Planned { target, bake })
            } else {
                Err(BuildError::Request(format!(
                    "'{}' and an earlier target share the build name '{bake}', so their results cannot be told apart",
                    target.name
                )))
            }
        })
        .collect()
}

fn bake_arguments(request: &Request<'_>, planned: &[Planned<'_>], metadata: &Path) -> Vec<String> {
    let mut arguments = vec![
        "buildx".to_owned(),
        "bake".to_owned(),
        "--builder".to_owned(),
        builder_name(),
        "--file".to_owned(),
        request.compose_file.to_string_lossy().into_owned(),
    ];
    match request.output {
        // Check runs the frontend only; an image would contradict the result.
        Output::Validate => arguments.push("--check".to_owned()),
        // Publication leaves the image with its registry, so there is no local
        // result to bind and no metadata to read.
        Output::Registry => arguments.push("--push".to_owned()),
        Output::Load => {
            arguments.push("--load".to_owned());
            arguments.push("--metadata-file".to_owned());
            arguments.push(metadata.to_string_lossy().into_owned());
        }
    }
    if request.no_cache {
        arguments.push("--no-cache".to_owned());
    }
    if request.pull {
        arguments.push("--pull".to_owned());
    }
    // Platforms travel in the captured Compose file, which upstream reads.
    for argument in request.build_args {
        arguments.push("--set".to_owned());
        arguments.push(format!("*.args.{argument}"));
    }
    arguments.extend(planned.iter().map(|planned| planned.bake.clone()));
    arguments
}

fn built_images(
    docker: &Docker<'_>,
    metadata: &Path,
    planned: &[Planned<'_>],
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Vec<BuiltImage>, BuildError> {
    let content = std::fs::read(metadata)
        .map_err(|error| BuildError::Result(format!("read the build result: {error}")))?;
    let results: BTreeMap<String, serde_json::Value> = serde_json::from_slice(&content)
        .map_err(|error| BuildError::Result(format!("parse the build result: {error}")))?;
    let mut first_error: Option<BuildError> = None;
    let images = planned
        .iter()
        .map(|planned| {
            let name = &planned.target.name;
            let result = results
                .get(&planned.bake)
                .ok_or_else(|| {
                    BuildError::Result(format!("the build reported no result for '{name}'"))
                })
                .and_then(|value| {
                    TargetMetadata::deserialize(value).map_err(|error| {
                        BuildError::Result(format!(
                            "the build result for '{name}' is incomplete: {error}"
                        ))
                    })
                })?;
            let tags = result
                .name
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let repository = tags
                .first()
                .map(|tag| repository(tag))
                .ok_or_else(|| {
                    BuildError::Result(format!("the build tagged no image for '{name}'"))
                })?
                .to_owned();
            let reference = format!("{repository}@{}", result.digest);
            let platform = verify(
                docker,
                &reference,
                &result.digest,
                planned.target.platform.as_deref(),
            )?;
            let image = BuiltImage {
                reference,
                tags,
                platform,
            };
            progress(Progress::Target {
                name: name.clone(),
                outcome: TargetEvidence::Image(image.clone()),
            });
            Ok(image)
        })
        .filter_map(|result| match result {
            Ok(image) => Some(image),
            Err(error) => {
                first_error = Some(match first_error.take() {
                    Some(first) => first.with_later_failure(error),
                    None => error,
                });
                None
            }
        })
        .collect();
    first_error.map_or(Ok(images), Err)
}

/// Confirm the execution host holds exactly the content this attempt claims,
/// and report the platform it actually holds.
fn verify(
    docker: &Docker<'_>,
    reference: &str,
    digest: &str,
    requested: Option<&str>,
) -> Result<String, BuildError> {
    let inspected = docker
        .run(
            "inspect the completed image",
            &["image", "inspect", reference, "--format", "{{json .}}"],
            Streams::Captured,
        )
        .map_err(|error| match error {
            BuildError::Cancelled | BuildError::TimedOut(_) | BuildError::UncertainTermination(_) => error,
            error @ (BuildError::AtStage { .. } | BuildError::Busy | BuildError::Prerequisite(_) | BuildError::Request(_) | BuildError::Docker { .. } | BuildError::Result(_)) => BuildError::Result(format!(
                "the completed image {reference} is not in the local image store, which Ployz Builds require Docker's containerd image store to provide: {error}"
            ))
        })?;
    let image: ImageInspection = serde_json::from_str(&inspected)
        .map_err(|error| BuildError::Result(format!("read the completed image: {error}")))?;
    // The descriptor names the content the store holds under this reference.
    // Docker reports one only with the containerd image store; without it an
    // image is identified by its configuration and cannot be bound to a build.
    let Some(descriptor) = image.descriptor else {
        return Err(BuildError::Result(format!(
            "the local image store reports no content descriptor for {reference}; Ployz Builds require Docker's containerd image store"
        )));
    };
    if descriptor.digest != digest {
        return Err(BuildError::Result(format!(
            "the local image store holds {} for {reference} rather than the completed content",
            descriptor.digest
        )));
    }
    // Several platforms arrive as an index whatever asked for them, and one
    // Build produces one image. Refuse the content rather than the request.
    if descriptor.media_type.contains("index") {
        return Err(BuildError::Result(format!(
            "{reference} contains several platforms; a Dockerfile Build produces one image for one platform"
        )));
    }
    let variant = image.variant.unwrap_or_default();
    let observed = if variant.is_empty() {
        format!("{}/{}", image.os, image.architecture)
    } else {
        format!("{}/{}/{variant}", image.os, image.architecture)
    };
    match requested {
        Some(requested) if !covers(&observed, requested) => Err(BuildError::Result(format!(
            "{reference} contains {observed}, not the requested {requested}"
        ))),
        Some(_) | None => Ok(observed),
    }
}

/// Whether an observed platform satisfies a request, ignoring an unstated
/// variant: `linux/arm64` asks for whichever variant the host builds.
fn covers(observed: &str, requested: &str) -> bool {
    observed == requested
        || observed
            .strip_prefix(requested)
            .or_else(|| requested.strip_prefix(observed))
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The repository part of a tag, keeping a registry port intact.
fn repository(tag: &str) -> &str {
    let start = tag.rfind('/').map_or(0, |slash| slash + 1);
    match tag.get(start..).and_then(|name| name.find([':', '@'])) {
        Some(offset) => tag.get(..start + offset).unwrap_or(tag),
        None => tag,
    }
}

#[derive(Deserialize)]
struct TargetMetadata {
    #[serde(rename = "containerimage.digest")]
    digest: String,
    #[serde(rename = "image.name")]
    name: String,
}

#[derive(Deserialize)]
struct ImageInspection {
    #[serde(rename = "Os")]
    os: String,
    #[serde(rename = "Architecture")]
    architecture: String,
    #[serde(rename = "Variant")]
    variant: Option<String>,
    #[serde(rename = "Descriptor")]
    descriptor: Option<Descriptor>,
}

#[derive(Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
}

/// When this attempt must be over, so no Docker command can outlast it.
#[derive(Clone, Copy)]
pub(crate) struct Deadline {
    expires: Instant,
    budget: Duration,
}

impl Deadline {
    fn starting_now(budget: Duration) -> Self {
        Self {
            expires: Instant::now() + budget,
            budget,
        }
    }

    fn remaining(self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }
}

/// Whether a command's output belongs to the operator or to this crate.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Streams {
    /// Build progress the operator watches.
    Inherited,
    /// Evidence this crate reads.
    Captured,
}

/// The Docker CLI this attempt drives, with the values captured for it.
///
/// Every command runs inside the attempt's deadline, so no phase of a bounded
/// Build can wait forever on Docker.
pub(crate) struct Docker<'a> {
    program: &'a Path,
    environment: &'a BTreeMap<String, String>,
    working_dir: &'a Path,
    deadline: Deadline,
    cancellation: Option<&'a Cancellation>,
    progress: Option<&'a (dyn Fn(Progress) + Sync)>,
}

impl<'a> Docker<'a> {
    /// The same Docker with a fresh budget for releasing resources, so
    /// cleanup still runs, bounded, after the attempt's deadline passes.
    pub(crate) fn releasing(&self) -> Docker<'a> {
        Docker {
            program: self.program,
            environment: self.environment,
            working_dir: self.working_dir,
            // Normal cleanup shares the active deadline. After expiry,
            // termination gets a separate bounded grace period.
            deadline: Deadline::starting_now(if self.deadline.remaining().is_zero() {
                CLEANUP_TIMEOUT
            } else {
                self.deadline.remaining().min(CLEANUP_TIMEOUT)
            }),
            cancellation: None,
            progress: self.progress,
        }
    }

    /// Run one Docker command to completion within the deadline.
    ///
    /// # Errors
    /// Returns the command's own diagnosis, or a timeout after terminating it.
    pub(crate) fn run(
        &self,
        action: &'static str,
        arguments: &[&str],
        streams: Streams,
    ) -> Result<String, BuildError> {
        self.run_started(action, arguments, streams, || {})
    }

    fn run_started(
        &self,
        action: &'static str,
        arguments: &[&str],
        streams: Streams,
        started: impl FnOnce(),
    ) -> Result<String, BuildError> {
        if self.cancellation.is_some_and(Cancellation::is_cancelled) {
            return Err(BuildError::Cancelled);
        }
        if self.deadline.remaining().is_zero() {
            return Err(BuildError::TimedOut(EXECUTION_TIMEOUT.as_secs()));
        }
        let mut command = Command::new(self.program);
        command
            .env_clear()
            .envs(self.environment)
            .current_dir(self.working_dir)
            .args(arguments)
            .stdin(Stdio::null());
        // Captured output goes to files rather than pipes: nothing reads a
        // pipe until the child exits, so a chatty command would fill one and
        // block until its deadline.
        let output = self.working_dir.join("docker-output");
        let diagnosis = self.working_dir.join("docker-diagnosis");
        if streams == Streams::Captured {
            command
                .stdout(self.create(action, &output)?)
                .stderr(self.create(action, &diagnosis)?);
        }
        let progress_file = self.working_dir.join("docker-progress");
        let mut progress_reader = if streams == Streams::Inherited && self.progress.is_some() {
            let file = self.create(action, &progress_file)?;
            command
                .stdout(file.try_clone().map_err(|error| BuildError::Docker {
                    action,
                    diagnostic: error.to_string(),
                })?)
                .stderr(file);
            Some(
                std::fs::File::open(&progress_file).map_err(|error| BuildError::Docker {
                    action,
                    diagnostic: error.to_string(),
                })?,
            )
        } else {
            None
        };
        let mut drain = || {
            use std::io::Read as _;
            if let (Some(reader), Some(progress)) = (&mut progress_reader, self.progress) {
                let mut bytes = [0; 16 * 1024];
                // Bound each polling round so chatty output cannot starve
                // cancellation or the deadline.
                for _ in 0..64 {
                    match reader.read(&mut bytes) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => progress(Progress::Output(bytes.split_at(n).0.to_vec())),
                    }
                }
            }
        };
        let mut child = command.spawn().map_err(|error| BuildError::Docker {
            action,
            diagnostic: error.to_string(),
        })?;
        started();
        let status = wait_controlled(&mut child, self.deadline, self.cancellation, &mut drain)?;
        drain();
        if !status.success() {
            // Only a captured command has a diagnosis this side can read back.
            // Inherited output already reached the operator, and the file
            // still holds whatever the previous captured command wrote.
            let diagnostic = match streams {
                Streams::Captured => std::fs::read_to_string(&diagnosis).unwrap_or_default(),
                Streams::Inherited => String::new(),
            };
            let diagnostic = diagnostic.trim();
            return Err(BuildError::Docker {
                action,
                diagnostic: if diagnostic.is_empty() {
                    format!("exited with {status}")
                } else {
                    diagnostic.to_owned()
                },
            });
        }
        match streams {
            Streams::Captured => {
                std::fs::read_to_string(&output).map_err(|error| BuildError::Docker {
                    action,
                    diagnostic: error.to_string(),
                })
            }
            Streams::Inherited => Ok(String::new()),
        }
    }

    fn create(&self, action: &'static str, path: &PathBuf) -> Result<std::fs::File, BuildError> {
        std::fs::File::create(path).map_err(|error| BuildError::Docker {
            action,
            diagnostic: error.to_string(),
        })
    }
}

/// Polling is bounded even after kill: inability to observe termination must
/// not become an unbounded wait or release shared builder ownership.
fn wait_controlled(
    child: &mut Child,
    deadline: Deadline,
    cancellation: Option<&Cancellation>,
    drain: &mut impl FnMut(),
) -> Result<ExitStatus, BuildError> {
    let cause = loop {
        drain();
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => break BuildError::UncertainTermination(error.to_string()),
        }
        if cancellation.is_some_and(Cancellation::is_cancelled) {
            break BuildError::Cancelled;
        }
        if deadline.remaining().is_zero() {
            break BuildError::TimedOut(deadline.budget.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.kill();
    let stop = Instant::now() + Duration::from_secs(5);
    while Instant::now() < stop {
        match child.try_wait() {
            Ok(Some(_)) => return Err(cause),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(BuildError::UncertainTermination(error.to_string())),
        }
    }
    Err(BuildError::UncertainTermination(format!(
        "{cause}; Docker CLI termination was not observed"
    )))
}

#[cfg(test)]
mod tests;
