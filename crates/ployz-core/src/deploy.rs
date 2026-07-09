//! Deploy policy and planning models.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::num::{NonZeroI64, NonZeroU16, NonZeroU64};

use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId, ServiceId,
};
use crate::image::OciDigest;
use crate::machine_runtime::{MachineContainerObservationSnapshot, ManagedContainerIdentity};
use crate::ops::{RoutePort, RouteTarget};
use crate::state::{RouteBindingState, ServingTargetEntry, VolumePinState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub namespace_id: NamespaceId,
    pub services: Vec<DeployServiceSpec>,
}

impl DeployRequest {
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

    #[must_use]
    pub fn service_requests(&self) -> Vec<DeployServiceRequest> {
        let namespace_revision_id = self.namespace_revision_id();
        self.services
            .iter()
            .map(|service| DeployServiceRequest {
                namespace_id: self.namespace_id.clone(),
                service_id: service.service_id.clone(),
                namespace_revision_id: namespace_revision_id.clone(),
                namespace_revision_entry_id: service
                    .namespace_revision_entry_id(&self.namespace_id),
                image: service.image.clone(),
                image_source: service.image_source.clone(),
                replicas: service.replicas,
                runtime: service.runtime.clone(),
                routes: service.routes.clone(),
            })
            .collect()
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
    pub runtime: ContainerRuntimeSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<DeployRoute>,
}

impl DeployServiceSpec {
    const NAMESPACE_REVISION_ENTRY_ENCODING_VERSION: &'static str =
        "ployz.namespace_revision_entry.v6";
    const NAMESPACE_REVISION_ENCODING_VERSION: &'static str = "ployz.namespace_revision.v3";

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
        hash_frame(
            &mut hasher,
            "replicas",
            service.replicas.get().to_string().as_bytes(),
        );
        hash_runtime_spec(&mut hasher, &service.runtime);

        let mut routes = service.routes.iter().collect::<Vec<_>>();
        routes.sort_by(|left, right| {
            left.target
                .cmp(&right.target)
                .then_with(|| left.endpoint_port.cmp(&right.endpoint_port))
        });
        for route in routes {
            match &route.target {
                DeployRouteTarget::AutoHostname { port } => {
                    hash_frame(&mut hasher, "route_target_kind", b"auto_hostname");
                    hash_frame(
                        &mut hasher,
                        "route_public_port",
                        port.get().to_string().as_bytes(),
                    );
                }
                DeployRouteTarget::Hostname { hostname, port } => {
                    hash_frame(&mut hasher, "route_target_kind", b"hostname");
                    hash_frame(&mut hasher, "route_hostname", hostname.as_str().as_bytes());
                    hash_frame(
                        &mut hasher,
                        "route_public_port",
                        port.get().to_string().as_bytes(),
                    );
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
        ImageSource::PushedToSeed { image_id, .. } => {
            hash_frame(&mut hasher, "image_id", image_id.as_str().as_bytes());
        }
    }
    hash_runtime_spec(&mut hasher, runtime);
    let digest = hasher.finalize();
    NamespaceRevisionEntryId::try_new(format!("{digest:x}"))
        .expect("sha256 hex digest is a subject token")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageSource {
    Registry,
    PushedToSeed {
        seed: MachineId,
        manifest_digest: OciDigest,
        image_id: OciDigest,
    },
}

impl ImageSource {
    #[must_use]
    pub const fn is_registry(&self) -> bool {
        matches!(self, Self::Registry)
    }
}

fn registry_image_source() -> ImageSource {
    ImageSource::Registry
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServiceRequest {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub image: ImageReference,
    pub image_source: ImageSource,
    pub replicas: ReplicaCount,
    pub runtime: ContainerRuntimeSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<DeployRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"EnvName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct EnvName(String);

impl EnvName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, EnvNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(EnvNameError::Empty);
        }
        if value.contains('=') {
            return Err(EnvNameError::ContainsEquals { value });
        }
        if value.contains('\0') {
            return Err(EnvNameError::ContainsNul { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EnvName {
    type Error = EnvNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<EnvName> for String {
    fn from(value: EnvName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvNameError {
    #[error("environment variable name is empty")]
    Empty,
    #[error("environment variable name contains '=': {value}")]
    ContainsEquals { value: String },
    #[error("environment variable name contains NUL: {value}")]
    ContainsNul { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"EnvValue\">"))]
#[serde(try_from = "String", into = "String")]
pub struct EnvValue(String);

impl EnvValue {
    pub fn try_new(value: impl Into<String>) -> Result<Self, EnvValueError> {
        let value = value.into();
        if value.contains('\0') {
            return Err(EnvValueError::ContainsNul { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EnvValue {
    type Error = EnvValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<EnvValue> for String {
    fn from(value: EnvValue) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvValueError {
    #[error("environment variable value contains NUL: {value}")]
    ContainsNul { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"VolumeName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct VolumeName(String);

impl VolumeName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, VolumeNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(VolumeNameError::Empty);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(VolumeNameError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for VolumeName {
    type Error = VolumeNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<VolumeName> for String {
    fn from(value: VolumeName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VolumeNameError {
    #[error("volume name is empty")]
    Empty,
    #[error("volume name contains invalid characters: {value}")]
    InvalidCharacter { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"ContainerMountPath\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct ContainerMountPath(String);

impl ContainerMountPath {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ContainerMountPathError> {
        let value = value.into();
        if !value.starts_with('/') {
            return Err(ContainerMountPathError::NotAbsolute { value });
        }
        if value.contains('\0') {
            return Err(ContainerMountPathError::ContainsNul { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContainerMountPath {
    type Error = ContainerMountPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ContainerMountPath> for String {
    fn from(value: ContainerMountPath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainerMountPathError {
    #[error("container mount path must be absolute: {value}")]
    NotAbsolute { value: String },
    #[error("container mount path contains NUL: {value}")]
    ContainsNul { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceVolumeMount {
    pub volume_name: VolumeName,
    pub target: ContainerMountPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Record<EnvName, EnvValue>"))]
#[serde(transparent)]
pub struct ServiceEnvironment(BTreeMap<EnvName, EnvValue>);

impl ServiceEnvironment {
    #[must_use]
    pub fn empty() -> Self {
        Self(BTreeMap::new())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&EnvName, &EnvValue)> {
        self.0.iter()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<BTreeMap<EnvName, EnvValue>> for ServiceEnvironment {
    fn from(value: BTreeMap<EnvName, EnvValue>) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Array<string>"))]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct ContainerCommand(Vec<String>);

impl ContainerCommand {
    pub fn try_new(value: Vec<String>) -> Result<Self, ContainerCommandError> {
        if value.is_empty() {
            return Err(ContainerCommandError::Empty);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl TryFrom<Vec<String>> for ContainerCommand {
    type Error = ContainerCommandError;

    fn try_from(value: Vec<String>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ContainerCommand> for Vec<String> {
    fn from(value: ContainerCommand) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainerCommandError {
    #[error("container command must not be empty")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"HealthcheckShellCommand\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct HealthcheckShellCommand(String);

impl HealthcheckShellCommand {
    pub fn try_new(value: impl Into<String>) -> Result<Self, HealthcheckShellCommandError> {
        let value = value.into();
        if value.is_empty() {
            return Err(HealthcheckShellCommandError::Empty);
        }
        if value.contains('\0') {
            return Err(HealthcheckShellCommandError::ContainsNul { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HealthcheckShellCommand {
    type Error = HealthcheckShellCommandError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<HealthcheckShellCommand> for String {
    fn from(value: HealthcheckShellCommand) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthcheckShellCommandError {
    #[error("healthcheck shell command must not be empty")]
    Empty,
    #[error("healthcheck shell command contains NUL: {value}")]
    ContainsNul { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ContainerEntrypoint {
    Clear,
    Argv(ContainerCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"HealthcheckDurationNanos\">")
)]
#[serde(try_from = "u64", into = "u64")]
pub struct HealthcheckDurationNanos(NonZeroU64);

impl HealthcheckDurationNanos {
    pub fn try_new(value: u64) -> Result<Self, HealthcheckDurationNanosError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(HealthcheckDurationNanosError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for HealthcheckDurationNanos {
    type Error = HealthcheckDurationNanosError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<HealthcheckDurationNanos> for u64 {
    fn from(value: HealthcheckDurationNanos) -> Self {
        value.as_nanos()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthcheckDurationNanosError {
    #[error("healthcheck duration must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"HealthcheckRetries\">")
)]
#[serde(try_from = "u16", into = "u16")]
pub struct HealthcheckRetries(NonZeroU16);

impl HealthcheckRetries {
    pub fn try_new(value: u16) -> Result<Self, HealthcheckRetriesError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(HealthcheckRetriesError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for HealthcheckRetries {
    type Error = HealthcheckRetriesError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<HealthcheckRetries> for u16 {
    fn from(value: HealthcheckRetries) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthcheckRetriesError {
    #[error("healthcheck retries must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ContainerHealthcheckTest {
    Inherit,
    Disable,
    Exec(ContainerCommand),
    Shell(HealthcheckShellCommand),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ContainerHealthcheck {
    pub test: ContainerHealthcheckTest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<HealthcheckDurationNanos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<HealthcheckDurationNanos>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<HealthcheckRetries>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_period: Option<HealthcheckDurationNanos>,
}

impl ContainerHealthcheck {
    /// Whether this healthcheck makes Docker run a probe and report a
    /// health status the deploy can wait on. `Disable` turns probing off,
    /// and `Inherit` only probes when the image defines its own healthcheck,
    /// so neither guarantees Docker will ever report health: gating a deploy
    /// on them would wait until the step timeout instead of succeeding.
    #[must_use]
    pub const fn reports_docker_health(&self) -> bool {
        match self.test {
            ContainerHealthcheckTest::Exec(_) | ContainerHealthcheckTest::Shell(_) => true,
            ContainerHealthcheckTest::Inherit | ContainerHealthcheckTest::Disable => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "kebab-case")]
pub enum ContainerRestartPolicy {
    DockerDefault,
    No,
    Always,
    OnFailure,
    UnlessStopped,
}

impl ContainerRestartPolicy {
    #[must_use]
    pub const fn as_docker_name(self) -> &'static str {
        match self {
            Self::DockerDefault => "",
            Self::No => "no",
            Self::Always => "always",
            Self::OnFailure => "on-failure",
            Self::UnlessStopped => "unless-stopped",
        }
    }
}

const fn default_restart_policy() -> ContainerRestartPolicy {
    ContainerRestartPolicy::DockerDefault
}

fn is_default_restart_policy(value: &ContainerRestartPolicy) -> bool {
    *value == ContainerRestartPolicy::DockerDefault
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"LinuxCapability\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct LinuxCapability(String);

impl LinuxCapability {
    pub fn try_new(value: impl Into<String>) -> Result<Self, LinuxCapabilityError> {
        let value = value.into();
        if value.is_empty() {
            return Err(LinuxCapabilityError::Empty);
        }
        if value.contains('\0') {
            return Err(LinuxCapabilityError::ContainsNul { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LinuxCapability {
    type Error = LinuxCapabilityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<LinuxCapability> for String {
    fn from(value: LinuxCapability) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinuxCapabilityError {
    #[error("Linux capability must not be empty")]
    Empty,
    #[error("Linux capability contains NUL: {value}")]
    ContainsNul { value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"NanoCpus\">"))]
#[serde(try_from = "u64", into = "u64")]
pub struct NanoCpus(NonZeroU64);

impl NanoCpus {
    pub fn try_new(value: u64) -> Result<Self, ResourceLimitError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(ResourceLimitError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for NanoCpus {
    type Error = ResourceLimitError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NanoCpus> for u64 {
    fn from(value: NanoCpus) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"MemoryBytes\">"))]
#[serde(try_from = "u64", into = "u64")]
pub struct MemoryBytes(NonZeroU64);

impl MemoryBytes {
    pub fn try_new(value: u64) -> Result<Self, ResourceLimitError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(ResourceLimitError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for MemoryBytes {
    type Error = ResourceLimitError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MemoryBytes> for u64 {
    fn from(value: MemoryBytes) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"PidsLimit\">"))]
#[serde(try_from = "i64", into = "i64")]
pub struct PidsLimit(NonZeroI64);

impl PidsLimit {
    pub fn try_new(value: i64) -> Result<Self, ResourceLimitError> {
        let Some(value) = NonZeroI64::new(value) else {
            return Err(ResourceLimitError::Zero);
        };
        if value.get() < -1 {
            return Err(ResourceLimitError::BelowMinusOne);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0.get()
    }
}

impl TryFrom<i64> for PidsLimit {
    type Error = ResourceLimitError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<PidsLimit> for i64 {
    fn from(value: PidsLimit) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceLimitError {
    #[error("resource limit must be greater than zero")]
    Zero,
    #[error("pids limit must be -1 or greater")]
    BelowMinusOne,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ContainerResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nano_cpus: Option<NanoCpus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<MemoryBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<PidsLimit>,
}

impl ContainerResourceLimits {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nano_cpus.is_none() && self.memory_bytes.is_none() && self.pids.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"StopGracePeriod\">"))]
#[serde(from = "u32", into = "u32")]
pub struct StopGracePeriod(u32);

impl StopGracePeriod {
    pub const DEFAULT_SECONDS: u32 = 10;

    #[must_use]
    pub const fn default_grace() -> Self {
        Self(Self::DEFAULT_SECONDS)
    }

    #[must_use]
    pub const fn as_seconds(self) -> u32 {
        self.0
    }
}

impl From<u32> for StopGracePeriod {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<StopGracePeriod> for u32 {
    fn from(value: StopGracePeriod) -> Self {
        value.as_seconds()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ContainerRuntimeSpec {
    pub command: Option<ContainerCommand>,
    pub entrypoint: Option<ContainerEntrypoint>,
    pub environment: ServiceEnvironment,
    pub stop_grace_period: StopGracePeriod,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_mounts: Vec<ServiceVolumeMount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<ContainerHealthcheck>,
    #[serde(
        default = "default_restart_policy",
        skip_serializing_if = "is_default_restart_policy"
    )]
    pub restart_policy: ContainerRestartPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_add: Vec<LinuxCapability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_drop: Vec<LinuxCapability>,
    #[serde(default, skip_serializing_if = "ContainerResourceLimits::is_empty")]
    pub resources: ContainerResourceLimits,
}

impl ContainerRuntimeSpec {
    #[must_use]
    pub fn image_defaults() -> Self {
        Self {
            command: None,
            entrypoint: None,
            environment: ServiceEnvironment::empty(),
            stop_grace_period: StopGracePeriod::default_grace(),
            volume_mounts: Vec::new(),
            healthcheck: None,
            restart_policy: ContainerRestartPolicy::DockerDefault,
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            resources: ContainerResourceLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRoute {
    pub target: DeployRouteTarget,
    pub endpoint_port: RoutePort,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployRouteTarget {
    AutoHostname {
        port: RoutePort,
    },
    Hostname {
        hostname: crate::ops::RouteHostname,
        port: RoutePort,
    },
}

impl DeployRouteTarget {
    /// The route table target this declaration binds to, if it already names
    /// one. `AutoHostname` is declared intent without a hostname: it commits
    /// no `RouteBindingState` until the lease flow mints its hostname, so it
    /// yields `None` here rather than a sentinel target.
    #[must_use]
    pub fn concrete_target(&self) -> Option<RouteTarget> {
        match self {
            Self::AutoHostname { port: _ } => None,
            Self::Hostname { hostname, port } => Some(RouteTarget {
                hostname: hostname.clone(),
                port: *port,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningInput {
    pub request: DeployServiceRequest,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
    pub volume_pins: Vec<VolumePinState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingServiceReplica {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupContainer {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPreparationInput {
    pub request: DeployServiceRequest,
    pub eligible_machines: Vec<MachineId>,
    /// Machines under drain intent: their running replicas do not count as
    /// existing capacity, so the plan replaces them elsewhere and the
    /// unselected containers fall through to cleanup. This is how drain
    /// evacuates on the next deploy (ADR 0026). A separate list rather than
    /// filtering by `eligible_machines`: replica reuse is reality-driven and
    /// deliberately independent of placement scope, so only explicit drain
    /// intent may disqualify observed capacity.
    pub draining_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeploy {
    pub request: DeployServiceRequest,
    pub route_commits: Vec<RouteBindingState>,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPlan {
    pub namespace_id: NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub services: Vec<DeployServicePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_pin_commits: Vec<VolumePinState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_containers: Vec<DeployCleanupContainer>,
}

impl DeployPlan {
    #[must_use]
    pub fn target_machines(&self) -> Vec<MachineId> {
        let mut machines = self
            .services
            .iter()
            .flat_map(|service| service.steps.iter())
            .map(|step| match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id.clone(),
            })
            .collect::<Vec<_>>();
        machines.sort();
        machines.dedup();
        machines
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServicePlan {
    pub service_id: ServiceId,
    pub steps: Vec<DeployPlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployPlanStep {
    UseExistingContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        slot: ReplicaSlot,
    },
    RunContainer {
        machine_id: MachineId,
        slot: ReplicaSlot,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"ReplicaSlot\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicaSlot(u16);

impl ReplicaSlot {
    pub fn try_new(value: u16) -> Result<Self, ReplicaSlotError> {
        if value == 0 {
            return Err(ReplicaSlotError::Zero);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for ReplicaSlot {
    type Error = ReplicaSlotError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicaSlot> for u16 {
    fn from(value: ReplicaSlot) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaSlotError {
    #[error("replica slot must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanError {
    NoEligibleMachines,
    ConflictingVolumePins {
        service_id: ServiceId,
        machines: Vec<MachineId>,
    },
}

#[must_use]
pub fn prepare_deploy(input: DeployPreparationInput) -> PreparedDeploy {
    let route_commits = route_binding_commits(&input.request);
    let existing_replicas = existing_replicas(
        &input.request,
        &input.observed_machines,
        &input.draining_machines,
    );
    let cleanup_candidates = cleanup_candidates(&input.request, &input.observed_machines);

    PreparedDeploy {
        request: input.request,
        route_commits,
        eligible_machines: input.eligible_machines,
        existing_replicas,
        cleanup_candidates,
    }
}

/// Route binding removals are a namespace-level decision: the manifest is
/// the full desired state, so any stored binding whose target no service in
/// the manifest declares is detached - including bindings owned by services
/// the manifest omits entirely.
#[must_use]
pub fn namespace_route_binding_removals(
    namespace_id: &NamespaceId,
    declared_targets: &[RouteTarget],
    stored_bindings: &[RouteBindingState],
) -> Vec<RouteTarget> {
    stored_bindings
        .iter()
        .filter(|binding| {
            binding.namespace_id == *namespace_id && !declared_targets.contains(&binding.target)
        })
        .map(|binding| binding.target.clone())
        .collect()
}

/// Serving target entries of services the manifest omits are unpublished:
/// manifest omission removes a service from the namespace, so its entry
/// must not stay serveable in stored state.
#[must_use]
pub fn namespace_serving_target_removals(
    namespace_id: &NamespaceId,
    declared_services: &[ServiceId],
    stored_entries: &[ServingTargetEntry],
) -> Vec<ServingTargetEntry> {
    stored_entries
        .iter()
        .filter(|entry| {
            entry.namespace_id == *namespace_id && !declared_services.contains(&entry.service_id)
        })
        .cloned()
        .collect()
}

/// Replicas on draining machines are excluded from reuse: they keep serving
/// until convergence, but not counting them here means the plan places their
/// replacement on an eligible machine and cleanup removes the original.
fn existing_replicas(
    request: &DeployServiceRequest,
    observed_machines: &[MachineContainerObservationSnapshot],
    draining_machines: &[MachineId],
) -> Vec<ExistingServiceReplica> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            !draining_machines.contains(&container.machine_id)
                && container.state.is_running()
                && container.identity.is_service_entry(
                    &request.namespace_id,
                    &request.service_id,
                    &request.namespace_revision_entry_id,
                )
        })
        .map(|container| ExistingServiceReplica {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
        })
        .collect()
}

fn cleanup_candidates(
    request: &DeployServiceRequest,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_service()
                && container.identity.namespace_id == request.namespace_id
                && container.identity.service_id == request.service_id
        })
        .map(|container| DeployCleanupContainer {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            identity: container.identity.clone(),
        })
        .collect()
}

fn route_binding_commits(request: &DeployServiceRequest) -> Vec<RouteBindingState> {
    request
        .routes
        .iter()
        .cloned()
        .filter_map(|route| {
            route
                .target
                .concrete_target()
                .map(|target| RouteBindingState {
                    namespace_id: request.namespace_id.clone(),
                    target,
                    endpoint_port: route.endpoint_port,
                    service_id: request.service_id.clone(),
                })
        })
        .collect()
}

pub fn plan_namespace_deploy(
    namespace_id: NamespaceId,
    namespace_revision_id: NamespaceRevisionId,
    services: Vec<DeployPlanningInput>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> Result<DeployPlan, DeployPlanError> {
    let mut service_plans = Vec::new();
    let mut cleanup_containers = cleanup_containers;
    let mut volume_pin_commits = Vec::new();
    let mut working_volume_pins = services
        .iter()
        .flat_map(|input| input.volume_pins.iter().cloned())
        .collect::<Vec<_>>();
    for mut input in services {
        input.volume_pins = working_volume_pins.clone();
        let plan = plan_deploy_service(input)?;
        working_volume_pins.extend(plan.volume_pin_commits.clone());
        volume_pin_commits.extend(plan.volume_pin_commits);
        service_plans.push(DeployServicePlan {
            service_id: plan.service_id,
            steps: plan.steps,
        });
        cleanup_containers.extend(plan.cleanup_containers);
    }

    Ok(DeployPlan {
        namespace_id,
        namespace_revision_id,
        services: service_plans,
        volume_pin_commits,
        cleanup_containers,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySingleServicePlan {
    pub service_id: ServiceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub steps: Vec<DeployPlanStep>,
    pub volume_pin_commits: Vec<VolumePinState>,
    pub cleanup_containers: Vec<DeployCleanupContainer>,
}

fn plan_deploy_service(
    input: DeployPlanningInput,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let volume_placement =
        volume_placement(&input.request, &input.eligible_machines, &input.volume_pins)?;
    let target_replicas = usize::from(input.request.replicas.get());
    let mut existing_replicas = input.existing_replicas;
    if let Some(machine_id) = &volume_placement.machine_id {
        existing_replicas.retain(|replica| &replica.machine_id == machine_id);
    }
    existing_replicas.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    existing_replicas.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });
    let mut steps = existing_replicas
        .into_iter()
        .take(target_replicas)
        .enumerate()
        .map(|(index, replica)| DeployPlanStep::UseExistingContainer {
            machine_id: replica.machine_id,
            container_id: replica.container_id,
            slot: ReplicaSlot((index + 1) as u16),
        })
        .collect::<Vec<_>>();
    let missing_replicas = target_replicas.saturating_sub(steps.len());
    if missing_replicas > 0 && input.eligible_machines.is_empty() {
        return Err(DeployPlanError::NoEligibleMachines);
    }

    let existing_replicas = steps.len();
    let run_machines = volume_placement
        .machine_id
        .as_ref()
        .map(|machine_id| vec![machine_id.clone()])
        .unwrap_or(input.eligible_machines);
    steps.extend(
        run_machines
            .iter()
            .cycle()
            .take(missing_replicas)
            .enumerate()
            .map(|(index, machine_id)| {
                let slot = ReplicaSlot((existing_replicas + index + 1) as u16);
                DeployPlanStep::RunContainer {
                    machine_id: machine_id.clone(),
                    slot,
                }
            }),
    );
    let selected_containers = steps
        .iter()
        .filter_map(|step| match step {
            DeployPlanStep::UseExistingContainer { container_id, .. } => Some(container_id),
            DeployPlanStep::RunContainer { .. } => None,
        })
        .collect::<Vec<_>>();
    let mut cleanup_containers = input.cleanup_candidates;
    cleanup_containers.retain(|candidate| !selected_containers.contains(&&candidate.container_id));
    cleanup_containers.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    cleanup_containers.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });

    Ok(DeploySingleServicePlan {
        service_id: input.request.service_id,
        namespace_revision_id: input.request.namespace_revision_id,
        steps,
        volume_pin_commits: volume_placement.commits,
        cleanup_containers,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumePlacement {
    machine_id: Option<MachineId>,
    commits: Vec<VolumePinState>,
}

fn volume_placement(
    request: &DeployServiceRequest,
    eligible_machines: &[MachineId],
    volume_pins: &[VolumePinState],
) -> Result<VolumePlacement, DeployPlanError> {
    if request.runtime.volume_mounts.is_empty() {
        return Ok(VolumePlacement {
            machine_id: None,
            commits: Vec::new(),
        });
    }

    let matching_pins = request
        .runtime
        .volume_mounts
        .iter()
        .filter_map(|mount| {
            volume_pins.iter().find(|pin| {
                pin.namespace_id == request.namespace_id && pin.volume_name == mount.volume_name
            })
        })
        .collect::<Vec<_>>();
    let mut pinned_machines = matching_pins
        .iter()
        .map(|pin| pin.machine_id.clone())
        .collect::<Vec<_>>();
    pinned_machines.sort();
    pinned_machines.dedup();
    match pinned_machines.as_slice() {
        [machine_id] => {
            if !eligible_machines.contains(machine_id) {
                return Err(DeployPlanError::NoEligibleMachines);
            }
            Ok(VolumePlacement {
                machine_id: Some(machine_id.clone()),
                commits: missing_volume_pin_commits(request, machine_id, volume_pins),
            })
        }
        [] => {
            let Some(machine_id) = eligible_machines.first() else {
                return Err(DeployPlanError::NoEligibleMachines);
            };
            Ok(VolumePlacement {
                machine_id: Some(machine_id.clone()),
                commits: missing_volume_pin_commits(request, machine_id, volume_pins),
            })
        }
        _ => Err(DeployPlanError::ConflictingVolumePins {
            service_id: request.service_id.clone(),
            machines: pinned_machines,
        }),
    }
}

fn missing_volume_pin_commits(
    request: &DeployServiceRequest,
    machine_id: &MachineId,
    volume_pins: &[VolumePinState],
) -> Vec<VolumePinState> {
    let mut volume_names = request
        .runtime
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<Vec<_>>();
    volume_names.sort();
    volume_names.dedup();

    volume_names
        .into_iter()
        .filter(|mount| {
            !volume_pins
                .iter()
                .any(|pin| pin.namespace_id == request.namespace_id && pin.volume_name == *mount)
        })
        .map(|volume_name| VolumePinState {
            namespace_id: request.namespace_id.clone(),
            volume_name,
            machine_id: machine_id.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"ImageReference\">"))]
#[serde(try_from = "String", into = "String")]
pub struct ImageReference(String);

impl ImageReference {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ImageReferenceError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ImageReferenceError::Empty);
        }

        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ImageReferenceError::InvalidCharacter { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ImageReference {
    type Error = ImageReferenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ImageReference> for String {
    fn from(value: ImageReference) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageReferenceError {
    #[error("image reference is empty")]
    Empty,
    #[error("image reference contains invalid characters: {value}")]
    InvalidCharacter { value: String },
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
