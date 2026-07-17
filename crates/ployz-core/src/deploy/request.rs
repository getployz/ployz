//! Caller-facing deploy requests and service declarations.

use super::images::registry_image_source;
use super::*;
use crate::network::internal_dns::InternalServiceName;

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

    pub fn replace_service_image(
        &mut self,
        service_id: &ServiceId,
        image: ImageReference,
    ) -> Result<(), DeployImageReplacementError> {
        let Some(service) = self
            .services
            .iter_mut()
            .find(|service| service.service_id == *service_id)
        else {
            return Err(DeployImageReplacementError::UnknownService {
                service_id: service_id.clone(),
            });
        };
        service.image = image;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployImageReplacementError {
    #[error("deploy request does not contain service {}", .service_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployTargetValidationError {
    #[error("service {} is declared more than once", .service_id.as_str())]
    DuplicateServiceId { service_id: ServiceId },
    #[error(transparent)]
    UndeclaredVolume(#[from] DeployVolumeDeclarationError),
    #[error(
        "service {} in namespace {} cannot form an internal DNS name",
        .service_id.as_str(),
        .namespace_id.as_str()
    )]
    InvalidInternalServiceName {
        service_id: ServiceId,
        namespace_id: NamespaceId,
    },
    #[error("service {} pushed image reference {source}", .service_id.as_str())]
    InvalidPushedImage {
        service_id: ServiceId,
        #[source]
        source: PushedImageReferenceError,
    },
}

pub(super) fn validate_deploy_target<'a>(
    namespace_id: &NamespaceId,
    volumes: &BTreeMap<VolumeName, VolumeSpec>,
    services: impl IntoIterator<Item = (&'a ServiceId, &'a ContainerRuntimeSpec)>,
) -> Result<(), DeployTargetValidationError> {
    let services = services.into_iter().collect::<Vec<_>>();
    for (service_id, runtime) in &services {
        for mount in &runtime.volume_mounts {
            if !volumes.contains_key(&mount.volume_name) {
                return Err(DeployVolumeDeclarationError {
                    service_id: (*service_id).clone(),
                    volume_name: mount.volume_name.clone(),
                }
                .into());
            }
        }
    }
    for (service_id, _) in services {
        if InternalServiceName::try_from_ids(service_id, namespace_id).is_err() {
            return Err(DeployTargetValidationError::InvalidInternalServiceName {
                service_id: service_id.clone(),
                namespace_id: namespace_id.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_normalization_rejects_an_invalid_internal_service_name() {
        let service_id = ServiceId::try_new("a".repeat(64)).expect("service id");
        let namespace_id = NamespaceId::try_new("default").expect("namespace id");
        let request = DeployRequest {
            namespace_id: namespace_id.clone(),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![DeployServiceSpec {
                service_id: service_id.clone(),
                image: ImageReference::try_new("ghcr.io/acme/api:current").expect("image"),
                image_source: ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("replicas"),
                keep: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        };
        let error = DeployPlanningTarget::try_from_deploy(&request)
            .expect_err("internal service name is invalid");

        assert_eq!(
            error,
            DeployTargetValidationError::InvalidInternalServiceName {
                service_id,
                namespace_id,
            }
        );
    }

    #[test]
    fn deploy_normalization_rejects_duplicate_service_ids() {
        let service_id = ServiceId::try_new("api").expect("service id");
        let service = DeployServiceSpec {
            service_id: service_id.clone(),
            image: ImageReference::try_new("ghcr.io/acme/api:current").expect("image"),
            image_source: ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("replicas"),
            keep: None,
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        };
        let request = DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![service.clone(), service],
        };

        assert_eq!(
            DeployPlanningTarget::try_from_deploy(&request)
                .expect_err("duplicate service ids must be rejected"),
            DeployTargetValidationError::DuplicateServiceId { service_id }
        );
    }
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
    /// Number of newest stopped superseded containers retained for inspection.
    /// Absence preserves full container cleanup and disables image reclamation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<ContainerRetentionCount>,
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
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"ContainerRetentionCount\">")
)]
#[serde(transparent)]
pub struct ContainerRetentionCount(u16);

impl ContainerRetentionCount {
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl From<u16> for ContainerRetentionCount {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

impl From<ContainerRetentionCount> for u16 {
    fn from(value: ContainerRetentionCount) -> Self {
        value.0
    }
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
        "ployz.namespace_revision_entry.v9";
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
