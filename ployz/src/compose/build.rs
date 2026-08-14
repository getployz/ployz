use std::collections::BTreeSet;

use ployz_core::MachineSelector;
use serde::Serialize;

use super::{
    ComposeError, ComposeProject, LoadOptions,
    loader::{TemporaryComposeFile, compose_command},
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildService {
    pub name: String,
    pub image: String,
    pub machines: Vec<MachineSelector>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildPlan {
    pub services: Vec<BuildService>,
}

impl BuildPlan {
    #[must_use]
    pub fn service_names(&self) -> Vec<&str> {
        self.services
            .iter()
            .map(|service| service.name.as_str())
            .collect()
    }
}

pub fn plan_build(
    project: &ComposeProject,
    options: &BuildOptions,
) -> Result<BuildPlan, ComposeError> {
    if options.services.is_empty() {
        return Ok(BuildPlan {
            services: project
                .builds
                .keys()
                .map(|name| build_service(project, name))
                .collect::<Result<_, _>>()?,
        });
    }

    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for name in &options.services {
        include_service(project, name, options.deps, &mut seen, &mut selected)?;
    }
    Ok(BuildPlan {
        services: selected
            .into_iter()
            .filter(|name| project.builds.contains_key(*name))
            .map(|name| build_service(project, name))
            .collect::<Result<_, _>>()?,
    })
}

pub fn execute_build(
    project: &ComposeProject,
    plan: &BuildPlan,
    options: &BuildOptions,
    load: &LoadOptions,
) -> Result<(), ComposeError> {
    if plan.services.is_empty() {
        return Ok(());
    }
    let override_file = TemporaryComposeFile::new(&build_override(project, plan)?)?;
    let docker = load
        .docker
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("docker"));
    let mut command = compose_command(docker, load, None)?;
    command.arg("--file").arg(&override_file.path).arg("build");
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
    command.args(plan.service_names());
    let output = command
        .output()
        .map_err(|error| ComposeError::Io(format!("run Docker Compose build: {error}")))?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(())
    } else {
        Err(ComposeError::Compose(format!(
            "Docker Compose build exited with {}",
            output.status
        )))
    }
}

#[derive(Serialize)]
struct ImageOverride<'a> {
    image: &'a str,
}

#[derive(Serialize)]
struct BuildOverride<'a> {
    services: std::collections::BTreeMap<&'a str, ImageOverride<'a>>,
}

fn build_override(project: &ComposeProject, plan: &BuildPlan) -> Result<String, ComposeError> {
    let services = plan
        .services
        .iter()
        .map(|service| {
            let image = &project
                .services
                .get(&service.name)
                .ok_or_else(|| {
                    ComposeError::Invalid(format!("undefined service '{}'", service.name))
                })?
                .container
                .image;
            Ok((service.name.as_str(), ImageOverride { image }))
        })
        .collect::<Result<_, ComposeError>>()?;
    serde_norway::to_string(&BuildOverride { services })
        .map_err(|error| ComposeError::Io(format!("encode Compose build override: {error}")))
}

fn include_service<'a>(
    project: &'a ComposeProject,
    name: &'a str,
    deps: bool,
    seen: &mut BTreeSet<&'a str>,
    selected: &mut Vec<&'a str>,
) -> Result<(), ComposeError> {
    if !project.services.contains_key(name) {
        return Err(ComposeError::Invalid(format!("undefined service '{name}'")));
    }
    if !seen.insert(name) {
        return Ok(());
    }
    selected.push(name);
    if let Some(build) = project.builds.get(name) {
        for dependency in &build.additional_services {
            include_service(project, dependency, deps, seen, selected)?;
        }
    }
    if deps {
        for dependency in project.dependencies.get(name).into_iter().flatten() {
            include_service(project, dependency, true, seen, selected)?;
        }
    }
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
        machines: service.placement.machines.clone(),
    })
}
