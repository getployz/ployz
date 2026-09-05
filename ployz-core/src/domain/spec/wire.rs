//! Checked wire admission and serialization for Service specifications.

use super::{
    ConfigMount, ConfigSpec, IngressProxyFragment, Placement, PortPublication, PreDeployHook,
    RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, ServiceContainerSpec,
    ServiceMode, UpdateConfig,
};
use crate::{
    ServiceConfigGraph, ServiceId, ServiceMount, ServiceName, ServiceSpecGraphError, ServiceVolume,
    ServiceVolumeGraph,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct ServiceContainerSpecWire {
    #[serde(flatten)]
    spec: ServiceContainerSpec,
    #[serde(default)]
    config_mounts: Vec<ConfigMount>,
}

/// External requested declarations, admitted as one mount graph.
#[derive(Serialize, Deserialize)]
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
    ingress_proxy_fragment: Option<IngressProxyFragment>,
    #[serde(default)]
    update: UpdateConfig,
}

/// External resolved declarations, additionally requiring scoped Volume sources.
#[derive(Serialize, Deserialize)]
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
    ingress_proxy_fragment: Option<IngressProxyFragment>,
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
            ingress_proxy_fragment: wire.ingress_proxy_fragment,
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
            ingress_proxy_fragment: spec.ingress_proxy_fragment,
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
                ServiceVolumeGraph::parse(
                    wire.volumes
                        .into_iter()
                        .map(|volume| ServiceVolume {
                            reference: volume.reference,
                            source: volume.source.into_requested(),
                        })
                        .collect(),
                    wire.mounts,
                )?,
                ServiceConfigGraph::parse(wire.configs, wire.container.config_mounts)?,
            )?
            .try_into()?,
            pre_deploy: wire.pre_deploy,
            ingress_proxy_fragment: wire.ingress_proxy_fragment,
            update: wire.update,
        })
    }
}

impl From<ResolvedServiceSpec> for ResolvedServiceSpecWire {
    fn from(spec: ResolvedServiceSpec) -> Self {
        let (volumes, configs) = spec.mount_graph.into_parts();
        let (volumes, mounts) = volumes.into_parts();
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
            volumes: volumes
                .into_iter()
                .map(|volume| ResolvedServiceVolumeWire {
                    reference: volume.reference,
                    source: volume
                        .source
                        .try_into()
                        .expect("resolved graph establishes scope"),
                })
                .collect(),
            mounts,
            configs,
            pre_deploy: spec.pre_deploy,
            ingress_proxy_fragment: spec.ingress_proxy_fragment,
            update: spec.update,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ResolvedServiceVolumeWire {
    reference: crate::ServiceVolumeReference,
    source: crate::ResolvedVolumeSource,
}
