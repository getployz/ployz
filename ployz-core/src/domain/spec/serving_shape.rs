//! Content identity of spec fields whose change requires a new Container.

use serde::Serialize;

use super::{
    ConfigMount, ConfigSpec, IngressProxyFragment, Placement, PortPublication,
    RequestedServiceSpec, ResolvedServiceSpec, ServiceContainerSpec, ServiceMode,
};
use crate::{ServiceMount, ServiceName, ServiceVolume};

/// Content identity of the Resolved Service Spec fields whose change requires a new Container.
///
/// Equal shapes mean interchangeable Containers for traffic and Global occupancy.
/// Service ID, replica count, resources, pull policy, pre-deploy hooks, and update
/// timing are not part of the shape.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServingShape(u64);

impl ServingShape {
    /// Shape of one observed Resolved Service Spec.
    #[must_use]
    pub fn of_resolved(spec: &ResolvedServiceSpec) -> Self {
        let ResolvedServiceSpec {
            service_id: _,
            name,
            mode,
            container,
            placement,
            ports,
            volume_graph,
            config_graph,
            pre_deploy: _,
            ingress_proxy_fragment,
            update: _,
        } = spec;
        Self::of_recreate(
            name,
            mode,
            container,
            placement,
            ports,
            volume_graph.volumes(),
            volume_graph.mounts(),
            config_graph.configs(),
            config_graph.mounts(),
            ingress_proxy_fragment.as_ref(),
        )
    }

    /// Shape of one requested spec. Comparable with [`Self::of_resolved`].
    #[must_use]
    pub fn of_requested(spec: &RequestedServiceSpec) -> Self {
        let RequestedServiceSpec {
            name,
            mode,
            container,
            placement,
            ports,
            volume_graph,
            config_graph,
            pre_deploy: _,
            ingress_proxy_fragment,
            update: _,
        } = spec;
        Self::of_recreate(
            name,
            mode,
            container,
            placement,
            ports,
            volume_graph.volumes(),
            volume_graph.mounts(),
            config_graph.configs(),
            config_graph.mounts(),
            ingress_proxy_fragment.as_ref(),
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
    fn of_recreate(
        name: &ServiceName,
        mode: &ServiceMode,
        container: &ServiceContainerSpec,
        placement: &Placement,
        ports: &[PortPublication],
        volumes: &[ServiceVolume],
        mounts: &[ServiceMount],
        configs: &[ConfigSpec],
        config_mounts: &[ConfigMount],
        ingress_proxy_fragment: Option<&IngressProxyFragment>,
    ) -> Self {
        let ServiceContainerSpec {
            image,
            command,
            entrypoint,
            environment,
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
            log_driver,
            resources: _,
            stop_timeout_secs,
            sysctls,
            restart,
        } = container;
        let payload = serde_json::json!({
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
            "log_driver": log_driver,
            "stop_timeout_secs": stop_timeout_secs,
            "sysctls": sysctls,
            "restart": restart,
            "placement": placement,
            "ports": sorted_json(ports),
            "volumes": sorted_json(volumes),
            "mounts": sorted_json(mounts),
            "configs": sorted_json(configs),
            "config_mounts": sorted_json(config_mounts),
            "ingress_proxy_fragment": ingress_proxy_fragment,
        });
        let bytes = serde_json::to_vec(&payload).expect("serving shape JSON is serializable");
        Self(fnv1a64(&bytes))
    }
}

impl ResolvedServiceSpec {
    /// Content identity of the fields whose change requires a new Container.
    #[must_use]
    pub fn serving_shape(&self) -> ServingShape {
        ServingShape::of_resolved(self)
    }
}

impl RequestedServiceSpec {
    /// Content identity of the fields whose change requires a new Container.
    #[must_use]
    pub fn serving_shape(&self) -> ServingShape {
        ServingShape::of_requested(self)
    }
}

fn service_mode_kind(mode: &ServiceMode) -> &'static str {
    match mode {
        ServiceMode::Replicated { .. } => "replicated",
        ServiceMode::Global => "global",
    }
}

fn sorted_json<T: Serialize>(items: &[T]) -> Vec<serde_json::Value> {
    let mut values = items
        .iter()
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
}
