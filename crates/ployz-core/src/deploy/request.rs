//! Caller-facing deploy requests and service declarations.

use super::images::registry_image_source;
use super::*;

pub const DEFAULT_DEPLOY_RESERVATION_TTL_SECONDS: u64 = 60 * 60;
const MAX_DEPLOY_ORIGIN_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"DeployOrigin\">"))]
#[serde(try_from = "String", into = "String")]
pub struct DeployOrigin(String);

impl DeployOrigin {
    pub fn try_new(value: impl Into<String>) -> Result<Self, DeployOriginError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DeployOriginError::Empty);
        }
        if value.len() > MAX_DEPLOY_ORIGIN_BYTES {
            return Err(DeployOriginError::TooLong { bytes: value.len() });
        }
        if value.chars().any(char::is_control) {
            return Err(DeployOriginError::ControlCharacter);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DeployOrigin {
    type Error = DeployOriginError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<DeployOrigin> for String {
    fn from(value: DeployOrigin) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployOriginError {
    #[error("deploy origin must not be empty")]
    Empty,
    #[error("deploy origin is {bytes} bytes; maximum is 128")]
    TooLong { bytes: usize },
    #[error("deploy origin must not contain control characters")]
    ControlCharacter,
}

positive_u64_wire_newtype! {
    pub struct DeployReservationId;
    ts_brand: "Brand<string, \"DeployReservationId\">";
    accessor: get;
    error: DeployReservationNumberError;
}

positive_u64_wire_newtype! {
    pub struct DeployReservationExpiresAt;
    ts_brand: "Brand<string, \"DeployReservationExpiresAt\">";
    accessor: unix_seconds;
    error: DeployReservationNumberError;
}

positive_u64_wire_error! {
    pub enum DeployReservationNumberError;
    noun: "deploy reservation number";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub namespace_id: NamespaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DeployOrigin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<VolumeName, VolumeSpec>,
    pub services: Vec<DeployServiceSpec>,
}

impl DeployRequest {
    pub fn synthesize_plain_volume_declarations(&mut self) {
        for service in &self.services {
            for mount in &service.runtime.volume_mounts {
                self.volumes
                    .entry(mount.volume_name.clone())
                    .or_insert(VolumeSpec::Plain);
            }
        }
    }

    #[must_use]
    pub fn namespace_revision_id(&self) -> NamespaceRevisionId {
        namespace_revision_id_for(&self.namespace_id, &self.services)
    }

    #[must_use]
    pub fn primary_service(&self) -> Option<&DeployServiceSpec> {
        self.services.first()
    }

    #[must_use]
    pub fn primary_service_id(&self) -> Option<&ServiceId> {
        self.primary_service().map(|service| &service.service_id)
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.primary_service_id().cloned().unwrap_or_else(|| {
            ServiceId::try_new(self.namespace_id.as_str().to_owned())
                .expect("namespace id is a valid service id fallback")
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDeployRequest {
    namespace_id: NamespaceId,
    origin: Option<DeployOrigin>,
    unmounted_volumes: BTreeMap<VolumeName, VolumeSpec>,
    services: Vec<DeployServiceRequest>,
}

impl NormalizedDeployRequest {
    pub fn try_new(request: DeployRequest) -> Result<Self, DeployVolumeDeclarationError> {
        let DeployRequest {
            namespace_id,
            origin,
            volumes,
            services,
        } = request;
        let namespace_revision_id = namespace_revision_id_for(&namespace_id, &services);
        let mut mounted_volumes = BTreeSet::new();
        let services = services
            .into_iter()
            .map(|service| {
                DeployServiceRequest::try_new(
                    namespace_id.clone(),
                    namespace_revision_id.clone(),
                    service,
                    &volumes,
                    &mut mounted_volumes,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let unmounted_volumes = volumes
            .into_iter()
            .filter(|(name, _)| !mounted_volumes.contains(name))
            .collect();
        Ok(Self {
            namespace_id,
            origin,
            unmounted_volumes,
            services,
        })
    }

    #[must_use]
    pub fn namespace_id(&self) -> &NamespaceId {
        &self.namespace_id
    }

    #[must_use]
    pub fn namespace_revision_id(&self) -> NamespaceRevisionId {
        self.services.first().map_or_else(
            || namespace_revision_id_for(&self.namespace_id, &[]),
            |service| service.namespace_revision_id.clone(),
        )
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.services.first().map_or_else(
            || {
                ServiceId::try_new(self.namespace_id.as_str().to_owned())
                    .expect("namespace id is a valid service id fallback")
            },
            |service| service.service_id.clone(),
        )
    }

    #[must_use]
    pub fn services(&self) -> &[DeployServiceRequest] {
        &self.services
    }

    #[must_use]
    pub fn into_parts(self) -> (DeployRequest, Vec<DeployServiceRequest>) {
        let Self {
            namespace_id,
            origin,
            mut unmounted_volumes,
            services,
        } = self;
        for service in &services {
            service.copy_volume_declarations_into(&mut unmounted_volumes);
        }
        let request = DeployRequest {
            namespace_id,
            origin,
            volumes: unmounted_volumes,
            services: services.iter().map(DeployServiceRequest::to_spec).collect(),
        };
        (request, services)
    }

    #[must_use]
    pub fn into_request(self) -> DeployRequest {
        let Self {
            namespace_id,
            origin,
            unmounted_volumes,
            services,
        } = self;
        let mut volumes = unmounted_volumes;
        let services = services
            .into_iter()
            .map(|service| service.into_spec(&mut volumes))
            .collect();
        DeployRequest {
            namespace_id,
            origin,
            volumes,
            services,
        }
    }

    #[must_use]
    pub fn to_request(&self) -> DeployRequest {
        let mut volumes = self.unmounted_volumes.clone();
        for service in &self.services {
            service.copy_volume_declarations_into(&mut volumes);
        }
        DeployRequest {
            namespace_id: self.namespace_id.clone(),
            origin: self.origin.clone(),
            volumes,
            services: self
                .services
                .iter()
                .map(DeployServiceRequest::to_spec)
                .collect(),
        }
    }

    pub fn replace_service_image(
        &mut self,
        service_id: &ServiceId,
        image: ImageReference,
    ) -> Result<(), NormalizedDeployInvariantError> {
        let Some(service) = self
            .services
            .iter_mut()
            .find(|service| service.service_id == *service_id)
        else {
            return Err(NormalizedDeployInvariantError::UnknownService {
                service_id: service_id.clone(),
            });
        };
        service.image = image;
        let specs = self
            .services
            .iter()
            .map(DeployServiceRequest::to_spec)
            .collect::<Vec<_>>();
        let revision_id = namespace_revision_id_for(&self.namespace_id, &specs);
        for (service, spec) in self.services.iter_mut().zip(&specs) {
            service.namespace_revision_id = revision_id.clone();
            service.namespace_revision_entry_id =
                spec.namespace_revision_entry_id(&self.namespace_id);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NormalizedDeployInvariantError {
    #[error("normalized deploy does not contain service {}", .service_id.as_str())]
    UnknownService { service_id: ServiceId },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "service {} mounts volume {} without a declaration",
    .service_id.as_str(),
    .volume_name.as_str()
)]
pub struct DeployVolumeDeclarationError {
    pub service_id: ServiceId,
    pub volume_name: VolumeName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServiceSpec {
    pub service_id: ServiceId,
    pub image: ImageReference,
    #[serde(
        default = "registry_image_source",
        skip_serializing_if = "ImageSource::is_registry"
    )]
    pub image_source: ImageSource,
    pub replicas: ReplicaCount,
    pub runtime: ContainerRuntimeSpec,
    // Pre-start hooks and dependencies guide planning and execution, not container identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_start: Option<PreStartHook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<ServiceDependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<DeployRoute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DependencyCondition {
    Started,
    Healthy,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceDependency {
    pub service_id: ServiceId,
    pub condition: DependencyCondition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PreStartHook {
    pub command: ContainerCommand,
}

impl DeployServiceSpec {
    pub(super) const NAMESPACE_REVISION_ENTRY_ENCODING_VERSION: &'static str =
        "ployz.namespace_revision_entry.v8";
    pub(super) const NAMESPACE_REVISION_ENCODING_VERSION: &'static str =
        "ployz.namespace_revision.v6";

    #[must_use]
    pub fn namespace_revision_entry_id(
        &self,
        namespace_id: &NamespaceId,
    ) -> NamespaceRevisionEntryId {
        namespace_revision_entry_id_for(
            namespace_id,
            &self.service_id,
            &self.image,
            &self.image_source,
            &self.runtime,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployServiceRequest {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub image: ImageReference,
    pub image_source: ImageSource,
    pub replicas: ReplicaCount,
    runtime: ContainerRuntimeSpec,
    declared_volume_mounts: Vec<DeclaredVolumeMount>,
    pub pre_start: Option<PreStartHook>,
    pub depends_on: Vec<ServiceDependency>,
    pub routes: Vec<DeployRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredVolumeMount {
    mount: ServiceVolumeMount,
    spec: VolumeSpec,
}

impl DeclaredVolumeMount {
    #[must_use]
    pub fn new(mount: ServiceVolumeMount, spec: VolumeSpec) -> Self {
        Self { mount, spec }
    }

    #[must_use]
    pub fn mount(&self) -> &ServiceVolumeMount {
        &self.mount
    }

    #[must_use]
    pub fn spec(&self) -> &VolumeSpec {
        &self.spec
    }
}

impl DeployServiceRequest {
    pub fn try_from_spec(
        namespace_id: NamespaceId,
        namespace_revision_id: NamespaceRevisionId,
        namespace_revision_entry_id: NamespaceRevisionEntryId,
        service: DeployServiceSpec,
        volumes: &BTreeMap<VolumeName, VolumeSpec>,
    ) -> Result<Self, DeployVolumeDeclarationError> {
        let mut mounted_volumes = BTreeSet::new();
        let mut request = Self::try_new(
            namespace_id,
            namespace_revision_id,
            service,
            volumes,
            &mut mounted_volumes,
        )?;
        request.namespace_revision_entry_id = namespace_revision_entry_id;
        Ok(request)
    }

    fn try_new(
        namespace_id: NamespaceId,
        namespace_revision_id: NamespaceRevisionId,
        service: DeployServiceSpec,
        volumes: &BTreeMap<VolumeName, VolumeSpec>,
        mounted_volumes: &mut BTreeSet<VolumeName>,
    ) -> Result<Self, DeployVolumeDeclarationError> {
        let declared_volume_mounts = service
            .runtime
            .volume_mounts
            .iter()
            .map(|mount| {
                let Some(spec) = volumes.get(&mount.volume_name) else {
                    return Err(DeployVolumeDeclarationError {
                        service_id: service.service_id.clone(),
                        volume_name: mount.volume_name.clone(),
                    });
                };
                mounted_volumes.insert(mount.volume_name.clone());
                Ok(DeclaredVolumeMount {
                    mount: mount.clone(),
                    spec: spec.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            namespace_revision_entry_id: service.namespace_revision_entry_id(&namespace_id),
            namespace_id,
            service_id: service.service_id,
            namespace_revision_id,
            image: service.image,
            image_source: service.image_source,
            replicas: service.replicas,
            runtime: service.runtime,
            declared_volume_mounts,
            pre_start: service.pre_start,
            depends_on: service.depends_on,
            routes: service.routes,
        })
    }

    #[must_use]
    pub fn runtime(&self) -> &ContainerRuntimeSpec {
        &self.runtime
    }

    #[must_use]
    pub fn declared_volume_mounts(&self) -> &[DeclaredVolumeMount] {
        &self.declared_volume_mounts
    }

    pub fn replace_declared_volume_mounts(&mut self, mounts: Vec<DeclaredVolumeMount>) {
        self.runtime.volume_mounts = mounts
            .iter()
            .map(|declared| declared.mount.clone())
            .collect();
        self.declared_volume_mounts = mounts;
    }

    fn to_spec(&self) -> DeployServiceSpec {
        DeployServiceSpec {
            service_id: self.service_id.clone(),
            image: self.image.clone(),
            image_source: self.image_source.clone(),
            replicas: self.replicas,
            runtime: self.runtime.clone(),
            pre_start: self.pre_start.clone(),
            depends_on: self.depends_on.clone(),
            routes: self.routes.clone(),
        }
    }

    fn copy_volume_declarations_into(&self, volumes: &mut BTreeMap<VolumeName, VolumeSpec>) {
        for declared in &self.declared_volume_mounts {
            volumes.insert(declared.mount.volume_name.clone(), declared.spec.clone());
        }
    }

    fn into_spec(self, volumes: &mut BTreeMap<VolumeName, VolumeSpec>) -> DeployServiceSpec {
        for declared in self.declared_volume_mounts {
            volumes.insert(declared.mount.volume_name, declared.spec);
        }
        DeployServiceSpec {
            service_id: self.service_id,
            image: self.image,
            image_source: self.image_source,
            replicas: self.replicas,
            runtime: self.runtime,
            pre_start: self.pre_start,
            depends_on: self.depends_on,
            routes: self.routes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"ReplicaCount\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicaCount(NonZeroU16);

impl ReplicaCount {
    pub fn try_new(value: u16) -> Result<Self, ReplicaCountError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(ReplicaCountError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for ReplicaCount {
    type Error = ReplicaCountError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicaCount> for u16 {
    fn from(value: ReplicaCount) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaCountError {
    #[error("replica count must be greater than zero")]
    Zero,
}
