use std::collections::BTreeSet;

use ployz_core::MachineTarget;
use serde::Serialize;
use serde_norway::Value;

use super::{
    BuildSpec, ComposeError, ComposeProject, LoadOptions,
    loader::{
        TemporaryComposeFile, compose_command, discover_default_compose_file,
        first_compose_file_from_environment,
    },
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildOptions {
    pub build_args: Vec<String>,
    pub check: bool,
    pub deps: bool,
    pub no_cache: bool,
    pub pull: bool,
    pub push_registry: bool,
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

pub fn execute_build(
    plan: &[BuildService],
    options: &BuildOptions,
    load: &LoadOptions,
) -> Result<(), ComposeError> {
    if plan.is_empty() {
        return Ok(());
    }
    let mut load = load.clone();
    if load.files.is_empty() && first_compose_file_from_environment().is_none() {
        load.files.push(discover_default_compose_file(&load)?);
    }
    let override_file = TemporaryComposeFile::new(&build_override(plan)?)?;
    let docker = load
        .docker
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("docker"));
    let mut command = compose_command(docker, &load, Some(&override_file))?;
    command.arg("build");
    for argument in &options.build_args {
        command.arg("--build-arg").arg(argument);
    }
    for (enabled, flag) in [
        (options.check, "--check"),
        (options.no_cache, "--no-cache"),
        (options.pull, "--pull"),
        (options.push_registry, "--push"),
    ] {
        if enabled {
            command.arg(flag);
        }
    }
    command.args(plan.iter().map(|service| service.name.as_str()));
    let status = command
        .status()
        .map_err(|error| ComposeError::Io(format!("run Docker Compose build: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(ComposeError::Compose(format!(
            "Docker Compose build exited with {status}"
        )))
    }
}

#[derive(Serialize)]
struct BuildServiceOverride<'a> {
    image: &'a str,
    build: &'a Value,
}

#[derive(Serialize)]
struct BuildOverride<'a> {
    services: std::collections::BTreeMap<&'a str, BuildServiceOverride<'a>>,
}

fn build_override(plan: &[BuildService]) -> Result<String, ComposeError> {
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
    serde_norway::to_string(&BuildOverride { services })
        .map_err(|error| ComposeError::Io(format!("encode Compose build override: {error}")))
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
