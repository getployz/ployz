use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
};

use ployz_build::{BuiltImage, Output};
use ployz_core::Placement;
use serde::Serialize;
use serde_norway::Value;

use super::{BuildSpec, ComposeError, ComposeProject, LoadOptions, build_inputs::BuildInputs};

#[path = "platforms.rs"]
mod platforms;
use platforms::RAILPACK_PLATFORMS;

#[path = "remote_steps.rs"]
mod remote_steps;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildOptions {
    pub build_args: Vec<String>,
    pub deps: bool,
    pub no_cache: bool,
    /// What this command does with the images it builds.
    pub output: Output,
    pub pull: bool,
    pub services: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BuildService {
    pub name: String,
    pub image: String,
    pub build: Value,
    pub placement: Placement,
}

pub fn plan_build(
    project: &ComposeProject,
    options: &BuildOptions,
) -> Result<Vec<BuildService>, ComposeError> {
    let names = if options.services.is_empty() {
        project.builds.keys().cloned().collect::<Vec<_>>()
    } else {
        options.services.clone()
    };

    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    for name in &names {
        include_service(
            project,
            name,
            options.deps,
            &mut visiting,
            &mut seen,
            &mut selected,
        )?;
    }
    selected
        .into_iter()
        .filter(|name| project.builds.contains_key(*name))
        .map(|name| build_service(project, name))
        .collect()
}

/// A build whose configuration, options, and local sources have already been captured.
pub struct CapturedBuild {
    plan: Vec<BuildService>,
    targets: Vec<ployz_build::Target>,
    railpack: Vec<ployz_build::Railpack>,
    options: BuildOptions,
    environment: BTreeMap<String, String>,
    inputs: BuildInputs,
    retained_tags: BTreeMap<String, String>,
    /// Targets whose platforms Compose authored, as opposed to the execution
    /// host's default a Deploy may replace with what its Machines run.
    authored_platforms: BTreeSet<String>,
}

impl CapturedBuild {
    /// Complete command requirements, including captured Build dependencies.
    pub(crate) fn targets(&self) -> &[ployz_build::Target] {
        &self.targets
    }
}

/// Where a completed image is available; Machine identity is already resolved.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BuildLocation {
    /// The invoking client’s Docker store contains the completed image.
    #[default]
    Local,
    /// This resolved Machine contains the completed image.
    Machine(ployz_core::MachineId),
}

/// A Service whose image this command built, bound to the content produced.
#[derive(Clone, Debug)]
pub struct BuiltService {
    /// Raw Compose Build target name, including build-only dependencies.
    /// Build names may contain dots/underscores, unlike Deploy Service names.
    pub name: String,
    /// Store that holds this completed image.
    pub location: BuildLocation,
    /// Reference the Service requested, used when the image is published.
    pub image: String,
    /// Runtime destination constraints used for local image prewarming.
    pub placement: Placement,
    /// The image this command built for it.
    pub built: BuiltImage,
    pub(super) _retention: Option<BuildRetention>,
}

#[derive(Clone)]
pub(super) enum BuildRetention {
    Local {
        _tags: Arc<ployz_build::ImageRetention>,
    },
    Remote {
        _stream: Arc<Mutex<tonic::Streaming<ployz_core::OpaquePayload>>>,
    },
}
impl std::fmt::Debug for BuildRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BuildRetention")
    }
}

impl PartialEq for BuiltService {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.location == other.location
            && self.image == other.image
            && self.placement == other.placement
            && self.built == other.built
    }
}

impl BuiltService {
    /// Deliver this command's content under the reference the Service asked
    /// for, so a later Build moving that tag cannot substitute its image.
    #[must_use]
    pub fn content(&self) -> crate::image::ImageContent<'_> {
        crate::image::ImageContent::built(&self.image, &self.built.reference)
    }
}

/// Build settings upstream translation drops on the way to BuildKit. Every
/// other setting reaches BuildKit, which validates it and reports its own
/// refusal. Ployz never silently discards a supplied setting.
const DROPPED_BY_UPSTREAM: &[&str] = &["entitlements", "isolation", "privileged"];

/// Cache settings naming a path rather than a registry. Capture relocates the
/// build into a private directory, so a local path would no longer mean what
/// it did, and the cache would be removed with the capture.
const LOCAL_CACHE: &[&str] = &["cache_from", "cache_to"];

/// Freeze build sources, options, and provider values before invoking Docker.
/// Authored files follow Docker's ignore rules, including Compose and environment
/// files. Generated argument/credential material is staged separately.
///
/// Service environment values default declared Dockerfile arguments; Compose
/// arguments override them, and CLI arguments override Compose. Ordinary build
/// arguments may be exposed in build output/history; use explicit secret mounts
/// when the recipe needs BuildKit's mount confidentiality.
///
/// # Errors
/// Rejects invalid build inputs, provider failures, and unreadable or unstable source files.
pub fn capture_build(
    plan: &[BuildService],
    options: &BuildOptions,
    project: &mut ComposeProject,
) -> Result<CapturedBuild, ComposeError> {
    let mut inputs = BuildInputs::new()?;
    for setting in [
        "DOCKER_CONTEXT",
        "DOCKER_CERT_PATH",
        "DOCKER_TLS",
        "DOCKER_TLS_VERIFY",
    ] {
        if project
            .environment
            .get(setting)
            .is_some_and(|value| !value.is_empty())
        {
            return Err(invalid_build(&format!(
                "{setting} is host-specific and cannot be preserved by a captured Build"
            )));
        }
    }
    inputs.docker_config(&project.environment, &project.working_dir)?;
    let mut ssh_agent = project
        .environment
        .get("DOCKER_HOST")
        .is_some_and(|host| host.starts_with("ssh://"));
    let mut plan = plan.to_vec();
    let mut secret_names = BTreeSet::new();
    let mut targets = Vec::new();
    let mut retained_tags = BTreeMap::new();
    let mut railpack_recipes = Vec::new();
    let mut authored_platforms = BTreeSet::new();
    for service in &mut plan {
        let image = service.image.clone();
        let name = service.name.clone();
        let build = service
            .build
            .as_mapping_mut()
            .ok_or_else(|| invalid_build("expected a build mapping"))?;
        refuse_unpassable_settings(&name, build)?;
        ssh_agent |= build
            .get("context")
            .and_then(Value::as_str)
            .is_some_and(is_ssh_context);
        let recipe = capture_recipe(&name, build, options, project, &mut inputs)?;
        let railpack = recipe.is_some();
        if let Some(recipe) = recipe {
            railpack_recipes.push(recipe);
        }
        let mut platforms = requested_platforms(&name, build, railpack)?;
        if !platforms.is_empty() {
            authored_platforms.insert(name.clone());
        }
        if railpack {
            // Assembly cannot carry attestations, and a Deploy may still turn
            // one platform into two, so Railpack never accepts them.
            for field in ["provenance", "sbom"] {
                if build.get(field).is_some_and(|value| {
                    !matches!(value, Value::Null | Value::Bool(false))
                        && value.as_str() != Some("false")
                }) {
                    return Err(invalid_build(&format!(
                        "Railpack does not support build.{field}; use false or a Dockerfile",
                    )));
                }
                build.remove(field);
            }
        }
        if platforms.is_empty()
            && let Some(platform) = project
                .environment
                .get("DOCKER_DEFAULT_PLATFORM")
                .filter(|p| !p.is_empty())
        {
            platforms.push(platform.clone());
        }
        if railpack
            && platforms
                .iter()
                .any(|p| !RAILPACK_PLATFORMS.contains(&p.as_str()))
        {
            return Err(invalid_build(
                "Railpack supports only linux/amd64 and linux/arm64",
            ));
        }
        // Railpack consumes Target.platforms directly. Keep an inherited host
        // default out of the recipe: Deploy may replace it after capture.
        if railpack && !authored_platforms.contains(&name) {
            build.remove("platforms");
        } else if !platforms.is_empty() {
            build.insert(
                Value::String("platforms".into()),
                Value::Sequence(platforms.iter().cloned().map(Value::String).collect()),
            );
        }
        targets.push(ployz_build::Target { name, platforms });
        retain_service_image_tag(&service.name, image, build)?;
        if options.output == Output::Load {
            // Bake can import multiple Services under one requested tag in any
            // order. Retain every result before a sibling or later client moves it.
            let reference = service
                .image
                .parse::<oci_client::Reference>()
                .map_err(|error| invalid_build(&error.to_string()))?;
            let retained = format!(
                "{}/{}:ployz-build-{}",
                reference.registry(),
                reference.repository(),
                uuid::Uuid::new_v4()
            );
            retained_tags.insert(service.name.clone(), retained.clone());
            let tags = build
                .entry(Value::String("tags".into()))
                .or_insert_with(|| Value::Sequence(vec![Value::String(service.image.clone())]));
            tags.as_sequence_mut()
                .ok_or_else(|| invalid_build("invalid build tags"))?
                .push(Value::String(retained));
        }
        if let Some(ssh) = build
            .get_mut(Value::String("ssh".into()))
            .and_then(Value::as_sequence_mut)
        {
            for key in ssh {
                let (id, path) = ssh_paths(key)?;
                let paths = path
                    .split(',')
                    .map(|path| {
                        inputs
                            .private_file(&project.working_dir.join(path))
                            .map(|path| inputs.relative(&path).to_string_lossy().into_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                *key = Value::String(format!("{id}={}", paths.join(",")));
            }
        }
        if let Some(contexts) = build.get_mut(Value::String("additional_contexts".into())) {
            match contexts {
                Value::Mapping(contexts) => {
                    for context in contexts.values_mut() {
                        let source = context
                            .as_str()
                            .ok_or_else(|| invalid_build("invalid additional context"))?;
                        ssh_agent |= is_ssh_context(source);
                        *context = Value::String(capture_context(
                            source,
                            &project.working_dir,
                            None,
                            &mut inputs,
                        )?);
                    }
                }
                Value::Sequence(contexts) => {
                    for context in contexts {
                        let (name, source) = context
                            .as_str()
                            .and_then(|value| value.split_once('='))
                            .ok_or_else(|| invalid_build("invalid additional context"))?;
                        ssh_agent |= is_ssh_context(source);
                        *context = Value::String(format!(
                            "{name}={}",
                            capture_context(source, &project.working_dir, None, &mut inputs)?
                        ));
                    }
                }
                Value::Null => {}
                Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
                    return Err(invalid_build("invalid additional contexts"));
                }
            }
        }
        if let Some(names) = build
            .get(Value::String("secrets".into()))
            .and_then(Value::as_sequence)
        {
            for secret in names {
                let name = secret
                    .as_str()
                    .or_else(|| {
                        secret
                            .as_mapping()?
                            .get(Value::String("source".into()))?
                            .as_str()
                    })
                    .ok_or_else(|| invalid_build("invalid build secret"))?;
                secret_names.insert(name.to_owned());
            }
        }
    }
    inputs.verify()?;
    inputs.railpack(&railpack_recipes)?;
    let mut secrets = BTreeMap::new();
    for name in secret_names {
        let value = project.resolve_secret(&name)?;
        let file = inputs.secret(secrets.len(), value)?;
        secrets.insert(
            name,
            BuildSecret {
                file: inputs.relative(&file).to_string_lossy().into_owned(),
            },
        );
    }
    let mut options = options.clone();
    // Effective arguments live only in the private Compose input, never argv.
    options.build_args.clear();
    let services = plan
        .iter()
        .map(|service| {
            (
                service.name.as_str(),
                BuildServiceOverride {
                    image: &service.image,
                    build: &service.build,
                },
            )
        })
        .collect();
    // Compose interpolates even normalized input. Escape dollars so captured values
    // cannot pick up a later shell/.env value or lose a literal dollar.
    let yaml = serde_norway::to_string(&BuildOverride { services, secrets })
        .map_err(|error| ComposeError::Io(format!("encode captured build: {error}")))?
        .replace('$', "$$");
    inputs.compose(&yaml)?;
    Ok(CapturedBuild {
        plan,
        targets,
        railpack: railpack_recipes,
        options,
        environment: project
            .environment
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "PATH"
                        | "DOCKER_HOST"
                        | "HTTP_PROXY"
                        | "HTTPS_PROXY"
                        | "NO_PROXY"
                        | "http_proxy"
                        | "https_proxy"
                        | "no_proxy"
                        | "TERM"
                        | "NO_COLOR"
                ) || (key.as_str() == "SSH_AUTH_SOCK" && ssh_agent)
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        inputs,
        retained_tags,
        authored_platforms,
    })
}

/// Own recipe selection, validation, variable placement, and source capture together.
fn capture_recipe(
    name: &str,
    build: &mut serde_norway::Mapping,
    options: &BuildOptions,
    project: &mut ComposeProject,
    inputs: &mut BuildInputs,
) -> Result<Option<ployz_build::Railpack>, ComposeError> {
    let railpack = selects_railpack(build, &project.working_dir)?;
    let refresh_cache = options.no_cache
        || options.pull
        || build.get("no_cache").and_then(Value::as_bool) == Some(true)
        || build.get("pull").and_then(Value::as_bool) == Some(true);
    if railpack && options.output == Output::Validate {
        return Err(invalid_build("Railpack does not support --check"));
    }
    if railpack {
        for key in build.keys().filter_map(Value::as_str) {
            if !matches!(
                key,
                "context"
                    | "dockerfile"
                    | "dockerfile_inline"
                    | "x-recipe"
                    | "args"
                    | "tags"
                    | "platforms"
                    | "cache_from"
                    | "cache_to"
                    | "secrets"
                    | "no_cache"
                    | "pull"
                    | "provenance"
                    | "sbom"
            ) {
                return Err(invalid_build(&format!(
                    "Railpack does not support build.{key}"
                )));
            }
        }
        if refresh_cache {
            // A cold build must not reimport the records just pruned locally.
            build.remove("cache_from");
        }
    }
    build.remove("x-recipe");
    let context = build
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_owned();
    let args = effective_build_args(name, build, &options.build_args, project)?;
    if railpack {
        if is_remote_context(&context) {
            return Err(invalid_build(
                "Railpack requires a captured local build.context",
            ));
        }
        let variables = args
            .iter()
            .map(|(key, value)| {
                key.as_str()
                    .zip(value.as_str())
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .ok_or_else(|| invalid_build("Railpack build variables must be strings"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let captured = inputs.railpack_context(&project.working_dir.join(context), &variables)?;
        let context = inputs.relative(&captured);
        build.remove("args");
        build.remove("dockerfile");
        build.remove("dockerfile_inline");
        build.insert(
            Value::String("context".into()),
            Value::String(context.to_string_lossy().into_owned()),
        );
        Ok(Some(ployz_build::Railpack {
            name: name.to_owned(),
            context,
            variables,
            refresh_cache,
        }))
    } else {
        build.insert(Value::String("args".into()), Value::Mapping(args));
        let dockerfile = (!build.contains_key("dockerfile_inline") && !is_remote_context(&context))
            .then(|| {
                project.working_dir.join(&context).join(
                    build
                        .get("dockerfile")
                        .and_then(Value::as_str)
                        .unwrap_or("Dockerfile"),
                )
            });
        let captured = capture_context(
            &context,
            &project.working_dir,
            dockerfile.as_deref(),
            inputs,
        )?;
        build.insert(Value::String("context".into()), Value::String(captured));
        if let Some(source) = dockerfile {
            let file = inputs.dockerfile(&source)?;
            // Both context and recipe live directly beneath source/.
            let file = Path::new("..").join(file.file_name().expect("captured recipe"));
            build.insert(
                Value::String("dockerfile".into()),
                Value::String(file.to_string_lossy().into_owned()),
            );
        }
        Ok(None)
    }
}

fn selects_railpack(build: &serde_norway::Mapping, directory: &Path) -> Result<bool, ComposeError> {
    if build
        .get("x-recipe")
        .is_some_and(|value| value.as_str().is_none())
    {
        return Err(invalid_build(
            "build.x-recipe must be auto, dockerfile, or railpack",
        ));
    }
    match build.get("x-recipe").and_then(Value::as_str) {
        Some("railpack") => Ok(true),
        Some("dockerfile") => Ok(false),
        Some("auto") | None => {
            if build.contains_key("dockerfile") || build.contains_key("dockerfile_inline") {
                return Ok(false);
            }
            let context = build.get("context").and_then(Value::as_str).unwrap_or(".");
            if is_remote_context(context) {
                return Ok(false);
            }
            directory
                .join(context)
                .join("Dockerfile")
                .try_exists()
                .map(|exists| !exists)
                .map_err(|error| ComposeError::Io(format!("select build recipe: {error}")))
        }
        Some(_) => Err(invalid_build(
            "build.x-recipe must be auto, dockerfile, or railpack",
        )),
    }
}

fn effective_build_args(
    service: &str,
    build: &serde_norway::Mapping,
    overrides: &[String],
    project: &mut ComposeProject,
) -> Result<serde_norway::Mapping, ComposeError> {
    // All values are fixed now; Compose/Buildx must never fill null arguments
    // from the execution host's shell. Build overrides affect only this copy.
    let runtime = project
        .services
        .get(service)
        .ok_or_else(|| invalid_build("build Service is missing"))?
        .container
        .environment
        .clone();
    let mut args = serde_norway::Mapping::new();
    for (key, value) in runtime {
        let value = if let Some(secret) = value.strip_prefix("secret://") {
            project.resolve_secret(secret)?.to_owned()
        } else {
            value
        };
        args.insert(Value::String(key), Value::String(value));
    }
    if let Some(declared) = build
        .get(Value::String("args".into()))
        .and_then(Value::as_mapping)
    {
        for (key, value) in declared {
            let value = if value.is_null() {
                key.as_str()
                    .and_then(|key| project.environment.get(key))
                    .map(|value| Value::String(value.clone()))
            } else {
                Some(value.clone())
            };
            match value {
                Some(value) => {
                    args.insert(key.clone(), value);
                }
                None => {
                    args.remove(key);
                }
            }
        }
    }
    for argument in overrides {
        let (key, value) = match argument.split_once('=') {
            Some(pair) => pair,
            None => (
                argument.as_str(),
                project
                    .environment
                    .get(argument)
                    .ok_or_else(|| {
                        invalid_build("a build argument has no captured environment value")
                    })?
                    .as_str(),
            ),
        };
        args.insert(Value::String(key.into()), Value::String(value.into()));
    }
    Ok(args)
}

impl CapturedBuild {
    /// Build this capture through the shared runner, without reading the
    /// original sources again.
    ///
    /// # Errors
    /// Fails if the runner cannot execute the build or the result cannot be
    /// bound to the content it produced.
    pub fn execute(
        &self,
        docker: Option<&Path>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<BuiltService>, ComposeError> {
        let mut environment = self.environment.clone();
        environment.insert(
            "HOME".into(),
            self.inputs
                .root()
                .join("private")
                .to_string_lossy()
                .into_owned(),
        );
        environment.insert(
            "DOCKER_CONFIG".into(),
            self.inputs
                .root()
                .join("private/docker")
                .to_string_lossy()
                .into_owned(),
        );
        let retention = BuildRetention::Local {
            _tags: Arc::new(
                ployz_build::ImageRetention::new(
                    self.retained_tags.values().cloned().collect(),
                    docker,
                    environment.clone(),
                )
                .map_err(|error| invalid_build(&error.to_string()))?,
            ),
        };
        let images = ployz_build::execute(
            &ployz_build::Request {
                image_contexts: &BTreeMap::new(),
                compose_file: Path::new("compose.yaml"),
                working_dir: self.inputs.root(),
                environment: &environment,
                docker,
                targets: &self.targets,
                railpack: &self.railpack,
                build_args: &self.options.build_args,
                output: self.options.output,
                no_cache: self.options.no_cache,
                pull: self.options.pull,
            },
            cancellation,
        )
        // BuildKit diagnoses its own failure; name the Builds it was running.
        .map_err(|source| ComposeError::Build {
            services: self
                .plan
                .iter()
                .map(|service| service.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            source,
        })?;
        // The runner returns one image per planned Service, in order.
        Ok(self
            .plan
            .iter()
            .zip(images)
            .map(|(service, built)| BuiltService {
                name: service.name.clone(),
                location: BuildLocation::Local,
                image: service.image.clone(),
                placement: service.placement.clone(),
                built,
                _retention: Some(retention.clone()),
            })
            .collect())
    }
}

/// Capture and build one plan in a single step.
///
/// # Errors
/// Propagates capture refusals and build failures.
pub fn execute_build(
    plan: &[BuildService],
    options: &BuildOptions,
    load: &LoadOptions,
    project: &mut ComposeProject,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<Vec<BuiltService>, ComposeError> {
    capture_build(plan, options, project)?.execute(load.docker.as_deref(), cancellation)
}

/// Refuse a setting Ployz cannot pass on, naming it rather than dropping it.
fn refuse_unpassable_settings(
    service: &str,
    build: &serde_norway::Mapping,
) -> Result<(), ComposeError> {
    for setting in DROPPED_BY_UPSTREAM {
        if build.contains_key(Value::String((*setting).into())) {
            return Err(invalid_build(&format!(
                "service '{service}' sets build.{setting}, which Ployz Builds cannot pass to BuildKit"
            )));
        }
    }
    if build
        .get("network")
        .and_then(Value::as_str)
        .is_some_and(|network| !matches!(network, "default" | "none"))
    {
        return Err(invalid_build(&format!(
            "service '{service}' sets host-specific build.network"
        )));
    }
    if let Some(extension) = build.get("x-bake").and_then(Value::as_mapping) {
        for key in extension.keys() {
            if key.as_str() != Some("no-cache-filter") {
                return Err(invalid_build(&format!(
                    "service '{service}' sets build.x-bake.{}, which a captured Build cannot preserve",
                    key.as_str().unwrap_or("unknown")
                )));
            }
        }
    }
    if build.get("extra_hosts").is_some_and(|hosts| {
        hosts.as_sequence().is_some_and(|hosts| {
            hosts.iter().any(|host| {
                host.as_str()
                    .is_some_and(|host| host.contains("host-gateway"))
            })
        }) || hosts.as_mapping().is_some_and(|hosts| {
            hosts
                .values()
                .any(|host| host.as_str() == Some("host-gateway"))
        })
    }) {
        return Err(invalid_build(&format!(
            "service '{service}' sets host-specific build.extra_hosts"
        )));
    }
    for setting in LOCAL_CACHE {
        let entries = build
            .get(Value::String((*setting).into()))
            .and_then(Value::as_sequence);
        for entry in entries.into_iter().flatten().filter_map(Value::as_str) {
            let kind = entry
                .split(',')
                .find_map(|field| field.trim().strip_prefix("type="))
                .map(|kind| kind.trim_end_matches('\\'));
            if kind.is_some_and(|kind| !matches!(kind, "registry" | "inline"))
                || (kind.is_none() && entry.contains('='))
            {
                return Err(invalid_build(&format!(
                    "service '{service}' sets host-specific build.{setting}; use a registry or inline cache"
                )));
            }
        }
    }
    Ok(())
}

/// Dockerfiles remain single-platform; Railpack assembles explicit variants.
fn requested_platforms(
    service: &str,
    build: &serde_norway::Mapping,
    railpack: bool,
) -> Result<Vec<String>, ComposeError> {
    let Some(value) = build.get("platforms") else {
        return Ok(Vec::new());
    };
    let platforms = value
        .as_sequence()
        .ok_or_else(|| invalid_build("build.platforms must be a list"))?;
    if !railpack && platforms.len() > 1 {
        return Err(invalid_build(&format!(
            "service '{service}' requests {} build platforms; a Dockerfile Build produces one platform",
            platforms.len()
        )));
    }
    platforms
        .iter()
        .map(|platform| {
            platform
                .as_str()
                .filter(|p| !p.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    invalid_build(&format!(
                        "service '{service}' has an invalid build platform"
                    ))
                })
        })
        .collect()
}

/// Upstream translation uses explicit build tags alone; keep the Service image
/// tagged too, so a Deploy still finds the image its Service names.
fn retain_service_image_tag(
    service: &str,
    image: String,
    build: &mut serde_norway::Mapping,
) -> Result<(), ComposeError> {
    let Some(declared) = build.get_mut(Value::String("tags".into())) else {
        return Ok(());
    };
    let tags = declared
        .as_sequence_mut()
        .ok_or_else(|| invalid_build(&format!("service '{service}' has invalid build tags")))?;
    if !tags.iter().any(|tag| tag.as_str() == Some(image.as_str())) {
        tags.push(Value::String(image));
    }
    Ok(())
}

fn capture_context(
    source: &str,
    directory: &Path,
    dockerfile: Option<&Path>,
    inputs: &mut BuildInputs,
) -> Result<String, ComposeError> {
    if source.starts_with("service:") {
        return Ok(source.into());
    }
    if is_remote_context(source) {
        validate_remote_context(source)?;
        return Ok(source.into());
    }
    let captured = inputs.context(&directory.join(source), dockerfile)?;
    Ok(inputs.relative(&captured).to_string_lossy().into_owned())
}

fn validate_remote_context(source: &str) -> Result<(), ComposeError> {
    ployz_build::remote::validate_remote_context(source)
        .map_err(|message| invalid_build(&message.to_string()))
}

fn ssh_paths(key: &Value) -> Result<(&str, &str), ComposeError> {
    key.as_str()
        .and_then(|key| key.split_once(": ").or_else(|| key.split_once('=')))
        .filter(|(id, paths)| !id.is_empty() && paths.split(',').all(|path| !path.is_empty()))
        .ok_or_else(|| {
            invalid_build("SSH agent sockets cannot be frozen; use an explicit key file")
        })
}

pub(super) fn is_remote_context(source: &str) -> bool {
    source.contains("://") || source.starts_with("git@") || source.starts_with("service:")
}

fn is_ssh_context(source: &str) -> bool {
    source.starts_with("ssh://") || source.starts_with("git@")
}

fn invalid_build(message: &str) -> ComposeError {
    ComposeError::Invalid(message.into())
}

#[derive(Serialize)]
struct BuildServiceOverride<'a> {
    image: &'a str,
    build: &'a Value,
}

#[derive(Serialize)]
struct BuildSecret {
    file: String,
}

#[derive(Serialize)]
struct BuildOverride<'a> {
    services: BTreeMap<&'a str, BuildServiceOverride<'a>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    secrets: BTreeMap<String, BuildSecret>,
}

fn include_service<'a>(
    project: &'a ComposeProject,
    name: &'a str,
    deps: bool,
    visiting: &mut BTreeSet<&'a str>,
    seen: &mut BTreeSet<&'a str>,
    selected: &mut Vec<&'a str>,
) -> Result<(), ComposeError> {
    if !project.services.contains_key(name) {
        return Err(ComposeError::Invalid(format!("undefined service '{name}'")));
    }
    if visiting.contains(name) {
        return Err(ComposeError::Invalid(format!(
            "build dependency cycle at service '{name}'"
        )));
    }
    if seen.contains(name) {
        return Ok(());
    }
    visiting.insert(name);
    if let Some(build) = project.builds.get(name) {
        for dependency in build.additional_services() {
            include_service(project, dependency, deps, visiting, seen, selected)?;
        }
    }
    if deps {
        for dependency in project.dependencies.get(name).into_iter().flatten() {
            include_service(
                project,
                dependency.service.as_str(),
                true,
                visiting,
                seen,
                selected,
            )?;
        }
    }
    selected.push(name);
    visiting.remove(name);
    seen.insert(name);
    Ok(())
}

fn build_service(project: &ComposeProject, name: &str) -> Result<BuildService, ComposeError> {
    let service = project
        .services
        .get(name)
        .ok_or_else(|| ComposeError::Invalid(format!("undefined service '{name}'")))?;
    Ok(BuildService {
        name: name.to_owned(),
        image: service.container.image.clone(),
        build: project
            .builds
            .get(name)
            .expect("build services come from the build map")
            .raw
            .clone(),
        placement: service.placement.clone(),
    })
}

impl BuildSpec {
    /// Service names referenced as `service:` additional build contexts.
    #[must_use]
    pub(crate) fn additional_services(&self) -> Vec<&str> {
        let Value::Mapping(map) = &self.raw else {
            return Vec::new();
        };
        let Some(contexts) = map.get(Value::String("additional_contexts".into())) else {
            return Vec::new();
        };
        match contexts {
            Value::Mapping(map) => map
                .values()
                .filter_map(Value::as_str)
                .filter_map(|context| context.strip_prefix("service:"))
                .collect(),
            Value::Sequence(values) => values
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|value| value.split_once('=').map(|(_, context)| context))
                .filter_map(|context| context.strip_prefix("service:"))
                .collect(),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Tagged(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_multi_platform_attestations_pass_remote_validation() {
        let root = std::env::temp_dir().join(format!(
            "ployz-disabled-attestations-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for settings in [
            "provenance: false",
            "sbom: false",
            "provenance: false, sbom: false",
        ] {
            let mut project = crate::compose::parse_normalized(
                &format!("services:\n  api:\n    build: {{context: ., x-recipe: railpack, platforms: [linux/amd64, linux/arm64], {settings}}}\n"),
                &root,
            ).unwrap();
            let options = BuildOptions::default();
            let plan = plan_build(&project, &options).unwrap();
            let captured = capture_build(&plan, &options, &mut project).unwrap();
            let definition = ployz_build::remote::Definition {
                image_contexts: Default::default(),
                retained_tags: Vec::new(),
                targets: captured.targets,
                output: options.output,
                no_cache: options.no_cache,
                pull: options.pull,
            };
            let recipes =
                ployz_build::remote::validate_capture(captured.inputs.root(), &definition)
                    .unwrap_or_else(|error| panic!("{settings}: {error}"));
            assert_eq!(recipes.len(), 1);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_context_suffix_rules_follow_buildkit_transports() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        for (repository, accepted) in [
            ("git://example.test/repo", true),
            ("ssh://git@example.test/repo", true),
            ("git@example.test:repo", true),
            ("https://github.com/moby/buildkit.git", true),
            ("https://github.com/moby/buildkit", false),
            ("https://example.test/source.tar.gz", false),
        ] {
            assert_eq!(
                validate_remote_context(&format!("{repository}#{commit}:src")).is_ok(),
                accepted,
                "{repository}"
            );
            assert!(validate_remote_context(&format!("{repository}#main")).is_err());
        }
    }
}
