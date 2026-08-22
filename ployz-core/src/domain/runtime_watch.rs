//! Complete Runtime Watch observations and their normalized transport.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{
    Container, ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, DockerVolume, DockerVolumeId, HealthcheckSpec, HookContainer,
    IngressHost, MachineId, MachineObservation, ProjectName, QualifiedService, ResolvedServiceSpec,
    ServiceContainer, ServiceId, ServiceName, ServiceObservation, derive_services,
};

crate::value::open_string_enum!(CertificateAvailability, Unrecognized {
    Available => "available",
    Pending => "pending",
    Failure => "failure",
    Unknown => "unknown",
});

crate::value::open_string_enum!(CertificateFailureKind, Unrecognized {
    DoesNotResolve => "does_not_resolve",
    ResolvesElsewhere => "resolves_elsewhere",
    Authority => "authority",
});

/// Shared backoff clock after a refusal or an authority failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CertificateBackoff {
    pub failure_kind: CertificateFailureKind,
    pub next_attempt_at: String,
    pub failures: u32,
}

/// Redacted certificate status keyed by Ingress Hostname.
///
/// Never carries Certificate Material or HTTP-01 challenge bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CertificateObservation {
    pub hostname: IngressHost,
    pub status: CertificateAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<CertificateBackoff>,
}

/// Typed incomplete replicated IDs. An incomplete row is not a delete.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWatchIncompleteIds {
    #[serde(default)]
    pub machines: Vec<MachineId>,
    #[serde(default)]
    pub containers: Vec<ContainerId>,
    #[serde(default)]
    pub volumes: Vec<DockerVolumeId>,
    #[serde(default)]
    pub certificates: Vec<IngressHost>,
}

/// One complete Runtime Watch observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWatchFrame {
    #[serde(default)]
    pub machines: Vec<MachineObservation>,
    #[serde(default)]
    pub containers: Vec<ContainerObservation>,
    #[serde(default)]
    pub services: Vec<ServiceObservation>,
    #[serde(default)]
    pub volumes: Vec<DockerVolume>,
    #[serde(default)]
    pub certificates: Vec<CertificateObservation>,
    /// Hosted DNS hostname only; never the renewal token or endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_dns_hostname: Option<String>,
    #[serde(default)]
    pub incomplete_ids: RuntimeWatchIncompleteIds,
    /// Freshness of the entry-local membership/RTT sample. Not Cluster truth.
    pub observed_at: String,
}

/// Normalized Runtime Watch transport decoded into an ergonomic [`RuntimeWatchFrame`].
///
/// This is public only because the daemon and native SDK live in separate crates. Runtime Watch
/// consumers receive [`RuntimeWatchFrame`] and never join this transport graph themselves.
#[doc(hidden)]
#[derive(Debug)]
pub struct RuntimeWatchTransportFrame(RuntimeWatchFrame);

impl RuntimeWatchTransportFrame {
    /// Normalize an ergonomic frame for transport.
    #[must_use]
    pub fn from_frame(frame: &RuntimeWatchFrame) -> Self {
        Self::from_owned_frame(frame.clone())
    }

    fn from_owned_frame(mut frame: RuntimeWatchFrame) -> Self {
        frame
            .containers
            .sort_by_key(|container| container.container_id);
        frame.services = derive_services(frame.containers.iter().cloned());
        Self(frame)
    }

    /// Return the validated ergonomic frame.
    #[must_use]
    pub fn into_frame(self) -> RuntimeWatchFrame {
        self.0
    }
}

impl Serialize for RuntimeWatchTransportFrame {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        RuntimeWatchTransportWire::from_frame(&self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RuntimeWatchTransportFrame {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        RuntimeWatchTransportWire::deserialize(deserializer)?
            .into_frame()
            .map(Self::from_owned_frame)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct RuntimeWatchSpecIndex(usize);

#[derive(Serialize, Deserialize)]
struct RuntimeWatchContainerWire {
    container_id: ContainerId,
    display_name: String,
    #[serde(default)]
    created_at_unix_nanos: i64,
    machine_id: MachineId,
    project_name: ProjectName,
    service_id: ServiceId,
    service_name: ServiceName,
    kind: ContainerKind,
    runtime: ContainerRuntimeObservation,
    #[serde(default)]
    effective_healthcheck: Option<HealthcheckSpec>,
    spec_index: RuntimeWatchSpecIndex,
    #[serde(default)]
    address: Option<ContainerAddress>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

impl RuntimeWatchContainerWire {
    fn from_observation(
        observation: &ContainerObservation,
        spec_index: RuntimeWatchSpecIndex,
    ) -> Self {
        Self {
            container_id: observation.container_id,
            display_name: observation.display_name.clone(),
            created_at_unix_nanos: observation.created_at_unix_nanos,
            machine_id: observation.machine_id,
            project_name: observation.project_name.clone(),
            service_id: observation.service_id,
            service_name: observation.service_name.clone(),
            kind: observation.kind,
            runtime: observation.runtime.clone(),
            effective_healthcheck: observation.effective_healthcheck.clone(),
            spec_index,
            address: observation.address,
            labels: observation.labels.clone(),
        }
    }

    fn into_observation(self, resolved_spec: ResolvedServiceSpec) -> ContainerObservation {
        ContainerObservation {
            container_id: self.container_id,
            display_name: self.display_name,
            created_at_unix_nanos: self.created_at_unix_nanos,
            machine_id: self.machine_id,
            project_name: self.project_name,
            service_id: self.service_id,
            service_name: self.service_name,
            kind: self.kind,
            runtime: self.runtime,
            effective_healthcheck: self.effective_healthcheck,
            resolved_spec,
            address: self.address,
            labels: self.labels,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RuntimeWatchServiceWire {
    identity: QualifiedService,
    #[serde(default)]
    member_ids: Vec<ContainerId>,
}

#[derive(Serialize, Deserialize)]
struct RuntimeWatchTransportWire {
    #[serde(default)]
    machines: Vec<MachineObservation>,
    #[serde(default)]
    specs: Vec<ResolvedServiceSpec>,
    #[serde(default)]
    containers: Vec<RuntimeWatchContainerWire>,
    #[serde(default)]
    services: Vec<RuntimeWatchServiceWire>,
    #[serde(default)]
    volumes: Vec<DockerVolume>,
    #[serde(default)]
    certificates: Vec<CertificateObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hosted_dns_hostname: Option<String>,
    #[serde(default)]
    incomplete_ids: RuntimeWatchIncompleteIds,
    observed_at: String,
}

impl RuntimeWatchTransportWire {
    fn from_frame(frame: &RuntimeWatchFrame) -> Self {
        let mut specs = Vec::<ResolvedServiceSpec>::new();
        let containers = frame
            .containers
            .iter()
            .map(|container| {
                // ponytail: linear dedupe is ideal while a frame has few distinct specs; use a
                // hashable canonical spec key only if distinct-spec counts become material.
                let spec_index = specs
                    .iter()
                    .position(|spec| spec == &container.resolved_spec)
                    .unwrap_or_else(|| {
                        specs.push(container.resolved_spec.clone());
                        specs.len() - 1
                    });
                RuntimeWatchContainerWire::from_observation(
                    container,
                    RuntimeWatchSpecIndex(spec_index),
                )
            })
            .collect();
        let mut services = frame
            .services
            .iter()
            .map(|service| {
                let mut member_ids = service
                    .members()
                    .map(|member| member.as_observation().container_id)
                    .collect::<Vec<_>>();
                member_ids.sort_unstable();
                RuntimeWatchServiceWire {
                    identity: service.identity.clone(),
                    member_ids,
                }
            })
            .collect::<Vec<_>>();
        services.sort_by(|left, right| left.identity.cmp(&right.identity));
        Self {
            machines: frame.machines.clone(),
            specs,
            containers,
            services,
            volumes: frame.volumes.clone(),
            certificates: frame.certificates.clone(),
            hosted_dns_hostname: frame.hosted_dns_hostname.clone(),
            incomplete_ids: frame.incomplete_ids.clone(),
            observed_at: frame.observed_at.clone(),
        }
    }

    fn into_frame(self) -> Result<RuntimeWatchFrame, RuntimeWatchGraphError> {
        for (duplicate, spec) in self.specs.iter().enumerate() {
            if let Some(first) = self
                .specs
                .iter()
                .take(duplicate)
                .position(|existing| existing == spec)
            {
                return Err(RuntimeWatchGraphError::DuplicateSpec { first, duplicate });
            }
        }

        let mut referenced_specs = vec![false; self.specs.len()];
        let mut container_indexes = BTreeMap::new();
        let mut containers = Vec::with_capacity(self.containers.len());
        for container in self.containers {
            if container_indexes.contains_key(&container.container_id) {
                return Err(RuntimeWatchGraphError::DuplicateContainer {
                    container_id: container.container_id,
                });
            }
            let spec = self.specs.get(container.spec_index.0).ok_or(
                RuntimeWatchGraphError::MissingSpec {
                    container_id: container.container_id,
                    spec_index: container.spec_index.0,
                },
            )?;
            if spec.service_id != container.service_id || spec.name != container.service_name {
                return Err(RuntimeWatchGraphError::ContradictorySpecIdentity {
                    container_id: container.container_id,
                });
            }
            *referenced_specs
                .get_mut(container.spec_index.0)
                .expect("the resolved spec index was checked") = true;
            let index = containers.len();
            container_indexes.insert(container.container_id, index);
            containers.push(container.into_observation(spec.clone()));
        }
        if let Some(spec_index) = referenced_specs.iter().position(|referenced| !referenced) {
            return Err(RuntimeWatchGraphError::UnreferencedSpec { spec_index });
        }

        let mut service_identities = BTreeSet::new();
        let mut assigned = vec![false; containers.len()];
        let mut services = Vec::with_capacity(self.services.len());
        for service in self.services {
            if !service_identities.insert(service.identity.clone()) {
                return Err(RuntimeWatchGraphError::DuplicateServiceIdentity {
                    identity: Box::new(service.identity),
                });
            }
            if service.member_ids.is_empty() {
                return Err(RuntimeWatchGraphError::EmptyService {
                    identity: Box::new(service.identity),
                });
            }

            let mut members = Vec::with_capacity(service.member_ids.len());
            for member_id in service.member_ids {
                let Some(&index) = container_indexes.get(&member_id) else {
                    return Err(RuntimeWatchGraphError::MissingMember {
                        identity: Box::new(service.identity),
                        container_id: member_id,
                    });
                };
                let assigned = assigned
                    .get_mut(index)
                    .expect("Container indexes are created with the Container rows");
                if *assigned {
                    return Err(RuntimeWatchGraphError::DuplicateMember {
                        container_id: member_id,
                    });
                }
                let member = containers
                    .get(index)
                    .expect("Container indexes are created with the Container rows");
                if member.identity() != service.identity {
                    return Err(RuntimeWatchGraphError::ContradictoryMembership {
                        container_id: member_id,
                        expected: Box::new(service.identity),
                        actual: Box::new(member.identity()),
                    });
                }
                *assigned = true;
                members.push(member.clone());
            }
            let service_id = members
                .iter()
                .max_by_key(|member| (member.created_at_unix_nanos, member.container_id.as_str()))
                .expect("empty Services were rejected")
                .service_id;

            let mut containers = Vec::<ServiceContainer>::new();
            let mut hook_containers = Vec::<HookContainer>::new();
            for member in members {
                match Container::from(member) {
                    Container::Service(container) => containers.push(container),
                    Container::Hook(container) => hook_containers.push(container),
                }
            }
            services.push(ServiceObservation {
                identity: service.identity,
                service_id,
                containers,
                hook_containers,
            });
        }
        if let Some(index) = assigned.iter().position(|assigned| !assigned) {
            return Err(RuntimeWatchGraphError::UnassignedContainer {
                container_id: containers
                    .get(index)
                    .expect("one assignment flag exists per Container row")
                    .container_id,
            });
        }

        Ok(RuntimeWatchFrame {
            machines: self.machines,
            containers,
            services,
            volumes: self.volumes,
            certificates: self.certificates,
            hosted_dns_hostname: self.hosted_dns_hostname,
            incomplete_ids: self.incomplete_ids,
            observed_at: self.observed_at,
        })
    }
}

/// Invalid normalized Runtime Watch reference graph.
#[derive(Debug, Error)]
enum RuntimeWatchGraphError {
    #[error("resolved specs {first} and {duplicate} are duplicates")]
    DuplicateSpec { first: usize, duplicate: usize },
    #[error("Container {container_id} references missing resolved spec {spec_index}")]
    MissingSpec {
        container_id: ContainerId,
        spec_index: usize,
    },
    #[error("resolved spec {spec_index} is not referenced by a Container")]
    UnreferencedSpec { spec_index: usize },
    #[error("Container {container_id} appears more than once")]
    DuplicateContainer { container_id: ContainerId },
    #[error("Container {container_id} contradicts its resolved spec identity")]
    ContradictorySpecIdentity { container_id: ContainerId },
    #[error("Service {identity} appears more than once")]
    DuplicateServiceIdentity { identity: Box<QualifiedService> },
    #[error("Service {identity} has no members")]
    EmptyService { identity: Box<QualifiedService> },
    #[error("Service {identity} references missing Container {container_id}")]
    MissingMember {
        identity: Box<QualifiedService>,
        container_id: ContainerId,
    },
    #[error("Container {container_id} is referenced more than once")]
    DuplicateMember { container_id: ContainerId },
    #[error("Container {container_id} belongs to {actual}, not {expected}")]
    ContradictoryMembership {
        container_id: ContainerId,
        expected: Box<QualifiedService>,
        actual: Box<QualifiedService>,
    },
    #[error("Container {container_id} does not belong to a Service")]
    UnassignedContainer { container_id: ContainerId },
}
