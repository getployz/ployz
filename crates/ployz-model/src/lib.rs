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

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct Namespace(String);

impl AsRef<str> for Namespace {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Namespace {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).expect("valid namespace")
    }

    pub fn try_new(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        if !valid_storage_segment(&value) {
            return Err(format!(
                "namespace '{value}' must be 1-63 chars of [a-z0-9_-], starting with a letter or digit"
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn system() -> Self {
        Self::new("system")
    }

    #[must_use]
    pub fn default_ns() -> Self {
        Self::new("default")
    }

    #[must_use]
    pub fn is_system(&self) -> bool {
        self.as_str() == "system"
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

impl From<Namespace> for String {
    fn from(value: Namespace) -> Self {
        value.into_string()
    }
}

impl TryFrom<String> for Namespace {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for Namespace {
    type Error = String;

    fn try_from(value: &str) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumeCloneDataPolicy {
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumeCloneConsistency {
    CrashConsistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumeScope {
    Single,
    Shared,
}

mod build;
mod certificate;
mod deploy;
mod image;
mod machine;
mod routing;

pub use build::*;
pub use certificate::*;
pub use deploy::*;
pub use image::*;
pub use machine::*;
pub use routing::*;

pub fn stable_hash_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }

    format!("{hash:016x}")
}

pub fn valid_storage_segment(value: &str) -> bool {
    if value.is_empty() || value.len() > 63 {
        return false;
    }
    let Some(first) = value.chars().next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
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
            source_namespace: Namespace::new("prod"),
            target_namespace: Namespace::new("pr-39"),
            state: BranchEnvironmentState::Prepared,
            default_service_mode: BranchEnvironmentResourceMode::Branch,
            default_volume_mode: BranchEnvironmentResourceMode::Fresh,
            services: vec![BranchEnvironmentResourceOverride {
                name: "worker".into(),
                mode: BranchEnvironmentResourceMode::Fresh,
            }],
            volumes: Vec::new(),
            prepared_deploy_id: Some(DeployId::new("prepare-1")),
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
            image: ImageRef::repository_digest(
                "registry.example/api",
                Some("latest".into()),
                ImageDigest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            ),
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
    fn image_ref_rejects_tag_without_repository_variant() {
        let json = serde_json::json!({
            "kind": "digest",
            "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "tag": "latest"
        });

        serde_json::from_value::<ImageRef>(json).expect_err("digest-only image cannot carry tag");
    }

    #[test]
    fn image_operation_target_outcome_rejects_failed_success_facts() {
        let json = serde_json::json!({
            "status": "failed",
            "machine_id": "machine-a",
            "bytes_transferred": 128,
            "last_error": "disk full"
        });

        serde_json::from_value::<ImageOperationTargetOutcome>(json)
            .expect_err("failed target cannot carry success bytes");
    }

    #[test]
    fn image_operation_state_rejects_success_with_error() {
        let json = serde_json::json!({
            "id": "image-push-1",
            "kind": "push",
            "stage": "complete",
            "digest": {
                "kind": "digest",
                "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "targets": [],
            "started_at": 1,
            "updated_at": 2,
            "state": {
                "status": "succeeded",
                "last_error": "copy failed"
            }
        });

        serde_json::from_value::<ImageOperationRecord>(json)
            .expect_err("succeeded image operation cannot carry last_error");
    }

    #[test]
    fn build_operation_state_rejects_failed_artifact() {
        let json = serde_json::json!({
            "status": "failed",
            "last_error": "build failed",
            "artifact": {
                "image": {
                    "kind": "digest",
                    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                "provenance": {
                    "kind": "external"
                },
                "created_at": 42
            }
        });

        serde_json::from_value::<BuildOperationState>(json)
            .expect_err("failed build operation cannot carry success artifact");
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
            posture.role(),
            AuthorityNodeRole::AuthorityStorage {
                authority_id: AuthorityId::default_authority(),
            }
        );
        assert_eq!(posture.data_bucket(), ControlPlaneDataBucket::StoredIntent);
        assert_eq!(
            posture.loss_impact(),
            ControlPlaneLossImpact::StoredTruthLost
        );
    }

    #[test]
    fn authority_posture_keeps_candidate_disposable_for_control_plane_truth() {
        let posture = AuthorityNodePosture::from_storage_participation(
            true,
            &StorageParticipation::Candidate,
        );

        assert_eq!(posture.role(), AuthorityNodeRole::StorageCandidate);
        assert_eq!(posture.data_bucket(), ControlPlaneDataBucket::StoredIntent);
        assert_eq!(
            posture.loss_impact(),
            ControlPlaneLossImpact::NoStoredTruthLost
        );
    }

    #[test]
    fn authority_posture_marks_non_storage_participant_as_compute_live_fact() {
        let posture = AuthorityNodePosture::from_storage_participation(
            false,
            &StorageParticipation::Candidate,
        );

        assert_eq!(posture.role(), AuthorityNodeRole::Compute);
        assert_eq!(posture.data_bucket(), ControlPlaneDataBucket::LiveFacts);
        assert_eq!(
            posture.loss_impact(),
            ControlPlaneLossImpact::NoStoredTruthLost
        );
    }

    #[test]
    fn authority_posture_serializes_as_single_variant_with_derived_impact() {
        let posture = AuthorityNodePosture::from_storage_participation(
            true,
            &StorageParticipation::default_authority(),
        );

        let json = serde_json::to_value(&posture).expect("serialize posture");

        assert_eq!(
            json,
            serde_json::json!({
                "kind": "authority_storage",
                "authority_id": "auth-default"
            })
        );
        let decoded: AuthorityNodePosture =
            serde_json::from_value(json).expect("deserialize posture");
        assert_eq!(decoded, posture);
        assert_eq!(decoded.data_bucket(), ControlPlaneDataBucket::StoredIntent);
        assert_eq!(
            decoded.loss_impact(),
            ControlPlaneLossImpact::StoredTruthLost
        );
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
        assert_eq!(record.state(), DeployState::Committed);
        assert_eq!(record.committed_at(), Some(42));
        assert_eq!(record.finished_at(), Some(42));
        assert_eq!(record.summary_json(), "{}");
    }

    #[test]
    fn deploy_phase_success_evidence_matches_commit_policy() {
        let end_commit = DeployId::new("deploy-1");
        let end_state = DeployPhaseRecordState::succeeded(
            DeployPhaseCommitPolicy::EndOfDeploy,
            42,
            Some(end_commit.clone()),
        )
        .expect("end-of-deploy success has commit id");
        assert_eq!(
            end_state.commit_policy(),
            DeployPhaseCommitPolicy::EndOfDeploy
        );
        assert_eq!(end_state.commit_deploy_id(), Some(end_commit));
        assert_eq!(
            end_state.lifecycle(),
            DeployPhaseState::Succeeded { completed_at: 42 }
        );

        let checkpoint_commit = DeployId::new("deploy-1:phase:db");
        let checkpoint_state = DeployPhaseRecordState::succeeded(
            DeployPhaseCommitPolicy::Checkpoint,
            43,
            Some(checkpoint_commit.clone()),
        )
        .expect("checkpoint success has commit id");
        assert_eq!(
            checkpoint_state.commit_policy(),
            DeployPhaseCommitPolicy::Checkpoint
        );
        assert_eq!(checkpoint_state.commit_deploy_id(), Some(checkpoint_commit));

        let no_store_state =
            DeployPhaseRecordState::succeeded(DeployPhaseCommitPolicy::NoStoreCommit, 44, None)
                .expect("no-store success omits commit id");
        assert_eq!(
            no_store_state.commit_policy(),
            DeployPhaseCommitPolicy::NoStoreCommit
        );
        assert_eq!(no_store_state.commit_deploy_id(), None);

        assert!(
            DeployPhaseRecordState::succeeded(DeployPhaseCommitPolicy::Checkpoint, 45, None)
                .is_err()
        );
        assert!(
            DeployPhaseRecordState::succeeded(
                DeployPhaseCommitPolicy::NoStoreCommit,
                46,
                Some(DeployId::new("deploy-1:phase:no-store")),
            )
            .is_err()
        );
    }

    #[test]
    fn deploy_phase_record_rejects_old_parallel_commit_fields() {
        let old_shape = serde_json::json!({
            "namespace": "prod",
            "deploy_id": "deploy-1",
            "phase_id": "deploy",
            "commit_deploy_id": "deploy-1",
            "name": "Deploy",
            "order": 0,
            "after": [],
            "participants": [],
            "work": [],
            "state": {
                "succeeded": {
                    "completed_at": 42
                }
            },
            "commit_policy": "end_of_deploy",
            "rollback_policy": "reversible",
            "advance_policy": "immediate",
            "started_at": 1
        });

        assert!(serde_json::from_value::<DeployPhaseRecord>(old_shape).is_err());
    }

    #[test]
    fn deploy_transition_idempotent_commit_preserves_original_completion() {
        let mut record = deploy_record(DeployState::Committed);
        record.mark_committed(42, 42, r#"{"started":1}"#.into());

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
        assert_eq!(record.state(), DeployState::Committed);
        assert_eq!(record.committed_at(), Some(42));
        assert_eq!(record.finished_at(), Some(42));
        assert_eq!(record.summary_json(), r#"{"started":1}"#);
    }

    #[test]
    fn deploy_transition_idempotent_cleanup_pending_preserves_original_timestamp() {
        let mut record = deploy_record(DeployState::CleanupPending);
        record.mark_committed(42, 42, "{}".into());
        record.mark_cleanup_pending(50).expect("cleanup pending");

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
        assert_eq!(record.state(), DeployState::CleanupPending);
        assert_eq!(record.committed_at(), Some(42));
        assert_eq!(record.finished_at(), Some(50));
        assert_eq!(record.summary_json(), "{}");
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
        assert_eq!(record.state(), DeployState::Applying);
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
        let record_state = match state {
            DeployState::Planning => DeployRecordState::Planning {
                summary_json: "null".into(),
            },
            DeployState::Applying => DeployRecordState::Applying {
                summary_json: "null".into(),
            },
            DeployState::Committed => DeployRecordState::Committed {
                committed_at: 1,
                finished_at: 1,
                summary_json: "null".into(),
            },
            DeployState::CheckpointCommitted => DeployRecordState::CheckpointCommitted {
                summary_json: "null".into(),
            },
            DeployState::CleanupPending => DeployRecordState::CleanupPending {
                committed_at: 1,
                finished_at: 1,
                summary_json: "null".into(),
            },
            DeployState::FailedAfterCheckpoint => DeployRecordState::FailedAfterCheckpoint {
                finished_at: 1,
                summary_json: "null".into(),
            },
            DeployState::Failed => DeployRecordState::Failed {
                finished_at: 1,
                summary_json: "null".into(),
            },
        };
        DeployRecord {
            deploy_id: DeployId::new("deploy-1"),
            namespace: Namespace::new("default"),
            coordinator_machine_id: MachineId::new("m1"),
            manifest_hash: "hash".into(),
            started_at: 1,
            state: record_state,
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
