use ipnet::Ipv4Net;
use ployz_types::model::{
    AuthorityNodePosture, MachineId, OverlayIp, PublicKey, RegionRole, StorageReplicaPolicy,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineAddOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_identity_private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<MachineInstallOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineStoragePromoteRequest {
    pub targets: Vec<String>,
    pub replicas: StorageReplicaPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineStoragePromotionPayload {
    pub operation_id: String,
    pub replicas: StorageReplicaPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<MachineStoragePromotionFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineStoragePromotionFailure {
    pub machine_id: String,
    pub cause: MachineStoragePromotionFailureCause,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineStoragePromotionFailureCause {
    DuplicateTarget,
    MachineNotFound,
    InvalidCandidate,
    VersionMismatch,
    RpcUnavailable,
    UnexpectedStatusPayload,
    PublishPromotedMembershipFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineStorageAuthorityPeer {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<Ipv4Net>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_ip: Option<OverlayIp>,
    pub region_role: RegionRole,
    pub endpoints: Vec<String>,
}

impl From<&ployz_types::model::MachineMembership> for MachineStorageAuthorityPeer {
    fn from(record: &ployz_types::model::MachineMembership) -> Self {
        Self {
            machine_id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            region_role: record.region_role,
            endpoints: record.endpoints.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallRuntimeTarget {
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallServiceMode {
    User,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstallGitUrl(String);

impl InstallGitUrl {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).expect("valid git install URL")
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("git install URL cannot be empty".into());
        }
        if value.chars().any(char::is_control) {
            return Err("git install URL cannot contain control characters".into());
        }
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

impl From<InstallGitUrl> for String {
    fn from(value: InstallGitUrl) -> Self {
        value.into_string()
    }
}

impl TryFrom<String> for InstallGitUrl {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for InstallGitUrl {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InstallSource {
    Release {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Git {
        git_url: InstallGitUrl,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInstallOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_target: Option<InstallRuntimeTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_mode: Option<InstallServiceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InstallSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineListPayload {
    pub rows: Vec<MachineListRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineListRow {
    pub id: String,
    pub lifecycle: String,
    pub authority: AuthorityNodePosture,
    pub region: String,
    pub region_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_zone: Option<String>,
    pub overlay_ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRttPayload {
    pub rows: Vec<MachineRttRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRttRow {
    pub machine: String,
    pub peer: String,
    pub median_ms: f64,
    pub stddev_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineAddPayload {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub awaiting_self_publication: Vec<MachineAwaitingSelfPublication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_preflight: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_join: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_self_record: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_ready: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_enable: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineAwaitingSelfPublication {
    pub target: String,
    pub joiner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineRemovePayload {
    pub id: String,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineUpdatePayload {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated: Vec<MachineUpdateRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<MachineUpdateRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineUpdateRow {
    pub id: String,
    pub version: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineTransitionGoal {
    Activate,
    Drain,
    Standby,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "goal")]
pub enum MachineSelfTransition {
    Activate { assigned_subnet: ipnet::Ipv4Net },
    Drain,
    Standby { force: bool },
}

impl MachineSelfTransition {
    #[must_use]
    pub fn goal(self) -> MachineTransitionGoal {
        match self {
            Self::Activate { .. } => MachineTransitionGoal::Activate,
            Self::Drain => MachineTransitionGoal::Drain,
            Self::Standby { .. } => MachineTransitionGoal::Standby,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInviteListPayload {
    pub invites: Vec<MachineInviteInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInviteInfo {
    pub invite_id: String,
    pub expires_at: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineOperationListPayload {
    pub operations: Vec<MachineOperationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineOperationPayload {
    pub operation: MachineOperationInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineOperationInfo {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    pub status: String,
    pub stage: String,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_subnet: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_source_release_owns_version() {
        let options = MachineInstallOptions {
            runtime_target: Some(InstallRuntimeTarget::Host),
            service_mode: Some(InstallServiceMode::User),
            source: Some(InstallSource::Release {
                version: Some("0.6.1".into()),
            }),
        };

        let json = serde_json::to_value(&options).expect("serialize install options");

        assert_eq!(
            json,
            serde_json::json!({
                "runtime_target": "host",
                "service_mode": "user",
                "source": {
                    "kind": "release",
                    "version": "0.6.1"
                }
            })
        );
    }

    #[test]
    fn install_source_git_owns_url_and_ref() {
        let options = MachineInstallOptions {
            runtime_target: None,
            service_mode: None,
            source: Some(InstallSource::Git {
                git_url: InstallGitUrl::new("https://example.invalid/ployz.git"),
                git_ref: Some("main".into()),
            }),
        };

        let json = serde_json::to_value(&options).expect("serialize install options");

        assert_eq!(
            json,
            serde_json::json!({
                "source": {
                    "kind": "git",
                    "git_url": "https://example.invalid/ployz.git",
                    "git_ref": "main"
                }
            })
        );
    }

    #[test]
    fn install_source_rejects_git_fields_on_release() {
        let json = serde_json::json!({
            "kind": "release",
            "version": "0.6.1",
            "git_url": "https://example.invalid/ployz.git"
        });

        serde_json::from_value::<InstallSource>(json)
            .expect_err("release install source cannot carry git url");
    }

    #[test]
    fn install_source_rejects_empty_git_url() {
        let json = serde_json::json!({
            "kind": "git",
            "git_url": ""
        });

        serde_json::from_value::<InstallSource>(json)
            .expect_err("git install source cannot carry an empty url");
    }
}
