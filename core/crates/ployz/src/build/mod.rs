//! Capture Git checkouts as Builds and run them on a Build Machine.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ployz_build::{
    BuiltImage, Output, Progress, Stage, TargetEvidence, WorkEvidence,
    remote::{Definition, Outcome},
};
use ployz_core::{
    DeployIntent, MachineId, OpenedSealed, Placement, RequestedServiceSpec, ServiceName,
    config::BuildMethod,
};
use serde::Serialize;
use thiserror::Error;

use inputs::BuildInputs;
pub use inputs::{UploadListing, content_digest};

mod ignore;
mod inputs;
mod local;
mod platforms;
pub use platforms::placement_platforms;
mod remote;
pub use local::LocalImage;
mod reuse;
pub(crate) use reuse::{holder, image_stores, runs_everywhere};
mod sealed;

/// How one Service's checked-out source becomes an image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildSpec {
    /// Build context directory inside the checkout.
    pub context: PathBuf,
    /// How the context becomes an image.
    pub recipe: Recipe,
}

/// The build implementation for one Service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Recipe {
    /// Dockerfile path inside the checkout.
    Dockerfile(PathBuf),
    /// Railpack detection, optionally with an explicit build command.
    Railpack { command: Option<String> },
}

/// Remote failure evidence cannot contain a successful build outcome.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteBuildFailure {
    /// The Build Machine confirmed the attempt ended without an image. The
    /// caller says the Build failed; this is why.
    #[error("{message}")]
    Failed {
        stage: Stage,
        message: String,
        work: WorkEvidence,
    },
    /// The attempt's termination was not confirmed.
    #[error("Build outcome unknown: {message}")]
    Unknown {
        stage: Stage,
        message: String,
        work: WorkEvidence,
    },
}

/// Why a Build could not be captured, run, or bound to its Service.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum Error {
    #[error(transparent)]
    RemoteBuild { outcome: Box<RemoteBuildFailure> },
    #[error("invalid Build: {0}")]
    Invalid(String),
    #[error("{0}")]
    Sealed(String),
    #[error("Could not build {service}.")]
    Build {
        service: ServiceName,
        #[source]
        source: ployz_build::BuildError,
    },
    #[error("{0}")]
    Io(String),
    /// No Build Machine admitted the Build by its deadline; it was withdrawn
    /// before any source left the client.
    #[error("no Build Machine admitted the Build in time; nothing was uploaded")]
    Withdrawn,
}

/// A Service whose image this preparation built, bound to the content produced.
#[derive(Clone, Debug)]
pub struct BuiltService {
    pub name: ServiceName,
    /// Machine whose image store holds the completed image.
    pub machine_id: MachineId,
    /// Reference the Service requested, used when the image is delivered.
    pub image: String,
    pub placement: Placement,
    /// The completed image, identified by content.
    pub built: BuiltImage,
    /// Keeps the Build Machine's temporary image retention alive.
    pub(crate) _retention: Option<Arc<Mutex<tonic::Streaming<ployz_core::OpaquePayload>>>>,
}

impl PartialEq for BuiltService {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.machine_id == other.machine_id
            && self.image == other.image
            && self.placement == other.placement
            && self.built == other.built
    }
}

/// Builds whose sources and settings have already been captured.
pub struct CapturedBuild {
    targets: Vec<CapturedTarget>,
    /// The Sealed values a runner fetched for this Build, held for every solve.
    opened: Option<OpenedSealed>,
}

struct CapturedTarget {
    name: ServiceName,
    image: String,
    placement: Placement,
    target: ployz_build::Target,
    build_method: BuildMethod,
    retained_tag: String,
    /// Names of the Sealed values the Build reads.
    sealed: Vec<String>,
    inputs: BuildInputs,
}

/// Freeze each Service's source and effective variables before any Build runs.
/// Text variables become build variables; a Railpack command is added as
/// `RAILPACK_BUILD_CMD`. Build variables may be exposed in build output or history.
/// Sealed values stay sealed in private material and reach the Build only as
/// secret mounts the Builder opens. A runner that already holds them `opened` plans
/// a Railpack Build with its sealed settings.
///
/// # Errors
/// Rejects Services absent from `intent`, unreadable or unstable source files, and
/// invalid ignore rules.
pub fn capture(
    intent: &DeployIntent,
    specs: BTreeMap<ServiceName, BuildSpec>,
    opened: Option<OpenedSealed>,
) -> Result<CapturedBuild, Error> {
    let targets = specs
        .into_iter()
        .map(|(name, spec)| {
            let service = intent
                .target
                .iter()
                .find(|service| service.name == name)
                .ok_or_else(|| invalid(format!("undefined service '{name}'")))?;
            capture_target(service, spec, opened.as_ref())
        })
        .collect::<Result<_, _>>()?;
    Ok(CapturedBuild { targets, opened })
}

fn capture_target(
    service: &RequestedServiceSpec,
    spec: BuildSpec,
    opened: Option<&OpenedSealed>,
) -> Result<CapturedTarget, Error> {
    let mut inputs = BuildInputs::new()?;
    let image = service.container.image.clone();
    let reference = image
        .parse::<oci_client::Reference>()
        .map_err(|error| invalid(error.to_string()))?;
    // A later Build moving the Service tag cannot remove this image before delivery.
    let retained_tag = format!(
        "{}/{}:ployz-build-{}",
        reference.registry(),
        reference.repository(),
        uuid::Uuid::new_v4()
    );
    let mut args: BTreeMap<String, String> = service
        .container
        .environment
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), value.text()?.to_owned())))
        .collect();
    let name = service.name.to_string();
    let (build_method, context, dockerfile, sealed) = match spec.recipe {
        Recipe::Railpack { command } => {
            let recipe = Recipe::Railpack {
                command: command.clone(),
            };
            let mut sealed = sealed::capture(service, &recipe, &inputs, opened)?;
            if let Some(command) = command {
                args.insert("RAILPACK_BUILD_CMD".into(), command);
            }
            let config = args
                .get("RAILPACK_CONFIG_FILE")
                .map(String::as_str)
                .or_else(|| opened.and_then(|opened| opened.get("RAILPACK_CONFIG_FILE")));
            let context = inputs.railpack_context(&spec.context, config)?;
            let context = inputs.relative(&context);
            // Railpack variables stay in private upload material, not the recipe.
            inputs.railpack(&ployz_build::Railpack {
                name: name.clone(),
                context: context.clone(),
                variables: std::mem::take(&mut args),
                sealed: std::mem::take(&mut sealed.fingerprints),
                refresh_cache: false,
            })?;
            (BuildMethod::Railpack, context, None, sealed)
        }
        Recipe::Dockerfile(dockerfile) => {
            let context = inputs.context(&spec.context, &dockerfile)?;
            let file = inputs.dockerfile(&dockerfile)?;
            let mut sealed =
                sealed::capture(service, &Recipe::Dockerfile(file.clone()), &inputs, opened)?;
            args.extend(std::mem::take(&mut sealed.args));
            if !sealed.declared.is_empty() {
                inputs.sealed_args(&name, &sealed.declared)?;
            }
            // Both context and recipe live directly beneath source/.
            let dockerfile = Path::new("..").join(file.file_name().expect("captured recipe"));
            (
                BuildMethod::Dockerfile,
                inputs.relative(&context),
                Some(dockerfile),
                sealed,
            )
        }
    };
    inputs.verify()?;
    let recipe = CapturedFile {
        services: BTreeMap::from([(
            name.as_str(),
            CapturedService {
                image: &image,
                build: CapturedRecipe {
                    context,
                    dockerfile,
                    args,
                    secrets: sealed.secrets.keys().cloned().collect(),
                    tags: vec![image.clone(), retained_tag.clone()],
                },
            },
        )]),
        secrets: &sealed.secrets,
    };
    // Buildx interpolates the recipe. Escape dollars so captured values cannot
    // pick up an execution host value or lose a literal dollar.
    let yaml = serde_norway::to_string(&recipe)
        .map_err(|error| Error::Io(format!("encode captured build: {error}")))?
        .replace('$', "$$");
    inputs.recipe(&yaml)?;
    Ok(CapturedTarget {
        target: ployz_build::Target {
            name,
            platforms: Vec::new(),
        },
        name: service.name.clone(),
        image,
        placement: service.placement.clone(),
        build_method,
        retained_tag,
        sealed: sealed.names,
        inputs,
    })
}

/// The Buildx file BuildKit reads, one Service per capture.
#[derive(Serialize)]
struct CapturedFile<'target> {
    services: BTreeMap<&'target str, CapturedService<'target>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    secrets: &'target BTreeMap<String, sealed::CapturedSecret>,
}

#[derive(Serialize)]
struct CapturedService<'target> {
    image: &'target str,
    build: CapturedRecipe,
}

#[derive(Serialize)]
struct CapturedRecipe {
    context: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    dockerfile: Option<PathBuf>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    args: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secrets: Vec<String>,
    tags: Vec<String>,
}

impl CapturedBuild {
    /// Images still to produce, with the platforms each must carry.
    pub fn targets(&self) -> impl Iterator<Item = &ployz_build::Target> {
        self.targets.iter().map(|captured| &captured.target)
    }

    /// Owned copies of [`Self::targets`], for requests that carry them.
    #[must_use]
    pub fn to_targets(&self) -> Vec<ployz_build::Target> {
        self.targets().cloned().collect()
    }

    /// How a Build that reads a Sealed value builds, which only a Builder that
    /// opens them can run. Railpack names it first: its refusal explains more.
    #[must_use]
    pub fn reads_sealed(&self) -> Option<BuildMethod> {
        let reading = self
            .targets
            .iter()
            .filter(|captured| !captured.sealed.is_empty());
        reading
            .clone()
            .find(|captured| captured.build_method == BuildMethod::Railpack)
            .or_else(|| reading.clone().next())
            .map(|captured| captured.build_method)
    }

    /// Run each Build in order on one resolved Machine over the authenticated
    /// Ployz stream, and bind each completed image to its Service. With
    /// `admit_by`, a Build the Machine hasn't admitted by then is withdrawn.
    ///
    /// # Errors
    /// Reports failed, unknown, or withdrawn work and leaves the remaining targets unattempted.
    pub async fn execute_remote_images(
        self,
        client: &crate::connect::Client,
        machine_id: MachineId,
        admit_by: Option<tokio::time::Instant>,
        cancellation: tokio_util::sync::CancellationToken,
        progress: impl Fn(Progress),
    ) -> Result<Vec<BuiltService>, Error> {
        let targets = self.to_targets();
        let mut work = WorkEvidence::new(&targets);
        let mut completed = Vec::new();
        for captured in self.targets {
            if cancellation.is_cancelled() {
                return Err(remote_error(
                    remote::failed(
                        Stage::Admission,
                        "Build cancelled before the next target was submitted",
                    )
                    .with_work(work),
                ));
            }
            let outcome = remote::execute(
                captured.inputs,
                Definition {
                    retained_tags: vec![captured.retained_tag],
                    targets: vec![captured.target.clone()],
                    image_contexts: BTreeMap::new(),
                    output: Output::Load,
                    no_cache: false,
                    pull: false,
                },
                client,
                machine_id,
                admit_by,
                cancellation.clone(),
                &progress,
            )
            .await;
            match outcome {
                remote::Completion::Images { images, stream } => {
                    let image = images
                        .into_iter()
                        .next()
                        .expect("remote adapter validated the count");
                    work.0.insert(
                        captured.target.name.clone(),
                        TargetEvidence::Image(image.clone()),
                    );
                    completed.push(BuiltService {
                        name: captured.name,
                        machine_id,
                        image: captured.image,
                        placement: captured.placement,
                        built: image,
                        _retention: Some(stream),
                    });
                }
                remote::Completion::Report(
                    outcome @ (Outcome::Failed { .. } | Outcome::Unknown { .. }),
                ) => {
                    if let Outcome::Failed { work: observed, .. }
                    | Outcome::Unknown { work: observed, .. } = &outcome
                        && let Some(evidence) = observed.0.get(&captured.target.name)
                    {
                        work.0
                            .insert(captured.target.name.clone(), evidence.clone());
                    }
                    return Err(remote_error(outcome.with_work(work)));
                }
                remote::Completion::Withdrawn => return Err(Error::Withdrawn),
                remote::Completion::Report(
                    Outcome::CapabilitiesChecked { .. }
                    | Outcome::Validated { .. }
                    | Outcome::Published { .. }
                    | Outcome::Images { .. },
                ) => unreachable!("adapter validated output disposition"),
            }
        }
        Ok(completed)
    }
}

/// Use each Service's completed content for Containers and hooks. A tag
/// overwritten by another Service or client cannot substitute its image.
///
/// # Errors
/// Rejects a completed image whose reference cannot name its content.
pub(crate) fn bind(intent: &mut DeployIntent, builds: &[BuiltService]) -> Result<(), Error> {
    for service in &mut intent.target {
        if let Some(build) = builds.iter().find(|build| build.name == service.name) {
            service.container.image =
                build
                    .built
                    .repository_reference(&build.image)
                    .map_err(|source| Error::Build {
                        service: build.name.clone(),
                        source,
                    })?;
            service.container.pull_policy = ployz_core::PullPolicy::Never;
        }
    }
    Ok(())
}

fn remote_error(outcome: Outcome) -> Error {
    let failure = match outcome {
        Outcome::Failed {
            stage,
            message,
            work,
        } => RemoteBuildFailure::Failed {
            stage,
            message,
            work,
        },
        Outcome::Unknown {
            stage,
            message,
            work,
        } => RemoteBuildFailure::Unknown {
            stage,
            message,
            work,
        },
        Outcome::CapabilitiesChecked { .. }
        | Outcome::Images { .. }
        | Outcome::Validated { .. }
        | Outcome::Published { .. } => {
            return invalid("Build produced no image available for Direct Image Transfer");
        }
    };
    Error::RemoteBuild {
        outcome: Box::new(failure),
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    /// Image-sourced Cloud Services; each test sets the fields it exercises.
    pub(crate) fn intent(names: &[&str]) -> DeployIntent {
        let snapshots = names
            .iter()
            .map(|name| {
                serde_json::json!({"config": {
                    "version": 2, "privateDns": name, "healthcheck": {"type": "none"},
                    "restartPolicy": "on-failure",
                    "source": {"type": "image", "version": 1,
                        "image": format!("registry.invalid/{name}:1"), "credentials": {"type": "none"}}
                }})
            })
            .collect::<Vec<_>>();
        ployz_core::config::lower_deployment(
            serde_json::from_value(serde_json::json!({"namespace": "app", "snapshots": snapshots}))
                .unwrap(),
        )
        .unwrap()
    }

    pub(super) fn capture_one(
        intent: &DeployIntent,
        spec: BuildSpec,
    ) -> Result<CapturedTarget, Error> {
        let name = intent.target.first().unwrap().name.clone();
        let mut captured = capture(intent, BTreeMap::from([(name, spec)]), None)?;
        Ok(captured.targets.pop().unwrap())
    }

    pub(super) fn recipe(captured: &CapturedTarget) -> serde_norway::Value {
        serde_norway::from_slice(&fs::read(captured.inputs.root().join("compose.yaml")).unwrap())
            .unwrap()
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn dockerfile_capture_follows_its_ignore_rules_and_freezes_variables() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("repository");
        fs::create_dir_all(source.join("cache")).unwrap();
        fs::write(source.join("cache/keep"), "keep").unwrap();
        fs::write(source.join("cache/drop"), "drop").unwrap();
        fs::write(source.join(".dockerignore"), "cache\n").unwrap();
        fs::write(source.join("app.Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(
            source.join("app.Dockerfile.dockerignore"),
            "cache\n!**/keep\n",
        )
        .unwrap();
        fs::write(source.join("value"), "included").unwrap();
        symlink("value", source.join("link")).unwrap();
        // Docker retains contained links even when their targets are excluded.
        symlink("cache/drop", source.join("dangling")).unwrap();
        let mut intent = intent(&["api"]);
        intent
            .target
            .first_mut()
            .unwrap()
            .container
            .environment
            .insert("PRICE".into(), "$5".into());
        let spec = BuildSpec {
            context: source.clone(),
            recipe: Recipe::Dockerfile(source.join("app.Dockerfile")),
        };
        let captured = capture_one(&intent, spec.clone()).unwrap();
        let recipe = recipe(&captured);
        let build = &recipe["services"]["api"]["build"];
        let context = captured
            .inputs
            .root()
            .join(build["context"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(context.join("cache/keep")).unwrap(),
            "keep"
        );
        assert!(!context.join("cache/drop").exists());
        assert_eq!(
            fs::read_to_string(context.join("link")).unwrap(),
            "included"
        );
        assert_eq!(
            fs::read_link(context.join("dangling")).unwrap(),
            Path::new("cache/drop")
        );
        assert!(
            context
                .join(build["dockerfile"].as_str().unwrap())
                .is_file()
        );
        // Buildx interpolation cannot turn a literal dollar into a host value.
        assert_eq!(build["args"]["PRICE"].as_str(), Some("$$5"));
        assert_eq!(
            build["tags"],
            serde_norway::to_value([&captured.image, &captured.retained_tag]).unwrap()
        );
        ployz_build::remote::validate_capture(
            captured.inputs.root(),
            &Definition {
                targets: vec![captured.target.clone()],
                retained_tags: vec![captured.retained_tag.clone()],
                image_contexts: BTreeMap::new(),
                output: Output::Load,
                no_cache: false,
                pull: false,
            },
            ployz_build::remote::SealedFiles::Opened,
        )
        .unwrap();
        for target in [root.path().join("outside"), "../outside".into()] {
            fs::write(root.path().join("outside"), "host-only").unwrap();
            symlink(target, source.join("escape")).unwrap();
            let error = capture_one(&intent, spec.clone())
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("symlink"), "{error}");
            fs::remove_file(source.join("escape")).unwrap();
        }
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "fixed capture fixture")]
    fn railpack_capture_applies_config_exclusions_and_keeps_variables_private() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path();
        fs::create_dir_all(source.join("config")).unwrap();
        fs::write(
            source.join(".dockerignore"),
            "*.txt\nconfig\n.dockerignore\n",
        )
        .unwrap();
        fs::write(
            source.join("config/custom.json"),
            "{ // Railpack permits comments\n\"exclude\": [\"!keep.txt\", \"drop.log\",],\n}",
        )
        .unwrap();
        for name in ["drop.txt", "drop.log"] {
            fs::write(source.join(name), "excluded").unwrap();
        }
        fs::write(source.join("keep.txt"), "captured").unwrap();
        let mut intent = intent(&["api"]);
        let environment = &mut intent.target.first_mut().unwrap().container.environment;
        environment.insert("TOKEN".into(), "private-token".into());
        environment.insert("RAILPACK_CONFIG_FILE".into(), "config/custom.json".into());
        let captured = capture_one(
            &intent,
            BuildSpec {
                context: source.to_owned(),
                recipe: Recipe::Railpack {
                    command: Some("make release".into()),
                },
            },
        )
        .unwrap();
        assert_eq!(captured.build_method, BuildMethod::Railpack);
        let recipe = recipe(&captured);
        let build = &recipe["services"]["api"]["build"];
        assert!(build.get("args").is_none());
        let context = captured
            .inputs
            .root()
            .join(build["context"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(context.join("keep.txt")).unwrap(),
            "captured"
        );
        assert!(context.join("config/custom.json").exists());
        assert!(context.join(".dockerignore").exists());
        for name in ["drop.txt", "drop.log"] {
            assert!(!context.join(name).exists(), "{name}");
        }
        let private: serde_json::Value = serde_json::from_slice(
            &fs::read(captured.inputs.root().join("private/railpack.json")).unwrap(),
        )
        .unwrap();
        let variables = &private[0]["variables"];
        assert_eq!(variables["TOKEN"], "private-token");
        assert_eq!(variables["RAILPACK_BUILD_CMD"], "make release");
        assert!(
            !fs::read_to_string(captured.inputs.root().join("compose.yaml"))
                .unwrap()
                .contains("private-token")
        );
    }

    #[test]
    fn deploy_binds_each_service_to_its_build_when_requested_tags_are_shared() {
        let spec = |name: &str| {
            serde_json::from_value::<RequestedServiceSpec>(serde_json::json!({
                "name": name,
                "mode": {"mode": "replicated", "replicas": 1},
                "container": {"image": "example.test/shared:latest", "pull_policy": "missing"},
            }))
            .unwrap()
        };
        let mut intent = DeployIntent::new(
            ployz_core::Namespace::parse("app").unwrap(),
            vec![spec("one"), spec("two")],
            Default::default(),
        );
        let builds = [("one", "1"), ("two", "2")].map(|(name, digit)| BuiltService {
            name: ServiceName::parse(name).unwrap(),
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            image: "example.test/shared:latest".into(),
            placement: Placement::default(),
            built: BuiltImage {
                reference: format!("sha256:{}", digit.repeat(64)),
                tags: vec![
                    "auxiliary.test:5000/other:extra".into(),
                    "example.test/shared:latest".into(),
                ],
                platforms: vec!["linux/amd64".into()],
                location: "unix:///var/run/docker.sock".into(),
            },
            _retention: None,
        });
        bind(&mut intent, &builds).unwrap();
        for (service, digit) in intent.target.iter().zip(["1", "2"]) {
            assert_eq!(
                service.container.image,
                format!("example.test/shared@sha256:{}", digit.repeat(64))
            );
            assert_eq!(service.container.pull_policy, ployz_core::PullPolicy::Never);
        }
    }
}
