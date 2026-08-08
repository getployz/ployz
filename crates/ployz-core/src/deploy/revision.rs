//! Stable namespace revision identity and its canonical encoding.

use super::*;

const KEY_DERIVATION_DOMAIN: &[u8] = b"ployz.environment_revision_key.v1";
const ENVIRONMENT_IDENTITY_DOMAIN: &[u8] = b"ployz.environment_revision_identity.v1";

/// Process-local key for opaque environment equality inside revision ids.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvironmentRevisionKey([u8; 32]);

impl EnvironmentRevisionKey {
    #[must_use]
    pub fn derive_from_key_material(key_material: &[u8]) -> Self {
        Self::derive(key_material)
    }

    fn derive(controller_seed: &[u8]) -> Self {
        let mut hmac = Hmac::<Sha256>::new_from_slice(controller_seed)
            .expect("HMAC accepts controller seed material of any length");
        hmac.update(KEY_DERIVATION_DOMAIN);
        Self(hmac.finalize().into_bytes().into())
    }

    fn environment_identity(&self, environment: &ServiceEnvironment) -> Option<[u8; 32]> {
        if environment.is_empty() {
            return None;
        }
        let mut hmac =
            Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC accepts a SHA-256-sized key");
        hmac.update(ENVIRONMENT_IDENTITY_DOMAIN);
        for (name, value) in environment.iter() {
            hmac_frame(&mut hmac, "name", name.as_str().as_bytes());
            hmac_frame(&mut hmac, "value", value.as_str().as_bytes());
        }
        Some(hmac.finalize().into_bytes().into())
    }
}

impl std::fmt::Debug for EnvironmentRevisionKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EnvironmentRevisionKey([redacted])")
    }
}

#[must_use]
pub fn namespace_revision_id_for(
    namespace_id: &NamespaceId,
    services: &[DeployServiceSpec],
    environment_key: &EnvironmentRevisionKey,
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
        hash_environment_identity(&mut hasher, environment_key, &service.runtime.environment);

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
        .expect("sha256 hex digest is a stable identifier token")
}

#[must_use]
pub fn namespace_revision_entry_id_for(
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    image: &ImageReference,
    runtime: &ContainerRuntimeSpec,
    environment_key: &EnvironmentRevisionKey,
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
    hash_runtime_spec(&mut hasher, runtime);
    hash_environment_identity(&mut hasher, environment_key, &runtime.environment);
    let digest = hasher.finalize();
    NamespaceRevisionEntryId::try_new(format!("{digest:x}"))
        .expect("sha256 hex digest is a stable identifier token")
}

#[must_use]
pub(super) fn namespace_revision_entry_id_without_environment_for(
    namespace_id: &NamespaceId,
    service: &DeployServiceSpec,
) -> Option<NamespaceRevisionEntryId> {
    if !service.runtime.environment.is_empty() {
        return None;
    }
    Some(namespace_revision_entry_id_for(
        namespace_id,
        &service.service_id,
        &service.image,
        &service.runtime,
        &EnvironmentRevisionKey([0; 32]),
    ))
}

fn hash_runtime_spec(hasher: &mut Sha256, runtime: &ContainerRuntimeSpec) {
    let ContainerRuntimeSpec {
        command,
        entrypoint,
        environment: _,
        volume_mounts,
        healthcheck,
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

fn hash_environment_identity(
    hasher: &mut Sha256,
    key: &EnvironmentRevisionKey,
    environment: &ServiceEnvironment,
) {
    if let Some(identity) = key.environment_identity(environment) {
        hash_frame(hasher, "environment_identity", &identity);
    }
}

fn hmac_frame(hmac: &mut Hmac<Sha256>, tag: &str, bytes: &[u8]) {
    hmac.update(tag.as_bytes());
    hmac.update(&(bytes.len() as u64).to_be_bytes());
    hmac.update(bytes);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(entries: &[(&str, &str)]) -> ServiceEnvironment {
        ServiceEnvironment::from(
            entries
                .iter()
                .map(|(name, value)| {
                    (
                        EnvName::try_new(*name).expect("environment name"),
                        EnvValue::try_new(*value).expect("environment value"),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn environment_identity_is_canonical_and_keyed() {
        let first_key = EnvironmentRevisionKey::derive(b"controller-seed-a");
        let same_key = EnvironmentRevisionKey::derive(b"controller-seed-a");
        let other_key = EnvironmentRevisionKey::derive(b"controller-seed-b");
        let ordered = environment(&[("ALPHA", "one"), ("BETA", "two")]);
        let reversed = environment(&[("BETA", "two"), ("ALPHA", "one")]);

        assert_eq!(
            first_key.environment_identity(&ordered),
            same_key.environment_identity(&reversed)
        );
        assert_ne!(
            first_key.environment_identity(&ordered),
            first_key.environment_identity(&environment(&[("ALPHA", "changed"), ("BETA", "two")]))
        );
        assert_ne!(
            first_key.environment_identity(&ordered),
            first_key.environment_identity(&environment(&[("RENAMED", "one"), ("BETA", "two")]))
        );
        assert_ne!(
            first_key.environment_identity(&ordered),
            other_key.environment_identity(&ordered)
        );
        assert_eq!(
            first_key.environment_identity(&ServiceEnvironment::empty()),
            None
        );
        assert_eq!(
            other_key.environment_identity(&ServiceEnvironment::empty()),
            None
        );
    }

    #[test]
    fn environment_revision_key_debug_is_redacted() {
        let key = EnvironmentRevisionKey::derive(b"sentinel-controller-seed");
        let rendered = format!("{key:?}");
        assert_eq!(rendered, "EnvironmentRevisionKey([redacted])");
        assert!(!rendered.contains("sentinel"));
    }

    #[test]
    fn environment_free_entry_identity_reuses_canonical_encoding() {
        let namespace_id = NamespaceId::try_new("production").expect("namespace");
        let service = DeployServiceSpec {
            service_id: ServiceId::try_new("api").expect("service"),
            image: ImageReference::try_new("ghcr.io/acme/api:current").expect("image"),
            mode: ServiceMode::Global,
            keep: None,
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        };
        let key = EnvironmentRevisionKey::derive(b"irrelevant-for-empty-environment");

        assert_eq!(
            service.namespace_revision_entry_id_without_environment(&namespace_id),
            Some(service.namespace_revision_entry_id(&namespace_id, &key))
        );
    }

    #[test]
    fn environmentful_entry_identity_requires_controller_key() {
        let namespace_id = NamespaceId::try_new("production").expect("namespace");
        let mut service = DeployServiceSpec {
            service_id: ServiceId::try_new("api").expect("service"),
            image: ImageReference::try_new("ghcr.io/acme/api:current").expect("image"),
            mode: ServiceMode::Global,
            keep: None,
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        };
        service.runtime.environment = environment(&[("TOKEN", "secret")]);

        assert_eq!(
            service.namespace_revision_entry_id_without_environment(&namespace_id),
            None
        );
    }
}
