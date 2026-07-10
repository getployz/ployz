use std::collections::BTreeMap;
use std::path::Path;

use ployz_core::deploy::{
    ContainerCommand, ContainerEntrypoint, ContainerHealthcheck, ContainerHealthcheckTest,
    ContainerResourceLimits, ContainerRestartPolicy, ContainerRuntimeSpec, DeployRoute,
    DeployRouteTarget, DeployServiceSpec, EnvName, EnvValue, HealthcheckDurationNanos,
    HealthcheckRetries, HealthcheckShellCommand, ImageReference, LinuxCapability, MemoryBytes,
    NanoCpus, PidsLimit, PreStartHook, ReplicaCount, ServiceEnvironment, StopGracePeriod,
};
use ployz_core::ids::ServiceId;
use ployz_core::ops::{RouteHostname, RoutePort};
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
        x_ports,
        build,
        cap_add,
        cap_drop,
        cgroup_parent,
        configs,
        depends_on,
        pre_start,
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
    reject_standard_ports(&mut findings, &service_path, ports);
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
    classify_unrecognized(&mut findings, &service_path, unrecognized);

    let deploy_runtime = parse_deploy_runtime(deploy, &service_path, &mut findings);
    let (replicas, restart_policy, resources, deploy_valid) = deploy_runtime;
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
    let healthcheck = parse_healthcheck(healthcheck, &service_path);
    let restart_policy = parse_restart_policy(restart, restart_policy, &service_path);
    let cap_add = parse_capabilities(cap_add, &service_path.field("cap_add"));
    let cap_drop = parse_capabilities(cap_drop, &service_path.field("cap_drop"));
    let routes = parse_routes(x_route, x_ports, &service_path);
    let depends_on = parse_depends_on(depends_on, &service_path.field("depends_on"));
    let pre_start = parse_pre_start(pre_start, &service_path.field("pre_start"));

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
    let healthcheck = match healthcheck {
        Ok(healthcheck) => healthcheck,
        Err(mut health_findings) => {
            findings.append(&mut health_findings);
            service_valid = false;
            None
        }
    };
    let restart_policy = match restart_policy {
        Ok(restart_policy) => restart_policy,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            ContainerRestartPolicy::DockerDefault
        }
    };
    let cap_add = match cap_add {
        Ok(cap_add) => cap_add,
        Err(mut cap_findings) => {
            findings.append(&mut cap_findings);
            service_valid = false;
            Vec::new()
        }
    };
    let cap_drop = match cap_drop {
        Ok(cap_drop) => cap_drop,
        Err(mut cap_findings) => {
            findings.append(&mut cap_findings);
            service_valid = false;
            Vec::new()
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
    let depends_on = match depends_on {
        Ok(depends_on) => depends_on,
        Err(mut dependency_findings) => {
            findings.append(&mut dependency_findings);
            service_valid = false;
            Vec::new()
        }
    };
    let pre_start = match pre_start {
        Ok(pre_start) => pre_start,
        Err(finding) => {
            findings.push(finding);
            service_valid = false;
            None
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
                healthcheck,
                restart_policy,
                cap_add,
                cap_drop,
                resources,
            },
            pre_start,
            depends_on,
            routes,
        })
    } else {
        None
    };
    (findings, spec)
}

fn parse_depends_on(
    value: Option<Value>,
    path: &ComposePath,
) -> Result<Vec<ServiceId>, Vec<ComposeFinding>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut findings = Vec::new();
    let mut names = Vec::new();
    match value {
        Value::Sequence(values) => {
            for (index, value) in values.into_iter().enumerate() {
                let item_path = path.index(index);
                match value.as_str() {
                    Some(name) => names.push((item_path, name.to_owned())),
                    None => findings.push(ComposeFinding::invalid(
                        item_path,
                        "dependency names must be strings",
                    )),
                }
            }
        }
        Value::Mapping(values) => {
            for (key, _value) in values {
                match key.as_str() {
                    Some(name) => names.push((path.field(name), name.to_owned())),
                    None => findings.push(ComposeFinding::invalid(
                        path.clone(),
                        "dependency keys must be strings",
                    )),
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            return Err(vec![ComposeFinding::invalid(
                path.clone(),
                "depends_on must be a list or mapping",
            )]);
        }
    }

    let mut service_ids = Vec::with_capacity(names.len());
    for (name_path, name) in names {
        match ServiceId::try_new(name) {
            Ok(service_id) => service_ids.push(service_id),
            Err(error) => findings.push(ComposeFinding::invalid(
                name_path,
                format!("invalid service id: {error}"),
            )),
        }
    }

    if findings.is_empty() {
        Ok(service_ids)
    } else {
        Err(findings)
    }
}

fn parse_pre_start(
    value: Option<Value>,
    path: &ComposePath,
) -> Result<Option<PreStartHook>, ComposeFinding> {
    let Some(value) = value else {
        return Ok(None);
    };
    let command = match value {
        Value::String(command) => parse_hook_command(ComposeCommand::Shell(command), path)?,
        Value::Sequence(command) => parse_hook_command(ComposeCommand::Exec(command), path)?,
        Value::Mapping(mapping) => {
            let Some(command) = mapping.get(Value::String("command".to_owned())) else {
                return Err(ComposeFinding::invalid(
                    path.clone(),
                    "pre_start mapping needs command",
                ));
            };
            match command {
                Value::String(command) => parse_hook_command(
                    ComposeCommand::Shell(command.clone()),
                    &path.field("command"),
                )?,
                Value::Sequence(command) => parse_hook_command(
                    ComposeCommand::Exec(command.clone()),
                    &path.field("command"),
                )?,
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Mapping(_)
                | Value::Tagged(_) => {
                    return Err(ComposeFinding::invalid(
                        path.field("command"),
                        "hook command must be a string or list",
                    ));
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Tagged(_) => {
            return Err(ComposeFinding::invalid(
                path.clone(),
                "pre_start must be a string, list, or mapping",
            ));
        }
    };
    Ok(Some(PreStartHook { command }))
}

fn parse_hook_command(
    command: ComposeCommand,
    path: &ComposePath,
) -> Result<ContainerCommand, ComposeFinding> {
    parse_command(command, path)?
        .ok_or_else(|| ComposeFinding::invalid(path.clone(), "hook command must not be empty"))
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

fn reject_standard_ports(
    findings: &mut Vec<ComposeFinding>,
    service_path: &ComposePath,
    ports: Option<Value>,
) {
    let Some(ports) = ports else {
        return;
    };
    let ports_path = service_path.field("ports");
    let mut found_host_mode = false;
    reject_host_mode_ports(findings, &ports_path, &ports, &mut found_host_mode);
    if !found_host_mode {
        findings.push(ComposeFinding::invalid(
            ports_path,
            "standard Compose ports publish host ports; use x-ports for Ployz Route Bindings",
        ));
    }
}

fn reject_host_mode_ports(
    findings: &mut Vec<ComposeFinding>,
    path: &ComposePath,
    value: &Value,
    found_host_mode: &mut bool,
) {
    match value {
        Value::Sequence(values) => {
            for (index, value) in values.iter().enumerate() {
                reject_host_mode_ports(findings, &path.index(index), value, found_host_mode);
            }
        }
        Value::Mapping(mapping) => {
            if mapping.contains_key(Value::String("published".to_owned())) {
                *found_host_mode = true;
                findings.push(ComposeFinding::invalid(
                    path.field("published"),
                    "long-syntax published ports are host publishing; use x-ports without published",
                ));
            }
            if mapping
                .get(Value::String("mode".to_owned()))
                .and_then(scalar_to_string)
                .is_some_and(|mode| mode == "host")
            {
                *found_host_mode = true;
                findings.push(ComposeFinding::invalid(
                    path.field("mode"),
                    "mode: host publishes host ports; use x-ports for Ployz Route Bindings",
                ));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {}
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

fn parse_deploy_runtime(
    deploy: Option<ComposeDeploy>,
    service_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> (
    ReplicaCount,
    Option<ContainerRestartPolicy>,
    ContainerResourceLimits,
    bool,
) {
    let default = (
        ReplicaCount::try_new(DEFAULT_REPLICA_COUNT).expect("one replica is valid"),
        None,
        ContainerResourceLimits::default(),
        true,
    );
    let Some(deploy) = deploy else {
        return default;
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
    let resources = parse_deploy_resources(resources, &deploy_path, findings);
    let restart_policy = parse_deploy_restart_policy(restart_policy, &deploy_path, findings);
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
                    restart_policy,
                    resources,
                    false,
                );
            }
        },
        None => DEFAULT_REPLICA_COUNT,
    };
    match ReplicaCount::try_new(replicas) {
        Ok(replicas) => (replicas, restart_policy, resources, true),
        Err(error) => {
            findings.push(ComposeFinding::invalid(
                deploy_path.field("replicas"),
                error,
            ));
            (
                ReplicaCount::try_new(DEFAULT_REPLICA_COUNT).expect("one replica is valid"),
                restart_policy,
                resources,
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

fn parse_deploy_resources(
    value: Option<Value>,
    deploy_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> ContainerResourceLimits {
    let Some(value) = value else {
        return ContainerResourceLimits::default();
    };
    let path = deploy_path.field("resources");
    let Some(map) = value.as_mapping() else {
        findings.push(ComposeFinding::invalid(
            path,
            "resources must be a mapping with limits",
        ));
        return ContainerResourceLimits::default();
    };
    let mut resources = ContainerResourceLimits::default();
    for (key, value) in map {
        let Some(key) = key.as_str() else {
            findings.push(ComposeFinding::invalid(
                path.clone(),
                "resources keys must be strings",
            ));
            continue;
        };
        match key {
            "limits" => resources = parse_resource_limits(value, &path.field("limits"), findings),
            "reservations" => findings.push(ComposeFinding::unsupported(
                path.field("reservations"),
                KnownUnsupported::DeployResources,
            )),
            other => findings.push(ComposeFinding::unknown(path.field(other))),
        }
    }
    resources
}

fn parse_resource_limits(
    value: &Value,
    path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> ContainerResourceLimits {
    let Some(map) = value.as_mapping() else {
        findings.push(ComposeFinding::invalid(
            path.clone(),
            "limits must be a mapping",
        ));
        return ContainerResourceLimits::default();
    };
    let mut limits = ContainerResourceLimits::default();
    for (key, value) in map {
        let Some(key) = key.as_str() else {
            findings.push(ComposeFinding::invalid(
                path.clone(),
                "limits keys must be strings",
            ));
            continue;
        };
        match key {
            "cpus" => match parse_nano_cpus(value) {
                Ok(value) => limits.nano_cpus = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            "memory" => match parse_memory_bytes(value) {
                Ok(value) => limits.memory_bytes = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            "pids" => match parse_pids_limit(value) {
                Ok(value) => limits.pids = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            other => findings.push(ComposeFinding::unknown(path.field(other))),
        }
    }
    limits
}

fn parse_deploy_restart_policy(
    value: Option<Value>,
    deploy_path: &ComposePath,
    findings: &mut Vec<ComposeFinding>,
) -> Option<ContainerRestartPolicy> {
    let value = value?;
    let path = deploy_path.field("restart_policy");
    match value {
        Value::Mapping(map) => {
            let mut condition = None;
            for (key, value) in map {
                let Some(key) = key.as_str() else {
                    findings.push(ComposeFinding::invalid(
                        path.clone(),
                        "restart_policy keys must be strings",
                    ));
                    continue;
                };
                match key {
                    "condition" => match scalar_to_string(&value) {
                        Some(value) => match parse_restart_policy_text(&value) {
                            Ok(policy) => condition = Some(policy),
                            Err(message) => {
                                findings.push(ComposeFinding::invalid(path.field(key), message));
                            }
                        },
                        None => findings.push(ComposeFinding::invalid(
                            path.field(key),
                            "restart condition must be a string",
                        )),
                    },
                    other => findings.push(ComposeFinding::unsupported(
                        path.field(other),
                        KnownUnsupported::DeployRestartPolicy,
                    )),
                }
            }
            condition
        }
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Sequence(_)
        | Value::Tagged(_) => {
            findings.push(ComposeFinding::invalid(
                path,
                "restart_policy must be a mapping",
            ));
            None
        }
    }
}

fn parse_healthcheck(
    value: Option<Value>,
    service_path: &ComposePath,
) -> Result<Option<ContainerHealthcheck>, Vec<ComposeFinding>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let path = service_path.field("healthcheck");
    let Some(map) = value.as_mapping() else {
        return Err(vec![ComposeFinding::invalid(
            path,
            "healthcheck must be a mapping",
        )]);
    };
    let mut findings = Vec::new();
    let mut test = None;
    let mut disabled = false;
    let mut interval = None;
    let mut timeout = None;
    let mut retries = None;
    let mut start_period = None;
    for (key, value) in map {
        let Some(key) = key.as_str() else {
            findings.push(ComposeFinding::invalid(
                path.clone(),
                "healthcheck keys must be strings",
            ));
            continue;
        };
        match key {
            "test" => match parse_healthcheck_test(value, &path.field(key)) {
                Ok(value) => test = Some(value),
                Err(finding) => findings.push(finding),
            },
            "disable" => match value.as_bool() {
                Some(true) => disabled = true,
                Some(false) => {}
                None => findings.push(ComposeFinding::invalid(
                    path.field(key),
                    "disable must be a boolean",
                )),
            },
            "interval" => match parse_healthcheck_duration(value) {
                Ok(value) => interval = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            "timeout" => match parse_healthcheck_duration(value) {
                Ok(value) => timeout = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            "retries" => match parse_healthcheck_retries(value) {
                Ok(value) => retries = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            "start_period" => match parse_healthcheck_duration(value) {
                Ok(value) => start_period = Some(value),
                Err(message) => findings.push(ComposeFinding::invalid(path.field(key), message)),
            },
            other => findings.push(ComposeFinding::unknown(path.field(other))),
        }
    }
    let test = match (disabled, test) {
        (true, Some(ContainerHealthcheckTest::Disable) | None) => ContainerHealthcheckTest::Disable,
        (true, Some(_)) => {
            findings.push(ComposeFinding::invalid(
                path.field("disable"),
                "disable: true conflicts with an explicit test",
            ));
            ContainerHealthcheckTest::Disable
        }
        (false, Some(test)) => test,
        (false, None) => ContainerHealthcheckTest::Inherit,
    };
    if !findings.is_empty() {
        return Err(findings);
    }
    Ok(Some(ContainerHealthcheck {
        test,
        interval,
        timeout,
        retries,
        start_period,
    }))
}

fn parse_healthcheck_test(
    value: &Value,
    path: &ComposePath,
) -> Result<ContainerHealthcheckTest, ComposeFinding> {
    match value {
        Value::String(command) => HealthcheckShellCommand::try_new(command.clone())
            .map(ContainerHealthcheckTest::Shell)
            .map_err(|error| ComposeFinding::invalid(path.clone(), error)),
        Value::Sequence(items) => {
            let [first, rest @ ..] = items.as_slice() else {
                return Ok(ContainerHealthcheckTest::Inherit);
            };
            let Some(kind) = scalar_to_string(first) else {
                return Err(ComposeFinding::invalid(
                    path.index(0),
                    "healthcheck test kind must be a string",
                ));
            };
            match kind.as_str() {
                "NONE" => Ok(ContainerHealthcheckTest::Disable),
                "CMD" => ContainerCommand::try_new(exec_argv(rest.to_vec(), path)?)
                    .map(ContainerHealthcheckTest::Exec)
                    .map_err(|error| ComposeFinding::invalid(path.clone(), error)),
                "CMD-SHELL" => {
                    let [command] = rest else {
                        return Err(ComposeFinding::invalid(
                            path.clone(),
                            "CMD-SHELL healthcheck needs exactly one command",
                        ));
                    };
                    let Some(command) = scalar_to_string(command) else {
                        return Err(ComposeFinding::invalid(
                            path.index(1),
                            "CMD-SHELL command must be scalar",
                        ));
                    };
                    HealthcheckShellCommand::try_new(command)
                        .map(ContainerHealthcheckTest::Shell)
                        .map_err(|error| ComposeFinding::invalid(path.clone(), error))
                }
                other => Err(ComposeFinding::invalid(
                    path.index(0),
                    format!("unsupported healthcheck test kind {other:?}"),
                )),
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Mapping(_) | Value::Tagged(_) => {
            Err(ComposeFinding::invalid(
                path.clone(),
                "healthcheck test must be a string or list",
            ))
        }
    }
}

fn parse_restart_policy(
    value: Option<Value>,
    deploy_policy: Option<ContainerRestartPolicy>,
    service_path: &ComposePath,
) -> Result<ContainerRestartPolicy, ComposeFinding> {
    let Some(value) = value else {
        return Ok(deploy_policy.unwrap_or(ContainerRestartPolicy::DockerDefault));
    };
    let Some(text) = scalar_to_string(&value) else {
        return Err(ComposeFinding::invalid(
            service_path.field("restart"),
            "restart must be a string",
        ));
    };
    parse_restart_policy_text(&text)
        .map_err(|message| ComposeFinding::invalid(service_path.field("restart"), message))
}

fn parse_restart_policy_text(value: &str) -> Result<ContainerRestartPolicy, String> {
    match value {
        "" => Ok(ContainerRestartPolicy::DockerDefault),
        "no" => Ok(ContainerRestartPolicy::No),
        "always" => Ok(ContainerRestartPolicy::Always),
        "on-failure" => Ok(ContainerRestartPolicy::OnFailure),
        "unless-stopped" => Ok(ContainerRestartPolicy::UnlessStopped),
        other => Err(format!("unsupported restart policy {other:?}")),
    }
}

fn parse_capabilities(
    value: Option<Value>,
    path: &ComposePath,
) -> Result<Vec<LinuxCapability>, Vec<ComposeFinding>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = match value {
        Value::String(value) => vec![(path.clone(), value)],
        Value::Sequence(items) => {
            let mut parsed = Vec::new();
            let mut findings = Vec::new();
            for (index, item) in items.into_iter().enumerate() {
                let path = path.index(index);
                match scalar_to_string(&item) {
                    Some(value) => parsed.push((path, value)),
                    None => findings.push(ComposeFinding::invalid(
                        path,
                        "capability entries must be scalar",
                    )),
                }
            }
            if !findings.is_empty() {
                return Err(findings);
            }
            parsed
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Mapping(_) | Value::Tagged(_) => {
            return Err(vec![ComposeFinding::invalid(
                path.clone(),
                "capabilities must be a string or list",
            )]);
        }
    };
    let mut findings = Vec::new();
    let mut capabilities = Vec::new();
    for (path, value) in values {
        match LinuxCapability::try_new(value) {
            Ok(value) => capabilities.push(value),
            Err(error) => findings.push(ComposeFinding::invalid(path, error)),
        }
    }
    if findings.is_empty() {
        Ok(capabilities)
    } else {
        Err(findings)
    }
}

fn parse_healthcheck_duration(value: &Value) -> Result<HealthcheckDurationNanos, String> {
    let micros = parse_duration_micros_value(value)?;
    let nanos = micros
        .checked_mul(1000)
        .ok_or_else(|| "duration exceeds u64 nanoseconds".to_owned())?;
    HealthcheckDurationNanos::try_new(nanos).map_err(|error| error.to_string())
}

fn parse_healthcheck_retries(value: &Value) -> Result<HealthcheckRetries, String> {
    let value = parse_u64_value(value, "retries")?;
    let value = u16::try_from(value).map_err(|_| "retries exceeds u16".to_owned())?;
    HealthcheckRetries::try_new(value).map_err(|error| error.to_string())
}

fn parse_nano_cpus(value: &Value) -> Result<NanoCpus, String> {
    let text = scalar_to_string(value).ok_or_else(|| "cpus must be scalar".to_owned())?;
    let cpus = text
        .parse::<f64>()
        .map_err(|error| format!("cpus must be numeric: {error}"))?;
    if !cpus.is_finite() || cpus <= 0.0 {
        return Err("cpus must be greater than zero".to_owned());
    }
    let nano_cpus = (cpus * 1_000_000_000.0).round();
    if nano_cpus > u64::MAX as f64 {
        return Err("cpus exceeds u64 nanocpus".to_owned());
    }
    NanoCpus::try_new(nano_cpus as u64).map_err(|error| error.to_string())
}

fn parse_memory_bytes(value: &Value) -> Result<MemoryBytes, String> {
    let text = scalar_to_string(value).ok_or_else(|| "memory must be scalar".to_owned())?;
    let bytes = parse_byte_quantity(&text)?;
    MemoryBytes::try_new(bytes).map_err(|error| error.to_string())
}

fn parse_pids_limit(value: &Value) -> Result<PidsLimit, String> {
    let text = scalar_to_string(value).ok_or_else(|| "pids must be scalar".to_owned())?;
    let pids = text
        .parse::<i64>()
        .map_err(|error| format!("pids must be an integer: {error}"))?;
    PidsLimit::try_new(pids).map_err(|error| error.to_string())
}

fn parse_u64_value(value: &Value, name: &str) -> Result<u64, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| format!("{name} must be a non-negative integer")),
        Value::String(value) => value
            .parse::<u64>()
            .map_err(|error| format!("{name} must be an integer: {error}")),
        Value::Null
        | Value::Bool(_)
        | Value::Sequence(_)
        | Value::Mapping(_)
        | Value::Tagged(_) => Err(format!("{name} must be an integer")),
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
    let total_micros = parse_duration_micros(value)?;
    if !total_micros.is_multiple_of(MICROS_PER_SECOND) {
        return Err("stop grace period must be whole seconds".to_owned());
    }
    Ok(total_micros / MICROS_PER_SECOND)
}

fn parse_duration_micros_value(value: &Value) -> Result<u64, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| "duration number must be a non-negative integer".to_owned())
            .and_then(|seconds| {
                seconds
                    .checked_mul(1_000_000)
                    .ok_or_else(|| "duration exceeds u64 microseconds".to_owned())
            }),
        Value::String(value) => parse_duration_micros(value),
        Value::Null
        | Value::Bool(_)
        | Value::Sequence(_)
        | Value::Mapping(_)
        | Value::Tagged(_) => {
            Err("duration must be a number of seconds or a Compose duration string".to_owned())
        }
    }
}

fn parse_duration_micros(value: &str) -> Result<u64, String> {
    const MICROS_PER_SECOND: u64 = 1_000_000;
    if value.is_empty() {
        return Err("duration is empty".to_owned());
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        let seconds = value
            .parse::<u64>()
            .map_err(|error| format!("invalid duration: {error}"))?;
        return seconds
            .checked_mul(MICROS_PER_SECOND)
            .ok_or_else(|| "duration exceeds u64 microseconds".to_owned());
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
    Ok(total_micros)
}

fn parse_byte_quantity(value: &str) -> Result<u64, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("memory is empty".to_owned());
    }
    let digits_end = value
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .unwrap_or(value.len());
    let amount = value[..digits_end]
        .parse::<f64>()
        .map_err(|error| format!("memory must start with a number: {error}"))?;
    if !amount.is_finite() || amount <= 0.0 {
        return Err("memory must be greater than zero".to_owned());
    }
    let unit = value[digits_end..].trim().to_ascii_lowercase();
    // Docker's RAMInBytes treats every suffix as a 1024 multiple, so `64m`
    // here means the same 64 MiB it means to `docker run -m 64m`.
    let multiplier = match unit.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "ki" | "kib" => 1024.0,
        "m" | "mb" | "mi" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gi" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "ti" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => return Err(format!("unsupported memory unit {other:?}")),
    };
    let bytes = (amount * multiplier).round();
    if bytes > u64::MAX as f64 {
        return Err("memory exceeds u64 bytes".to_owned());
    }
    Ok(bytes as u64)
}

fn parse_routes(
    x_route: Option<ComposeRoutes>,
    x_ports: Option<Value>,
    service_path: &ComposePath,
) -> Result<Vec<DeployRoute>, Vec<ComposeFinding>> {
    let mut findings = Vec::new();
    let mut parsed = Vec::new();
    if let Some(routes) = x_route {
        for (index, shorthand) in routes.into_shorthands().into_iter().enumerate() {
            match parse_route_shorthand(&shorthand) {
                Ok(route) => parsed.push(route),
                Err(error) => findings.push(ComposeFinding::invalid(
                    service_path.field("x-route").index(index),
                    error,
                )),
            }
        }
    }
    if let Some(x_ports) = x_ports {
        parse_x_ports(x_ports, service_path, &mut parsed, &mut findings);
    }
    if findings.is_empty() {
        Ok(parsed)
    } else {
        Err(findings)
    }
}

fn parse_x_ports(
    value: Value,
    service_path: &ComposePath,
    parsed: &mut Vec<DeployRoute>,
    findings: &mut Vec<ComposeFinding>,
) {
    let path = service_path.field("x-ports");
    let entries = match value {
        Value::Sequence(entries) => entries,
        value @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Mapping(_)
        | Value::Tagged(_)) => vec![value],
    };
    for (index, entry) in entries.into_iter().enumerate() {
        let path = path.index(index);
        match parse_x_port_entry(entry, &path) {
            Ok(route) => parsed.push(route),
            Err(finding) => findings.push(finding),
        }
    }
}

fn parse_x_port_entry(value: Value, path: &ComposePath) -> Result<DeployRoute, ComposeFinding> {
    match value {
        Value::Number(number) => {
            let endpoint_port = number.as_u64().ok_or_else(|| {
                ComposeFinding::invalid(path.clone(), "x-ports port must be a positive integer")
            })?;
            let endpoint_port = u16::try_from(endpoint_port)
                .map_err(|_| ComposeFinding::invalid(path.clone(), "x-ports port exceeds u16"))?;
            Ok(auto_hostname_route(
                endpoint_port,
                RouteProtocol::Https,
                path,
            )?)
        }
        Value::String(value) => parse_x_port_shorthand(&value, path),
        Value::Mapping(mapping) => {
            if mapping.contains_key(Value::String("published".to_owned())) {
                return Err(ComposeFinding::invalid(
                    path.field("published"),
                    "long-syntax published ports are host publishing; use x-ports shorthand without published",
                ));
            }
            if mapping
                .get(Value::String("mode".to_owned()))
                .and_then(scalar_to_string)
                .is_some_and(|mode| mode == "host")
            {
                return Err(ComposeFinding::invalid(
                    path.field("mode"),
                    "mode: host publishes host ports; use x-ports shorthand",
                ));
            }
            Err(ComposeFinding::invalid(
                path.clone(),
                "x-ports entries must be 8080, 8080/http, or app.example.com:8080",
            ))
        }
        Value::Null | Value::Bool(_) | Value::Sequence(_) | Value::Tagged(_) => {
            Err(ComposeFinding::invalid(
                path.clone(),
                "x-ports entries must be 8080, 8080/http, or app.example.com:8080",
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteProtocol {
    Http,
    Https,
}

fn parse_x_port_shorthand(value: &str, path: &ComposePath) -> Result<DeployRoute, ComposeFinding> {
    let (route, protocol) = match value.rsplit_once('/') {
        Some((route, "http")) => (route, RouteProtocol::Http),
        Some((route, "https")) => (route, RouteProtocol::Https),
        Some((_route, other)) => {
            return Err(ComposeFinding::invalid(
                path.clone(),
                format!("unsupported x-ports protocol {other:?}; use /http or /https"),
            ));
        }
        None => (value, RouteProtocol::Https),
    };
    if let Some((hostname, endpoint_port)) = route.rsplit_once(':') {
        return custom_hostname_route(hostname, endpoint_port, protocol, path);
    }
    let endpoint_port = route.parse::<u16>().map_err(|error| {
        ComposeFinding::invalid(
            path.clone(),
            format!("x-ports endpoint port must be an integer: {error}"),
        )
    })?;
    auto_hostname_route(endpoint_port, protocol, path)
}

fn auto_hostname_route(
    endpoint_port: u16,
    protocol: RouteProtocol,
    path: &ComposePath,
) -> Result<DeployRoute, ComposeFinding> {
    Ok(DeployRoute {
        target: DeployRouteTarget::AutoHostname {
            port: protocol_port(protocol),
        },
        endpoint_port: RoutePort::try_new(endpoint_port)
            .map_err(|error| ComposeFinding::invalid(path.clone(), error))?,
    })
}

fn custom_hostname_route(
    hostname: &str,
    endpoint_port: &str,
    protocol: RouteProtocol,
    path: &ComposePath,
) -> Result<DeployRoute, ComposeFinding> {
    let hostname = RouteHostname::try_new(hostname)
        .map_err(|error| ComposeFinding::invalid(path.clone(), error))?;
    let endpoint_port = endpoint_port.parse::<u16>().map_err(|error| {
        ComposeFinding::invalid(
            path.clone(),
            format!("x-ports endpoint port must be an integer: {error}"),
        )
    })?;
    Ok(DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname,
            port: protocol_port(protocol),
        },
        endpoint_port: RoutePort::try_new(endpoint_port)
            .map_err(|error| ComposeFinding::invalid(path.clone(), error))?,
    })
}

fn protocol_port(protocol: RouteProtocol) -> RoutePort {
    let port = match protocol {
        RouteProtocol::Http => 80,
        RouteProtocol::Https => 443,
    };
    RoutePort::try_new(port).expect("route protocol port is valid")
}

#[cfg(test)]
mod tests {
    use ployz_core::deploy::{ContainerEntrypoint, StopGracePeriod};

    use super::{parse_compose_duration, parse_depends_on};
    use crate::compose::diagnostics::{ComposePath, UnsupportedFieldMode};
    use crate::compose::{ComposeInput, parse_deploy_file};

    #[test]
    fn depends_on_reports_every_invalid_entry() {
        let value = serde_yaml::from_str("[1, {}, 'bad id']").expect("valid yaml");
        let findings = parse_depends_on(Some(value), &ComposePath::root().field("depends_on"))
            .expect_err("dependencies are invalid");

        let [first, second, third] = findings.as_slice() else {
            panic!("expected three dependency findings");
        };
        assert_eq!(first.path.render(), "depends_on[0]");
        assert_eq!(second.path.render(), "depends_on[1]");
        assert_eq!(third.path.render(), "depends_on[2]");
    }

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
