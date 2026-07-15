//! Container runtime specifications used by deploy requests and execution.

use super::*;

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

    #[must_use]
    pub fn stable_storage_name(&self, namespace_id: &NamespaceId) -> String {
        let namespace = namespace_id.as_str();
        format!(
            "ployz-n{}-{namespace}-v{}-{}",
            namespace.len(),
            self.0.len(),
            self.0
        )
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

const ZFS_DATASET_NAME_MAX_BYTES: usize = 255;

/// One physical ZFS pool component; it cannot name an arbitrary dataset root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsPoolName(String);

impl ZfsPoolName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ZfsPoolNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ZfsPoolNameError::Empty);
        }
        if value.contains('/') {
            return Err(ZfsPoolNameError::Hierarchical { value });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
        {
            return Err(ZfsPoolNameError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZfsPoolNameError {
    #[error("ZFS pool name is empty")]
    Empty,
    #[error("ZFS pool name must be one path component: {value}")]
    Hierarchical { value: String },
    #[error("ZFS pool name contains an invalid character: {value}")]
    InvalidCharacter { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"DatasetName\">"))]
#[serde(try_from = "String", into = "String")]
pub struct DatasetName(String);

impl DatasetName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, DatasetNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(DatasetNameError::Empty);
        }
        if value.len() > ZFS_DATASET_NAME_MAX_BYTES {
            return Err(DatasetNameError::NameBudgetExceeded {
                bytes: value.len(),
                maximum: ZFS_DATASET_NAME_MAX_BYTES,
            });
        }
        if value.split('/').any(str::is_empty) {
            return Err(DatasetNameError::EmptyComponent);
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        }) {
            return Err(DatasetNameError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    /// Constructs the only dataset namespace used for Provisioned Volumes.
    pub fn for_volume(
        pool: &ZfsPoolName,
        namespace_id: &NamespaceId,
        volume_name: &VolumeName,
    ) -> Result<Self, DatasetNameError> {
        Self::try_new(format!(
            "{}/ployz/volumes/{}",
            pool.as_str(),
            volume_name.stable_storage_name(namespace_id)
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DatasetName {
    type Error = DatasetNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<DatasetName> for String {
    fn from(value: DatasetName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatasetNameError {
    #[error("dataset name is empty")]
    Empty,
    #[error("dataset name contains an empty path component")]
    EmptyComponent,
    #[error("dataset name contains an invalid character: {value}")]
    InvalidCharacter { value: String },
    #[error("dataset name is {bytes} bytes; maximum is {maximum}")]
    NameBudgetExceeded { bytes: usize, maximum: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"VolumeMaxSizeBytes\">")
)]
#[serde(try_from = "u64", into = "u64")]
pub struct VolumeMaxSizeBytes(NonZeroU64);

impl VolumeMaxSizeBytes {
    pub fn try_new(value: u64) -> Result<Self, VolumeMaxSizeError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(VolumeMaxSizeError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for VolumeMaxSizeBytes {
    type Error = VolumeMaxSizeError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<VolumeMaxSizeBytes> for u64 {
    fn from(value: VolumeMaxSizeBytes) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VolumeMaxSizeError {
    #[error("volume maximum size must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeSpec {
    Plain,
    Provisioned { max_size_bytes: VolumeMaxSizeBytes },
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
