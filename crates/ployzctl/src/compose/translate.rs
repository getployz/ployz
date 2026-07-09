use std::collections::BTreeMap;
use std::path::Path;

use ployz_core::deploy::{
    ContainerCommand, ContainerEntrypoint, ContainerRuntimeSpec, DeployRoute, DeployServiceSpec,
    EnvName, EnvValue, ImageReference, ReplicaCount, ServiceEnvironment, StopGracePeriod,
};
use ployz_core::ids::ServiceId;
use serde_yaml::Value;

use super::diagnostics::{ComposeFinding, ComposePath, KnownUnsupported, classify_service_key};
use super::env_files::{EnvFileMergeIssue, EnvFileRequired, EnvFileSource, merge_env_files};
use super::model::{
    ComposeCommand, ComposeDeploy, ComposeEnvFile, ComposeEnvFileEntry, ComposeEnvironment,
    ComposeRoutes, ComposeService,
};
use crate::commands::deploy::parse_route_shorthand;

const DEFAULT_REPLICA_COUNT: u16 = 1;

pub(crate) struct ServiceTranslateInput<'a> {
    pub name: String,
    pub service: ComposeService,
    pub base_dir: &'a Path,
    pub resolution_env: &'a BTreeMap<String, String>,
}

pub(crate) fn classify_service(
    input: ServiceTranslateInput<'_>,
) -> (Vec<ComposeFinding>, Option<DeployServiceSpec>) {
    let ServiceTranslateInput {
        name,
        service,
        base_dir,
        resolution_env,
    } = input;
    let ComposeService {
        image,
        command,
        entrypoint,
        environment,
        env_file,
        deploy,
        stop_grace_period,
        x_route,
        build,
        cap_add,
        cap_drop,
        cgroup_parent,
        configs,
        depends_on,
        devices,
        dns,
        dns_search,
        expose,
        extra_hosts,
        healthcheck,
        init,
        labels,
        logging,
        networks,
        platform,
        ports,
        privileged,
        profiles,
        pull_policy,
        restart,
        secrets,
        security_opt,
        ulimits,
        user,
        volumes,
        working_dir,
        x_pre_deploy,
        unrecognized,
    } = service;
    let service_path = ComposePath::root().field("services").field(&name);
    let mut findings = Vec::new();
    push_if_some(
        &mut findings,
        &service_path,
        "build",
        build,
        KnownUnsupported::Build,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "cap_add",
        cap_add,
        KnownUnsupported::CapAdd,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "cap_drop",
        cap_drop,
        KnownUnsupported::CapDrop,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "cgroup_parent",
        cgroup_parent,
        KnownUnsupported::CgroupParent,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "configs",
        configs,
        KnownUnsupported::Configs,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "depends_on",
        depends_on,
        KnownUnsupported::DependsOn,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "devices",
        devices,
        KnownUnsupported::Devices,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "dns",
        dns,
        KnownUnsupported::Dns,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "dns_search",
        dns_search,
        KnownUnsupported::DnsSearch,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "expose",
        expose,
        KnownUnsupported::Expose,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "extra_hosts",
        extra_hosts,
        KnownUnsupported::ExtraHosts,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "healthcheck",
        healthcheck,
        KnownUnsupported::Healthcheck,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "init",
        init,
        KnownUnsupported::Init,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "labels",
        labels,
        KnownUnsupported::Labels,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "logging",
        logging,
        KnownUnsupported::Logging,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "networks",
        networks,
        KnownUnsupported::Networks,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "platform",
        platform,
        KnownUnsupported::Platform,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "ports",
        ports,
        KnownUnsupported::Ports,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "privileged",
        privileged,
        KnownUnsupported::Privileged,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "profiles",
        profiles,
        KnownUnsupported::Profiles,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "pull_policy",
        pull_policy,
        KnownUnsupported::PullPolicy,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "restart",
        restart,
        KnownUnsupported::Restart,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "secrets",
        secrets,
        KnownUnsupported::Secrets,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "security_opt",
        security_opt,
        KnownUnsupported::SecurityOpt,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "ulimits",
        ulimits,
        KnownUnsupported::Ulimits,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "user",
        user,
        KnownUnsupported::User,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "volumes",
        volumes,
        KnownUnsupported::Volumes,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "working_dir",
        working_dir,
        KnownUnsupported::WorkingDir,
    );
    push_if_some(
        &mut findings,
        &service_path,
        "x-pre_deploy",
        x_pre_deploy,
        KnownUnsupported::XPreDeploy,
    );
    classify_unrecognized(&mut findings, &service_path, unrecognized);

    let (replicas, deploy_valid) = classify_deploy(deploy, &service_path, &mut findings);
    let service_id = ServiceId::try_new(name.clone()).map_err(|error| {
        ComposeFinding::invalid(
            service_path.clone(),
            format!("service id {name:?}: {error}"),
        )
    });
    let image = match image {
        Some(image) => ImageReference::try_new(image)
            .map_err(|error| ComposeFinding::invalid(service_path.field("image"), error)),
        None => Err(ComposeFinding::invalid(
            service_path.field("image"),
            "service image is required",
        )),
    };
    let command = match command {
        Some(command) => parse_command(command, &service_path.field("command")),
        None => Ok(None),
    };
    let entrypoint = match entrypoint {
        Some(entrypoint) => parse_entrypoint(entrypoint, &service_path.field("entrypoint")),
        None => Ok(None),
    };
    let environment = parse_environment(
        env_file,
        environment,
        resolution_env,
        base_dir,
        &service_path,
        &mut findings,
    );
    let stop_grace_period = parse_stop_grace_period(stop_grace_period, &service_path);
    let routes = parse_routes(x_route, &service_path);

    let mut service_valid = true;
    let service_id = match service_id {
        Ok(service_id) => service_id,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            ServiceId::try_new("invalid").expect("literal service id is valid")
        }
    };
    let image = match image {
        Ok(image) => image,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            ImageReference::try_new("invalid:latest").expect("literal image is valid")
        }
    };
    let command = match command {
        Ok(command) => command,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            None
        }
    };
    let entrypoint = match entrypoint {
        Ok(entrypoint) => entrypoint,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            None
        }
    };
    let environment = match environment {
        Some(environment) => environment,
        None => {
            service_valid = false;
            ServiceEnvironment::empty()
        }
    };
    let stop_grace_period = match stop_grace_period {
        Ok(stop_grace_period) => stop_grace_period,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            StopGracePeriod::default_grace()
        }
    };
    let routes = match routes {
        Ok(routes) => routes,
        Err(mut route_findings) => {
            findings.append(&mut route_findings);
            service_valid = false;
            Vec::new()
        }
    };

    let spec = if service_valid && deploy_valid {
        Some(DeployServiceSpec {
            service_id,
            image,
            replicas,
            runtime: ContainerRuntimeSpec {
                command,
                entrypoint,
                environment,
                stop_grace_period,
                volume_mounts: Vec::new(),
            },
            routes,
        })
    } else {
        None
    };
    (findings, spec)
}

fn push_if_some(
    findings: &mut Vec<ComposeFinding>,
    service_path: &ComposePath,
    field: &str,
    value: Option<Value>,
    feature: KnownUnsupported,
) {
    if value.is_some() {
        findings.push(ComposeFinding::unsupported(
            service_path.field(field),
            feature,
        ));
    }
}

fn classify_unrecognized(
    findings: &mut Vec<ComposeFinding>,
    service_path: &ComposePath,
    unrecognized: BTreeMap<String, Value>,
) {
    for (key, _value) in unrecognized {
        match classify_service_key(&key) {
            Some(feature) => findings.push(ComposeFinding::unsupported(
                service_path.field(&key),
                feature,
            )),
            None => findings.push(ComposeFinding::unknown(service_path.field(&key))),
        }
    }
}

fn classify_deploy(
    deploy: Option<ComposeDeploy>,
    service_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> (ReplicaCount, bool) {
    let Some(deploy) = deploy else {
        return (
            ReplicaCount::try_new(DEFAULT_REPLICA_COUNT).expect("one replica is valid"),
            true,
        );
    };
    let ComposeDeploy {
        replicas,
        mode,
        placement,
        resources,
        restart_policy,
        update_config,
        unrecognized,
    } = deploy;
    let deploy_path = service_path.field("deploy");
    push_if_some(
        findings,
        &deploy_path,
        "mode",
        mode,
        KnownUnsupported::DeployMode,
    );
    push_if_some(
        findings,
        &deploy_path,
        "placement",
        placement,
        KnownUnsupported::DeployPlacement,
    );
    push_if_some(
        findings,
        &deploy_path,
        "resources",
        resources,
        KnownUnsupported::DeployResources,
    );
    push_if_some(
        findings,
        &deploy_path,
        "restart_policy",
        restart_policy,
        KnownUnsupported::DeployRestartPolicy,
    );
    push_if_some(
        findings,
        &deploy_path,
        "update_config",
        update_config,
        KnownUnsupported::DeployUpdateConfig,
    );
    for (key, _value) in unrecognized {
        match classify_service_key(&format!("deploy.{key}")) {
            Some(feature) => findings.push(ComposeFinding::unsupported(
                deploy_path.field(&key),
                feature,
            )),
            None => findings.push(ComposeFinding::unknown(deploy_path.field(&key))),
        }
    }
    let replicas = match replicas {
        Some(value) => match parse_replica_count_value(value) {
            Ok(value) => value,
            Err(message) => {
                findings.push(ComposeFinding::invalid(
                    deploy_path.field("replicas"),
                    message,
                ));
                return (
                    ReplicaCount::try_new(DEFAULT_REPLICA_COUNT).expect("one replica is valid"),
                    false,
                );
            }
        },
        None => DEFAULT_REPLICA_COUNT,
    };
    match ReplicaCount::try_new(replicas) {
        Ok(replicas) => (replicas, true),
        Err(error) => {
            findings.push(ComposeFinding::invalid(
                deploy_path.field("replicas"),
                error,
            ));
            (
                ReplicaCount::try_new(DEFAULT_REPLICA_COUNT).expect("one replica is valid"),
                false,
            )
        }
    }
}

fn parse_replica_count_value(value: Value) -> Result<u16, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| "replicas must be a non-negative integer".to_owned())
            .and_then(|value| u16::try_from(value).map_err(|_| "replicas exceeds u16".to_owned())),
        Value::String(value) => value
            .parse::<u16>()
            .map_err(|error| format!("replicas must be an integer: {error}")),
        Value::Null
        | Value::Bool(_)
        | Value::Sequence(_)
        | Value::Mapping(_)
        | Value::Tagged(_) => Err("replicas must be an integer".to_owned()),
    }
}

fn parse_command(
    command: ComposeCommand,
    path: &ComposePath,
) -> Result<Option<ContainerCommand>, ComposeFinding> {
    let argv = match command {
        ComposeCommand::Shell(value) => shell_words::split(&value)
            .map_err(|error| ComposeFinding::invalid(path.clone(), error))?,
        ComposeCommand::Exec(items) => exec_argv(items, path)?,
    };
    ContainerCommand::try_new(argv)
        .map(Some)
        .map_err(|error| ComposeFinding::invalid(path.clone(), error))
}

fn exec_argv(items: Vec<Value>, path: &ComposePath) -> Result<Vec<String>, ComposeFinding> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            scalar_to_string(&item).ok_or_else(|| {
                ComposeFinding::invalid(path.index(index), "exec-form items must be scalar values")
            })
        })
        .collect()
}

fn parse_entrypoint(
    entrypoint: ComposeCommand,
    path: &ComposePath,
) -> Result<Option<ContainerEntrypoint>, ComposeFinding> {
    match entrypoint {
        ComposeCommand::Shell(value) if value.is_empty() => Ok(Some(ContainerEntrypoint::Clear)),
        ComposeCommand::Shell(value) => {
            let argv = shell_words::split(&value)
                .map_err(|error| ComposeFinding::invalid(path.clone(), error))?;
            ContainerCommand::try_new(argv)
                .map(|argv| Some(ContainerEntrypoint::Argv(argv)))
                .map_err(|error| ComposeFinding::invalid(path.clone(), error))
        }
        ComposeCommand::Exec(items) if items.is_empty() => Ok(Some(ContainerEntrypoint::Clear)),
        ComposeCommand::Exec(items) => ContainerCommand::try_new(exec_argv(items, path)?)
            .map(|argv| Some(ContainerEntrypoint::Argv(argv)))
            .map_err(|error| ComposeFinding::invalid(path.clone(), error)),
    }
}

fn parse_environment(
    env_file: Option<ComposeEnvFile>,
    environment: Option<ComposeEnvironment>,
    resolution_env: &BTreeMap<String, String>,
    base_dir: &Path,
    service_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> Option<ServiceEnvironment> {
    let findings_before = findings.len();
    let env_sources = load_env_file_sources(env_file, base_dir, service_path, findings);
    let (mut merged, issues) = merge_env_files(&env_sources);
    for issue in issues {
        match issue {
            EnvFileMergeIssue::MissingRequired { path } => {
                findings.push(ComposeFinding::invalid(
                    service_path.field("env_file"),
                    format!("env file {path:?} was not found"),
                ));
            }
            EnvFileMergeIssue::Parse { path, message } => {
                findings.push(ComposeFinding::invalid(
                    service_path.field("env_file"),
                    format!("env file {path:?}: {message}"),
                ));
            }
        }
    }
    if let Some(environment) = environment {
        apply_environment(
            environment,
            &mut merged,
            resolution_env,
            service_path,
            findings,
        );
    }
    let mut typed = BTreeMap::new();
    for (name, value) in merged {
        let name = match EnvName::try_new(name.clone()) {
            Ok(name) => name,
            Err(error) => {
                findings.push(ComposeFinding::invalid(
                    service_path.field("environment").field(&name),
                    error,
                ));
                continue;
            }
        };
        let value = match EnvValue::try_new(value) {
            Ok(value) => value,
            Err(error) => {
                findings.push(ComposeFinding::invalid(
                    service_path.field("environment").field(name.as_str()),
                    error,
                ));
                continue;
            }
        };
        typed.insert(name, value);
    }
    if findings.iter().skip(findings_before).any(|finding| {
        matches!(
            finding.kind,
            super::diagnostics::ComposeFindingKind::InvalidValue { .. }
        )
    }) {
        return None;
    }
    Some(ServiceEnvironment::from(typed))
}

fn load_env_file_sources(
    env_file: Option<ComposeEnvFile>,
    base_dir: &Path,
    service_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> Vec<EnvFileSource> {
    let Some(env_file) = env_file else {
        return Vec::new();
    };
    let entries = match env_file {
        ComposeEnvFile::One(path) => vec![ComposeEnvFileEntry::Path(path)],
        ComposeEnvFile::Many(entries) => entries,
    };
    entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let path = service_path.field("env_file").index(index);
            match entry {
                ComposeEnvFileEntry::Path(path_text) => {
                    read_env_file(base_dir, path_text, EnvFileRequired::Required)
                }
                ComposeEnvFileEntry::Options {
                    path: path_text,
                    required,
                    unrecognized,
                } => {
                    for (key, _value) in unrecognized {
                        findings.push(ComposeFinding::unknown(path.field(&key)));
                    }
                    read_env_file(
                        base_dir,
                        path_text,
                        match required {
                            Some(true) | None => EnvFileRequired::Required,
                            Some(false) => EnvFileRequired::Optional,
                        },
                    )
                }
            }
        })
        .collect()
}

fn read_env_file(base_dir: &Path, path: String, required: EnvFileRequired) -> EnvFileSource {
    let contents = std::fs::read_to_string(base_dir.join(&path)).ok();
    EnvFileSource {
        path,
        required,
        contents,
    }
}

fn apply_environment(
    environment: ComposeEnvironment,
    merged: &mut BTreeMap<String, String>,
    resolution_env: &BTreeMap<String, String>,
    service_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) {
    match environment {
        ComposeEnvironment::Map(values) => {
            for (name, value) in values {
                match value {
                    Value::Null => match resolution_env.get(&name) {
                        Some(value) => {
                            merged.insert(name, value.clone());
                        }
                        None => findings.push(ComposeFinding::advisory(
                            service_path.field("environment").field(&name),
                            "value is null and the variable is not set in the OS environment or .env; omitted",
                        )),
                },
                    other @ Value::Bool(_)
                    | other @ Value::Number(_)
                    | other @ Value::String(_)
                    | other @ Value::Sequence(_)
                    | other @ Value::Mapping(_)
                    | other @ Value::Tagged(_) => match scalar_to_string(&other) {
                        Some(value) => {
                            merged.insert(name, value);
                        }
                        None => findings.push(ComposeFinding::invalid(
                            service_path.field("environment").field(&name),
                            "environment value must be a scalar or null",
                        )),
                    },
                }
            }
        }
        ComposeEnvironment::List(values) => {
            for (index, item) in values.into_iter().enumerate() {
                match item.split_once('=') {
                    Some((name, value)) => {
                        merged.insert(name.to_owned(), value.to_owned());
                    }
                    None => match resolution_env.get(&item) {
                        Some(value) => {
                            merged.insert(item, value.clone());
                        }
                        None => findings.push(ComposeFinding::advisory(
                            service_path.field("environment").index(index),
                            "variable is not set in the OS environment or .env; omitted",
                        )),
                    },
                }
            }
        }
    }
}

fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => None,
    }
}

fn parse_stop_grace_period(
    value: Option<Value>,
    service_path: &ComposePath,
) -> Result<StopGracePeriod, ComposeFinding> {
    let Some(value) = value else {
        return Ok(StopGracePeriod::default_grace());
    };
    let seconds = match value {
        Value::Number(number) => number.as_u64().ok_or_else(|| {
            ComposeFinding::invalid(
                service_path.field("stop_grace_period"),
                "duration number must be a non-negative integer",
            )
        })?,
        Value::String(value) => parse_compose_duration(&value).map_err(|message| {
            ComposeFinding::invalid(service_path.field("stop_grace_period"), message)
        })?,
        Value::Null
        | Value::Bool(_)
        | Value::Sequence(_)
        | Value::Mapping(_)
        | Value::Tagged(_) => {
            return Err(ComposeFinding::invalid(
                service_path.field("stop_grace_period"),
                "duration must be a number of seconds or a Compose duration string",
            ));
        }
    };
    let seconds = u32::try_from(seconds).map_err(|_| {
        ComposeFinding::invalid(
            service_path.field("stop_grace_period"),
            "duration exceeds u32 seconds",
        )
    })?;
    Ok(StopGracePeriod::from(seconds))
}

fn parse_compose_duration(value: &str) -> Result<u64, String> {
    const MICROS_PER_SECOND: u64 = 1_000_000;
    if value.is_empty() {
        return Err("duration is empty".to_owned());
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return value
            .parse::<u64>()
            .map_err(|error| format!("invalid duration: {error}"));
    }
    let mut total_micros = 0_u64;
    let mut seen_units: Vec<&str> = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            return Err(format!("duration unit has no value in {value:?}"));
        }
        let amount = rest[..digits_end]
            .parse::<u64>()
            .map_err(|error| format!("invalid duration: {error}"))?;
        rest = &rest[digits_end..];
        let unit_end = rest
            .find(|ch: char| ch.is_ascii_digit())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = &rest[unit_end..];
        let micros_per_unit = match unit {
            "h" => 3600 * MICROS_PER_SECOND,
            "m" => 60 * MICROS_PER_SECOND,
            "s" => MICROS_PER_SECOND,
            "ms" => 1000,
            "us" => 1,
            "" => return Err("duration must end with a unit".to_owned()),
            other => return Err(format!("unsupported duration unit {other:?}")),
        };
        if seen_units.contains(&unit) {
            return Err(format!("duration repeats unit {unit:?}"));
        }
        seen_units.push(unit);
        total_micros = total_micros.saturating_add(amount.saturating_mul(micros_per_unit));
    }
    if !total_micros.is_multiple_of(MICROS_PER_SECOND) {
        return Err("stop grace period must be whole seconds".to_owned());
    }
    Ok(total_micros / MICROS_PER_SECOND)
}

fn parse_routes(
    routes: Option<ComposeRoutes>,
    service_path: &ComposePath,
) -> Result<Vec<DeployRoute>, Vec<ComposeFinding>> {
    let Some(routes) = routes else {
        return Ok(Vec::new());
    };
    let mut findings = Vec::new();
    let mut parsed = Vec::new();
    for (index, shorthand) in routes.into_shorthands().into_iter().enumerate() {
        match parse_route_shorthand(&shorthand) {
            Ok(route) => parsed.push(route),
            Err(error) => findings.push(ComposeFinding::invalid(
                service_path.field("x-route").index(index),
                error,
            )),
        }
    }
    if findings.is_empty() {
        Ok(parsed)
    } else {
        Err(findings)
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::deploy::{ContainerEntrypoint, StopGracePeriod};

    use super::parse_compose_duration;
    use crate::compose::diagnostics::UnsupportedFieldMode;
    use crate::compose::{ComposeInput, parse_deploy_file};

    #[test]
    fn parses_runtime_fields() {
        let parsed = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                command: "sh -c 'echo hi'"
                entrypoint: []
                environment:
                  A: from-map
                stop_grace_period: 1m30s
            "#,
            base_dir: std::path::Path::new("."),
            interpolation_env: std::collections::BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("compose parses")
        .0;

        let runtime = &parsed.services.first().expect("one parsed service").runtime;
        assert_eq!(
            runtime.command.as_ref().map(|cmd| cmd.as_slice().len()),
            Some(3)
        );
        assert_eq!(runtime.entrypoint, Some(ContainerEntrypoint::Clear));
        assert_eq!(runtime.stop_grace_period, StopGracePeriod::from(90));
        assert_eq!(
            runtime
                .environment
                .iter()
                .next()
                .map(|(name, value)| { (name.as_str().to_owned(), value.as_str().to_owned()) }),
            Some(("A".to_owned(), "from-map".to_owned()))
        );
    }

    #[test]
    fn parses_duration_to_whole_seconds() {
        assert_eq!(parse_compose_duration("1m30s"), Ok(90));
        assert_eq!(parse_compose_duration("10"), Ok(10));
        assert_eq!(parse_compose_duration("1h"), Ok(3600));
        assert_eq!(parse_compose_duration("1000ms"), Ok(1));
        assert_eq!(
            parse_compose_duration("2s500000us"),
            Err("stop grace period must be whole seconds".to_owned())
        );
    }

    #[test]
    fn rejects_sub_second_empty_and_repeated_unit_durations() {
        assert_eq!(
            parse_compose_duration("500ms"),
            Err("stop grace period must be whole seconds".to_owned())
        );
        assert_eq!(
            parse_compose_duration(""),
            Err("duration is empty".to_owned())
        );
        assert_eq!(
            parse_compose_duration("1m1m"),
            Err("duration repeats unit \"m\"".to_owned())
        );
        assert!(parse_compose_duration("1x").is_err());
        assert!(parse_compose_duration("1m30").is_err());
    }

    #[test]
    fn exec_form_coerces_scalars_and_string_entrypoint_clears() {
        let parsed = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                command: ["sleep", 600]
                entrypoint: ""
            "#,
            base_dir: std::path::Path::new("."),
            interpolation_env: std::collections::BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect("compose parses")
        .0;

        let runtime = &parsed.services.first().expect("one parsed service").runtime;
        assert_eq!(
            runtime.command.as_ref().map(|cmd| cmd.as_slice().to_vec()),
            Some(vec!["sleep".to_owned(), "600".to_owned()])
        );
        assert_eq!(runtime.entrypoint, Some(ContainerEntrypoint::Clear));
    }

    #[test]
    fn exec_form_non_scalar_item_is_invalid_value_not_serde_bail() {
        let error = parse_deploy_file(ComposeInput {
            source: r#"
            name: default
            services:
              web:
                image: nginx
                command: ["sleep", {a: b}]
            "#,
            base_dir: std::path::Path::new("."),
            interpolation_env: std::collections::BTreeMap::new(),
            namespace_override: None,
            mode: UnsupportedFieldMode::Strict,
        })
        .expect_err("non-scalar exec item rejects");

        let rendered = error.to_string();
        assert!(rendered.contains("services.web.command[1]"));
        assert!(rendered.contains("exec-form items must be scalar values"));
    }
}
