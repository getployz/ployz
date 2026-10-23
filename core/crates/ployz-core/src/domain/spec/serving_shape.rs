//! Content identity of spec fields whose change requires a new Container.

use serde::Serialize;

use super::{
    ConfigMount, Placement, PortPublication, RequestedServiceSpec, ResolvedServiceSpec,
    ServiceContainerSpec, ServiceMode,
};
use crate::{RawVolumeSource, ServiceMount, ServiceName, ServiceVolume, VolumeSource};

/// Content identity of the Resolved Service Spec fields whose change requires a new Container.
///
/// Equal shapes mean interchangeable Containers for traffic and Global occupancy.
/// Service ID, replica count, resources, pull policy, pre-deploy hooks, and update
/// timing are not part of the shape.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServingShape(u64);

/// Serving Shape of a published observation, whose Config content is redacted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedServingShape(ServingShape);

impl ServingShape {
    /// Shape of one observed Resolved Service Spec.
    #[must_use]
    pub fn of_resolved(spec: &ResolvedServiceSpec) -> Self {
        Self::from_fields(&Self::fields_of_resolved(spec))
    }

    /// Canonical observed fields shared by serving identity and setting comparison.
    pub(super) fn fields_of_resolved(spec: &ResolvedServiceSpec) -> serde_json::Value {
        Self::resolved_fields(spec, sorted_json(spec.mount_graph.config_graph().configs()))
    }

    fn resolved_fields(
        spec: &ResolvedServiceSpec,
        config_fields: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        let ResolvedServiceSpec {
            service_id: _,
            name,
            mode,
            container,
            placement,
            ports,
            mount_graph,
            pre_deploy: _,
            update: _,
        } = spec;
        Self::recreate_fields(
            name,
            mode,
            container,
            placement,
            ports,
            mount_graph.volume_graph().volumes(),
            mount_graph.volume_graph().mounts(),
            config_fields,
            mount_graph.config_graph().mounts(),
        )
    }

    /// Shape of one requested spec. Comparable with [`Self::of_resolved`].
    #[must_use]
    pub fn of_requested(spec: &RequestedServiceSpec) -> Self {
        Self::from_fields(&Self::fields_of_requested(spec))
    }

    /// Canonical requested fields shared by serving identity and setting comparison.
    pub(super) fn fields_of_requested(spec: &RequestedServiceSpec) -> serde_json::Value {
        let RequestedServiceSpec {
            name,
            mode,
            container,
            placement,
            ports,
            mount_graph,
            pre_deploy: _,
            update: _,
        } = spec;
        Self::recreate_fields(
            name,
            mode,
            container,
            placement,
            ports,
            mount_graph.volume_graph().volumes(),
            mount_graph.volume_graph().mounts(),
            sorted_json(mount_graph.config_graph().configs()),
            mount_graph.config_graph().mounts(),
        )
    }

    /// Docker-name-safe hex token for this shape.
    #[must_use]
    pub fn token(self) -> String {
        format!("{:016x}", self.0)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one argument per hashed recreate field family"
    )]
    fn recreate_fields(
        name: &ServiceName,
        mode: &ServiceMode,
        container: &ServiceContainerSpec,
        placement: &Placement,
        ports: &[PortPublication],
        volumes: &[ServiceVolume],
        mounts: &[ServiceMount],
        configs: Vec<serde_json::Value>,
        config_mounts: &[ConfigMount],
    ) -> serde_json::Value {
        let ServiceContainerSpec {
            image,
            command,
            entrypoint,
            environment,
            sealed_identity: _,
            labels,
            hostname,
            extra_hosts,
            cap_add,
            cap_drop,
            healthcheck,
            pull_policy: _,
            init,
            user,
            working_directory,
            tty,
            open_stdin,
            privileged,
            pid_mode,
            resources: _,
            stop_timeout_secs,
            sysctls,
            restart,
            ipv6,
        } = container;
        let mut payload = serde_json::json!({
            "name": name,
            "mode": service_mode_kind(mode),
            "image": image,
            "command": command,
            "entrypoint": entrypoint,
            "environment": environment,
            "labels": labels,
            "hostname": hostname,
            "extra_hosts": extra_hosts,
            "cap_add": sorted_json(cap_add),
            "cap_drop": sorted_json(cap_drop),
            "healthcheck": healthcheck,
            "init": init,
            "user": user,
            "working_directory": working_directory,
            "tty": tty,
            "open_stdin": open_stdin,
            "privileged": privileged,
            "pid_mode": pid_mode,
            "log_driver": null,
            "stop_timeout_secs": stop_timeout_secs,
            "sysctls": sysctls,
            "restart": restart,
            "placement": placement,
            "ports": sorted_json(ports),
            "volumes": sorted_json(volumes.iter().map(|volume| (&volume.reference, source_shape(&volume.source), volume.source.creation_labels()))),
            "mounts": sorted_json(mounts),
            "configs": configs,
            "config_mounts": sorted_json(config_mounts),
        });
        // Only a spec that takes IPv6 hashes it, so every earlier token stays as it was.
        if *ipv6 && let Some(fields) = payload.as_object_mut() {
            fields.insert("ipv6".to_owned(), serde_json::Value::Bool(true));
        }
        payload
    }

    fn from_fields(payload: &serde_json::Value) -> Self {
        let bytes = serde_json::to_vec(payload).expect("serving shape JSON is serializable");
        Self(fnv1a64(&bytes))
    }
}

impl ResolvedServiceSpec {
    /// Content identity of the fields whose change requires a new Container.
    #[must_use]
    pub fn serving_shape(&self) -> ServingShape {
        ServingShape::of_resolved(self)
    }

    /// Serving Shape of this spec as a daemon publishes it.
    #[must_use]
    pub fn published_serving_shape(&self) -> PublishedServingShape {
        let names = self.mount_graph.config_graph().configs().iter();
        let configs = sorted_json(names.map(|config| &config.name));
        PublishedServingShape(ServingShape::from_fields(&ServingShape::resolved_fields(
            self, configs,
        )))
    }
}

impl RequestedServiceSpec {
    /// Content identity of the fields whose change requires a new Container.
    #[must_use]
    pub fn serving_shape(&self) -> ServingShape {
        ServingShape::of_requested(self)
    }
}

/// A Provisioned Volume's bound is the Volume's, not the Container's: a resize raises
/// it on every copy while the Container runs, and a Deploy accepts a bound at or
/// above what it asks, so the Container that mounts it keeps serving.
fn source_shape(source: &VolumeSource) -> serde_json::Value {
    match source.kind() {
        RawVolumeSource::Provisioned { name, labels, .. } => {
            serde_json::json!({ "kind": "provisioned", "name": name, "labels": labels })
        }
        other @ (RawVolumeSource::External { .. }
        | RawVolumeSource::Ordinary { .. }
        | RawVolumeSource::Bind { .. }
        | RawVolumeSource::Tmpfs { .. }) => {
            serde_json::to_value(other).expect("serving shape JSON is serializable")
        }
    }
}

fn service_mode_kind(mode: &ServiceMode) -> &'static str {
    match mode {
        ServiceMode::Replicated { .. } => "replicated",
        ServiceMode::Global => "global",
    }
}

fn sorted_json<T: Serialize>(items: impl IntoIterator<Item = T>) -> Vec<serde_json::Value> {
    let mut values = items
        .into_iter()
        .map(|item| serde_json::to_value(item).expect("serving shape JSON is serializable"))
        .collect::<Vec<_>>();
    values.sort_by_cached_key(ToString::to_string);
    values
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use serde_json::json;

    use crate::{PullPolicy, ResolvedServiceSpec, ServiceId, ServiceMode};

    #[test]
    fn serving_shape_ignores_service_id_resources_replica_count_and_pull_policy() {
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "api:1", "pull_policy": "missing" }
        }))
        .unwrap();
        assert_eq!(spec.serving_shape(), spec.to_requested().serving_shape());

        let mut other = spec.clone();
        other.service_id = ServiceId::parse("b".repeat(32)).unwrap();
        other.container.resources.memory_bytes = Some(crate::ByteQuantity::try_from(64).unwrap());
        other.container.pull_policy = PullPolicy::Always;
        other.mode = ServiceMode::Replicated {
            replicas: NonZeroU32::new(3).unwrap(),
        };
        assert_eq!(spec.serving_shape(), other.serving_shape());

        other.container.image = "api:2".into();
        assert_ne!(spec.serving_shape(), other.serving_shape());
    }

    fn mounting(source: serde_json::Value) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "db",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "postgres:17", "pull_policy": "missing" },
            "volumes": [{ "reference": "data", "source": source }],
            "mounts": [{ "volume": "data", "target": "/data" }],
        }))
        .unwrap()
    }

    #[test]
    fn a_provisioned_volume_grown_live_keeps_the_serving_shape() {
        let provisioned = |name: &str, maximum_bytes: u64| {
            mounting(json!({
                "kind": "provisioned", "name": format!("app_{name}"), "maximum_bytes": maximum_bytes,
                "scope": { "namespace": "app", "logical_name": name }
            }))
        };
        let at_5 = provisioned("data", 5_000_000_000);
        assert_eq!(
            at_5.serving_shape(),
            provisioned("data", 8_000_000_000).serving_shape()
        );
        assert_eq!(
            at_5.serving_shape(),
            provisioned("data", 8_000_000_000)
                .to_requested()
                .serving_shape()
        );
        assert_ne!(
            at_5.serving_shape(),
            provisioned("other", 5_000_000_000).serving_shape()
        );
        let tmpfs =
            |size_bytes: u64| mounting(json!({ "kind": "tmpfs", "size_bytes": size_bytes }));
        assert_ne!(tmpfs(4096).serving_shape(), tmpfs(8192).serving_shape());
    }

    fn web_spec(container: serde_json::Value) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "web",
            "mode": { "mode": "global" },
            "container": container,
            "ports": [{
                "mode": "host",
                "bind": { "kind": "address", "address": "0.0.0.0" },
                "published_port": 8080,
                "container_port": 80,
                "transport_protocol": "tcp"
            }]
        }))
        .unwrap()
    }

    #[test]
    fn serving_shape_token_matches_trunk_for_a_spec_without_a_log_driver() {
        let spec = web_spec(json!({
            "image": "nginx:1.29-alpine",
            "environment": { "PORT": "80" },
            "pull_policy": "missing"
        }));
        assert_eq!(spec.serving_shape().token(), "1c6e8ff538d68038");
        assert_eq!(
            spec.to_requested().serving_shape().token(),
            "1c6e8ff538d68038"
        );
    }

    #[test]
    fn serving_shape_of_a_stored_spec_with_a_log_driver_ignores_it() {
        let stored = web_spec(json!({
            "image": "nginx:1.29-alpine",
            "environment": { "PORT": "80" },
            "pull_policy": "missing",
            "log_driver": { "name": "json-file" }
        }));
        assert_eq!(stored.serving_shape().token(), "1c6e8ff538d68038");
    }

    #[test]
    fn only_a_spec_that_takes_ipv6_changes_the_serving_shape() {
        let container = |ipv6: Option<bool>| {
            let mut container = json!({ "image": "nginx:1.29-alpine", "environment": { "PORT": "80" }, "pull_policy": "missing" });
            if let Some(ipv6) = ipv6 {
                container
                    .as_object_mut()
                    .unwrap()
                    .insert("ipv6".to_owned(), json!(ipv6));
            }
            web_spec(container)
        };
        assert_eq!(
            container(Some(false)).serving_shape().token(),
            "1c6e8ff538d68038"
        );
        assert!(
            serde_json::to_value(&container(Some(false)).container)
                .unwrap()
                .get("ipv6")
                .is_none()
        );
        let ipv6 = container(Some(true));
        assert_ne!(ipv6.serving_shape(), container(None).serving_shape());
        assert_eq!(ipv6.serving_shape(), ipv6.to_requested().serving_shape());
    }
}
