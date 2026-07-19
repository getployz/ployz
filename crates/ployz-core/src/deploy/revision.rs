//! Stable namespace revision identity and its canonical encoding.

use super::*;

#[must_use]
pub fn namespace_revision_id_for(
    namespace_id: &NamespaceId,
    services: &[DeployServiceSpec],
) -> NamespaceRevisionId {
    let mut services = services.iter().collect::<Vec<_>>();
    services.sort_by(|left, right| left.service_id.cmp(&right.service_id));

    let mut hasher = Sha256::new();
    hash_frame(
        &mut hasher,
        "version",
        DeployServiceSpec::NAMESPACE_REVISION_ENCODING_VERSION.as_bytes(),
    );
    hash_frame(
        &mut hasher,
        "namespace_id",
        namespace_id.as_str().as_bytes(),
    );
    for service in services {
        hash_frame(
            &mut hasher,
            "service_id",
            service.service_id.as_str().as_bytes(),
        );
        hash_frame(&mut hasher, "image", service.image.as_str().as_bytes());
        match service.mode {
            ServiceMode::Replicated { replicas } => {
                hash_frame(&mut hasher, "service_mode", b"replicated");
                hash_frame(
                    &mut hasher,
                    "replicas",
                    replicas.get().to_string().as_bytes(),
                );
            }
            ServiceMode::Global => hash_frame(&mut hasher, "service_mode", b"global"),
        }
        hash_runtime_spec(&mut hasher, &service.runtime);

        match &service.pre_start {
            Some(pre_start) => {
                hash_frame(&mut hasher, "pre_start", b"some");
                for argument in pre_start.command.as_slice() {
                    hash_frame(&mut hasher, "pre_start_arg", argument.as_bytes());
                }
            }
            None => hash_frame(&mut hasher, "pre_start", b"none"),
        }

        let mut dependencies = service.depends_on.iter().collect::<Vec<_>>();
        dependencies.sort();
        dependencies.dedup();
        for dependency in dependencies {
            hash_frame(
                &mut hasher,
                "depends_on_service_id",
                dependency.service_id.as_str().as_bytes(),
            );
            hash_frame(
                &mut hasher,
                "depends_on_condition",
                match dependency.condition {
                    DependencyCondition::Started => b"started",
                    DependencyCondition::Healthy => b"healthy",
                },
            );
        }

        let mut routes = service.routes.iter().collect::<Vec<_>>();
        routes.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.endpoint_port.cmp(&right.endpoint_port))
        });
        for route in routes {
            match &route.target {
                DeployRouteTarget::AutoHostname { label } => {
                    hash_frame(&mut hasher, "route_target_kind", b"auto_hostname");
                    hash_frame(&mut hasher, "route_label", label.as_str().as_bytes());
                }
                DeployRouteTarget::Hostname { hostname } => {
                    hash_frame(&mut hasher, "route_target_kind", b"hostname");
                    hash_frame(&mut hasher, "route_hostname", hostname.as_str().as_bytes());
                }
            }
            hash_frame(
                &mut hasher,
                "route_endpoint_port",
                route.endpoint_port.get().to_string().as_bytes(),
            );
        }
    }
    let digest = hasher.finalize();
    NamespaceRevisionId::try_new(format!("{digest:x}"))
        .expect("sha256 hex digest is a subject token")
}

#[must_use]
pub fn namespace_revision_entry_id_for(
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    image: &ImageReference,
    image_source: &ImageSource,
    runtime: &ContainerRuntimeSpec,
) -> NamespaceRevisionEntryId {
    let mut hasher = Sha256::new();
    hash_frame(
        &mut hasher,
        "version",
        DeployServiceSpec::NAMESPACE_REVISION_ENTRY_ENCODING_VERSION.as_bytes(),
    );
    hash_frame(
        &mut hasher,
        "namespace_id",
        namespace_id.as_str().as_bytes(),
    );
    hash_frame(&mut hasher, "service_id", service_id.as_str().as_bytes());
    hash_frame(&mut hasher, "image", image.as_str().as_bytes());
    match image_source {
        ImageSource::Registry => {}
        ImageSource::PushedToSeed(receipt) => {
            hash_frame(
                &mut hasher,
                "index_digest",
                receipt.index_digest().as_str().as_bytes(),
            );
        }
    }
    hash_runtime_spec(&mut hasher, runtime);
    let digest = hasher.finalize();
    NamespaceRevisionEntryId::try_new(format!("{digest:x}"))
        .expect("sha256 hex digest is a subject token")
}

fn hash_runtime_spec(hasher: &mut Sha256, runtime: &ContainerRuntimeSpec) {
    let ContainerRuntimeSpec {
        command,
        entrypoint,
        environment,
        stop_grace_period,
        volume_mounts,
        healthcheck,
        restart_policy,
        cap_add,
        cap_drop,
        resources,
    } = runtime;

    match command {
        Some(command) => {
            hash_frame(hasher, "command", b"some");
            for arg in command.as_slice() {
                hash_frame(hasher, "command_arg", arg.as_bytes());
            }
        }
        None => hash_frame(hasher, "command", b"none"),
    }

    match entrypoint {
        Some(ContainerEntrypoint::Clear) => hash_frame(hasher, "entrypoint", b"clear"),
        Some(ContainerEntrypoint::Argv(argv)) => {
            hash_frame(hasher, "entrypoint", b"argv");
            for arg in argv.as_slice() {
                hash_frame(hasher, "entrypoint_arg", arg.as_bytes());
            }
        }
        None => hash_frame(hasher, "entrypoint", b"none"),
    }

    for (name, value) in environment.iter() {
        hash_frame(hasher, "env_name", name.as_str().as_bytes());
        hash_frame(hasher, "env_value", value.as_str().as_bytes());
    }
    hash_frame(
        hasher,
        "stop_grace_period",
        stop_grace_period.as_seconds().to_string().as_bytes(),
    );
    for mount in volume_mounts {
        hash_frame(hasher, "volume_name", mount.volume_name.as_str().as_bytes());
        hash_frame(hasher, "volume_target", mount.target.as_str().as_bytes());
    }

    match healthcheck {
        Some(healthcheck) => {
            hash_frame(hasher, "healthcheck", b"some");
            hash_healthcheck(hasher, healthcheck);
        }
        None => hash_frame(hasher, "healthcheck", b"none"),
    }
    hash_frame(
        hasher,
        "restart_policy",
        restart_policy.as_docker_name().as_bytes(),
    );
    for capability in canonical_capabilities(cap_add) {
        hash_frame(hasher, "cap_add", capability.as_str().as_bytes());
    }
    for capability in canonical_capabilities(cap_drop) {
        hash_frame(hasher, "cap_drop", capability.as_str().as_bytes());
    }
    if let Some(nano_cpus) = resources.nano_cpus {
        hash_frame(hasher, "nano_cpus", nano_cpus.get().to_string().as_bytes());
    }
    if let Some(memory_bytes) = resources.memory_bytes {
        hash_frame(
            hasher,
            "memory_bytes",
            memory_bytes.get().to_string().as_bytes(),
        );
    }
    if let Some(pids) = resources.pids {
        hash_frame(hasher, "pids", pids.get().to_string().as_bytes());
    }
}

fn hash_frame(hasher: &mut Sha256, tag: &str, bytes: &[u8]) {
    hasher.update(tag.as_bytes());
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// The canonical (sorted, deduplicated) order for a capability list. Both
/// the namespace revision entry hash and the Docker create call go through
/// this one ordering, so the identity digest can never disagree with what a
/// container was actually created with.
#[must_use]
pub fn canonical_capabilities(capabilities: &[LinuxCapability]) -> Vec<&LinuxCapability> {
    let mut capabilities = capabilities.iter().collect::<Vec<_>>();
    capabilities.sort();
    capabilities.dedup();
    capabilities
}

fn hash_healthcheck(hasher: &mut Sha256, healthcheck: &ContainerHealthcheck) {
    let ContainerHealthcheck {
        test,
        interval,
        timeout,
        retries,
        start_period,
    } = healthcheck;
    match test {
        ContainerHealthcheckTest::Inherit => hash_frame(hasher, "healthcheck_test", b"inherit"),
        ContainerHealthcheckTest::Disable => hash_frame(hasher, "healthcheck_test", b"disable"),
        ContainerHealthcheckTest::Exec(command) => {
            hash_frame(hasher, "healthcheck_test", b"exec");
            for arg in command.as_slice() {
                hash_frame(hasher, "healthcheck_arg", arg.as_bytes());
            }
        }
        ContainerHealthcheckTest::Shell(command) => {
            hash_frame(hasher, "healthcheck_test", b"shell");
            hash_frame(hasher, "healthcheck_shell", command.as_str().as_bytes());
        }
    }
    if let Some(value) = interval {
        hash_frame(
            hasher,
            "healthcheck_interval",
            value.as_nanos().to_string().as_bytes(),
        );
    }
    if let Some(value) = timeout {
        hash_frame(
            hasher,
            "healthcheck_timeout",
            value.as_nanos().to_string().as_bytes(),
        );
    }
    if let Some(value) = retries {
        hash_frame(
            hasher,
            "healthcheck_retries",
            value.get().to_string().as_bytes(),
        );
    }
    if let Some(value) = start_period {
        hash_frame(
            hasher,
            "healthcheck_start_period",
            value.as_nanos().to_string().as_bytes(),
        );
    }
}
