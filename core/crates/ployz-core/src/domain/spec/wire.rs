//! Checked wire admission and serialization for Service specifications.

use super::{
    ConfigMount, ConfigSpec, Placement, PortPublication, PreDeployHook, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, ServiceContainerSpec, ServiceMode, UpdateConfig,
};
use crate::{
    ServiceConfigGraph, ServiceId, ServiceMount, ServiceName, ServiceSpecGraphError, ServiceVolume,
    ServiceVolumeGraph,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS)]
#[ts(rename = "ServiceContainerSpec")]
struct ServiceContainerSpecWire {
    #[serde(flatten)]
    spec: ServiceContainerSpec,
    #[serde(default)]
    config_mounts: Vec<ConfigMount>,
}

/// External requested declarations, admitted as one mount graph.
#[derive(Serialize, Deserialize, TS)]
#[ts(rename = "RequestedServiceSpec")]
pub(super) struct RequestedServiceSpecWire {
    name: ServiceName,
    mode: ServiceMode,
    container: ServiceContainerSpecWire,
    #[serde(default)]
    placement: Placement,
    #[serde(default)]
    ports: Vec<PortPublication>,
    #[serde(default)]
    volumes: Vec<ServiceVolume>,
    #[serde(default)]
    mounts: Vec<ServiceMount>,
    #[serde(default)]
    configs: Vec<ConfigSpec>,
    #[serde(default)]
    pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    update: UpdateConfig,
}

/// External resolved declarations, additionally requiring scoped Volume sources.
#[derive(Serialize, Deserialize, TS)]
#[ts(rename = "ResolvedServiceSpec")]
pub(super) struct ResolvedServiceSpecWire {
    service_id: ServiceId,
    name: ServiceName,
    mode: ServiceMode,
    container: ServiceContainerSpecWire,
    #[serde(default)]
    placement: Placement,
    #[serde(default)]
    ports: Vec<PortPublication>,
    #[serde(default)]
    volumes: Vec<ResolvedServiceVolumeWire>,
    #[serde(default)]
    mounts: Vec<ServiceMount>,
    #[serde(default)]
    configs: Vec<ConfigSpec>,
    #[serde(default)]
    pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    update: ResolvedUpdateConfig,
}

impl TryFrom<RequestedServiceSpecWire> for RequestedServiceSpec {
    type Error = ServiceSpecGraphError;

    fn try_from(wire: RequestedServiceSpecWire) -> Result<Self, Self::Error> {
        Ok(Self {
            name: wire.name,
            mode: wire.mode,
            container: wire.container.spec,
            placement: wire.placement,
            ports: wire.ports,
            mount_graph: crate::ServiceMountGraph::parse(
                ServiceVolumeGraph::parse(wire.volumes, wire.mounts)?,
                ServiceConfigGraph::parse(wire.configs, wire.container.config_mounts)?,
            )?,
            pre_deploy: wire.pre_deploy,
            update: wire.update,
        })
    }
}

impl From<RequestedServiceSpec> for RequestedServiceSpecWire {
    fn from(spec: RequestedServiceSpec) -> Self {
        let (volumes, configs) = spec.mount_graph.into_parts();
        let (volumes, mounts) = volumes.into_parts();
        let (configs, config_mounts) = configs.into_parts();
        Self {
            name: spec.name,
            mode: spec.mode,
            container: ServiceContainerSpecWire {
                spec: spec.container,
                config_mounts,
            },
            placement: spec.placement,
            ports: spec.ports,
            volumes,
            mounts,
            configs,
            pre_deploy: spec.pre_deploy,
            update: spec.update,
        }
    }
}

impl TryFrom<ResolvedServiceSpecWire> for ResolvedServiceSpec {
    type Error = ServiceSpecGraphError;

    fn try_from(wire: ResolvedServiceSpecWire) -> Result<Self, Self::Error> {
        Ok(Self {
            service_id: wire.service_id,
            name: wire.name,
            mode: wire.mode,
            container: wire.container.spec,
            placement: wire.placement,
            ports: wire.ports,
            mount_graph: crate::ServiceMountGraph::parse(
                ServiceVolumeGraph::parse(volumes_from_wire(wire.volumes), wire.mounts)?,
                ServiceConfigGraph::parse(wire.configs, wire.container.config_mounts)?,
            )?
            .try_into()?,
            pre_deploy: wire.pre_deploy,
            update: wire.update,
        })
    }
}

impl From<ResolvedServiceSpec> for ResolvedServiceSpecWire {
    fn from(spec: ResolvedServiceSpec) -> Self {
        let (volumes, configs) = spec.mount_graph.into_parts();
        let (volumes, mounts) = volumes_to_wire(volumes);
        let (configs, config_mounts) = configs.into_parts();
        Self {
            service_id: spec.service_id,
            name: spec.name,
            mode: spec.mode,
            container: ServiceContainerSpecWire {
                spec: spec.container,
                config_mounts,
            },
            placement: spec.placement,
            ports: spec.ports,
            volumes,
            mounts,
            configs,
            pre_deploy: spec.pre_deploy,
            update: spec.update,
        }
    }
}

#[derive(Serialize, Deserialize, TS)]
#[ts(rename = "ResolvedServiceVolume")]
struct ResolvedServiceVolumeWire {
    reference: crate::ServiceVolumeReference,
    source: crate::ResolvedVolumeSource,
}

fn volumes_from_wire(volumes: Vec<ResolvedServiceVolumeWire>) -> Vec<ServiceVolume> {
    volumes
        .into_iter()
        .map(|volume| ServiceVolume {
            reference: volume.reference,
            source: volume.source.into_requested(),
        })
        .collect()
}

fn volumes_to_wire(
    graph: crate::ResolvedServiceVolumeGraph,
) -> (Vec<ResolvedServiceVolumeWire>, Vec<ServiceMount>) {
    let (volumes, mounts) = graph.into_parts();
    let volumes = volumes
        .into_iter()
        .map(|volume| ResolvedServiceVolumeWire {
            reference: volume.reference,
            source: volume
                .source
                .try_into()
                .expect("a resolved graph holds only scoped sources"),
        })
        .collect();
    (volumes, mounts)
}

/// Storage-only wire admission retains the same checked scope as resolved Services.
#[derive(Serialize, Deserialize, TS)]
#[ts(rename = "ServiceStorageSpec")]
pub(super) struct ServiceStorageSpecWire {
    placement: Placement,
    volumes: Vec<ResolvedServiceVolumeWire>,
    mounts: Vec<ServiceMount>,
}

impl TryFrom<ServiceStorageSpecWire> for super::ServiceStorageSpec {
    type Error = crate::ServiceVolumeGraphError;

    fn try_from(wire: ServiceStorageSpecWire) -> Result<Self, Self::Error> {
        Ok(Self {
            placement: wire.placement,
            volumes: ServiceVolumeGraph::parse(volumes_from_wire(wire.volumes), wire.mounts)?
                .try_into()?,
        })
    }
}

impl From<super::ServiceStorageSpec> for ServiceStorageSpecWire {
    fn from(spec: super::ServiceStorageSpec) -> Self {
        let (volumes, mounts) = volumes_to_wire(spec.volumes);
        Self {
            placement: spec.placement,
            volumes,
            mounts,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{ResolvedServiceSpec, ServiceStorageSpec};

    fn volumes() -> serde_json::Value {
        json!([
            {
                "reference": "data",
                "source": {
                    "kind": "provisioned",
                    "name": "app_data",
                    "maximum_bytes": 5_000_000_000_u64,
                    "labels": {},
                    "scope": { "namespace": "app", "logical_name": "data" }
                }
            },
            {
                "reference": "scratch",
                "source": { "kind": "tmpfs", "size_bytes": 4096, "mode": null, "options": [], "scope": null }
            }
        ])
    }

    fn mounts() -> serde_json::Value {
        json!([
            { "volume": "data", "target": "/data", "read_only": false, "no_copy": false, "subpath": null },
            { "volume": "scratch", "target": "/scratch", "read_only": false, "no_copy": false, "subpath": null }
        ])
    }

    fn resolved() -> serde_json::Value {
        json!({
            "service_id": "a".repeat(32),
            "name": "db",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "postgres:17",
                "pull_policy": "missing",
                "config_mounts": [{ "config_name": "app", "target": "/app" }]
            },
            "volumes": volumes(),
            "mounts": mounts(),
            "configs": [{ "name": "app", "content": [104, 105] }],
        })
    }

    /// The Resolved Service Spec is replicated and sealed into Container labels: its Volumes,
    /// mounts and Configs keep their wire shape through admission and back.
    #[test]
    fn a_resolved_spec_with_a_provisioned_volume_writes_the_volumes_it_read() {
        let spec: ResolvedServiceSpec = serde_json::from_value(resolved()).unwrap();
        let wire = serde_json::to_value(&spec).unwrap();
        assert_eq!(wire.pointer("/volumes"), Some(&volumes()));
        assert_eq!(wire.pointer("/mounts"), Some(&mounts()));
        assert_eq!(wire.pointer("/configs"), resolved().pointer("/configs"));
        assert_eq!(
            wire.pointer("/container/config_mounts/0/target"),
            Some(&json!("/app"))
        );
        assert_eq!(
            serde_json::from_value::<ResolvedServiceSpec>(wire).unwrap(),
            spec
        );
    }

    #[test]
    fn a_storage_spec_writes_the_volumes_its_resolved_spec_read() {
        let spec: ResolvedServiceSpec = serde_json::from_value(resolved()).unwrap();
        let storage = ServiceStorageSpec::from(&spec);
        let wire = serde_json::to_value(&storage).unwrap();
        assert_eq!(wire.pointer("/volumes"), Some(&volumes()));
        assert_eq!(wire.pointer("/mounts"), Some(&mounts()));
        assert_eq!(
            serde_json::from_value::<ServiceStorageSpec>(wire).unwrap(),
            storage
        );
    }

    #[test]
    fn a_managed_volume_without_its_scope_is_refused_by_both_wire_forms() {
        let mut unscoped = resolved();
        unscoped
            .pointer_mut("/volumes/0/source")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("scope");
        assert!(serde_json::from_value::<ResolvedServiceSpec>(unscoped.clone()).is_err());
        let storage =
            json!({ "placement": {}, "volumes": unscoped.pointer("/volumes"), "mounts": mounts() });
        assert!(serde_json::from_value::<ServiceStorageSpec>(storage).is_err());
    }
}
