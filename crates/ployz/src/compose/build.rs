use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use ployz_build::{BuiltImage, Output};
use ployz_core::MachineTarget;
use serde::Serialize;
use serde_norway::Value;

use super::{BuildSpec, ComposeError, ComposeProject, LoadOptions, build_inputs::BuildInputs};

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
    pub machines: Vec<MachineTarget>,
}

pub fn plan_build(
    project: &ComposeProject,
    options: &BuildOptions,
) -> Result<Vec<BuildService>, ComposeError> {
    if options.services.is_empty() {
        return project
            .builds
            .keys()
            .map(|name| build_service(project, name))
            .collect();
    }

    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    for name in &options.services {
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
    options: BuildOptions,
    environment: BTreeMap<String, String>,
    inputs: BuildInputs,
}

/// A Service whose image this command built, bound to the content produced.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltService {
    /// Reference the Service requested, used when the image is published.
    pub image: String,
    /// Machines this Service is placed on.
    pub machines: Vec<MachineTarget>,
    /// The image this command built for it.
    pub built: BuiltImage,
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
    for service in &mut plan {
        let image = service.image.clone();
        let name = service.name.clone();
        let build = service
            .build
            .as_mapping_mut()
            .ok_or_else(|| invalid_build("expected a build mapping"))?;
        refuse_unpassable_settings(&name, build)?;
        let platform = requested_platform(&name, build)?.or_else(|| {
            project
                .environment
                .get("DOCKER_DEFAULT_PLATFORM")
                .filter(|platform| !platform.is_empty())
                .cloned()
        });
        if let Some(platform) = &platform {
            build.insert(
                Value::String("platforms".into()),
                Value::Sequence(vec![Value::String(platform.clone())]),
            );
        }
        targets.push(ployz_build::Target { name, platform });
        retain_service_image_tag(&service.name, image, build)?;
        let args = effective_build_args(&service.name, build, &options.build_args, project)?;
        build.insert(Value::String("args".into()), Value::Mapping(args));
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
        let context = build
            .get(Value::String("context".into()))
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_owned();
        ssh_agent |= is_ssh_context(&context);
        let dockerfile = (!build.contains_key(Value::String("dockerfile_inline".into()))
            && !is_remote_context(&context))
        .then(|| {
            project.working_dir.join(&context).join(
                build
                    .get(Value::String("dockerfile".into()))
                    .and_then(Value::as_str)
                    .unwrap_or("Dockerfile"),
            )
        });
        let captured = capture_context(
            &context,
            &project.working_dir,
            dockerfile.as_deref(),
            &mut inputs,
        )?;
        build.insert(Value::String("context".into()), Value::String(captured));
        if let Some(source) = dockerfile {
            build.insert(
                Value::String("dockerfile".into()),
                Value::String({
                    let file = inputs.dockerfile(&source)?;
                    // Both context and recipe live directly beneath source/.
                    Path::new("..")
                        .join(file.file_name().expect("captured recipe"))
                        .to_string_lossy()
                        .into_owned()
                }),
            );
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
    })
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
    /// Execute this capture on one resolved Machine over the authenticated
    /// Ployz stream. No local Docker or local image store is consulted.
    pub async fn execute_remote(
        self,
        client: &crate::connect::Client,
        machine_id: ployz_core::MachineId,
        cancellation: tokio_util::sync::CancellationToken,
        progress: impl Fn(ployz_build::Progress),
    ) -> ployz_build::remote::Outcome {
        let definition = ployz_build::remote::Definition {
            targets: self.targets,
            output: self.options.output,
            no_cache: self.options.no_cache,
            pull: self.options.pull,
        };
        super::remote_build::execute(
            self.inputs,
            definition,
            client,
            machine_id,
            cancellation,
            progress,
        )
        .await
    }

    /// Build this capture through the shared runner, without reading the
    /// original sources again.
    ///
    /// # Errors
    /// Fails if the runner cannot execute the build or the result cannot be
    /// bound to the content it produced.
    pub fn execute(&self, docker: Option<&Path>) -> Result<Vec<BuiltService>, ComposeError> {
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
        let images = ployz_build::execute(&ployz_build::Request {
            compose_file: Path::new("compose.yaml"),
            working_dir: self.inputs.root(),
            environment: &environment,
            docker,
            targets: &self.targets,
            build_args: &self.options.build_args,
            output: self.options.output,
            no_cache: self.options.no_cache,
            pull: self.options.pull,
        })
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
                image: service.image.clone(),
                machines: service.machines.clone(),
                built,
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
) -> Result<Vec<BuiltService>, ComposeError> {
    capture_build(plan, options, project)?.execute(load.docker.as_deref())
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

/// The single platform this Service asks for, if it asks for one.
///
/// A Dockerfile Build produces one image for one platform, so several
/// requested platforms are refused here, where the Service is named.
fn requested_platform(
    service: &str,
    build: &serde_norway::Mapping,
) -> Result<Option<String>, ComposeError> {
    let Some(platforms) = build
        .get(Value::String("platforms".into()))
        .and_then(Value::as_sequence)
    else {
        return Ok(None);
    };
    match platforms.as_slice() {
        [] => Ok(None),
        [platform] => platform
            .as_str()
            .map(ToOwned::to_owned)
            .map(Some)
            .ok_or_else(|| {
                invalid_build(&format!(
                    "service '{service}' has an invalid build platform"
                ))
            }),
        several => Err(invalid_build(&format!(
            "service '{service}' requests {} build platforms; a Dockerfile Build produces one platform",
            several.len()
        ))),
    }
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

fn is_remote_context(source: &str) -> bool {
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
    selected.push(name);
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
        machines: service.placement.machines.clone(),
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
