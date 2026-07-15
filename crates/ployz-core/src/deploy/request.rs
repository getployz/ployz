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
pub struct NormalizedDeployRequest(Arc<DeployRequest>);

impl NormalizedDeployRequest {
    pub fn try_new(request: DeployRequest) -> Result<Self, DeployVolumeDeclarationError> {
        for service in &request.services {
            for mount in &service.runtime.volume_mounts {
                if !request.volumes.contains_key(&mount.volume_name) {
                    return Err(DeployVolumeDeclarationError {
                        service_id: service.service_id.clone(),
                        volume_name: mount.volume_name.clone(),
                    });
                }
            }
        }
        Ok(Self(Arc::new(request)))
    }

    #[must_use]
    pub fn namespace_id(&self) -> &NamespaceId {
        &self.0.namespace_id
    }

    #[must_use]
    pub fn namespace_revision_id(&self) -> NamespaceRevisionId {
        self.0.namespace_revision_id()
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.0.status_service_id()
    }

    #[must_use]
    pub fn services(&self) -> Vec<DeployServiceRequest> {
        (0..self.0.services.len())
            .map(|service_index| DeployServiceRequest {
                request: Arc::clone(&self.0),
                service_index,
            })
            .collect()
    }

    #[must_use]
    pub fn into_request(self) -> DeployRequest {
        Arc::try_unwrap(self.0).unwrap_or_else(|request| (*request).clone())
    }

    #[must_use]
    pub fn to_request(&self) -> DeployRequest {
        (*self.0).clone()
    }

    pub fn replace_service_image(
        &mut self,
        service_id: &ServiceId,
        image: ImageReference,
    ) -> Result<(), NormalizedDeployInvariantError> {
        let request = Arc::make_mut(&mut self.0);
        let Some(service) = request
            .services
            .iter_mut()
            .find(|service| service.service_id == *service_id)
        else {
            return Err(NormalizedDeployInvariantError::UnknownService {
                service_id: service_id.clone(),
            });
        };
        service.image = image;
        Ok(())
    }
}

impl std::ops::Deref for NormalizedDeployRequest {
    type Target = DeployRequest;

    fn deref(&self) -> &Self::Target {
        &self.0
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
    request: Arc<DeployRequest>,
    service_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredVolumeMount<'a> {
    mount: &'a ServiceVolumeMount,
    spec: &'a VolumeSpec,
}

impl DeclaredVolumeMount<'_> {
    #[must_use]
    pub fn mount(&self) -> &ServiceVolumeMount {
        self.mount
    }

    #[must_use]
    pub fn spec(&self) -> &VolumeSpec {
        self.spec
    }
}

impl DeployServiceRequest {
    #[must_use]
    pub fn namespace_id(&self) -> &NamespaceId {
        &self.request.namespace_id
    }

    #[must_use]
    pub fn namespace_revision_id(&self) -> NamespaceRevisionId {
        self.request.namespace_revision_id()
    }

    #[must_use]
    pub fn namespace_revision_entry_id(&self) -> NamespaceRevisionEntryId {
        self.service()
            .namespace_revision_entry_id(&self.request.namespace_id)
    }

    pub fn declared_volume_mounts(&self) -> impl Iterator<Item = DeclaredVolumeMount<'_>> {
        self.runtime
            .volume_mounts
            .iter()
            .map(|mount| DeclaredVolumeMount {
                mount,
                spec: self
                    .request
                    .volumes
                    .get(&mount.volume_name)
                    .expect("normalized deploy validates every mounted volume declaration"),
            })
    }
}

impl std::ops::Deref for DeployServiceRequest {
    type Target = DeployServiceSpec;

    fn deref(&self) -> &Self::Target {
        self.service()
    }
}

impl DeployServiceRequest {
    fn service(&self) -> &DeployServiceSpec {
        self.request
            .services
            .get(self.service_index)
            .expect("service views are created only from canonical request indices")
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
