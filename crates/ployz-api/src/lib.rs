use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_types::model::{
    DeployApplyResult, DeployPreview, JoinResponse, MachineId, MachineRecord,
};
use ployz_types::spec::DeployManifest;
use serde::{Deserialize, Serialize};

pub const JOIN_RESPONSE_PREFIX: &str = "PLOYZ_JOIN_RESPONSE:";

pub fn encode_join_response(response: &JoinResponse) -> Result<String, String> {
    let json = serde_json::to_string(response).map_err(|error| format!("serialize: {error}"))?;
    Ok(format!(
        "{JOIN_RESPONSE_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    ))
}

pub fn decode_join_response(encoded: &str) -> Result<JoinResponse, String> {
    let payload = encoded
        .strip_prefix(JOIN_RESPONSE_PREFIX)
        .ok_or_else(|| format!("missing prefix '{JOIN_RESPONSE_PREFIX}'"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|error| format!("base64 decode: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("json decode: {error}"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_files: Vec<String>,
    #[serde(default)]
    pub prune: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineAddOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_identity_private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<MachineInstallOptions>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInstallOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebugTickTask {
    PeerSync,
    Heartbeat,
    Heal,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    Status,
    Doctor,
    DebugTick {
        task: DebugTickTask,
        repeat: u32,
    },
    MeshList,
    MeshStatus {
        network: String,
    },
    MeshJoin {
        token: String,
    },
    MeshReady,
    MeshCreate {
        network: String,
    },
    MeshInit {
        network: String,
    },
    MeshUp {
        network: String,
        allow_disconnected_bootstrap: bool,
    },
    MeshDown,
    MeshDestroy {
        network: String,
    },
    MachineList,
    MachineInit {
        target: String,
        network: String,
        install: MachineInstallOptions,
    },
    MachineAdd {
        targets: Vec<String>,
        options: MachineAddOptions,
    },
    MachineRemove {
        id: String,
        force: bool,
    },
    MachineOperationList,
    MachineOperationGet {
        id: String,
    },
    MachineInviteCreate {
        ttl_secs: u64,
    },
    MachineInviteImport {
        token: String,
    },
    MeshSelfRecord,
    MeshAccept {
        response: String,
    },
    CoordinationPrepare {
        request: CoordinationPrepareRequest,
    },
    CoordinationRenew {
        request: CoordinationRenewRequest,
    },
    CoordinationCommit {
        request: CoordinationCommitRequest,
    },
    CoordinationAbort {
        request: CoordinationAbortRequest,
    },
    DeployPreview {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployApply {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployExport {
        namespace: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DaemonPayload {
    Status(StatusPayload),
    MeshStatus(MeshStatusPayload),
    MachineList(MachineListPayload),
    MachineAdd(MachineAddPayload),
    MachineRemove(MachineRemovePayload),
    MeshReady(MeshReadyPayload),
    MeshSelfRecord(MeshSelfRecordPayload),
    CoordinationPrepare(CoordinationPreparePayload),
    CoordinationRenew(CoordinationRenewPayload),
    CoordinationCommit(CoordinationCommitPayload),
    DeployPreview(DeployPreviewPayload),
    DeployApply(DeployApplyPayload),
    DeployExport(DeployExportPayload),
    MachineOperationList(MachineOperationListPayload),
    MachineOperation(MachineOperationPayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub protocol_version: u32,
    pub daemon_version: String,
    pub machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_network_name: Option<String>,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStatusPayload {
    pub network_name: String,
    pub network_id: String,
    pub overlay: String,
    pub state: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineListPayload {
    pub rows: Vec<MachineListRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineListRow {
    pub id: String,
    pub status: String,
    pub participation: String,
    pub liveness: String,
    pub overlay_ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<String>,
    pub last_heartbeat: u64,
    pub created_at: u64,
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
pub struct MeshReadyPayload {
    pub ready: bool,
    pub phase: String,
    pub store_healthy: bool,
    pub sync_connected: bool,
    pub heartbeat_started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSelfRecordPayload {
    pub encoded: String,
    pub record: MachineRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinationLockKey {
    DeployNamespace {
        namespace: String,
    },
    MembershipMachine {
        machine_id: String,
    },
    SubnetClaim {
        subnet: String,
    },
    MachineOperation {
        machine_id: String,
        operation: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinationOperation {
    LockAcquire {
        key: CoordinationLockKey,
    },
    MembershipPrepare {
        machine_id: String,
        proposed_subnet: Option<String>,
    },
    MembershipCommit {
        machine_id: String,
        committed_subnet: Option<String>,
    },
    MembershipAbort {
        machine_id: String,
    },
    SubnetClaimPrepare {
        machine_id: String,
        subnet: String,
    },
    SubnetClaimCommit {
        machine_id: String,
        subnet: String,
    },
    SubnetClaimAbort {
        machine_id: String,
        subnet: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationPrepareRequest {
    pub owner_id: String,
    pub nonce: String,
    pub lease_ttl_secs: u64,
    pub operation: CoordinationOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationPreparePayload {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationRenewRequest {
    pub owner_id: String,
    pub nonce: String,
    pub lease_ttl_secs: u64,
    pub operation: CoordinationOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationRenewPayload {
    pub renewed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationCommitRequest {
    pub owner_id: String,
    pub nonce: String,
    pub prepare_tokens: Vec<String>,
    pub operation: CoordinationOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationCommitPayload {
    pub committed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationAbortRequest {
    pub owner_id: String,
    pub nonce: String,
    pub operation: CoordinationOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployPreviewPayload {
    pub preview: DeployPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployApplyPayload {
    pub result: DeployApplyResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployExportPayload {
    pub manifest: DeployManifest,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<DaemonPayload>,
}

#[cfg(test)]
mod tests {
    use super::{JOIN_RESPONSE_PREFIX, decode_join_response, encode_join_response};
    use ployz_types::model::{JoinResponse, MachineId, OverlayIp, PublicKey};
    use std::net::Ipv6Addr;

    #[test]
    fn join_response_codec_roundtrip() {
        let response = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([0xab; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            subnet: Some("10.42.1.0/24".parse().expect("valid subnet")),
            endpoints: vec!["1.2.3.4:51820".into()],
        };

        let encoded = encode_join_response(&response).expect("encode join response");
        assert!(encoded.starts_with(JOIN_RESPONSE_PREFIX));

        let decoded = decode_join_response(&encoded).expect("decode join response");
        assert_eq!(decoded.machine_id, response.machine_id);
        assert_eq!(decoded.public_key, response.public_key);
        assert_eq!(decoded.overlay_ip, response.overlay_ip);
        assert_eq!(decoded.subnet, response.subnet);
        assert_eq!(decoded.endpoints, response.endpoints);
    }
}
