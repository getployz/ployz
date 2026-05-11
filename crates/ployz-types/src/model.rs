use derive_more::Display;
use ipnet::Ipv4Net;
use schemars::JsonSchema;
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU16;
use strum::EnumString;

use crate::spec::{
    Namespace, VolumeCloneConsistency, VolumeCloneDataPolicy, VolumeScope, stable_hash_hex,
};

macro_rules! validated_string_id {
    ($(#[$meta:meta])* pub struct $name:ident($label:literal);) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self::try_new(value).expect(concat!("valid ", $label))
            }

            pub fn try_new(value: impl Into<String>) -> std::result::Result<Self, String> {
                let value = value.into();
                validate_non_empty_identifier(&value, $label)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.into_string()
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = String;

            fn try_from(value: &str) -> std::result::Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }
    };
}

fn validate_non_empty_identifier(value: &str, label: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} cannot contain control characters"));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(format!("{label} cannot contain whitespace"));
    }
    Ok(())
}

validated_string_id!(pub struct MachineId("machine id"););

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "u16", into = "u16")]
pub struct NonZeroReplicaCount(NonZeroU16);

impl NonZeroReplicaCount {
    pub fn try_new(value: u16) -> std::result::Result<Self, String> {
        NonZeroU16::new(value)
            .map(Self)
            .ok_or_else(|| "replica count must be non-zero".to_string())
    }

    #[must_use]
    pub fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for NonZeroReplicaCount {
    type Error = String;

    fn try_from(value: u16) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NonZeroReplicaCount> for u16 {
    fn from(value: NonZeroReplicaCount) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "f64", into = "f64")]
pub struct PositiveScalar(f64);

impl PositiveScalar {
    pub fn try_new(value: f64) -> std::result::Result<Self, String> {
        if !value.is_finite() || value <= 0.0 {
            return Err("positive scalar must be finite and greater than zero".to_string());
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for PositiveScalar {
    type Error = String;

    fn try_from(value: f64) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<PositiveScalar> for f64 {
    fn from(value: PositiveScalar) -> Self {
        value.get()
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(with = "String")]
#[serde(try_from = "String")]
pub struct RedactedSecretString(String);

impl RedactedSecretString {
    const REDACTED: &'static str = "<redacted>";

    pub fn try_new(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        if value.is_empty() {
            return Err("secret cannot be empty".to_string());
        }
        if value.chars().any(char::is_control) {
            return Err("secret cannot contain control characters".to_string());
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RedactedSecretString {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl fmt::Debug for RedactedSecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::REDACTED)
    }
}

impl fmt::Display for RedactedSecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::REDACTED)
    }
}

impl Serialize for RedactedSecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(Self::REDACTED)
    }
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
pub struct ImageRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub digest: ImageDigest,
}

impl ImageRef {
    #[must_use]
    pub fn digest(&self) -> &ImageDigest {
        &self.digest
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
pub struct ImageOperationTargetOutcome {
    pub machine_id: MachineId,
    pub status: OperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_transferred: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImageOperationRecord {
    pub id: String,
    pub kind: ImageOperationKind,
    pub status: OperationStatus,
    pub stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<ImageDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_machine: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<ImageOperationTargetOutcome>,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct BuildInputSummary {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<BuildSecretSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docker_build_args: Vec<String>,
}

impl BuildInputSummary {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.secrets.is_empty() && self.docker_build_args.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BuildSecretSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BuildOperationRecord {
    pub id: String,
    pub kind: BuildOperationKind,
    pub method: BuildMethod,
    pub location: BuildLocation,
    pub status: OperationStatus,
    pub stage: String,
    #[serde(default, skip_serializing_if = "BuildInputSummary::is_empty")]
    pub inputs: BuildInputSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ImageArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
pub struct NetworkName(pub String);

impl AsRef<str> for NetworkName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

validated_string_id!(pub struct NetworkId("network id"););

impl NetworkId {
    #[must_use]
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        let mut value = String::with_capacity(32);
        for b in &bytes {
            let _ = write!(&mut value, "{b:02x}");
        }
        Self::new(value)
    }
}

validated_string_id!(pub struct InstallationId("installation id"););

impl InstallationId {
    #[must_use]
    pub fn local() -> Self {
        Self::new("local")
    }
}

validated_string_id!(pub struct AuthorityId("authority id"););

impl AuthorityId {
    #[must_use]
    pub fn default_authority() -> Self {
        Self::new("auth-default")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
pub enum AuthorityTier {
    #[display("stable")]
    Stable,
    #[display("dev")]
    Dev,
    #[display("lab")]
    Lab,
    #[display("edge")]
    Edge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthorityRecord {
    pub id: AuthorityId,
    pub tier: AuthorityTier,
    pub home_region: RegionName,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegionRole {
    #[display("home_data")]
    HomeData,
    #[display("compute")]
    Compute,
    #[display("disabled")]
    Disabled,
    #[display("draining")]
    Draining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegionRecord {
    pub id: RegionName,
    pub role: RegionRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthorityId>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
pub enum AuthorityParticipationRole {
    #[display("participant")]
    Participant,
    #[display("storage")]
    Storage,
    #[display("gateway")]
    Gateway,
    #[display("dns")]
    Dns,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthorityParticipationRecord {
    pub authority: AuthorityId,
    pub machine_id: MachineId,
    pub role: AuthorityParticipationRole,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StorageParticipation {
    Candidate,
    Authority { authority_id: AuthorityId },
}

impl StorageParticipation {
    #[must_use]
    pub fn default_authority() -> Self {
        Self::Authority {
            authority_id: AuthorityId::default_authority(),
        }
    }

    #[must_use]
    pub fn is_authority(&self) -> bool {
        matches!(self, Self::Authority { .. })
    }

    #[must_use]
    pub fn authority_id(&self) -> Option<&AuthorityId> {
        match self {
            Self::Candidate => None,
            Self::Authority { authority_id } => Some(authority_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineStorageRole {
    Compute,
    Candidate,
    Authority { authority_id: AuthorityId },
}

impl MachineStorageRole {
    #[must_use]
    pub fn default_authority() -> Self {
        Self::Authority {
            authority_id: AuthorityId::default_authority(),
        }
    }

    #[must_use]
    pub fn is_storage_capable(&self) -> bool {
        !matches!(self, Self::Compute)
    }

    #[must_use]
    pub fn authority_id(&self) -> Option<&AuthorityId> {
        match self {
            Self::Authority { authority_id } => Some(authority_id),
            Self::Compute | Self::Candidate => None,
        }
    }

    #[must_use]
    pub fn storage_participation(&self) -> StorageParticipation {
        match self {
            Self::Compute | Self::Candidate => StorageParticipation::Candidate,
            Self::Authority { authority_id } => StorageParticipation::Authority {
                authority_id: authority_id.clone(),
            },
        }
    }
}

impl From<StorageParticipation> for MachineStorageRole {
    fn from(value: StorageParticipation) -> Self {
        match value {
            StorageParticipation::Candidate => Self::Candidate,
            StorageParticipation::Authority { authority_id } => Self::Authority { authority_id },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageReplicaPolicy {
    #[display("single")]
    Single,
    #[display("r3")]
    R3,
    #[display("r5")]
    R5,
}

impl StorageReplicaPolicy {
    #[must_use]
    pub fn replicas(self) -> usize {
        match self {
            Self::Single => 1,
            Self::R3 => 3,
            Self::R5 => 5,
        }
    }

    pub fn try_from_replicas(replicas: usize) -> std::result::Result<Self, String> {
        match replicas {
            1 => Ok(Self::Single),
            3 => Ok(Self::R3),
            5 => Ok(Self::R5),
            _ => Err(format!(
                "storage replicas must be 1, 3, or 5 (got {replicas})"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneDataBucket {
    #[display("stored_intent")]
    StoredIntent,
    #[display("projection")]
    Projection,
    #[display("live_facts")]
    LiveFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneLossImpact {
    #[display("stored_truth_lost")]
    StoredTruthLost,
    #[display("no_stored_truth_lost")]
    NoStoredTruthLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthorityNodeRole {
    AuthorityStorage { authority_id: AuthorityId },
    StorageCandidate,
    Compute,
}

impl fmt::Display for AuthorityNodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityStorage { authority_id } => {
                write!(f, "authority_storage:{authority_id}")
            }
            Self::StorageCandidate => f.write_str("storage_candidate"),
            Self::Compute => f.write_str("compute"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthorityNodePosture {
    pub role: AuthorityNodeRole,
    pub data_bucket: ControlPlaneDataBucket,
    pub loss_impact: ControlPlaneLossImpact,
}

impl AuthorityNodePosture {
    #[must_use]
    pub fn from_storage_participation(storage: bool, participation: &StorageParticipation) -> Self {
        match participation {
            StorageParticipation::Authority { authority_id } => Self {
                role: AuthorityNodeRole::AuthorityStorage {
                    authority_id: authority_id.clone(),
                },
                data_bucket: ControlPlaneDataBucket::StoredIntent,
                loss_impact: ControlPlaneLossImpact::StoredTruthLost,
            },
            StorageParticipation::Candidate if storage => Self {
                role: AuthorityNodeRole::StorageCandidate,
                data_bucket: ControlPlaneDataBucket::StoredIntent,
                loss_impact: ControlPlaneLossImpact::NoStoredTruthLost,
            },
            StorageParticipation::Candidate => Self {
                role: AuthorityNodeRole::Compute,
                data_bucket: ControlPlaneDataBucket::LiveFacts,
                loss_impact: ControlPlaneLossImpact::NoStoredTruthLost,
            },
        }
    }

    #[must_use]
    pub fn from_machine_membership(machine: &MachineMembership) -> Self {
        match &machine.storage_role {
            MachineStorageRole::Authority { authority_id } => Self {
                role: AuthorityNodeRole::AuthorityStorage {
                    authority_id: authority_id.clone(),
                },
                data_bucket: ControlPlaneDataBucket::StoredIntent,
                loss_impact: ControlPlaneLossImpact::StoredTruthLost,
            },
            MachineStorageRole::Candidate => Self {
                role: AuthorityNodeRole::StorageCandidate,
                data_bucket: ControlPlaneDataBucket::StoredIntent,
                loss_impact: ControlPlaneLossImpact::NoStoredTruthLost,
            },
            MachineStorageRole::Compute => Self {
                role: AuthorityNodeRole::Compute,
                data_bucket: ControlPlaneDataBucket::LiveFacts,
                loss_impact: ControlPlaneLossImpact::NoStoredTruthLost,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct RegionName(pub String);

impl RegionName {
    #[must_use]
    pub fn local() -> Self {
        Self("local".into())
    }

    pub fn new(value: impl AsRef<str>) -> Result<Self, String> {
        normalize_topology_label(value.as_ref(), "region").map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RegionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(TopologyLabelVisitor { field: "region" })?;
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct AvailabilityZoneName(pub String);

impl AvailabilityZoneName {
    pub fn new(value: impl AsRef<str>) -> Result<Self, String> {
        normalize_topology_label(value.as_ref(), "availability_zone").map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for AvailabilityZoneName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(TopologyLabelVisitor {
            field: "availability_zone",
        })?;
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineTopology {
    pub region: RegionName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_zone: Option<AvailabilityZoneName>,
}

impl MachineTopology {
    #[must_use]
    pub fn local() -> Self {
        Self {
            region: RegionName::local(),
            availability_zone: None,
        }
    }

    pub fn new(
        region: impl AsRef<str>,
        availability_zone: Option<impl AsRef<str>>,
    ) -> Result<Self, String> {
        Ok(Self {
            region: RegionName::new(region)?,
            availability_zone: availability_zone
                .map(AvailabilityZoneName::new)
                .transpose()?,
        })
    }
}

struct TopologyLabelVisitor {
    field: &'static str,
}

impl Visitor<'_> for TopologyLabelVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a non-empty {} label", self.field)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        normalize_topology_label(value, self.field).map_err(E::custom)
    }
}

fn normalize_topology_label(value: &str, field: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{field} cannot be empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(format!("{field} must start with an ASCII letter or digit"));
    }
    if !chars.all(|char| char.is_ascii_alphanumeric() || matches!(char, '-' | '_' | '.')) {
        return Err(format!(
            "{field} may only contain ASCII letters, digits, '-', '_', and '.'"
        ));
    }
    Ok(normalized)
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PublicKey(pub [u8; 32]);

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(bytes) = self;
        let [b0, b1, b2, b3, ..] = bytes;
        write!(f, "PublicKey({b0:02x}{b1:02x}{b2:02x}{b3:02x}..)")
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateKey(pub [u8; 32]);

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(_) = self;
        f.write_str("PrivateKey(***)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
#[display("{_0}")]
pub struct OverlayIp(pub Ipv6Addr);

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    Default,
    JsonSchema,
)]
pub enum MachineLifecycle {
    #[default]
    #[display("standby")]
    #[strum(serialize = "standby")]
    Standby,
    #[display("active")]
    #[strum(serialize = "active")]
    Active,
    #[display("draining")]
    #[strum(serialize = "draining")]
    Draining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineLifecycleTransition {
    pub goal: MachineLifecycleGoal,
    pub evidence: MachineTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum MachineLifecycleGoal {
    Activate {
        #[schemars(with = "String")]
        assigned_subnet: Ipv4Net,
    },
    Drain,
    Standby {
        clearance: StandbyTransitionClearance,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StandbyTransitionClearance {
    DrainingComplete,
    OperatorForced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineTransitionEvidence {
    OperatorCommand { command: String },
    BootstrapActivation { operation_id: Option<String> },
    MeshStop { network: NetworkName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MachineTransitionError {
    code: &'static str,
    message: String,
}

impl MachineTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_TRANSITION",
            message: message.into(),
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, Default,
)]
pub enum NetworkLifecycle {
    #[default]
    #[display("stopped")]
    #[strum(serialize = "stopped")]
    Stopped,
    #[display("running")]
    #[strum(serialize = "running")]
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkLifecycleTransition {
    pub goal: NetworkLifecycleGoal,
    pub evidence: NetworkTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NetworkLifecycleGoal {
    Start,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkTransitionEvidence {
    OperatorCommand { command: String },
    BootstrapJoin { network: NetworkName },
    StartupResumeFailure { network: NetworkName },
    MeshTeardown { network: NetworkName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct NetworkTransitionError {
    code: &'static str,
    message: String,
}

impl NetworkTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl NetworkLifecycle {
    pub fn apply_transition(
        &mut self,
        transition: NetworkLifecycleTransition,
    ) -> Result<NetworkTransitionOutcome, NetworkTransitionError> {
        let NetworkLifecycleTransition {
            goal,
            evidence: _,
            at_unix_secs: _,
        } = transition;
        match (*self, goal) {
            (NetworkLifecycle::Stopped, NetworkLifecycleGoal::Start) => {
                *self = NetworkLifecycle::Running;
                Ok(NetworkTransitionOutcome::Applied)
            }
            (NetworkLifecycle::Running, NetworkLifecycleGoal::Stop) => {
                *self = NetworkLifecycle::Stopped;
                Ok(NetworkTransitionOutcome::Applied)
            }
            (NetworkLifecycle::Running, NetworkLifecycleGoal::Start)
            | (NetworkLifecycle::Stopped, NetworkLifecycleGoal::Stop) => {
                Ok(NetworkTransitionOutcome::AlreadyInState)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineMembership {
    pub id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub topology: MachineTopology,
    pub region_role: RegionRole,
    #[schemars(with = "Option<String>")]
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
    pub storage_role: MachineStorageRole,
    pub created_at: u64,
    pub updated_at: u64,
    pub labels: BTreeMap<String, String>,
}

impl MachineMembership {
    /// Create a minimal seed record for bootstrap/peer-discovery purposes.
    ///
    /// Control-plane fields (`lifecycle`, timestamps, `labels`)
    /// are zeroed — the real values arrive once the store is online.
    #[must_use]
    pub fn seed(
        id: MachineId,
        public_key: PublicKey,
        overlay_ip: OverlayIp,
        subnet: Option<Ipv4Net>,
        endpoints: Vec<String>,
    ) -> Self {
        Self {
            id,
            public_key,
            overlay_ip,
            topology: MachineTopology::local(),
            region_role: RegionRole::Compute,
            subnet,
            bridge_ip: None,
            endpoints,
            lifecycle: MachineLifecycle::Standby,
            storage_role: MachineStorageRole::Candidate,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    /// All CIDRs this peer should route, used by both host and docker WireGuard adapters.
    #[must_use]
    pub fn allowed_cidrs(&self) -> Vec<String> {
        let mut cidrs = vec![format!("{}/128", self.overlay_ip.0)];
        if let Some(subnet) = &self.subnet {
            cidrs.push(subnet.to_string());
        }
        if let Some(bridge_ip) = &self.bridge_ip {
            cidrs.push(format!("{}/128", bridge_ip.0));
        }
        cidrs
    }

    #[must_use]
    pub fn storage(&self) -> bool {
        self.storage_role.is_storage_capable()
    }

    #[must_use]
    pub fn storage_participation(&self) -> StorageParticipation {
        self.storage_role.storage_participation()
    }

    #[must_use]
    pub fn identity(&self) -> MachineIdentity {
        MachineIdentity {
            id: self.id.clone(),
            public_key: self.public_key.clone(),
            overlay_ip: self.overlay_ip,
        }
    }

    #[must_use]
    pub fn placement_candidate(&self) -> PlacementCandidate {
        PlacementCandidate {
            id: self.id.clone(),
            lifecycle: self.lifecycle,
            region_role: self.region_role,
            labels: self.labels.clone(),
        }
    }

    #[must_use]
    pub fn wireguard_peer_spec(&self) -> WireGuardPeerSpec {
        WireGuardPeerSpec {
            identity: self.identity(),
            subnet: self.subnet,
            bridge_ip: self.bridge_ip,
            endpoints: self.endpoints.clone(),
        }
    }

    #[must_use]
    pub fn observation(&self) -> MachineObservation {
        MachineObservation {
            identity: self.identity(),
            subnet: self.subnet,
            bridge_ip: self.bridge_ip,
            endpoints: self.endpoints.clone(),
        }
    }

    pub fn apply_lifecycle_transition(
        &mut self,
        transition: MachineLifecycleTransition,
    ) -> Result<MachineTransitionOutcome, MachineTransitionError> {
        let MachineLifecycleTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;

        match goal {
            MachineLifecycleGoal::Activate { assigned_subnet } => {
                if self.lifecycle == MachineLifecycle::Active
                    && self.subnet == Some(assigned_subnet)
                {
                    return Ok(MachineTransitionOutcome::AlreadyInState);
                }
                if self.lifecycle == MachineLifecycle::Draining {
                    return Err(MachineTransitionError::invalid(
                        "cannot activate a draining machine without first entering standby",
                    ));
                }
                self.lifecycle = MachineLifecycle::Active;
                self.subnet = Some(assigned_subnet);
            }
            MachineLifecycleGoal::Drain => {
                if self.lifecycle == MachineLifecycle::Draining {
                    return Ok(MachineTransitionOutcome::AlreadyInState);
                }
                if self.lifecycle == MachineLifecycle::Standby {
                    return Err(MachineTransitionError::invalid(
                        "cannot drain a standby machine",
                    ));
                }
                self.lifecycle = MachineLifecycle::Draining;
            }
            MachineLifecycleGoal::Standby { clearance } => {
                if self.lifecycle == MachineLifecycle::Standby && self.subnet.is_none() {
                    return Ok(MachineTransitionOutcome::AlreadyInState);
                }
                match (self.lifecycle, clearance) {
                    (MachineLifecycle::Draining, StandbyTransitionClearance::DrainingComplete)
                    | (MachineLifecycle::Standby, StandbyTransitionClearance::OperatorForced)
                    | (MachineLifecycle::Active, StandbyTransitionClearance::OperatorForced)
                    | (MachineLifecycle::Draining, StandbyTransitionClearance::OperatorForced) => {
                        self.lifecycle = MachineLifecycle::Standby;
                        self.subnet = None;
                    }
                    (
                        MachineLifecycle::Standby | MachineLifecycle::Active,
                        StandbyTransitionClearance::DrainingComplete,
                    ) => {
                        return Err(MachineTransitionError::invalid(
                            "machine must be draining before standby",
                        ));
                    }
                }
            }
        }

        self.updated_at = at_unix_secs;
        Ok(MachineTransitionOutcome::Applied)
    }
}

/// Immutable identity assigned at join — the (id, key, overlay_ip) triple every
/// other view type carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineIdentity {
    pub id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
}

/// Everything a WireGuard adapter needs to render a peer. No lifecycle,
/// timestamps, or labels — those don't influence WG config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeerSpec {
    pub identity: MachineIdentity,
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
}

impl WireGuardPeerSpec {
    #[must_use]
    pub fn id(&self) -> &MachineId {
        let Self { identity, .. } = self;
        &identity.id
    }

    #[must_use]
    pub fn public_key(&self) -> &PublicKey {
        let Self { identity, .. } = self;
        &identity.public_key
    }

    #[must_use]
    pub fn overlay_ip(&self) -> OverlayIp {
        let Self { identity, .. } = self;
        identity.overlay_ip
    }

    /// All CIDRs this peer should route, used by both host and docker WireGuard adapters.
    #[must_use]
    pub fn allowed_cidrs(&self) -> Vec<String> {
        let Self {
            identity,
            subnet,
            bridge_ip,
            endpoints: _,
        } = self;
        let mut cidrs = vec![format!("{}/128", identity.overlay_ip.0)];
        if let Some(subnet) = subnet {
            cidrs.push(subnet.to_string());
        }
        if let Some(bridge_ip) = bridge_ip {
            cidrs.push(format!("{}/128", bridge_ip.0));
        }
        cidrs
    }
}

impl From<&MachineMembership> for WireGuardPeerSpec {
    fn from(record: &MachineMembership) -> Self {
        record.wireguard_peer_spec()
    }
}

/// Placement and coordination policy input — all `machine_policy.rs` reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementCandidate {
    pub id: MachineId,
    pub lifecycle: MachineLifecycle,
    pub region_role: RegionRole,
    pub labels: BTreeMap<String, String>,
}

impl From<&MachineMembership> for PlacementCandidate {
    fn from(record: &MachineMembership) -> Self {
        record.placement_candidate()
    }
}

/// Transient observation pushed into peer-state from `endpoint_maintainer` and
/// `peer_sync`. Mirrors the fields that `PeerState` actually keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineObservation {
    pub identity: MachineIdentity,
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
}

impl MachineObservation {
    /// Build a transient observation for bootstrap/peer-discovery purposes.
    #[must_use]
    pub fn seed(
        id: MachineId,
        public_key: PublicKey,
        overlay_ip: OverlayIp,
        subnet: Option<Ipv4Net>,
        endpoints: Vec<String>,
    ) -> Self {
        Self {
            identity: MachineIdentity {
                id,
                public_key,
                overlay_ip,
            },
            subnet,
            bridge_ip: None,
            endpoints,
        }
    }

    #[must_use]
    pub fn id(&self) -> &MachineId {
        let Self { identity, .. } = self;
        &identity.id
    }
}

impl From<&MachineMembership> for MachineObservation {
    fn from(record: &MachineMembership) -> Self {
        record.observation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub invite_id: String,
    pub network_id: NetworkId,
    pub issuer_machine_id: MachineId,
    pub issuer_verify_key: String,
    pub expires_at: u64,
    pub status: InviteStatus,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InviteStatus {
    Active,
    Consumed {
        consumed_by: MachineId,
        consumed_at: u64,
    },
    Revoked {
        revoked_at: u64,
    },
}

impl InviteStatus {
    #[must_use]
    pub fn consumed_by(&self) -> Option<&MachineId> {
        match self {
            Self::Consumed { consumed_by, .. } => Some(consumed_by),
            Self::Active | Self::Revoked { .. } => None,
        }
    }

    #[must_use]
    pub fn is_consumed_by(&self, machine_id: &MachineId) -> bool {
        self.consumed_by() == Some(machine_id)
    }

    #[must_use]
    pub fn is_consumed(&self) -> bool {
        matches!(self, Self::Consumed { .. })
    }

    #[must_use]
    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked { .. })
    }
}

#[derive(Debug, Clone)]
pub enum MachineEvent {
    Upsert(MachineMembership),
    Removed { id: MachineId },
}

#[derive(Debug, Clone)]
pub enum CertificateEvent {
    Upsert(CertificateRecord),
    Removed { hostname: String },
}

#[derive(Debug, Clone)]
pub enum AcmeChallengeEvent {
    Upsert(AcmeChallengeRecord),
    Removed { hostname: String, token: String },
}

validated_string_id!(pub struct SidecarId("sidecar id"););

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarRecord {
    pub id: SidecarId,
    pub machine_id: MachineId,
    pub overlay_ip: Ipv4Addr,
    pub public_key: PublicKey,
    pub sidecar_container: String,
}

validated_string_id!(pub struct InstanceId("instance id"););

validated_string_id!(pub struct DeployId("deploy id"););

validated_string_id!(pub struct SlotId("slot id"););

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRevisionRecord {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub spec_json: String,
    pub created_by: MachineId,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceReleaseRecord {
    pub namespace: Namespace,
    pub service: String,
    pub release: ServiceRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRelease {
    pub primary_revision_hash: String,
    pub referenced_revision_hashes: Vec<String>,
    pub routing: ServiceRoutingPolicy,
    pub slots: Vec<ServiceReleaseSlot>,
    pub updated_by_deploy_id: DeployId,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceRoutingPolicy {
    Direct {
        revision_hash: String,
    },
    Split {
        allocations: Vec<ServiceTrafficAllocation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceTrafficAllocation {
    pub revision_hash: String,
    pub percent: u8,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceReleaseSlot {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub active_instance_id: InstanceId,
    pub revision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceBranchLineageRecord {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub source_namespace: Namespace,
    pub source_service: String,
    pub source_revision_hash: String,
    pub deploy_id: DeployId,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeMovementRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub from_machine: MachineId,
    pub to_machine: MachineId,
    pub final_machine: MachineId,
    pub deploy_id: DeployId,
    pub commit_deploy_id: DeployId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<DeployPhaseId>,
    pub snapshot_name: String,
    pub snapshot_guid: u64,
    pub bytes_transferred: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeBranchLineageRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub source_namespace: Namespace,
    pub source_volume_name: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub data_policy: VolumeCloneDataPolicy,
    pub consistency: VolumeCloneConsistency,
    pub deploy_id: DeployId,
    pub commit_deploy_id: DeployId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<DeployPhaseId>,
    pub snapshot_name: String,
    pub snapshot_guid: u64,
    pub target_dataset: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeployPhaseCommitRecord {
    pub namespace: Namespace,
    pub deploy_id: DeployId,
    pub phase_id: DeployPhaseId,
    pub commit_deploy_id: DeployId,
    pub committed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingState {
    pub machines: Vec<MachineMembership>,
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RouteAudience {
    PublicGlobal,
    Authority(AuthorityId),
    Group(String),
    Gateway(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RouteGrantState {
    Active { route_id: String, expires_at: u64 },
    Revoked { revoked_at: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RouteGrantRecord {
    pub grant_id: String,
    pub owner_authority: AuthorityId,
    pub audience: RouteAudience,
    pub namespace: Namespace,
    pub state: RouteGrantState,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RouteExportLifecycleEvent {
    GrantCreated(RouteGrantRecord),
    GrantRevoked {
        grant_id: String,
        owner_authority: AuthorityId,
        revoked_at: u64,
    },
    GrantExpired {
        grant_id: String,
        owner_authority: AuthorityId,
        expired_at: u64,
    },
    RouteWithdrawn {
        owner_authority: AuthorityId,
        audience: RouteAudience,
        namespace: Namespace,
        route_id: String,
        reason: String,
        sequence: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingEvent {
    MachineUpsert(MachineMembership),
    MachineRemoved {
        id: MachineId,
    },
    RevisionUpsert(ServiceRevisionRecord),
    RevisionRemoved {
        namespace: Namespace,
        service: String,
        revision_hash: String,
    },
    ReleaseUpsert(ServiceReleaseRecord),
    ReleaseRemoved {
        namespace: Namespace,
        service: String,
    },
    InstanceUpsert(InstanceStatusRecord),
    InstanceRemoved {
        instance_id: InstanceId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeAccountRecord {
    pub account_id: String,
    pub issuer_url: String,
    pub contact_email: Option<String>,
    // SECURITY: serialized `instant_acme::AccountCredentials` containing the
    // account private key. Safe only while replication stays inside the
    // WireGuard mesh and local store files are not backed up unencrypted;
    // revisit if either assumption changes.
    pub account_credentials_json: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum CertificateState {
    #[display("pending")]
    #[strum(serialize = "pending")]
    Pending,
    #[display("issuing")]
    #[strum(serialize = "issuing")]
    Issuing,
    #[display("active")]
    #[strum(serialize = "active")]
    Active,
    #[display("renewal_due")]
    #[strum(serialize = "renewal_due")]
    RenewalDue,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CertificateLifecycle {
    Pending {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Issuing {
        order_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_version_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Active {
        active_version_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_renewal_at: Option<u64>,
    },
    RenewalDue {
        active_version_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_renewal_at: Option<u64>,
    },
    Failed {
        last_error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_version_id: Option<String>,
    },
}

impl CertificateLifecycle {
    #[must_use]
    pub fn phase(&self) -> CertificateState {
        match self {
            Self::Pending { .. } => CertificateState::Pending,
            Self::Issuing { .. } => CertificateState::Issuing,
            Self::Active { .. } => CertificateState::Active,
            Self::RenewalDue { .. } => CertificateState::RenewalDue,
            Self::Failed { .. } => CertificateState::Failed,
        }
    }

    #[must_use]
    pub fn active_version_id(&self) -> Option<&str> {
        match self {
            Self::Pending { .. } => None,
            Self::Issuing {
                active_version_id, ..
            }
            | Self::Failed {
                active_version_id, ..
            } => active_version_id.as_deref(),
            Self::Active {
                active_version_id, ..
            }
            | Self::RenewalDue {
                active_version_id, ..
            } => Some(active_version_id.as_str()),
        }
    }

    #[must_use]
    pub fn order_url(&self) -> Option<&str> {
        match self {
            Self::Issuing { order_url, .. } => Some(order_url.as_str()),
            Self::Pending { .. }
            | Self::Active { .. }
            | Self::RenewalDue { .. }
            | Self::Failed { .. } => None,
        }
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Pending { last_error } | Self::Issuing { last_error, .. } => {
                last_error.as_deref()
            }
            Self::Failed { last_error, .. } => Some(last_error.as_str()),
            Self::Active { .. } | Self::RenewalDue { .. } => None,
        }
    }

    #[must_use]
    pub fn next_renewal_at(&self) -> Option<u64> {
        match self {
            Self::Active {
                next_renewal_at, ..
            }
            | Self::RenewalDue {
                next_renewal_at, ..
            } => *next_renewal_at,
            Self::Pending { .. } | Self::Issuing { .. } | Self::Failed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateStateTransition {
    pub goal: CertificateStateGoal,
    pub evidence: CertificateTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum CertificateStateGoal {
    StartIssuing {
        order_url: String,
    },
    MarkOrderFailed {
        error: String,
    },
    FinalizeActive {
        active_version_id: String,
        next_renewal_at: Option<u64>,
    },
    KeepIssuingAfterRetryableFailure {
        error: String,
    },
    MarkFinalizeFailed {
        error: String,
        previous_active_version_id: Option<String>,
    },
    MarkRenewalDue,
    ResetStalledIssuing {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CertificateTransitionEvidence {
    AcmeOrderStart { hostname: String },
    AcmeFinalize { hostname: String },
    RenewalScheduler { hostname: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CertificateTransitionError {
    code: &'static str,
    message: String,
}

impl CertificateTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_TRANSITION",
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateVersion {
    pub version_id: String,
    pub fullchain_pem: String,
    // SECURITY: leaf private key in PEM form, replicated as plaintext JSON
    // through the certificate store. Safe only under the WireGuard-only
    // replication + no-unencrypted-backup assumption documented on the
    // schema; revisit if either assumption changes.
    pub private_key_pem: String,
    pub not_before: Option<u64>,
    pub not_after: Option<u64>,
    pub issued_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub hostname: String,
    pub issuer_url: String,
    pub account_id: String,
    pub lifecycle: CertificateLifecycle,
    pub versions: Vec<CertificateVersion>,
    pub requested_at: u64,
    pub updated_at: u64,
}

impl CertificateRecord {
    #[must_use]
    pub fn state(&self) -> CertificateState {
        self.lifecycle.phase()
    }

    #[must_use]
    pub fn active_version_id(&self) -> Option<&str> {
        self.lifecycle.active_version_id()
    }

    #[must_use]
    pub fn order_url(&self) -> Option<&str> {
        self.lifecycle.order_url()
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.lifecycle.last_error()
    }

    #[must_use]
    pub fn next_renewal_at(&self) -> Option<u64> {
        self.lifecycle.next_renewal_at()
    }

    pub fn apply_state_transition(
        &mut self,
        transition: CertificateStateTransition,
    ) -> Result<CertificateTransitionOutcome, CertificateTransitionError> {
        let CertificateStateTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;
        match goal {
            CertificateStateGoal::StartIssuing { order_url } => {
                if self.state() == CertificateState::Issuing
                    && self.order_url() == Some(order_url.as_str())
                {
                    return Ok(CertificateTransitionOutcome::AlreadyInState);
                }
                match self.lifecycle.clone() {
                    CertificateLifecycle::Pending { .. }
                    | CertificateLifecycle::Failed { .. }
                    | CertificateLifecycle::RenewalDue { .. } => {
                        let active_version_id =
                            self.lifecycle.active_version_id().map(ToString::to_string);
                        self.lifecycle = CertificateLifecycle::Issuing {
                            order_url,
                            active_version_id,
                            last_error: None,
                        };
                    }
                    CertificateLifecycle::Issuing { .. } | CertificateLifecycle::Active { .. } => {
                        return Err(CertificateTransitionError::invalid(format!(
                            "certificate '{}' cannot start issuing from state {}",
                            self.hostname,
                            self.state()
                        )));
                    }
                }
            }
            CertificateStateGoal::MarkOrderFailed { error } => {
                self.lifecycle = CertificateLifecycle::Failed {
                    last_error: error,
                    active_version_id: self.active_version_id().map(ToString::to_string),
                };
            }
            CertificateStateGoal::FinalizeActive {
                active_version_id,
                next_renewal_at,
            } => {
                if self.state() != CertificateState::Issuing {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before finalize; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                }
                self.lifecycle = CertificateLifecycle::Active {
                    active_version_id,
                    next_renewal_at,
                };
            }
            CertificateStateGoal::KeepIssuingAfterRetryableFailure { error } => {
                let CertificateLifecycle::Issuing {
                    order_url,
                    active_version_id,
                    ..
                } = self.lifecycle.clone()
                else {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before retryable finalize failure; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                };
                self.lifecycle = CertificateLifecycle::Issuing {
                    order_url,
                    active_version_id,
                    last_error: Some(error),
                };
            }
            CertificateStateGoal::MarkFinalizeFailed {
                error,
                previous_active_version_id,
            } => {
                if self.state() != CertificateState::Issuing {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before finalize failure; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                }
                self.lifecycle = CertificateLifecycle::Failed {
                    last_error: error,
                    active_version_id: previous_active_version_id,
                };
            }
            CertificateStateGoal::MarkRenewalDue => {
                if self.state() == CertificateState::RenewalDue {
                    return Ok(CertificateTransitionOutcome::AlreadyInState);
                }
                let CertificateLifecycle::Active {
                    active_version_id,
                    next_renewal_at,
                } = self.lifecycle.clone()
                else {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be active before renewal due; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                };
                self.lifecycle = CertificateLifecycle::RenewalDue {
                    active_version_id,
                    next_renewal_at,
                };
            }
            CertificateStateGoal::ResetStalledIssuing { error } => {
                if self.state() != CertificateState::Issuing {
                    return Err(CertificateTransitionError::invalid(format!(
                        "certificate '{}' must be issuing before stalled reset; current state is {}",
                        self.hostname,
                        self.state()
                    )));
                }
                self.lifecycle = CertificateLifecycle::Pending {
                    last_error: Some(error),
                };
            }
        }
        self.updated_at = at_unix_secs;
        Ok(CertificateTransitionOutcome::Applied)
    }

    /// The currently-installable version, if any. Independent of `state`:
    /// renewal transitions a healthy cert through `RenewalDue → Issuing` and
    /// `active_version_id` keeps pointing at the existing leaf the whole way;
    /// a non-retryable finalize failure explicitly restores the previous
    /// `active_version_id` so callers can keep serving the old cert. TLS
    /// consumers should ask the type for material here, not gate on `state`.
    #[must_use]
    pub fn installed_version(&self) -> Option<&CertificateVersion> {
        let id = self.active_version_id()?;
        self.versions
            .iter()
            .find(|version| version.version_id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeRecord {
    pub hostname: String,
    pub token: String,
    // SECURITY: HTTP-01 key authorization is the secret an ACME verifier must
    // echo back. Replicated as plaintext JSON. Safe only under the WireGuard-
    // only replication + no-unencrypted-backup assumption documented on the
    // schema; revisit if either assumption changes.
    pub key_authorization: String,
    pub expires_at: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeReadinessRecord {
    pub hostname: String,
    pub token: String,
    pub machine_id: MachineId,
    pub observed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainDnsAdvice {
    pub hostname: String,
    pub resolved_ips: Vec<IpAddr>,
    pub recommended_ips: Vec<IpAddr>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
pub enum InstancePhase {
    #[display("pending")]
    #[strum(serialize = "pending")]
    Pending,
    #[display("starting")]
    #[strum(serialize = "starting")]
    Starting,
    #[display("ready")]
    #[strum(serialize = "ready")]
    Ready,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
    #[display("draining")]
    #[strum(serialize = "draining")]
    Draining,
    #[display("removed")]
    #[strum(serialize = "removed")]
    Removed,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
pub enum DrainState {
    #[display("none")]
    #[strum(serialize = "none")]
    None,
    #[display("requested")]
    #[strum(serialize = "requested")]
    Requested,
    #[display("complete")]
    #[strum(serialize = "complete")]
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceStatusRecord {
    pub instance_id: InstanceId,
    pub namespace: Namespace,
    pub service: String,
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub revision_hash: String,
    pub deploy_id: DeployId,
    pub docker_container_id: String,
    pub overlay_ip: Option<Ipv4Addr>,
    pub backend_ports: BTreeMap<String, u16>,
    pub phase: InstancePhase,
    pub ready: bool,
    pub drain_state: DrainState,
    pub error: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceStatusTransition {
    pub goal: InstanceStatusGoal,
    pub evidence: InstanceStatusEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum InstanceStatusGoal {
    MarkDraining,
    MarkFailed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstanceStatusEvidence {
    DeployCleanup { deploy_id: DeployId },
    RuntimeStart { deploy_id: DeployId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceStatusTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InstanceStatusTransitionError {
    code: &'static str,
    message: String,
}

impl InstanceStatusTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl InstanceStatusRecord {
    pub fn apply_status_transition(
        &mut self,
        transition: InstanceStatusTransition,
    ) -> Result<InstanceStatusTransitionOutcome, InstanceStatusTransitionError> {
        let InstanceStatusTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;
        match goal {
            InstanceStatusGoal::MarkDraining => {
                if self.phase == InstancePhase::Draining
                    && !self.ready
                    && self.drain_state == DrainState::Requested
                {
                    return Ok(InstanceStatusTransitionOutcome::AlreadyInState);
                }
                self.phase = InstancePhase::Draining;
                self.ready = false;
                self.drain_state = DrainState::Requested;
                self.error = None;
            }
            InstanceStatusGoal::MarkFailed { error } => {
                if self.phase == InstancePhase::Failed
                    && !self.ready
                    && self.error.as_deref() == Some(error.as_str())
                {
                    return Ok(InstanceStatusTransitionOutcome::AlreadyInState);
                }
                self.phase = InstancePhase::Failed;
                self.ready = false;
                self.drain_state = DrainState::None;
                self.error = Some(error);
            }
        }
        self.updated_at = at_unix_secs;
        Ok(InstanceStatusTransitionOutcome::Applied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum DeployState {
    #[display("planning")]
    #[strum(serialize = "planning")]
    Planning,
    #[display("applying")]
    #[strum(serialize = "applying")]
    Applying,
    #[display("committed")]
    #[strum(serialize = "committed")]
    Committed,
    #[display("checkpoint_committed")]
    #[strum(serialize = "checkpoint_committed")]
    CheckpointCommitted,
    #[display("cleanup_pending")]
    #[strum(serialize = "cleanup_pending")]
    CleanupPending,
    #[display("failed_after_checkpoint")]
    #[strum(serialize = "failed_after_checkpoint")]
    FailedAfterCheckpoint,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeployStateTransition {
    pub goal: DeployStateGoal,
    pub evidence: DeployTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum DeployStateGoal {
    Commit { summary_json: String },
    MarkCleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployTransitionEvidence {
    DeployExecutor { coordinator_machine_id: MachineId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DeployTransitionError {
    code: &'static str,
    message: String,
}

impl DeployTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_TRANSITION",
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    pub deploy_id: DeployId,
    pub namespace: Namespace,
    pub coordinator_machine_id: MachineId,
    pub manifest_hash: String,
    pub state: DeployState,
    pub started_at: u64,
    pub committed_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub summary_json: String,
}

impl DeployRecord {
    pub fn apply_state_transition(
        &mut self,
        transition: DeployStateTransition,
    ) -> Result<DeployTransitionOutcome, DeployTransitionError> {
        let DeployStateTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;
        match goal {
            DeployStateGoal::Commit { summary_json } => {
                if self.state == DeployState::Committed {
                    return Ok(DeployTransitionOutcome::AlreadyInState);
                }
                if self.state != DeployState::Applying {
                    return Err(DeployTransitionError::invalid(format!(
                        "deploy '{}' must be applying before commit; current state is {}",
                        self.deploy_id, self.state
                    )));
                }
                self.state = DeployState::Committed;
                self.committed_at = Some(at_unix_secs);
                self.finished_at = Some(at_unix_secs);
                self.summary_json = summary_json;
            }
            DeployStateGoal::MarkCleanupPending => {
                if self.state == DeployState::CleanupPending {
                    return Ok(DeployTransitionOutcome::AlreadyInState);
                }
                if self.state != DeployState::Committed {
                    return Err(DeployTransitionError::invalid(format!(
                        "deploy '{}' must be committed before cleanup pending; current state is {}",
                        self.deploy_id, self.state
                    )));
                }
                self.state = DeployState::CleanupPending;
                self.finished_at = Some(at_unix_secs);
            }
        }
        Ok(DeployTransitionOutcome::Applied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub scope: VolumeScope,
    pub machine_id: MachineId,
    pub quota: String,
    pub mode: String,
    pub owner: String,
    pub attached_services: Vec<String>,
    pub created_at: u64,
    pub created_by_deploy_id: DeployId,
    pub last_modified_at: u64,
    pub last_modified_by_deploy_id: DeployId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployChangeKind {
    Create,
    Replace,
    Remove,
    Unchanged,
}

validated_string_id!(pub struct DeployPhaseId("deploy phase id"););

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseCommitPolicy {
    EndOfDeploy,
    Checkpoint,
    NoStoreCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseRollbackPolicy {
    Reversible,
    ForwardOnly,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseAdvancePolicy {
    Immediate,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployPhaseWork {
    Service {
        service: String,
        action: DeployChangeKind,
    },
    Volume {
        volume: String,
        action: DeployChangeKind,
    },
    VolumeMove {
        volume: String,
        from_machine: MachineId,
        to_machine: MachineId,
        attached_services: Vec<String>,
    },
    VolumeClone {
        volume: String,
        source_namespace: Namespace,
        source_volume: String,
        source_machine: MachineId,
        target_machine: MachineId,
        data_policy: VolumeCloneDataPolicy,
        consistency: VolumeCloneConsistency,
        attached_services: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPhasePlan {
    pub phase_id: DeployPhaseId,
    pub name: String,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<DeployPhaseId>,
    pub participants: Vec<MachineId>,
    pub work: Vec<DeployPhaseWork>,
    pub commit_policy: DeployPhaseCommitPolicy,
    pub rollback_policy: DeployPhaseRollbackPolicy,
    pub advance_policy: DeployPhaseAdvancePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseState {
    Pending,
    Running,
    Succeeded {
        completed_at: u64,
    },
    Failed {
        completed_at: u64,
        failure: DeployPhaseFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPhaseFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPhaseRecord {
    pub namespace: Namespace,
    pub deploy_id: DeployId,
    pub phase_id: DeployPhaseId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_deploy_id: Option<DeployId>,
    pub name: String,
    pub order: u32,
    pub after: Vec<DeployPhaseId>,
    pub participants: Vec<MachineId>,
    pub work: Vec<DeployPhaseWork>,
    pub state: DeployPhaseState,
    pub commit_policy: DeployPhaseCommitPolicy,
    pub rollback_policy: DeployPhaseRollbackPolicy,
    pub advance_policy: DeployPhaseAdvancePolicy,
    pub started_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPlan {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub current_instance_id: Option<InstanceId>,
    pub next_instance_id: Option<InstanceId>,
    pub current_revision_hash: Option<String>,
    pub next_revision_hash: Option<String>,
    pub action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePlan {
    pub service: String,
    pub current_revision_hash: Option<String>,
    pub next_revision_hash: Option<String>,
    pub slots: Vec<SlotPlan>,
    pub action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceBranchSourcePlan {
    pub service: String,
    pub source_namespace: Namespace,
    pub source_service: String,
    pub source_revision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceSourceMode {
    Fresh,
    Branch {
        source_namespace: Namespace,
        source_service: String,
        source_revision_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSourcePlan {
    pub service: String,
    pub mode: ServiceSourceMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployBaselineComponent {
    Manifest,
    Participants,
    Phases,
    Services,
    ServiceSources,
    Volumes,
    VolumeMoves,
    VolumeClones,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPreviewBaselineComponents {
    pub manifest: String,
    pub participants: String,
    pub phases: String,
    pub services: String,
    pub service_sources: String,
    pub volumes: String,
    pub volume_moves: String,
    pub volume_clones: String,
}

impl DeployPreviewBaselineComponents {
    #[must_use]
    pub fn ordered(&self) -> [(DeployBaselineComponent, &str); 8] {
        [
            (DeployBaselineComponent::Manifest, &self.manifest),
            (DeployBaselineComponent::Participants, &self.participants),
            (DeployBaselineComponent::Phases, &self.phases),
            (DeployBaselineComponent::Services, &self.services),
            (
                DeployBaselineComponent::ServiceSources,
                &self.service_sources,
            ),
            (DeployBaselineComponent::Volumes, &self.volumes),
            (DeployBaselineComponent::VolumeMoves, &self.volume_moves),
            (DeployBaselineComponent::VolumeClones, &self.volume_clones),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeployPreviewBaseline {
    pub fingerprint: String,
    pub components: DeployPreviewBaselineComponents,
}

impl<'de> Deserialize<'de> for DeployPreviewBaseline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawDeployPreviewBaseline {
            fingerprint: String,
            components: DeployPreviewBaselineComponents,
        }

        let raw = RawDeployPreviewBaseline::deserialize(deserializer)?;
        let baseline = Self::new(raw.components);
        if baseline.fingerprint != raw.fingerprint {
            return Err(de::Error::custom(format!(
                "deploy preview baseline fingerprint '{}' does not match canonical fingerprint '{}'",
                raw.fingerprint, baseline.fingerprint
            )));
        }
        Ok(baseline)
    }
}

impl DeployPreviewBaseline {
    #[must_use]
    pub fn new(components: DeployPreviewBaselineComponents) -> Self {
        let mut input = String::new();
        for (_, component_fingerprint) in components.ordered() {
            append_fingerprint_segment(&mut input, component_fingerprint);
        }
        Self {
            fingerprint: stable_hash_hex(input.as_bytes()),
            components,
        }
    }

    #[must_use]
    pub fn changed_components(&self, actual: &Self) -> Vec<DeployBaselineComponent> {
        let mut changed = Vec::new();
        for ((component, expected), (_, actual)) in self
            .components
            .ordered()
            .into_iter()
            .zip(actual.components.ordered())
        {
            if expected != actual {
                changed.push(component);
            }
        }
        changed
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fingerprint.is_empty()
    }

    #[must_use]
    pub fn is_canonical(&self) -> bool {
        self.fingerprint == Self::new(self.components.clone()).fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployBaselineDiff {
    pub expected: DeployPreviewBaseline,
    pub actual: DeployPreviewBaseline,
}

impl DeployBaselineDiff {
    #[must_use]
    pub fn new(expected: DeployPreviewBaseline, actual: DeployPreviewBaseline) -> Self {
        Self { expected, actual }
    }

    #[must_use]
    pub fn changed_components(&self) -> Vec<DeployBaselineComponent> {
        self.expected.changed_components(&self.actual)
    }
}

impl std::fmt::Display for DeployBaselineDiff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "deploy baseline changed: expected fingerprint '{}', got '{}'",
            self.expected.fingerprint, self.actual.fingerprint
        )
    }
}

#[must_use]
pub fn service_source_fingerprint(sources: &[ServiceSourcePlan]) -> String {
    if sources.is_empty() {
        return String::new();
    }

    let mut sources = sources.to_vec();
    sources.sort_by(|left, right| left.service.cmp(&right.service));

    let mut input = String::new();
    for source in sources {
        append_fingerprint_segment(&mut input, &source.service);
        match source.mode {
            ServiceSourceMode::Fresh => {
                input.push_str("fresh:");
            }
            ServiceSourceMode::Branch {
                source_namespace,
                source_service,
                source_revision_hash,
            } => {
                input.push_str("branch:");
                append_fingerprint_segment(&mut input, source_namespace.as_ref());
                append_fingerprint_segment(&mut input, &source_service);
                append_fingerprint_segment(&mut input, &source_revision_hash);
            }
        }
    }

    stable_hash_hex(input.as_bytes())
}

fn append_fingerprint_segment(input: &mut String, value: &str) {
    input.push_str(&value.len().to_string());
    input.push(':');
    input.push_str(value);
    input.push(';');
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMovePlan {
    pub volume: String,
    pub from_machine: MachineId,
    pub to_machine: MachineId,
    pub attached_services: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeClonePlan {
    pub volume: String,
    pub source_namespace: Namespace,
    pub source_volume: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub data_policy: VolumeCloneDataPolicy,
    pub consistency: VolumeCloneConsistency,
    pub attached_services: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeClonePreflightAction {
    DrainAndRemoveBeforeCloneReplacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeClonePreflightScope {
    UncommittedNamespaceInstances,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeClonePreflightPlan {
    pub phase_id: DeployPhaseId,
    pub volumes: Vec<String>,
    pub action: VolumeClonePreflightAction,
    pub scope: VolumeClonePreflightScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployImageAvailabilityStatus {
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployImageAvailabilityPlan {
    pub service: String,
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub image: String,
    pub digest: ImageDigest,
    pub status: DeployImageAvailabilityStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPreview {
    pub namespace: Namespace,
    pub manifest_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<DeployPreviewBaseline>,
    pub participants: Vec<MachineId>,
    pub phases: Vec<DeployPhasePlan>,
    pub services: Vec<ServicePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_sources: Vec<ServiceSourcePlan>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service_source_fingerprint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_branch_sources: Vec<ServiceBranchSourcePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_moves: Vec<VolumeMovePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_clones: Vec<VolumeClonePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_clone_preflights: Vec<VolumeClonePreflightPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_availability: Vec<DeployImageAvailabilityPlan>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedDeployState {
    Prepared,
    Applied,
    Expired,
    Superseded,
}

impl std::fmt::Display for PreparedDeployState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Prepared => "prepared",
            Self::Applied => "applied",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedDeployRecord {
    pub prepared_deploy_id: DeployId,
    pub namespace: Namespace,
    pub manifest_hash: String,
    pub manifest_json: String,
    pub preview: DeployPreview,
    pub baseline: DeployPreviewBaseline,
    pub coordinator_machine_id: MachineId,
    pub state: PreparedDeployState,
    pub created_at: u64,
    pub expires_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchEnvironmentState {
    Prepared,
    Applying,
    Active,
    Failed,
}

impl std::fmt::Display for BranchEnvironmentState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Prepared => "prepared",
            Self::Applying => "applying",
            Self::Active => "active",
            Self::Failed => "failed",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchEnvironmentResourceMode {
    Fresh,
    Branch,
}

impl std::fmt::Display for BranchEnvironmentResourceMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Fresh => "fresh",
            Self::Branch => "branch",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEnvironmentResourceOverride {
    pub name: String,
    pub mode: BranchEnvironmentResourceMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEnvironmentFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<DeployId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEnvironmentRecord {
    pub source_namespace: Namespace,
    pub target_namespace: Namespace,
    pub state: BranchEnvironmentState,
    pub default_service_mode: BranchEnvironmentResourceMode,
    pub default_volume_mode: BranchEnvironmentResourceMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<BranchEnvironmentResourceOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<BranchEnvironmentResourceOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_deploy_id: Option<DeployId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_deploy_id: Option<DeployId>,
    pub manifest_hash: String,
    pub baseline: DeployPreviewBaseline,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_branch_sources: Vec<ServiceBranchSourcePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_clones: Vec<VolumeClonePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_availability: Vec<DeployImageAvailabilityPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<BranchEnvironmentFailure>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchEnvironmentRecordValidationError {
    PreparedMissingPreparedDeploy,
    PreparedHasAppliedDeploy,
    PreparedHasFailure,
    ApplyingMissingPreparedDeploy,
    ApplyingHasAppliedDeploy,
    ApplyingHasFailure,
    ActiveMissingPreparedDeploy,
    ActiveMissingAppliedDeploy,
    ActiveHasFailure,
    FailedMissingPreparedDeploy,
    FailedMissingFailure,
}

impl std::fmt::Display for BranchEnvironmentRecordValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::PreparedMissingPreparedDeploy => {
                "prepared branch environment requires a prepared deploy id"
            }
            Self::PreparedHasAppliedDeploy => {
                "prepared branch environment cannot have an applied deploy id"
            }
            Self::PreparedHasFailure => "prepared branch environment cannot have a failure",
            Self::ApplyingMissingPreparedDeploy => {
                "applying branch environment requires a prepared deploy id"
            }
            Self::ApplyingHasAppliedDeploy => {
                "applying branch environment cannot have an applied deploy id"
            }
            Self::ApplyingHasFailure => "applying branch environment cannot have a failure",
            Self::ActiveMissingPreparedDeploy => {
                "active branch environment requires a prepared deploy id"
            }
            Self::ActiveMissingAppliedDeploy => {
                "active branch environment requires an applied deploy id"
            }
            Self::ActiveHasFailure => "active branch environment cannot have a failure",
            Self::FailedMissingPreparedDeploy => {
                "failed branch environment requires a prepared deploy id"
            }
            Self::FailedMissingFailure => "failed branch environment requires a failure",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for BranchEnvironmentRecordValidationError {}

impl BranchEnvironmentRecord {
    pub fn validate(&self) -> std::result::Result<(), BranchEnvironmentRecordValidationError> {
        match self.state {
            BranchEnvironmentState::Prepared => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::PreparedMissingPreparedDeploy,
                    );
                }
                if self.applied_deploy_id.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::PreparedHasAppliedDeploy);
                }
                if self.failure.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::PreparedHasFailure);
                }
            }
            BranchEnvironmentState::Applying => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::ApplyingMissingPreparedDeploy,
                    );
                }
                if self.applied_deploy_id.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::ApplyingHasAppliedDeploy);
                }
                if self.failure.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::ApplyingHasFailure);
                }
            }
            BranchEnvironmentState::Active => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::ActiveMissingPreparedDeploy,
                    );
                }
                if self.applied_deploy_id.is_none() {
                    return Err(BranchEnvironmentRecordValidationError::ActiveMissingAppliedDeploy);
                }
                if self.failure.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::ActiveHasFailure);
                }
            }
            BranchEnvironmentState::Failed => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::FailedMissingPreparedDeploy,
                    );
                }
                if self.failure.is_none() {
                    return Err(BranchEnvironmentRecordValidationError::FailedMissingFailure);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployEvent {
    pub step: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployApplyResult {
    pub deploy_id: DeployId,
    pub preview: DeployPreview,
    pub state: DeployState,
    pub events: Vec<DeployEvent>,
}

/// Derive a deterministic overlay IP from a public key (fd00::/8 ULA + first 15 key bytes).
#[must_use]
pub fn management_ip_from_key(key: &PublicKey) -> OverlayIp {
    let mut octets = [0u8; 16];
    octets[0] = 0xfd;
    octets[1..16].copy_from_slice(&key.0[..15]);
    OverlayIp(Ipv6Addr::from(octets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn validated_string_ids_reject_empty_and_control_characters() {
        assert!(MachineId::try_new("machine-a").is_ok());
        assert!(MachineId::try_new("").is_err());
        assert!(DeployId::try_new("deploy\n1").is_err());
    }

    #[test]
    fn non_zero_replica_count_rejects_zero() {
        let count = NonZeroReplicaCount::try_new(3).expect("valid replica count");

        assert_eq!(count.get(), 3);
        assert!(NonZeroReplicaCount::try_new(0).is_err());
        assert!(serde_json::from_str::<NonZeroReplicaCount>("0").is_err());
        assert_eq!(serde_json::to_string(&count).expect("json"), "3");
    }

    #[test]
    fn positive_scalar_rejects_zero_negative_and_non_finite_values() {
        let scalar = PositiveScalar::try_new(1.25).expect("valid positive scalar");

        assert_eq!(scalar.get(), 1.25);
        assert!(PositiveScalar::try_new(0.0).is_err());
        assert!(PositiveScalar::try_new(-1.0).is_err());
        assert!(PositiveScalar::try_new(f64::INFINITY).is_err());
    }

    #[test]
    fn redacted_secret_string_never_displays_raw_value() {
        let secret = RedactedSecretString::try_new("super-secret").expect("valid secret");

        assert_eq!(secret.expose_secret(), "super-secret");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(
            serde_json::to_string(&secret).expect("json"),
            r#""<redacted>""#
        );
        assert!(RedactedSecretString::try_new("").is_err());
    }

    #[test]
    fn image_digest_requires_algorithm_and_hex_hash() {
        assert!(ImageDigest::try_new("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_ok());
        assert!(ImageDigest::try_new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        assert!(ImageDigest::try_new("sha256:not-hex").is_err());
    }

    #[test]
    fn image_digest_json_deserialization_rejects_invalid_values() {
        let result = serde_json::from_str::<ImageDigest>(r#""sha256:not-hex""#);

        assert!(result.is_err());
    }

    #[test]
    fn image_availability_record_json_deserialization_rejects_invalid_digest() {
        let json = r#"
            {
                "machine_id": "machine-a",
                "digest": "sha256:not-hex",
                "presence": {
                    "state": "absent",
                    "observed_at": 12
                },
                "updated_at": 12
            }
        "#;
        let result = serde_json::from_str::<ImageAvailabilityRecord>(json);

        assert!(result.is_err());
    }

    #[test]
    fn branch_environment_record_serializes_lifecycle_identity_and_modes() {
        let baseline = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: "sources".into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        });
        let record = BranchEnvironmentRecord {
            source_namespace: Namespace("prod".into()),
            target_namespace: Namespace("pr-39".into()),
            state: BranchEnvironmentState::Prepared,
            default_service_mode: BranchEnvironmentResourceMode::Branch,
            default_volume_mode: BranchEnvironmentResourceMode::Fresh,
            services: vec![BranchEnvironmentResourceOverride {
                name: "worker".into(),
                mode: BranchEnvironmentResourceMode::Fresh,
            }],
            volumes: Vec::new(),
            prepared_deploy_id: Some(DeployId("prepare-1".into())),
            applied_deploy_id: None,
            manifest_hash: "manifest-hash".into(),
            baseline,
            service_branch_sources: Vec::new(),
            volume_clones: Vec::new(),
            image_availability: Vec::new(),
            failure: None,
            created_at: 1,
            updated_at: 2,
        };

        let json = serde_json::to_value(&record).expect("serialize branch environment");

        assert_eq!(json["source_namespace"], serde_json::json!("prod"));
        assert_eq!(json["target_namespace"], serde_json::json!("pr-39"));
        assert_eq!(json["state"], serde_json::json!("prepared"));
        assert_eq!(json["default_service_mode"], serde_json::json!("branch"));
        assert_eq!(json["default_volume_mode"], serde_json::json!("fresh"));
        assert_eq!(
            json["services"],
            serde_json::json!([{ "name": "worker", "mode": "fresh" }])
        );
        assert_eq!(json["prepared_deploy_id"], serde_json::json!("prepare-1"));
        assert!(json.get("applied_deploy_id").is_none());
        assert!(json.get("failure").is_none());
    }

    #[test]
    fn deploy_preview_serializes_volume_clone_preflight_contract() {
        let preview = DeployPreview {
            namespace: Namespace::new("pr-39"),
            manifest_hash: "manifest".into(),
            baseline: None,
            participants: vec![MachineId::new("machine-a")],
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: vec![VolumeClonePreflightPlan {
                phase_id: DeployPhaseId::new("data"),
                volumes: vec!["data".into(), "cache".into()],
                action: VolumeClonePreflightAction::DrainAndRemoveBeforeCloneReplacement,
                scope: VolumeClonePreflightScope::UncommittedNamespaceInstances,
            }],
            image_availability: Vec::new(),
            warnings: Vec::new(),
        };

        let json = serde_json::to_value(&preview).expect("serialize deploy preview");

        assert_eq!(
            json["volume_clone_preflights"][0]["phase_id"],
            serde_json::json!("data")
        );
        assert_eq!(
            json["volume_clone_preflights"][0]["volumes"],
            serde_json::json!(["data", "cache"])
        );
        assert_eq!(
            json["volume_clone_preflights"][0]["action"],
            serde_json::json!("drain_and_remove_before_clone_replacement")
        );
        assert_eq!(
            json["volume_clone_preflights"][0]["scope"],
            serde_json::json!("uncommitted_namespace_instances")
        );
        let roundtrip: DeployPreview =
            serde_json::from_value(json).expect("deserialize deploy preview");
        assert_eq!(
            roundtrip.volume_clone_preflights,
            preview.volume_clone_preflights
        );
    }

    #[test]
    fn deploy_preview_defaults_missing_volume_clone_preflights() {
        let json = r#"
            {
                "namespace": "pr-39",
                "manifest_hash": "manifest",
                "participants": [],
                "phases": [],
                "services": [],
                "warnings": []
            }
        "#;

        let preview: DeployPreview =
            serde_json::from_str(json).expect("deserialize legacy deploy preview");

        assert!(preview.volume_clone_preflights.is_empty());
        assert!(preview.baseline.is_none());
        assert!(preview.service_sources.is_empty());
        assert!(preview.service_source_fingerprint.is_empty());
        assert!(preview.image_availability.is_empty());
    }

    #[test]
    fn deploy_preview_serializes_image_availability_contract() {
        let digest =
            ImageDigest::try_new("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("valid digest");
        let preview = DeployPreview {
            namespace: Namespace::new("default"),
            manifest_hash: "manifest".into(),
            baseline: None,
            participants: vec![MachineId::new("machine-a")],
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            image_availability: vec![DeployImageAvailabilityPlan {
                service: "web".into(),
                slot_id: SlotId::new("web-0"),
                machine_id: MachineId::new("machine-a"),
                image: digest.as_str().into(),
                digest: digest.clone(),
                status: DeployImageAvailabilityStatus::Present,
            }],
            warnings: Vec::new(),
        };

        let json = serde_json::to_value(&preview).expect("serialize deploy preview");

        assert_eq!(
            json["image_availability"][0]["status"],
            serde_json::json!("present")
        );
        assert_eq!(
            json["image_availability"][0]["digest"],
            serde_json::json!(digest.as_str())
        );
        let roundtrip: DeployPreview =
            serde_json::from_value(json).expect("deserialize deploy preview");
        assert_eq!(roundtrip.image_availability, preview.image_availability);
    }

    #[test]
    fn deploy_preview_baseline_serializes_contract_and_changed_components() {
        let baseline = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: "sources".into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        });
        let changed = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            service_sources: "other-sources".into(),
            ..baseline.components.clone()
        });
        let preview = DeployPreview {
            namespace: Namespace::new("pr-39"),
            manifest_hash: "manifest".into(),
            baseline: Some(baseline.clone()),
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            image_availability: Vec::new(),
            warnings: Vec::new(),
        };

        let json = serde_json::to_value(&preview).expect("serialize deploy preview");

        assert_eq!(
            json["baseline"]["fingerprint"],
            serde_json::json!(baseline.fingerprint)
        );
        assert_eq!(
            json["baseline"]["components"]["service_sources"],
            serde_json::json!("sources")
        );
        assert_eq!(
            baseline.changed_components(&changed),
            vec![DeployBaselineComponent::ServiceSources]
        );
        assert!(baseline.changed_components(&baseline).is_empty());
        assert!(baseline.is_canonical());
        let roundtrip: DeployPreview =
            serde_json::from_value(json).expect("deserialize deploy preview");
        assert_eq!(roundtrip.baseline, Some(baseline));
    }

    #[test]
    fn deploy_preview_baseline_rejects_noncanonical_fingerprint() {
        let baseline = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: "sources".into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        });
        let mut json = serde_json::to_value(&baseline).expect("serialize baseline");
        json["fingerprint"] = serde_json::json!("bogus");

        let error = serde_json::from_value::<DeployPreviewBaseline>(json)
            .expect_err("mismatched fingerprint should fail");
        assert!(
            error.to_string().contains("canonical fingerprint"),
            "got: {error}"
        );
    }

    #[test]
    fn deploy_preview_baseline_changed_components_cover_every_component() {
        let baseline = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: "sources".into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        });

        for component in [
            DeployBaselineComponent::Manifest,
            DeployBaselineComponent::Participants,
            DeployBaselineComponent::Phases,
            DeployBaselineComponent::Services,
            DeployBaselineComponent::ServiceSources,
            DeployBaselineComponent::Volumes,
            DeployBaselineComponent::VolumeMoves,
            DeployBaselineComponent::VolumeClones,
        ] {
            let mut components = baseline.components.clone();
            match component {
                DeployBaselineComponent::Manifest => components.manifest = "changed".into(),
                DeployBaselineComponent::Participants => components.participants = "changed".into(),
                DeployBaselineComponent::Phases => components.phases = "changed".into(),
                DeployBaselineComponent::Services => components.services = "changed".into(),
                DeployBaselineComponent::ServiceSources => {
                    components.service_sources = "changed".into()
                }
                DeployBaselineComponent::Volumes => components.volumes = "changed".into(),
                DeployBaselineComponent::VolumeMoves => components.volume_moves = "changed".into(),
                DeployBaselineComponent::VolumeClones => {
                    components.volume_clones = "changed".into()
                }
            }
            let changed = DeployPreviewBaseline::new(components);

            assert_ne!(baseline.fingerprint, changed.fingerprint);
            assert_eq!(baseline.changed_components(&changed), vec![component]);
        }
    }

    #[test]
    fn prepared_deploy_record_serializes_contract() {
        let baseline = DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: "sources".into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        });
        let preview = DeployPreview {
            namespace: Namespace::new("prod"),
            manifest_hash: "manifest".into(),
            baseline: Some(baseline.clone()),
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            image_availability: Vec::new(),
            warnings: Vec::new(),
        };
        let record = PreparedDeployRecord {
            prepared_deploy_id: DeployId::new("prepare-1"),
            namespace: Namespace::new("prod"),
            manifest_hash: "manifest".into(),
            manifest_json: r#"{"namespace":"prod","services":[]}"#.into(),
            preview,
            baseline,
            coordinator_machine_id: MachineId::new("machine-a"),
            state: PreparedDeployState::Prepared,
            created_at: 10,
            expires_at: 20,
            updated_at: 10,
        };

        let json = serde_json::to_value(&record).expect("serialize prepared deploy");

        assert_eq!(json["prepared_deploy_id"], serde_json::json!("prepare-1"));
        assert_eq!(json["state"], serde_json::json!("prepared"));
        assert_eq!(json["expires_at"], serde_json::json!(20));
        let roundtrip: PreparedDeployRecord =
            serde_json::from_value(json).expect("deserialize prepared deploy");
        assert_eq!(roundtrip, record);
    }

    #[test]
    fn deploy_preview_serializes_service_source_contract() {
        let service_sources = vec![
            ServiceSourcePlan {
                service: "api".into(),
                mode: ServiceSourceMode::Fresh,
            },
            ServiceSourcePlan {
                service: "web".into(),
                mode: ServiceSourceMode::Branch {
                    source_namespace: Namespace::new("prod"),
                    source_service: "web".into(),
                    source_revision_hash: "source-rev".into(),
                },
            },
        ];
        let fingerprint = service_source_fingerprint(&service_sources);
        let preview = DeployPreview {
            namespace: Namespace::new("pr-39"),
            manifest_hash: "manifest".into(),
            baseline: None,
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources,
            service_source_fingerprint: fingerprint.clone(),
            service_branch_sources: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            image_availability: Vec::new(),
            warnings: Vec::new(),
        };

        let json = serde_json::to_value(&preview).expect("serialize deploy preview");

        assert_eq!(
            json["service_sources"][0]["mode"]["kind"],
            serde_json::json!("fresh")
        );
        assert_eq!(
            json["service_sources"][1]["mode"]["kind"],
            serde_json::json!("branch")
        );
        assert_eq!(
            json["service_sources"][1]["mode"]["source_revision_hash"],
            serde_json::json!("source-rev")
        );
        assert_eq!(
            json["service_source_fingerprint"],
            serde_json::json!(fingerprint)
        );
        let roundtrip: DeployPreview =
            serde_json::from_value(json).expect("deserialize deploy preview");
        assert_eq!(roundtrip.service_sources, preview.service_sources);
        assert_eq!(roundtrip.service_source_fingerprint, fingerprint);
    }

    #[test]
    fn service_source_fingerprint_is_order_independent_and_source_sensitive() {
        let fresh = ServiceSourcePlan {
            service: "api".into(),
            mode: ServiceSourceMode::Fresh,
        };
        let branch = ServiceSourcePlan {
            service: "web".into(),
            mode: ServiceSourceMode::Branch {
                source_namespace: Namespace::new("prod"),
                source_service: "web".into(),
                source_revision_hash: "rev-1".into(),
            },
        };

        assert!(service_source_fingerprint(&[]).is_empty());

        let fingerprint = service_source_fingerprint(&[fresh.clone(), branch.clone()]);

        assert_eq!(
            fingerprint,
            service_source_fingerprint(&[branch.clone(), fresh.clone()])
        );

        let branch_with_other_namespace = ServiceSourcePlan {
            service: "web".into(),
            mode: ServiceSourceMode::Branch {
                source_namespace: Namespace::new("staging"),
                source_service: "web".into(),
                source_revision_hash: "rev-1".into(),
            },
        };
        assert_ne!(
            fingerprint,
            service_source_fingerprint(&[fresh.clone(), branch_with_other_namespace])
        );

        let branch_with_other_service = ServiceSourcePlan {
            service: "web".into(),
            mode: ServiceSourceMode::Branch {
                source_namespace: Namespace::new("prod"),
                source_service: "api".into(),
                source_revision_hash: "rev-1".into(),
            },
        };
        assert_ne!(
            fingerprint,
            service_source_fingerprint(&[fresh.clone(), branch_with_other_service])
        );

        let branch_with_other_revision = ServiceSourcePlan {
            service: "web".into(),
            mode: ServiceSourceMode::Branch {
                source_namespace: Namespace::new("prod"),
                source_service: "web".into(),
                source_revision_hash: "rev-2".into(),
            },
        };
        assert_ne!(
            fingerprint,
            service_source_fingerprint(&[fresh.clone(), branch_with_other_revision])
        );

        let web_fresh = ServiceSourcePlan {
            service: "web".into(),
            mode: ServiceSourceMode::Fresh,
        };
        assert_ne!(fingerprint, service_source_fingerprint(&[fresh, web_fresh]));
    }

    #[test]
    fn image_artifact_preserves_digest_identity_through_json() {
        let artifact = ImageArtifact {
            image: ImageRef {
                repository: Some("registry.example/api".into()),
                tag: Some("latest".into()),
                digest: ImageDigest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            },
            platform: Some(ImagePlatform {
                os: "linux".into(),
                architecture: "amd64".into(),
                variant: None,
            }),
            provenance: ImageArtifactProvenance::Build {
                method: BuildMethod::Railpack,
                location: BuildLocation::Machine {
                    machine_id: MachineId::new("builder-a"),
                },
                source_digest: Some("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            },
            created_at: 42,
        };

        let json = serde_json::to_value(&artifact).expect("serialize artifact");
        let roundtrip: ImageArtifact = serde_json::from_value(json).expect("deserialize artifact");

        assert_eq!(
            roundtrip.digest().as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            roundtrip.provenance,
            ImageArtifactProvenance::Build {
                method: BuildMethod::Railpack,
                location: BuildLocation::Machine {
                    machine_id: MachineId::new("builder-a"),
                },
                source_digest: Some("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            }
        );
    }

    #[test]
    fn image_availability_record_carries_machine_scoped_presence() {
        let digest = ImageDigest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        let record = ImageAvailabilityRecord {
            machine_id: MachineId::new("machine-a"),
            digest: digest.clone(),
            presence: ImagePresence::Failed {
                reason: "transfer interrupted".into(),
                failed_at: 12,
                operation_id: Some("op-1".into()),
            },
            updated_at: 12,
        };

        assert_eq!(record.digest, digest);
        assert_eq!(record.machine_id, MachineId::new("machine-a"));
    }

    #[test]
    fn management_ip_deterministic() {
        let key = PublicKey([0xab; 32]);
        let ip1 = management_ip_from_key(&key);
        let ip2 = management_ip_from_key(&key);
        assert_eq!(ip1, ip2);
        assert!(ip1.0.segments()[0] >> 8 == 0xfd);
    }

    #[test]
    fn different_keys_different_ips() {
        let k1 = PublicKey([0x01; 32]);
        let k2 = PublicKey([0x02; 32]);
        assert_ne!(management_ip_from_key(&k1), management_ip_from_key(&k2));
    }

    #[test]
    fn machine_record_without_topology_is_rejected() {
        let json = r#"{
            "id":"node-1",
            "public_key":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],
            "overlay_ip":"fd00::1",
            "subnet":null,
            "bridge_ip":null,
            "endpoints":[],
            "lifecycle":"Standby",
            "created_at":0,
            "updated_at":0,
            "labels":{}
        }"#;

        let error =
            serde_json::from_str::<MachineMembership>(json).expect_err("record should fail");

        assert!(error.to_string().contains("missing field `topology`"));
    }

    #[test]
    fn machine_lifecycle_display_is_explicit() {
        assert_eq!(MachineLifecycle::Standby.to_string(), "standby");
    }

    #[test]
    fn machine_lifecycle_from_str_is_explicit() {
        assert_eq!(
            MachineLifecycle::from_str("active"),
            Ok(MachineLifecycle::Active)
        );
    }

    #[test]
    fn authority_posture_marks_default_authority_storage_as_stored_truth_owner() {
        let posture = AuthorityNodePosture::from_storage_participation(
            true,
            &StorageParticipation::default_authority(),
        );

        assert_eq!(
            posture.role,
            AuthorityNodeRole::AuthorityStorage {
                authority_id: AuthorityId::default_authority(),
            }
        );
        assert_eq!(posture.data_bucket, ControlPlaneDataBucket::StoredIntent);
        assert_eq!(posture.loss_impact, ControlPlaneLossImpact::StoredTruthLost);
    }

    #[test]
    fn authority_posture_keeps_candidate_disposable_for_control_plane_truth() {
        let posture = AuthorityNodePosture::from_storage_participation(
            true,
            &StorageParticipation::Candidate,
        );

        assert_eq!(posture.role, AuthorityNodeRole::StorageCandidate);
        assert_eq!(posture.data_bucket, ControlPlaneDataBucket::StoredIntent);
        assert_eq!(
            posture.loss_impact,
            ControlPlaneLossImpact::NoStoredTruthLost
        );
    }

    #[test]
    fn authority_posture_marks_non_storage_participant_as_compute_live_fact() {
        let posture = AuthorityNodePosture::from_storage_participation(
            false,
            &StorageParticipation::Candidate,
        );

        assert_eq!(posture.role, AuthorityNodeRole::Compute);
        assert_eq!(posture.data_bucket, ControlPlaneDataBucket::LiveFacts);
        assert_eq!(
            posture.loss_impact,
            ControlPlaneLossImpact::NoStoredTruthLost
        );
    }

    #[test]
    fn authority_posture_serializes_with_typed_role_and_impact() {
        let posture = AuthorityNodePosture::from_storage_participation(
            true,
            &StorageParticipation::default_authority(),
        );

        let json = serde_json::to_value(&posture).expect("serialize posture");

        assert_eq!(
            json,
            serde_json::json!({
                "role": {
                    "kind": "authority_storage",
                    "authority_id": "auth-default"
                },
                "data_bucket": "stored_intent",
                "loss_impact": "stored_truth_lost"
            })
        );
        let decoded: AuthorityNodePosture =
            serde_json::from_value(json).expect("deserialize posture");
        assert_eq!(decoded, posture);
    }

    #[test]
    fn machine_transition_activates_with_evidence_and_timestamp() {
        let mut record = sample_record();
        record.lifecycle = MachineLifecycle::Standby;
        record.subnet = None;
        let assigned_subnet = "10.42.1.0/24".parse().expect("valid subnet");

        let outcome = record
            .apply_lifecycle_transition(MachineLifecycleTransition {
                goal: MachineLifecycleGoal::Activate { assigned_subnet },
                evidence: MachineTransitionEvidence::OperatorCommand {
                    command: "machine transition activate".into(),
                },
                at_unix_secs: 42,
            })
            .expect("activation is valid");

        assert_eq!(outcome, MachineTransitionOutcome::Applied);
        assert_eq!(record.lifecycle, MachineLifecycle::Active);
        assert_eq!(record.subnet, Some(assigned_subnet));
        assert_eq!(record.updated_at, 42);
    }

    #[test]
    fn machine_transition_idempotent_activation_preserves_timestamp() {
        let mut record = sample_record();
        record.lifecycle = MachineLifecycle::Active;
        record.subnet = Some("10.42.1.0/24".parse().expect("valid subnet"));
        record.updated_at = 7;

        let outcome = record
            .apply_lifecycle_transition(MachineLifecycleTransition {
                goal: MachineLifecycleGoal::Activate {
                    assigned_subnet: "10.42.1.0/24".parse().expect("valid subnet"),
                },
                evidence: MachineTransitionEvidence::OperatorCommand {
                    command: "machine transition activate".into(),
                },
                at_unix_secs: 42,
            })
            .expect("idempotent activation is valid");

        assert_eq!(outcome, MachineTransitionOutcome::AlreadyInState);
        assert_eq!(record.lifecycle, MachineLifecycle::Active);
        assert_eq!(record.updated_at, 7);
    }

    #[test]
    fn machine_transition_draining_preserves_subnet_until_standby_clearance() {
        let mut record = sample_record();
        let assigned_subnet = "10.42.1.0/24".parse().expect("valid subnet");
        record.lifecycle = MachineLifecycle::Active;
        record.subnet = Some(assigned_subnet);

        let drain = record
            .apply_lifecycle_transition(MachineLifecycleTransition {
                goal: MachineLifecycleGoal::Drain,
                evidence: MachineTransitionEvidence::OperatorCommand {
                    command: "machine transition drain".into(),
                },
                at_unix_secs: 42,
            })
            .expect("drain is valid");

        assert_eq!(drain, MachineTransitionOutcome::Applied);
        assert_eq!(record.lifecycle, MachineLifecycle::Draining);
        assert_eq!(record.subnet, Some(assigned_subnet));

        let standby = record
            .apply_lifecycle_transition(MachineLifecycleTransition {
                goal: MachineLifecycleGoal::Standby {
                    clearance: StandbyTransitionClearance::DrainingComplete,
                },
                evidence: MachineTransitionEvidence::OperatorCommand {
                    command: "machine transition standby".into(),
                },
                at_unix_secs: 43,
            })
            .expect("standby after drain is valid");

        assert_eq!(standby, MachineTransitionOutcome::Applied);
        assert_eq!(record.lifecycle, MachineLifecycle::Standby);
        assert!(record.subnet.is_none());
    }

    #[test]
    fn machine_transition_rejects_drain_from_standby() {
        let mut record = sample_record();
        record.lifecycle = MachineLifecycle::Standby;

        let error = record
            .apply_lifecycle_transition(MachineLifecycleTransition {
                goal: MachineLifecycleGoal::Drain,
                evidence: MachineTransitionEvidence::OperatorCommand {
                    command: "machine transition drain".into(),
                },
                at_unix_secs: 42,
            })
            .expect_err("standby cannot drain");

        assert_eq!(error.code(), "INVALID_TRANSITION");
        assert_eq!(record.lifecycle, MachineLifecycle::Standby);
    }

    #[test]
    fn machine_transition_requires_clearance_for_standby() {
        let mut record = sample_record();
        record.lifecycle = MachineLifecycle::Active;
        record.subnet = Some("10.42.1.0/24".parse().expect("valid subnet"));

        let error = record
            .apply_lifecycle_transition(MachineLifecycleTransition {
                goal: MachineLifecycleGoal::Standby {
                    clearance: StandbyTransitionClearance::DrainingComplete,
                },
                evidence: MachineTransitionEvidence::OperatorCommand {
                    command: "machine transition standby".into(),
                },
                at_unix_secs: 42,
            })
            .expect_err("active requires force or drain first");

        assert_eq!(error.code(), "INVALID_TRANSITION");
        assert_eq!(record.lifecycle, MachineLifecycle::Active);
        assert!(record.subnet.is_some());
    }

    #[test]
    fn network_lifecycle_display_is_explicit() {
        assert_eq!(NetworkLifecycle::Stopped.to_string(), "stopped");
    }

    #[test]
    fn network_lifecycle_from_str_is_explicit() {
        assert_eq!(
            NetworkLifecycle::from_str("running"),
            Ok(NetworkLifecycle::Running)
        );
    }

    #[test]
    fn network_transition_starts_and_stops_explicitly() {
        let mut lifecycle = NetworkLifecycle::Stopped;

        let start = lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Start,
                evidence: NetworkTransitionEvidence::OperatorCommand {
                    command: "mesh start".into(),
                },
                at_unix_secs: 42,
            })
            .expect("start transition is valid");
        assert_eq!(start, NetworkTransitionOutcome::Applied);
        assert_eq!(lifecycle, NetworkLifecycle::Running);

        let stop = lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Stop,
                evidence: NetworkTransitionEvidence::MeshTeardown {
                    network: NetworkName("alpha".into()),
                },
                at_unix_secs: 43,
            })
            .expect("stop transition is valid");
        assert_eq!(stop, NetworkTransitionOutcome::Applied);
        assert_eq!(lifecycle, NetworkLifecycle::Stopped);
    }

    #[test]
    fn deploy_transition_commits_from_applying() {
        let mut record = deploy_record(DeployState::Applying);

        let outcome = record
            .apply_state_transition(DeployStateTransition {
                goal: DeployStateGoal::Commit {
                    summary_json: "{}".into(),
                },
                evidence: DeployTransitionEvidence::DeployExecutor {
                    coordinator_machine_id: MachineId::new("m1"),
                },
                at_unix_secs: 42,
            })
            .expect("commit is valid");

        assert_eq!(outcome, DeployTransitionOutcome::Applied);
        assert_eq!(record.state, DeployState::Committed);
        assert_eq!(record.committed_at, Some(42));
        assert_eq!(record.finished_at, Some(42));
        assert_eq!(record.summary_json, "{}");
    }

    #[test]
    fn deploy_transition_idempotent_commit_preserves_original_completion() {
        let mut record = deploy_record(DeployState::Committed);
        record.committed_at = Some(42);
        record.finished_at = Some(42);
        record.summary_json = r#"{"started":1}"#.into();

        let outcome = record
            .apply_state_transition(DeployStateTransition {
                goal: DeployStateGoal::Commit {
                    summary_json: r#"{"started":2}"#.into(),
                },
                evidence: DeployTransitionEvidence::DeployExecutor {
                    coordinator_machine_id: MachineId::new("m1"),
                },
                at_unix_secs: 99,
            })
            .expect("committed deploy commit is idempotent");

        assert_eq!(outcome, DeployTransitionOutcome::AlreadyInState);
        assert_eq!(record.state, DeployState::Committed);
        assert_eq!(record.committed_at, Some(42));
        assert_eq!(record.finished_at, Some(42));
        assert_eq!(record.summary_json, r#"{"started":1}"#);
    }

    #[test]
    fn deploy_transition_idempotent_cleanup_pending_preserves_original_timestamp() {
        let mut record = deploy_record(DeployState::CleanupPending);
        record.committed_at = Some(42);
        record.finished_at = Some(50);
        record.summary_json = "{}".into();

        let outcome = record
            .apply_state_transition(DeployStateTransition {
                goal: DeployStateGoal::MarkCleanupPending,
                evidence: DeployTransitionEvidence::DeployExecutor {
                    coordinator_machine_id: MachineId::new("m1"),
                },
                at_unix_secs: 99,
            })
            .expect("cleanup-pending deploy cleanup transition is idempotent");

        assert_eq!(outcome, DeployTransitionOutcome::AlreadyInState);
        assert_eq!(record.state, DeployState::CleanupPending);
        assert_eq!(record.committed_at, Some(42));
        assert_eq!(record.finished_at, Some(50));
        assert_eq!(record.summary_json, "{}");
    }

    #[test]
    fn deploy_transition_rejects_cleanup_pending_before_commit() {
        let mut record = deploy_record(DeployState::Applying);

        let error = record
            .apply_state_transition(DeployStateTransition {
                goal: DeployStateGoal::MarkCleanupPending,
                evidence: DeployTransitionEvidence::DeployExecutor {
                    coordinator_machine_id: MachineId::new("m1"),
                },
                at_unix_secs: 42,
            })
            .expect_err("cleanup pending requires committed");

        assert_eq!(error.code(), "INVALID_TRANSITION");
        assert_eq!(record.state, DeployState::Applying);
    }

    #[test]
    fn certificate_transition_starts_issuing_from_pending() {
        let mut record = cert_record(CertificateState::Pending);

        let outcome = record
            .apply_state_transition(CertificateStateTransition {
                goal: CertificateStateGoal::StartIssuing {
                    order_url: "https://issuer/order/1".into(),
                },
                evidence: CertificateTransitionEvidence::AcmeOrderStart {
                    hostname: "example.com".into(),
                },
                at_unix_secs: 42,
            })
            .expect("pending certificate can issue");

        assert_eq!(outcome, CertificateTransitionOutcome::Applied);
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://issuer/order/1"));
        assert_eq!(record.updated_at, 42);
        assert!(record.last_error().is_none());
    }

    #[test]
    fn certificate_transition_finalize_failure_preserves_previous_active_version() {
        let mut record = cert_record(CertificateState::Issuing);
        record.lifecycle = CertificateLifecycle::Issuing {
            order_url: "https://issuer/order/1".into(),
            active_version_id: Some("v1".into()),
            last_error: None,
        };

        record
            .apply_state_transition(CertificateStateTransition {
                goal: CertificateStateGoal::MarkFinalizeFailed {
                    error: "bad challenge".into(),
                    previous_active_version_id: Some("v1".into()),
                },
                evidence: CertificateTransitionEvidence::AcmeFinalize {
                    hostname: "example.com".into(),
                },
                at_unix_secs: 43,
            })
            .expect("issuing certificate can fail finalization");

        assert_eq!(record.state(), CertificateState::Failed);
        assert_eq!(record.active_version_id(), Some("v1"));
        assert!(record.order_url().is_none());
        assert_eq!(record.last_error(), Some("bad challenge"));
    }

    #[test]
    fn certificate_transition_retryable_failure_stays_issuing_until_success_clears_error() {
        let mut record = cert_record(CertificateState::Issuing);
        record.lifecycle = CertificateLifecycle::Issuing {
            order_url: "https://issuer/order/1".into(),
            active_version_id: Some("v1".into()),
            last_error: None,
        };

        let outcome = record
            .apply_state_transition(CertificateStateTransition {
                goal: CertificateStateGoal::KeepIssuingAfterRetryableFailure {
                    error: "acme rate limited".into(),
                },
                evidence: CertificateTransitionEvidence::AcmeFinalize {
                    hostname: "example.com".into(),
                },
                at_unix_secs: 43,
            })
            .expect("retryable finalize failure remains in issuing");

        assert_eq!(outcome, CertificateTransitionOutcome::Applied);
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.active_version_id(), Some("v1"));
        assert_eq!(record.order_url(), Some("https://issuer/order/1"));
        assert_eq!(record.last_error(), Some("acme rate limited"));
        assert_eq!(record.updated_at, 43);

        let outcome = record
            .apply_state_transition(CertificateStateTransition {
                goal: CertificateStateGoal::FinalizeActive {
                    active_version_id: "v2".into(),
                    next_renewal_at: Some(90),
                },
                evidence: CertificateTransitionEvidence::AcmeFinalize {
                    hostname: "example.com".into(),
                },
                at_unix_secs: 44,
            })
            .expect("successful finalize resolves the visible retryable error");

        assert_eq!(outcome, CertificateTransitionOutcome::Applied);
        assert_eq!(record.state(), CertificateState::Active);
        assert_eq!(record.active_version_id(), Some("v2"));
        assert!(record.order_url().is_none());
        assert!(record.last_error().is_none());
        assert_eq!(record.next_renewal_at(), Some(90));
        assert_eq!(record.updated_at, 44);
    }

    #[test]
    fn instance_transition_marks_draining_consistently() {
        let mut record = instance_record();

        let outcome = record
            .apply_status_transition(InstanceStatusTransition {
                goal: InstanceStatusGoal::MarkDraining,
                evidence: InstanceStatusEvidence::DeployCleanup {
                    deploy_id: DeployId::new("deploy-1"),
                },
                at_unix_secs: 42,
            })
            .expect("ready instance can drain");

        assert_eq!(outcome, InstanceStatusTransitionOutcome::Applied);
        assert_eq!(record.phase, InstancePhase::Draining);
        assert!(!record.ready);
        assert_eq!(record.drain_state, DrainState::Requested);
        assert_eq!(record.updated_at, 42);
        assert!(record.error.is_none());
    }

    #[test]
    fn instance_transition_idempotent_draining_preserves_original_timestamp() {
        let mut record = instance_record();
        record.phase = InstancePhase::Draining;
        record.ready = false;
        record.drain_state = DrainState::Requested;
        record.error = None;
        record.updated_at = 42;

        let outcome = record
            .apply_status_transition(InstanceStatusTransition {
                goal: InstanceStatusGoal::MarkDraining,
                evidence: InstanceStatusEvidence::DeployCleanup {
                    deploy_id: DeployId::new("deploy-1"),
                },
                at_unix_secs: 99,
            })
            .expect("repeated drain request is idempotent");

        assert_eq!(outcome, InstanceStatusTransitionOutcome::AlreadyInState);
        assert_eq!(record.phase, InstancePhase::Draining);
        assert!(!record.ready);
        assert_eq!(record.drain_state, DrainState::Requested);
        assert_eq!(record.updated_at, 42);
        assert!(record.error.is_none());
    }

    #[test]
    fn instance_transition_failure_visibility_changes_only_when_error_changes() {
        let mut record = instance_record();

        record
            .apply_status_transition(InstanceStatusTransition {
                goal: InstanceStatusGoal::MarkFailed {
                    error: "image pull failed".into(),
                },
                evidence: InstanceStatusEvidence::RuntimeStart {
                    deploy_id: DeployId::new("deploy-1"),
                },
                at_unix_secs: 42,
            })
            .expect("runtime failure should be recorded");

        assert_eq!(record.phase, InstancePhase::Failed);
        assert!(!record.ready);
        assert_eq!(record.error.as_deref(), Some("image pull failed"));
        assert_eq!(record.updated_at, 42);

        let outcome = record
            .apply_status_transition(InstanceStatusTransition {
                goal: InstanceStatusGoal::MarkFailed {
                    error: "image pull failed".into(),
                },
                evidence: InstanceStatusEvidence::RuntimeStart {
                    deploy_id: DeployId::new("deploy-1"),
                },
                at_unix_secs: 99,
            })
            .expect("same runtime failure is idempotent");

        assert_eq!(outcome, InstanceStatusTransitionOutcome::AlreadyInState);
        assert_eq!(record.error.as_deref(), Some("image pull failed"));
        assert_eq!(record.updated_at, 42);

        let outcome = record
            .apply_status_transition(InstanceStatusTransition {
                goal: InstanceStatusGoal::MarkFailed {
                    error: "health check failed".into(),
                },
                evidence: InstanceStatusEvidence::RuntimeStart {
                    deploy_id: DeployId::new("deploy-1"),
                },
                at_unix_secs: 100,
            })
            .expect("new runtime failure should update visible error");

        assert_eq!(outcome, InstanceStatusTransitionOutcome::Applied);
        assert_eq!(record.error.as_deref(), Some("health check failed"));
        assert_eq!(record.updated_at, 100);
    }

    #[test]
    fn authority_region_and_participation_records_serialize_explicitly() {
        let authority = AuthorityRecord {
            id: AuthorityId::default_authority(),
            tier: AuthorityTier::Stable,
            home_region: RegionName::local(),
            created_at: 1,
            updated_at: 2,
        };
        let region = RegionRecord {
            id: RegionName::local(),
            role: RegionRole::HomeData,
            authority: Some(AuthorityId::default_authority()),
            created_at: 1,
            updated_at: 2,
        };
        let participation = AuthorityParticipationRecord {
            authority: AuthorityId::default_authority(),
            machine_id: MachineId::new("node-1"),
            role: AuthorityParticipationRole::Participant,
            created_at: 1,
            updated_at: 2,
        };

        assert!(
            serde_json::to_string(&authority)
                .expect("authority json")
                .contains("Stable")
        );
        assert!(
            serde_json::to_string(&region)
                .expect("region json")
                .contains("home_data")
        );
        assert!(
            serde_json::to_string(&participation)
                .expect("participation json")
                .contains("Participant")
        );
    }

    // -------------------------------------------------------------------
    // CertificateRecord::installed_version — installable-material lookup
    //
    // These tests pin the contract that TLS consumers (gateway, doctor,
    // future status surfaces) must use when deciding whether to serve a
    // managed cert. The rule is:
    //
    //   "Installable" == there is a `CertificateVersion` whose `version_id`
    //   matches the record's `active_version_id`.
    //
    // It is deliberately independent of `state`. The renewal flow walks a
    // healthy cert through `Active → RenewalDue → Issuing` (and possibly
    // `→ Failed` on a non-retryable finalize) without clearing
    // `active_version_id`, so the existing leaf must remain serviceable
    // throughout. Gating on `state` would blackhole TLS handshakes during
    // every renewal window.
    // -------------------------------------------------------------------

    fn cert_version(id: &str) -> CertificateVersion {
        CertificateVersion {
            version_id: id.into(),
            fullchain_pem: format!(
                "-----BEGIN CERTIFICATE-----\n{id}\n-----END CERTIFICATE-----\n"
            ),
            private_key_pem: format!(
                "-----BEGIN PRIVATE KEY-----\n{id}\n-----END PRIVATE KEY-----\n"
            ),
            not_before: Some(0),
            not_after: Some(0),
            issued_at: 0,
        }
    }

    fn cert_record(state: CertificateState) -> CertificateRecord {
        CertificateRecord {
            hostname: "example.com".into(),
            issuer_url: "https://acme.example/directory".into(),
            account_id: "acct".into(),
            lifecycle: match state {
                CertificateState::Pending => CertificateLifecycle::Pending { last_error: None },
                CertificateState::Issuing => CertificateLifecycle::Issuing {
                    order_url: "https://issuer/order/1".into(),
                    active_version_id: None,
                    last_error: None,
                },
                CertificateState::Active => CertificateLifecycle::Active {
                    active_version_id: "v1".into(),
                    next_renewal_at: None,
                },
                CertificateState::RenewalDue => CertificateLifecycle::RenewalDue {
                    active_version_id: "v1".into(),
                    next_renewal_at: None,
                },
                CertificateState::Failed => CertificateLifecycle::Failed {
                    last_error: "failed".into(),
                    active_version_id: None,
                },
            },
            versions: Vec::new(),
            requested_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn installed_version_returns_none_without_active_version_id() {
        // Brand-new Pending record with no successful issuance: nothing to serve.
        let record = cert_record(CertificateState::Pending);
        assert!(record.installed_version().is_none());
    }

    #[test]
    fn installed_version_returns_none_when_active_id_points_at_missing_version() {
        // The pointer is dangling — `versions` was rolled back or never
        // populated. Treat as "no installable material" rather than panicking.
        let mut record = cert_record(CertificateState::Active);
        record.lifecycle = CertificateLifecycle::Active {
            active_version_id: "v-missing".into(),
            next_renewal_at: None,
        };
        assert!(record.installed_version().is_none());
    }

    #[test]
    fn installed_version_returns_match_for_active_record() {
        // Steady state: `state == Active`, single version, pointer matches.
        let mut record = cert_record(CertificateState::Active);
        record.versions.push(cert_version("v1"));
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_serves_during_renewal_due() {
        // The renewal job flips `Active → RenewalDue` without touching the
        // cert material. The old leaf must remain installable until a fresh
        // version is committed; otherwise the gateway drops TLS during every
        // renewal window.
        let mut record = cert_record(CertificateState::RenewalDue);
        record.versions.push(cert_version("v1"));
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_serves_during_issuing_renewal() {
        // `start_one` flips the record to `Issuing` for the renewal order while
        // `active_version_id` still points at the previous valid leaf. We
        // serve the old material until finalize replaces it.
        let mut record = cert_record(CertificateState::Issuing);
        record.versions.push(cert_version("v1"));
        record.lifecycle = CertificateLifecycle::Issuing {
            order_url: "https://issuer/order/1".into(),
            active_version_id: Some("v1".into()),
            last_error: None,
        };
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_serves_after_failed_renewal_when_previous_id_restored() {
        // `finalize_one` non-retryable error restores `previous_active_version_id`
        // before downgrading state to `Failed`, exactly so the gateway keeps
        // serving the previously-issued cert until the next reconcile attempt.
        // Old version is still in `versions`; new (failed) version is not added.
        let mut record = cert_record(CertificateState::Failed);
        record.versions.push(cert_version("v1"));
        record.lifecycle = CertificateLifecycle::Failed {
            last_error: "failed".into(),
            active_version_id: Some("v1".into()),
        };
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_picks_newest_when_multiple_versions_present() {
        // Successful renewal pushes a new `CertificateVersion`. The pointer
        // must determine which one is served — not insertion order.
        let mut record = cert_record(CertificateState::Active);
        record.versions.push(cert_version("v1"));
        record.versions.push(cert_version("v2"));
        record.lifecycle = CertificateLifecycle::Active {
            active_version_id: "v2".into(),
            next_renewal_at: None,
        };
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v2")
        );
    }

    fn sample_record() -> MachineMembership {
        let mut labels = BTreeMap::new();
        labels.insert("region".into(), "iad".into());
        MachineMembership {
            id: MachineId::new("m1"),
            public_key: PublicKey([0x11; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 7)),
            topology: MachineTopology::local(),
            region_role: RegionRole::HomeData,
            subnet: Some("10.42.7.0/24".parse().expect("valid subnet")),
            bridge_ip: Some(OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 8))),
            endpoints: vec!["1.2.3.4:51820".into(), "5.6.7.8:51820".into()],
            lifecycle: MachineLifecycle::Active,
            storage_role: StorageParticipation::default_authority().into(),
            created_at: 100,
            updated_at: 200,
            labels,
        }
    }

    fn deploy_record(state: DeployState) -> DeployRecord {
        DeployRecord {
            deploy_id: DeployId::new("deploy-1"),
            namespace: Namespace::new("default"),
            coordinator_machine_id: MachineId::new("m1"),
            manifest_hash: "hash".into(),
            state,
            started_at: 1,
            committed_at: None,
            finished_at: None,
            summary_json: "null".into(),
        }
    }

    fn instance_record() -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId::new("instance-1"),
            namespace: Namespace::new("default"),
            service: "api".into(),
            slot_id: SlotId::new("slot-1"),
            machine_id: MachineId::new("m1"),
            revision_hash: "rev1".into(),
            deploy_id: DeployId::new("deploy-1"),
            docker_container_id: "container-1".into(),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: Some("old transient error".into()),
            started_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn machine_record_identity_carries_id_key_overlay() {
        let record = sample_record();
        let identity = record.identity();
        assert_eq!(identity.id, record.id);
        assert_eq!(identity.public_key, record.public_key);
        assert_eq!(identity.overlay_ip, record.overlay_ip);
    }

    #[test]
    fn machine_record_placement_candidate_only_carries_policy_fields() {
        let record = sample_record();
        let candidate = record.placement_candidate();
        assert_eq!(candidate.id, record.id);
        assert_eq!(candidate.lifecycle, MachineLifecycle::Active);
        assert_eq!(candidate.region_role, RegionRole::HomeData);
        assert_eq!(
            candidate.labels.get("region").map(String::as_str),
            Some("iad")
        );
    }

    #[test]
    fn machine_record_serializes_region_role_with_operator_vocabulary() {
        let record = sample_record();
        let json = serde_json::to_value(&record).expect("serialize machine record");

        assert_eq!(json["region_role"], "home_data");
        let roundtrip: MachineMembership =
            serde_json::from_value(json).expect("deserialize machine record");
        assert_eq!(roundtrip.region_role, RegionRole::HomeData);
    }

    #[test]
    fn machine_record_rejects_missing_region_role() {
        let mut json = serde_json::to_value(sample_record()).expect("serialize machine record");
        json.as_object_mut()
            .expect("machine record object")
            .remove("region_role");

        serde_json::from_value::<MachineMembership>(json)
            .expect_err("missing region role should not default");
    }

    #[test]
    fn machine_record_wireguard_peer_spec_drops_control_plane_fields() {
        let record = sample_record();
        let spec = record.wireguard_peer_spec();
        assert_eq!(spec.id(), &record.id);
        assert_eq!(spec.public_key(), &record.public_key);
        assert_eq!(spec.overlay_ip(), record.overlay_ip);
        assert_eq!(spec.subnet, record.subnet);
        assert_eq!(spec.bridge_ip, record.bridge_ip);
        assert_eq!(spec.endpoints, record.endpoints);
    }

    #[test]
    fn wireguard_peer_spec_allowed_cidrs_matches_record_helper() {
        let record = sample_record();
        let spec = record.wireguard_peer_spec();
        assert_eq!(spec.allowed_cidrs(), record.allowed_cidrs());
    }

    #[test]
    fn machine_observation_carries_observable_fields() {
        let record = sample_record();
        let observation = record.observation();
        assert_eq!(observation.id(), &record.id);
        assert_eq!(observation.identity.public_key, record.public_key);
        assert_eq!(observation.subnet, record.subnet);
        assert_eq!(observation.bridge_ip, record.bridge_ip);
        assert_eq!(observation.endpoints, record.endpoints);
    }

    #[test]
    fn machine_observation_seed_omits_bridge_ip() {
        let observation = MachineObservation::seed(
            MachineId::new("m9"),
            PublicKey([0x22; 32]),
            OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 9)),
            None,
            vec!["1.1.1.1:51820".into()],
        );
        assert_eq!(observation.id().0, "m9");
        assert!(observation.bridge_ip.is_none());
        assert!(observation.subnet.is_none());
        assert_eq!(observation.endpoints, vec!["1.1.1.1:51820"]);
    }
}
