use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributeRequest {
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub target_machines: Vec<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceiveSessionRequest {
    pub operation_id: String,
    pub source_machine: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceivedImportRequest {
    pub operation_id: String,
    pub source_machine: MachineId,
    pub repository: String,
    pub reference: String,
    pub expected_digest: ImageDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo_tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStatusRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<ImageDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInspectRequest {
    pub digest: ImageDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machines: Vec<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePushRequest {
    pub source_image: String,
    pub target_machines: Vec<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<ImageDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStatusPayload {
    pub records: Vec<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInspectPayload {
    pub operation_id: String,
    pub records: Vec<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePushPayload {
    pub operation_id: String,
    pub artifact: ImageArtifact,
    pub targets: Vec<ImageTransferTargetResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributePayload {
    pub operation_id: String,
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub targets: Vec<ImageTransferTargetResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributeValidationPayload {
    #[serde(flatten)]
    pub request: ImageDistributeRequest,
    pub failure: ImageDistributeValidationFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageDistributeValidationFailure {
    TargetRequired {
        target_count: usize,
    },
    DuplicateTarget {
        duplicate_target: MachineId,
    },
    SourceNotLocal {
        source_machine: MachineId,
        local_machine: MachineId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceiveSessionPayload {
    pub target_machine: MachineId,
    pub endpoint: String,
    pub token: String,
    pub expires_at_unix_secs: u64,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceivedImportPayload {
    pub target_machine: MachineId,
    pub record: ImageAvailabilityRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOperationPayload {
    pub operation: ImageOperationRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOperationListPayload {
    pub operations: Vec<ImageOperationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageTransferTargetResult {
    Present {
        machine_id: MachineId,
        record: ImageAvailabilityRecord,
    },
    SkippedPresent {
        machine_id: MachineId,
        record: ImageAvailabilityRecord,
    },
    Failed {
        machine_id: MachineId,
        failure: ImageTransferFailure,
    },
}

impl ImageTransferTargetResult {
    #[must_use]
    pub fn present(machine_id: MachineId, record: ImageAvailabilityRecord) -> Self {
        Self::Present { machine_id, record }
    }

    #[must_use]
    pub fn skipped_present(machine_id: MachineId, record: ImageAvailabilityRecord) -> Self {
        Self::SkippedPresent { machine_id, record }
    }

    #[must_use]
    pub fn failed(machine_id: MachineId, failure: ImageTransferFailure) -> Self {
        Self::Failed {
            machine_id,
            failure,
        }
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        match self {
            Self::Present { machine_id, .. }
            | Self::SkippedPresent { machine_id, .. }
            | Self::Failed { machine_id, .. } => machine_id,
        }
    }

    #[must_use]
    pub fn status(&self) -> ImageTransferTargetStatus {
        match self {
            Self::Present { .. } => ImageTransferTargetStatus::Present,
            Self::SkippedPresent { .. } => ImageTransferTargetStatus::SkippedPresent,
            Self::Failed { .. } => ImageTransferTargetStatus::Failed,
        }
    }

    #[must_use]
    pub fn record(&self) -> Option<&ImageAvailabilityRecord> {
        match self {
            Self::Present { record, .. } | Self::SkippedPresent { record, .. } => Some(record),
            Self::Failed { .. } => None,
        }
    }

    #[must_use]
    pub fn failure(&self) -> Option<&ImageTransferFailure> {
        match self {
            Self::Failed { failure, .. } => Some(failure),
            Self::Present { .. } | Self::SkippedPresent { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageTransferFailure {
    pub code: String,
    pub stage: ImageTransferFailureStage,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransferFailureStage {
    AvailabilityRead,
    SourceVerify,
    SourceExport,
    ArchiveParse,
    ReceiveSession,
    Upload,
    Import,
    LocalAvailability,
    DistributingPushedImage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransferTargetStatus {
    Present,
    SkippedPresent,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Display, JsonSchema)]
pub struct ImageDigest(pub String);

impl ImageDigest {
    pub fn try_new(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        validate_image_digest(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ImageDigest {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for ImageDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(ImageDigestVisitor)
    }
}

struct ImageDigestVisitor;

impl Visitor<'_> for ImageDigestVisitor {
    type Value = ImageDigest;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a valid image digest string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        ImageDigest::try_new(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        ImageDigest::try_new(value).map_err(E::custom)
    }
}

fn validate_image_digest(value: &str) -> std::result::Result<(), String> {
    let Some((algorithm, encoded)) = value.split_once(':') else {
        return Err(format!(
            "image digest '{value}' must include algorithm prefix"
        ));
    };
    if algorithm.is_empty()
        || !algorithm
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | '+'))
    {
        return Err(format!("image digest '{value}' has invalid algorithm"));
    }
    if encoded.len() < 32 || !encoded.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("image digest '{value}' has invalid encoded hash"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImagePlatform {
    pub os: String,
    pub architecture: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageRef {
    Digest {
        digest: ImageDigest,
    },
    RepositoryDigest {
        repository: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        digest: ImageDigest,
    },
}

impl ImageRef {
    #[must_use]
    pub fn digest_only(digest: ImageDigest) -> Self {
        Self::Digest { digest }
    }

    #[must_use]
    pub fn repository_digest(
        repository: impl Into<String>,
        tag: Option<String>,
        digest: ImageDigest,
    ) -> Self {
        Self::RepositoryDigest {
            repository: repository.into(),
            tag,
            digest,
        }
    }

    #[must_use]
    pub fn digest(&self) -> &ImageDigest {
        match self {
            Self::Digest { digest } | Self::RepositoryDigest { digest, .. } => digest,
        }
    }

    #[must_use]
    pub fn repository(&self) -> Option<&str> {
        match self {
            Self::Digest { .. } => None,
            Self::RepositoryDigest { repository, .. } => Some(repository.as_str()),
        }
    }

    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Digest { .. } => None,
            Self::RepositoryDigest { tag, .. } => tag.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildMethod {
    #[display("dockerfile")]
    Dockerfile,
    #[display("railpack")]
    Railpack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildLocation {
    Local,
    Machine { machine_id: MachineId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageArtifactProvenance {
    External {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    Build {
        method: BuildMethod,
        location: BuildLocation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_digest: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageArtifact {
    pub image: ImageRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    pub provenance: ImageArtifactProvenance,
    pub created_at: u64,
}

impl ImageArtifact {
    #[must_use]
    pub fn digest(&self) -> &ImageDigest {
        self.image.digest()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImagePresence {
    Present {
        artifact: ImageArtifact,
        recorded_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_operation_id: Option<String>,
    },
    Absent {
        observed_at: u64,
    },
    Transferring {
        operation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
        updated_at: u64,
    },
    Failed {
        reason: String,
        failed_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageAvailabilityRecord {
    pub machine_id: MachineId,
    pub digest: ImageDigest,
    pub presence: ImagePresence,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    #[display("running")]
    Running,
    #[display("succeeded")]
    Succeeded,
    #[display("failed")]
    Failed,
    #[display("interrupted")]
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageOperationKind {
    #[display("push")]
    Push,
    #[display("distribute")]
    Distribute,
    #[display("inspect")]
    Inspect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BuildOperationKind {
    #[display("local")]
    Local,
    #[display("machine")]
    Machine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageOperationTargetOutcome {
    Running {
        machine_id: MachineId,
    },
    Succeeded {
        machine_id: MachineId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
    },
    Failed {
        machine_id: MachineId,
        last_error: String,
    },
    Interrupted {
        machine_id: MachineId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
}

impl ImageOperationTargetOutcome {
    #[must_use]
    pub fn running(machine_id: MachineId) -> Self {
        Self::Running { machine_id }
    }

    #[must_use]
    pub fn succeeded(machine_id: MachineId, bytes_transferred: Option<u64>) -> Self {
        Self::Succeeded {
            machine_id,
            bytes_transferred,
        }
    }

    #[must_use]
    pub fn failed(machine_id: MachineId, last_error: impl Into<String>) -> Self {
        Self::Failed {
            machine_id,
            last_error: last_error.into(),
        }
    }

    #[must_use]
    pub fn interrupted(machine_id: MachineId, last_error: Option<String>) -> Self {
        Self::Interrupted {
            machine_id,
            last_error,
        }
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        match self {
            Self::Running { machine_id }
            | Self::Succeeded { machine_id, .. }
            | Self::Failed { machine_id, .. }
            | Self::Interrupted { machine_id, .. } => machine_id,
        }
    }

    #[must_use]
    pub fn status(&self) -> OperationStatus {
        match self {
            Self::Running { .. } => OperationStatus::Running,
            Self::Succeeded { .. } => OperationStatus::Succeeded,
            Self::Failed { .. } => OperationStatus::Failed,
            Self::Interrupted { .. } => OperationStatus::Interrupted,
        }
    }

    #[must_use]
    pub fn bytes_transferred(&self) -> Option<u64> {
        match self {
            Self::Succeeded {
                bytes_transferred, ..
            } => *bytes_transferred,
            Self::Running { .. } | Self::Failed { .. } | Self::Interrupted { .. } => None,
        }
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Failed { last_error, .. } => Some(last_error.as_str()),
            Self::Interrupted { last_error, .. } => last_error.as_deref(),
            Self::Running { .. } | Self::Succeeded { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageOperationState {
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Succeeded {},
    Failed {
        last_error: String,
    },
    Interrupted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
}

impl ImageOperationState {
    #[must_use]
    pub fn status(&self) -> OperationStatus {
        match self {
            Self::Running { .. } => OperationStatus::Running,
            Self::Succeeded { .. } => OperationStatus::Succeeded,
            Self::Failed { .. } => OperationStatus::Failed,
            Self::Interrupted { .. } => OperationStatus::Interrupted,
        }
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Failed { last_error } => Some(last_error.as_str()),
            Self::Running { last_error } | Self::Interrupted { last_error } => {
                last_error.as_deref()
            }
            Self::Succeeded { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageOperationRecord {
    pub id: String,
    pub kind: ImageOperationKind,
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<ImageDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_machine: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<ImageOperationTargetOutcome>,
    pub started_at: u64,
    pub updated_at: u64,
    pub state: ImageOperationState,
}

impl ImageOperationRecord {
    #[must_use]
    pub fn status(&self) -> OperationStatus {
        self.state.status()
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.state.last_error()
    }
}
