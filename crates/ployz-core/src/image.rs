use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::ids::{MachineId, NamespaceId, ServiceId};
use crate::machine::rpc::{MachineRpcResponder, MachineRpcResponse};
use crate::operation::FailureMessage;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

pub const IMAGE_MESH_REGISTRY_PORT: u16 = 5000;
pub const IMAGE_BLOB_CHUNK_MAX_BYTES: usize = 512 * 1024;
pub const IMAGE_BLOB_PUSH_ACTION_HEADER: &str = "ployz-image-push-action";
pub const IMAGE_BLOB_PUSH_UPLOAD_ID_HEADER: &str = "ployz-image-upload-id";
pub const IMAGE_BLOB_PUSH_OFFSET_HEADER: &str = "ployz-image-offset";
pub const IMAGE_BLOB_PUSH_ACTION_CHUNK: &str = "chunk";
pub const OCI_IMAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_IMAGE_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
pub const OCI_IMAGE_LAYER_GZIP_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct OciDigest(String);

impl OciDigest {
    pub fn try_new(value: impl Into<String>) -> Result<Self, OciDigestError> {
        let value = value.into();
        let Some(encoded) = value.strip_prefix("sha256:") else {
            return Err(OciDigestError::Invalid { value });
        };
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(OciDigestError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn sha256(bytes: &[u8]) -> Self {
        Self(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

impl fmt::Display for OciDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for OciDigest {
    type Error = OciDigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OciDigest> for String {
    fn from(value: OciDigest) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OciDigestError {
    #[error("OCI digest {value:?} must be canonical sha256:<64 lowercase hex characters>")]
    Invalid { value: String },
}

/// One deploy-scoped registry credential. It may cross the operator and
/// machine RPC boundaries, but it is never part of deploy intent or evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryCredential {
    Basic {
        username: RegistryCredentialUsername,
        password: RegistryCredentialSecret,
    },
    IdentityToken {
        token: RegistryCredentialSecret,
    },
}

impl RegistryCredential {
    pub fn try_basic(
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<Self, RegistryCredentialError> {
        Ok(Self::Basic {
            username: RegistryCredentialUsername::try_new(username)?,
            password: RegistryCredentialSecret::try_new(password)?,
        })
    }

    pub fn try_identity_token(token: impl Into<String>) -> Result<Self, RegistryCredentialError> {
        Ok(Self::IdentityToken {
            token: RegistryCredentialSecret::try_new(token)?,
        })
    }

    #[must_use]
    pub fn redact_secret_in(&self, message: impl Into<String>) -> String {
        let secret = match self {
            Self::Basic {
                username: _,
                password,
            } => password,
            Self::IdentityToken { token } => token,
        };
        message.into().replace(secret.secret(), "[redacted]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct RegistryCredentialUsername(String);

impl RegistryCredentialUsername {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RegistryCredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RegistryCredentialError::EmptyUsername);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RegistryCredentialUsername {
    type Error = RegistryCredentialError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RegistryCredentialUsername> for String {
    fn from(value: RegistryCredentialUsername) -> Self {
        let RegistryCredentialUsername(value) = value;
        value
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct RegistryCredentialSecret(String);

impl RegistryCredentialSecret {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RegistryCredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RegistryCredentialError::EmptySecret);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RegistryCredentialSecret {
    type Error = RegistryCredentialError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RegistryCredentialSecret> for String {
    fn from(value: RegistryCredentialSecret) -> Self {
        let RegistryCredentialSecret(value) = value;
        value
    }
}

impl fmt::Debug for RegistryCredentialSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(_secret) = self;
        formatter.write_str("RegistryCredentialSecret([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryCredentialError {
    #[error("registry credential username is empty")]
    EmptyUsername,
    #[error("registry credential secret is empty")]
    EmptySecret,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ImageRepository(String);

impl ImageRepository {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ImageRepositoryError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.split('/').all(|component| {
                component
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                    && component
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && component.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'.' | b'_' | b'-')
                    })
            });
        if !valid {
            return Err(ImageRepositoryError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn for_service(namespace_id: &NamespaceId, service_id: &ServiceId) -> Self {
        Self(format!(
            "ployz/{:x}/{:x}",
            Sha256::digest(namespace_id.as_str().as_bytes()),
            Sha256::digest(service_id.as_str().as_bytes())
        ))
    }
}

impl fmt::Display for ImageRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for ImageRepository {
    type Error = ImageRepositoryError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ImageRepository> for String {
    fn from(value: ImageRepository) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageRepositoryError {
    #[error("image repository {value:?} is invalid")]
    Invalid { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ImageUploadId(String);

impl ImageUploadId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ImageUploadIdError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(ImageUploadIdError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ImageUploadId {
    type Error = ImageUploadIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ImageUploadId> for String {
    fn from(value: ImageUploadId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageUploadIdError {
    #[error("image upload id {value:?} is invalid")]
    Invalid { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "OciPlatformWire", into = "OciPlatformWire")]
pub struct OciPlatform {
    os: String,
    architecture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OciPlatformWire {
    os: String,
    architecture: String,
}

impl OciPlatform {
    pub fn try_new(
        os: impl Into<String>,
        architecture: impl Into<String>,
    ) -> Result<Self, OciPlatformError> {
        let os = normalize_platform_component("os", os.into())?;
        let architecture = normalize_platform_component("architecture", architecture.into())?;
        let architecture = match architecture.as_str() {
            "x86_64" | "x86-64" => "amd64".to_owned(),
            "aarch64" => "arm64".to_owned(),
            _ => architecture,
        };
        Ok(Self { os, architecture })
    }

    #[must_use]
    pub fn current() -> Self {
        Self::try_new(std::env::consts::OS, std::env::consts::ARCH)
            .expect("Rust target OS and architecture form a valid OCI platform")
    }

    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
}

impl TryFrom<OciPlatformWire> for OciPlatform {
    type Error = OciPlatformError;

    fn try_from(value: OciPlatformWire) -> Result<Self, Self::Error> {
        Self::try_new(value.os, value.architecture)
    }
}

impl From<OciPlatform> for OciPlatformWire {
    fn from(value: OciPlatform) -> Self {
        let OciPlatform { os, architecture } = value;
        Self { os, architecture }
    }
}

fn normalize_platform_component(
    field: &'static str,
    value: String,
) -> Result<String, OciPlatformError> {
    let value = value.to_ascii_lowercase();
    if value.is_empty() {
        return Err(OciPlatformError::Empty { field });
    }
    if value.chars().any(|character| {
        !character.is_ascii_lowercase()
            && !character.is_ascii_digit()
            && !matches!(character, '-' | '_' | '.' | '+')
    }) {
        return Err(OciPlatformError::InvalidCharacter { field, value });
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OciPlatformError {
    #[error("OCI platform {field} must not be empty")]
    Empty { field: &'static str },
    #[error("OCI platform {field} contains an invalid character: {value:?}")]
    InvalidCharacter { field: &'static str, value: String },
}

#[cfg(test)]
mod oci_platform_tests {
    use super::*;

    #[test]
    fn platform_normalizes_case_and_architecture_aliases() {
        assert_eq!(
            OciPlatform::try_new("LINUX", "x86_64").expect("platform"),
            OciPlatform::try_new("linux", "amd64").expect("platform")
        );
        assert_eq!(
            OciPlatform::try_new("Linux", "AARCH64").expect("platform"),
            OciPlatform::try_new("linux", "arm64").expect("platform")
        );
    }

    #[test]
    fn platform_wire_cannot_bypass_validation() {
        assert!(
            serde_json::from_str::<OciPlatform>(r#"{"os":"","architecture":"amd64"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<OciPlatform>(r#"{"os":"linux","architecture":"amd 64"}"#)
                .is_err()
        );
        assert_eq!(
            serde_json::from_str::<OciPlatform>(r#"{"os":"LINUX","architecture":"x86_64"}"#)
                .expect("normalized platform"),
            OciPlatform::try_new("linux", "amd64").expect("platform")
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageBlobCheckRequest {
    pub digests: Vec<OciDigest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageBlobCheckOk {
    pub machine_id: MachineId,
    pub present: Vec<OciDigest>,
}

impl MachineRpcResponder for ImageBlobCheckOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type ImageBlobCheckResponse = MachineRpcResponse<ImageBlobCheckOk, ImageRpcDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageBlobPushRequest {
    Begin { digest: OciDigest, total_size: u64 },
    Commit { upload_id: ImageUploadId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageBlobPushOutcome {
    Begun {
        upload_id: ImageUploadId,
    },
    Retained {
        digest: OciDigest,
        size: u64,
        lease_expires_at: ImageContentLeaseExpiresAt,
    },
    ChunkAccepted {
        upload_id: ImageUploadId,
    },
    Committed {
        digest: OciDigest,
        size: u64,
        lease_expires_at: ImageContentLeaseExpiresAt,
    },
}

positive_u64_wire_newtype! {
    /// The machine-reported containerd lease expiration for pushed image content.
    pub struct ImageContentLeaseExpiresAt;
    ts_brand: "Brand<string, \"ImageContentLeaseExpiresAt\">";
    accessor: unix_seconds;
    error: ImageContentLeaseTimestampError;
}

positive_u64_wire_error! {
    pub enum ImageContentLeaseTimestampError;
    noun: "image content lease timestamp";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageBlobPushOk {
    pub machine_id: MachineId,
    pub outcome: ImageBlobPushOutcome,
}

impl MachineRpcResponder for ImageBlobPushOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type ImageBlobPushResponse = MachineRpcResponse<ImageBlobPushOk, ImageRpcDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageManifestPushRequest {
    pub manifest_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageManifestPushOk {
    pub machine_id: MachineId,
    pub manifest_digest: OciDigest,
    pub image_id: OciDigest,
    pub platform: OciPlatform,
    pub lease_expires_at: ImageContentLeaseExpiresAt,
}

impl MachineRpcResponder for ImageManifestPushOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type ImageManifestPushResponse = MachineRpcResponse<ImageManifestPushOk, ImageRpcDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageEnsureRequest {
    Start {
        owner: crate::machine::runtime::ManagedContainerIdentity,
        source: ImageEnsureSource,
    },
    Status {
        owner: crate::machine::runtime::ManagedContainerIdentity,
    },
    Cancel {
        owner: crate::machine::runtime::ManagedContainerIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageEnsureSource {
    Registry {
        reference: crate::deploy::ImageReference,
        platform: OciPlatform,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<crate::deploy::RegistryCredential>,
    },
    MeshSeed {
        seed_host: std::net::Ipv4Addr,
        repository: ImageRepository,
        manifest_digest: OciDigest,
        image_id: OciDigest,
        platform: OciPlatform,
    },
    LocalSeed {
        repository: ImageRepository,
        manifest_digest: OciDigest,
        image_id: OciDigest,
        platform: OciPlatform,
    },
}

impl ImageEnsureSource {
    #[must_use]
    pub fn reference(&self) -> String {
        match self {
            Self::Registry { reference, .. } => reference.as_str().to_owned(),
            Self::MeshSeed {
                seed_host,
                repository,
                manifest_digest,
                ..
            } => format!("{seed_host}:{IMAGE_MESH_REGISTRY_PORT}/{repository}@{manifest_digest}"),
            Self::LocalSeed {
                repository,
                manifest_digest,
                ..
            } => format!("127.0.0.1:{IMAGE_MESH_REGISTRY_PORT}/{repository}@{manifest_digest}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageEnsureStatus {
    Accepted,
    Running {
        progress_sequence: u64,
    },
    Completed {
        reference: crate::deploy::ImageReference,
    },
    Failed {
        failure: ImageEnsureFailure,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageEnsureFailure {
    PullFailed {
        message: crate::operation::FailureMessage,
    },
    Stalled {
        timeout_millis: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageEnsureOk {
    pub machine_id: MachineId,
    pub ensure_status: ImageEnsureStatus,
}

impl MachineRpcResponder for ImageEnsureOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type ImageEnsureResponse = MachineRpcResponse<ImageEnsureOk, ImageRpcDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRemoveRequest {
    pub operation_id: crate::ids::OperationId,
    pub image_identity: OciDigest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageRemoveOutcome {
    Removed,
    AlreadyAbsent,
    RetainedInUse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRemoveOk {
    pub machine_id: MachineId,
    pub outcome: ImageRemoveOutcome,
}

impl MachineRpcResponder for ImageRemoveOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            machine_id,
            outcome: _,
        } = self;
        machine_id
    }
}

pub type ImageRemoveResponse = MachineRpcResponse<ImageRemoveOk, ImageRemoveDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageRemoveDomainError {
    InvalidRequest { message: FailureMessage },
    RemoveFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageRpcDomainError {
    InvalidRequest {
        message: FailureMessage,
    },
    UploadNotFound {
        upload_id: ImageUploadId,
    },
    OffsetMismatch {
        expected: u64,
        actual: u64,
    },
    ChunkTooLarge {
        size: u64,
        maximum: u64,
    },
    ChunkOutOfBounds {
        total_size: u64,
        offset: u64,
        size: u64,
    },
    UploadBusy {
        maximum: u16,
    },
    ServiceUnavailable {
        message: FailureMessage,
    },
    ImageMissing {
        digest: OciDigest,
    },
    DigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
    ConfigMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
    PlatformMismatch {
        expected: OciPlatform,
        actual: OciPlatform,
    },
    StorageFailed {
        message: FailureMessage,
    },
    ImageEnsureConflict {
        owner: Box<crate::machine::runtime::ManagedContainerIdentity>,
    },
    ImageEnsureNotFound {
        owner: Box<crate::machine::runtime::ManagedContainerIdentity>,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        ImageEnsureFailure, ImageEnsureRequest, ImageEnsureSource, ImageEnsureStatus,
        ImageRepository, OciDigest,
    };
    use crate::deploy::ImageReference;
    use crate::ids::{NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use crate::machine::runtime::{ManagedContainerIdentity, ManagedContainerKind};
    use crate::operation::FailureMessage;

    #[test]
    fn oci_digest_accepts_a_canonical_sha256_digest() {
        let value = format!("sha256:{}", "a".repeat(64));

        let digest = OciDigest::try_new(value.clone()).expect("valid digest");

        assert_eq!(digest.as_str(), value);
    }

    #[test]
    fn oci_digest_rejects_non_canonical_values() {
        let values = [
            String::new(),
            "a".repeat(64),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha512:{}", "a".repeat(64)),
        ];

        for value in values {
            assert!(OciDigest::try_new(value).is_err());
        }
    }

    #[test]
    fn image_ensure_actions_round_trip_and_reject_unknown_fields() {
        let identity = owner();
        let actions = [
            ImageEnsureRequest::Start {
                owner: identity.clone(),
                source: ImageEnsureSource::Registry {
                    reference: ImageReference::try_new("registry.example/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("image"),
                    platform: super::OciPlatform::try_new("linux", "amd64").expect("platform"),
                    credential: None,
                },
            },
            ImageEnsureRequest::Status { owner: identity.clone() },
            ImageEnsureRequest::Cancel { owner: identity },
        ];
        for action in actions {
            let json = serde_json::to_value(&action).expect("serialize");
            assert_eq!(
                serde_json::from_value::<ImageEnsureRequest>(json).expect("deserialize"),
                action
            );
        }
        assert!(
            serde_json::from_value::<ImageEnsureRequest>(
                serde_json::json!({"action":"status","owner":owner(),"unexpected":true})
            )
            .is_err()
        );
    }

    #[test]
    fn image_ensure_terminal_statuses_round_trip() {
        let statuses = [
            ImageEnsureStatus::Completed { reference: ImageReference::try_new("registry.example/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("image") },
            ImageEnsureStatus::Failed { failure: ImageEnsureFailure::PullFailed { message: FailureMessage::try_new("pull failed").expect("message") } },
            ImageEnsureStatus::Failed { failure: ImageEnsureFailure::Stalled { timeout_millis: 30_000 } },
            ImageEnsureStatus::Cancelled,
        ];
        for status in statuses {
            let json = serde_json::to_value(&status).expect("serialize");
            assert_eq!(
                serde_json::from_value::<ImageEnsureStatus>(json).expect("deserialize"),
                status
            );
        }
    }

    fn owner() -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            namespace_id: NamespaceId::try_new("default").expect("namespace"),
            service_id: ServiceId::try_new("api").expect("service"),
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_a")
                .expect("entry"),
            operation_id: OperationId::try_new("operation_a").expect("operation"),
            step_id: StepId::try_new("run_1").expect("step"),
            kind: ManagedContainerKind::Service,
        }
    }

    #[test]
    fn repository_rejects_path_traversal() {
        assert!(ImageRepository::try_new("team/../api").is_err());
    }

    #[test]
    fn service_repository_is_valid_and_case_sensitive_for_all_valid_ids() {
        let namespace = NamespaceId::try_new("Prod-East").expect("valid namespace id");
        let uppercase = ServiceId::try_new("API_v2").expect("valid service id");
        let lowercase = ServiceId::try_new("api_v2").expect("valid service id");

        let uppercase = ImageRepository::for_service(&namespace, &uppercase);
        let lowercase = ImageRepository::for_service(&namespace, &lowercase);

        assert!(ImageRepository::try_new(uppercase.as_str()).is_ok());
        assert_ne!(uppercase, lowercase);
    }
}
