use super::*;
use ployz_api::StatusPayload;
use ployz_node_runtime::{
    MachineStorageNodeClient, MachineStorageRpcTransport, NODE_STATUS_RPC_POLICY,
    NodeProbeNodeClient, NodeProbeRpcOperation, NodeProbeRpcTransport, NodeRpcError,
};

use crate::daemon::node_rpc::{STATUS_PAYLOAD_KIND, decode_daemon_node_payload};

pub(super) fn promotion_payload(
    operation_id: &str,
    replicas: StorageReplicaPolicy,
    promoted: Vec<String>,
    failed: Vec<MachineStoragePromotionFailure>,
) -> MachineStoragePromotionPayload {
    MachineStoragePromotionPayload {
        operation_id: operation_id.to_string(),
        replicas,
        promoted,
        failed,
    }
}

pub(super) struct RemoteStorageRollback {
    pub(super) machine_id: MachineId,
    pub(super) participation: StorageParticipation,
    pub(super) replicas: StorageReplicaPolicy,
    pub(super) authority_peers: Vec<MachineStorageAuthorityPeer>,
}

pub(super) enum StoragePromotionError {
    StoreList {
        error: ployz_error::Error,
    },
    InvalidLocalAuthority {
        message: String,
    },
    Preflight {
        failed: Vec<MachineStoragePromotionFailure>,
    },
    ReplicaCount {
        message: String,
    },
    RpcUnavailable {
        error: String,
    },
    PromoteTargets {
        promoted: Vec<String>,
        failed: Vec<MachineStoragePromotionFailure>,
    },
    RecordReplicaIntent {
        error: String,
        promoted: Vec<String>,
    },
    BootstrapPeerRead {
        error: String,
        rollback_error: Option<String>,
        promoted: Vec<String>,
    },
    BootstrapPeerWrite {
        error: String,
        rollback_error: Option<String>,
        promoted: Vec<String>,
    },
    AuthorityRestart {
        error: String,
        promoted: Vec<String>,
    },
}

impl StoragePromotionError {
    pub(super) fn message(&self) -> String {
        match self {
            Self::StoreList { error } => format!("list machines for storage promotion: {error}"),
            Self::InvalidLocalAuthority { message } => message.clone(),
            Self::Preflight { .. } => "storage promotion preflight failed".into(),
            Self::ReplicaCount { message } => message.clone(),
            Self::RpcUnavailable { error } => {
                format!("NATS RPC unavailable for storage promotion: {error}")
            }
            Self::PromoteTargets { .. } => {
                "storage promotion failed while promoting authority members".into()
            }
            Self::RecordReplicaIntent { error, .. } => {
                format!("record storage replica intent: {error}")
            }
            Self::BootstrapPeerRead {
                error,
                rollback_error,
                ..
            } => append_rollback_error(
                format!("load bootstrap peers before storage promotion: {error}"),
                rollback_error.as_deref(),
            ),
            Self::BootstrapPeerWrite {
                error,
                rollback_error,
                ..
            } => append_rollback_error(
                format!("write authority bootstrap peers: {error}"),
                rollback_error.as_deref(),
            ),
            Self::AuthorityRestart { error, .. } => format!(
                "restart authority storage after durable promotion intent was recorded: {error}; retry mesh start or inspect status before retrying promotion"
            ),
        }
    }

    pub(super) fn into_payload(
        self,
        operation_id: &str,
        replicas: StorageReplicaPolicy,
    ) -> MachineStoragePromotionPayload {
        match self {
            Self::Preflight { failed } => {
                promotion_payload(operation_id, replicas, Vec::new(), failed)
            }
            Self::PromoteTargets { promoted, failed } => {
                promotion_payload(operation_id, replicas, promoted, failed)
            }
            Self::RecordReplicaIntent { promoted, .. }
            | Self::BootstrapPeerRead { promoted, .. }
            | Self::BootstrapPeerWrite { promoted, .. }
            | Self::AuthorityRestart { promoted, .. } => {
                promotion_payload(operation_id, replicas, promoted, Vec::new())
            }
            Self::StoreList { .. }
            | Self::InvalidLocalAuthority { .. }
            | Self::ReplicaCount { .. }
            | Self::RpcUnavailable { .. } => {
                promotion_payload(operation_id, replicas, Vec::new(), Vec::new())
            }
        }
    }
}

pub(super) fn append_rollback_error(mut message: String, rollback_error: Option<&str>) -> String {
    if let Some(rollback_error) = rollback_error {
        message.push_str("; rollback failed: ");
        message.push_str(rollback_error);
    }
    message
}

pub(super) fn append_rollback_failures(
    mut failed: Vec<MachineStoragePromotionFailure>,
    intent_rollback_error: Option<String>,
    peers_rollback_error: Option<String>,
) -> Vec<MachineStoragePromotionFailure> {
    if let Some(message) = intent_rollback_error {
        failed.push(MachineStoragePromotionFailure {
            machine_id: String::from("local"),
            cause: MachineStoragePromotionFailureCause::PublishPromotedMembershipFailed,
            message: format!("rollback storage replica intent: {message}"),
        });
    }
    if let Some(message) = peers_rollback_error {
        failed.push(MachineStoragePromotionFailure {
            machine_id: String::from("local"),
            cause: MachineStoragePromotionFailureCause::PublishPromotedMembershipFailed,
            message: format!("rollback bootstrap peers: {message}"),
        });
    }
    failed
}

pub(super) async fn rollback_remote_storage_promotions<T>(
    client: &MachineStorageNodeClient<T>,
    rollbacks: &[RemoteStorageRollback],
) -> Vec<MachineStoragePromotionFailure>
where
    T: MachineStorageRpcTransport,
{
    let mut failed = Vec::new();
    for rollback in rollbacks.iter().rev() {
        if let Err(error) = restore_remote_storage(client, rollback).await {
            failed.push(MachineStoragePromotionFailure {
                machine_id: rollback.machine_id.as_str().to_string(),
                cause: MachineStoragePromotionFailureCause::RpcUnavailable,
                message: format!("rollback promoted storage config: {error}"),
            });
        }
    }
    failed
}

pub(super) async fn restore_remote_storage<T>(
    client: &MachineStorageNodeClient<T>,
    rollback: &RemoteStorageRollback,
) -> Result<(), String>
where
    T: MachineStorageRpcTransport,
{
    client
        .restore_storage_self(
            &rollback.machine_id,
            &rollback.participation,
            rollback.replicas,
            &rollback.authority_peers,
        )
        .await
        .map_err(|error| machine_storage_rpc_error_message(&error))
}

pub(super) async fn promote_remote_storage<T>(
    client: &MachineStorageNodeClient<T>,
    target: &MachineMembership,
    replicas: StorageReplicaPolicy,
    authority_peers: &[MachineStorageAuthorityPeer],
) -> Result<(), String>
where
    T: MachineStorageRpcTransport,
{
    client
        .promote_storage_self(&target.id, replicas, authority_peers)
        .await
        .map_err(|error| machine_storage_rpc_error_message(&error))
}

pub(super) async fn preflight_remote_storage_promotion<T>(
    client: &NodeProbeNodeClient<T>,
    targets: &[&MachineMembership],
) -> Result<(), Vec<MachineStoragePromotionFailure>>
where
    T: NodeProbeRpcTransport,
{
    let mut failed = Vec::new();
    for target in targets {
        match remote_status(client, target).await {
            Ok(status) => {
                if status.version != env!("CARGO_PKG_VERSION") {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.as_str().to_string(),
                        cause: MachineStoragePromotionFailureCause::VersionMismatch,
                        message: format!(
                            "remote daemon version {} does not match required version {} for storage promotion",
                            status.version,
                            env!("CARGO_PKG_VERSION")
                        ),
                    });
                }
            }
            Err(message) => failed.push(MachineStoragePromotionFailure {
                machine_id: target.id.as_str().to_string(),
                cause: MachineStoragePromotionFailureCause::RpcUnavailable,
                message,
            }),
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed)
    }
}

pub(super) async fn remote_status<T>(
    client: &NodeProbeNodeClient<T>,
    target: &MachineMembership,
) -> Result<StatusPayload, String>
where
    T: NodeProbeRpcTransport,
{
    let response = client
        .with_policy(NODE_STATUS_RPC_POLICY)
        .status(&target.id)
        .await
        .map_err(|error| format!("{}: {}", error.code, error.message))?;
    let status = decode_daemon_node_payload::<StatusPayload>(
        NodeProbeRpcOperation::Status.operation_name(),
        response,
        STATUS_PAYLOAD_KIND,
    )
    .map_err(|error| error.to_string())?;
    if status.machine_id == target.id.as_str() {
        Ok(status)
    } else {
        Err(format!(
            "remote status reported machine {} instead of {}",
            status.machine_id, target.id
        ))
    }
}

pub(super) fn machine_storage_rpc_error_message(error: &NodeRpcError) -> String {
    format!("{}: {}", error.code, error.message)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use async_trait::async_trait;
    use ployz_model::{
        MachineLifecycle, MachineStorageRole, MachineTopology, OverlayIp, PublicKey, RegionRole,
    };
    use ployz_node_api::{NodeRequest, NodeResponse};
    use ployz_node_runtime::{
        MachineStorageRpcOperation, NodeRpcError, NodeRpcErrorKind, NodeRpcPolicy,
    };

    #[derive(Clone, Default)]
    struct FakeMachineTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineStorageRpcOperation, NodeRequest)>>>,
    }

    impl FakeMachineTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineStorageRpcTransport for FakeMachineTransport {
        fn with_node_rpc_policy(&self, _policy: NodeRpcPolicy) -> Self {
            self.clone()
        }

        async fn machine_request(
            &self,
            machine_id: &MachineId,
            operation: MachineStorageRpcOperation,
            request: &NodeRequest,
        ) -> Result<NodeResponse, NodeRpcError> {
            self.requests.lock().expect("requests").push((
                machine_id.clone(),
                operation,
                request.clone(),
            ));
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[test]
    fn machine_storage_rpc_error_message_preserves_existing_code_message_shape() {
        let remote = NodeRpcError::remote(
            "machine_storage_promote_self",
            "REMOTE_REFUSED",
            "peer refused",
        );
        assert_eq!(
            machine_storage_rpc_error_message(&remote),
            "REMOTE_REFUSED: peer refused"
        );

        let transport = NodeRpcError {
            kind: NodeRpcErrorKind::Transport,
            operation: "machine_storage_restore_self",
            code: "NATS_RPC_TIMEOUT".into(),
            message: "timed out".into(),
        };
        assert_eq!(
            machine_storage_rpc_error_message(&transport),
            "NATS_RPC_TIMEOUT: timed out"
        );
    }

    #[tokio::test]
    async fn promote_remote_storage_builds_peer_payload_and_maps_remote_failure() {
        let target = machine_record("target");
        let peer = machine_record("authority");
        let authority_peers = vec![MachineStorageAuthorityPeer::from(&peer)];
        let transport = FakeMachineTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let client = MachineStorageNodeClient::new(transport.clone());

        let error =
            promote_remote_storage(&client, &target, StorageReplicaPolicy::R3, &authority_peers)
                .await
                .expect_err("remote promote should fail");

        assert_eq!(error, "REMOTE_REFUSED: peer refused");
        let requests = transport.requests.lock().expect("requests");
        let [request] = requests.as_slice() else {
            panic!("expected one request");
        };
        assert_eq!(request.0, target.id);
        assert_eq!(request.1, MachineStorageRpcOperation::StoragePromoteSelf);
        assert!(matches!(
            &request.2,
            NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers: request_peers,
            } if replicas == &StorageReplicaPolicy::R3 && request_peers == &authority_peers
        ));
    }

    #[tokio::test]
    async fn rollback_remote_storage_promotions_rolls_back_in_reverse_order() {
        let authority = machine_record("authority");
        let authority_peers = vec![MachineStorageAuthorityPeer::from(&authority)];
        let rollbacks = vec![
            RemoteStorageRollback {
                machine_id: MachineId::new("first"),
                participation: StorageParticipation::Candidate,
                replicas: StorageReplicaPolicy::R3,
                authority_peers: authority_peers.clone(),
            },
            RemoteStorageRollback {
                machine_id: MachineId::new("second"),
                participation: StorageParticipation::Candidate,
                replicas: StorageReplicaPolicy::R3,
                authority_peers: authority_peers.clone(),
            },
        ];
        let transport = FakeMachineTransport::with_responses(vec![
            Ok(NodeResponse::success("ok", None)),
            Ok(NodeResponse::error("REMOTE_ROLLBACK_FAILED", "nope", None)),
        ]);
        let client = MachineStorageNodeClient::new(transport.clone());

        let failed = rollback_remote_storage_promotions(&client, &rollbacks).await;

        let requests = transport.requests.lock().expect("requests");
        let [second, first] = requests.as_slice() else {
            panic!("expected two rollback requests");
        };
        assert_eq!(second.0, MachineId::new("second"));
        assert_eq!(second.1, MachineStorageRpcOperation::StorageRestoreSelf);
        assert_eq!(first.0, MachineId::new("first"));
        assert_eq!(first.1, MachineStorageRpcOperation::StorageRestoreSelf);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].machine_id, "first");
        assert_eq!(
            failed[0].message,
            "rollback promoted storage config: REMOTE_ROLLBACK_FAILED: nope"
        );
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([id.len() as u8; 32]),
            overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
                .parse()
                .map(OverlayIp)
                .expect("valid overlay"),
            topology: MachineTopology::local(),
            region_role: RegionRole::HomeData,
            subnet: Some("10.42.0.0/24".parse().expect("valid subnet")),
            bridge_ip: None,
            endpoints: vec![format!("127.0.0.1:{}", 51000 + id.len())],
            lifecycle: MachineLifecycle::Active,
            storage_role: MachineStorageRole::default_authority(),
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }
}

impl From<&MachineStorageAuthorityPeer> for BootstrapPeerRecord {
    fn from(peer: &MachineStorageAuthorityPeer) -> Self {
        Self {
            machine_id: peer.machine_id.clone(),
            public_key: peer.public_key.clone(),
            overlay_ip: peer.overlay_ip,
            subnet: peer.subnet,
            bridge_ip: peer.bridge_ip,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            region_role: peer.region_role,
            endpoints: peer.endpoints.clone(),
        }
    }
}

pub(super) fn validate_authority_peer_payload(
    replicas: StorageReplicaPolicy,
    authority_peers: &[MachineStorageAuthorityPeer],
    local_machine_id: &ployz_model::MachineId,
) -> Result<(), String> {
    if authority_peers.len() != replicas.replicas() {
        return Err(format!(
            "storage promotion self payload for {replicas} must contain exactly {} authority peers, got {}",
            replicas.replicas(),
            authority_peers.len()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut has_local = false;
    for peer in authority_peers {
        if !seen.insert(peer.machine_id.clone()) {
            return Err(format!(
                "storage promotion self payload contains duplicate authority peer '{}'",
                peer.machine_id
            ));
        }
        if peer.machine_id == *local_machine_id {
            has_local = true;
        }
        if peer.endpoints.is_empty() {
            return Err(format!(
                "storage promotion self payload authority peer '{}' has no endpoints",
                peer.machine_id
            ));
        }
    }
    if !has_local {
        return Err(format!(
            "storage promotion self payload must include local authority peer '{}'",
            local_machine_id
        ));
    }
    Ok(())
}

pub(super) fn validate_authority_peers_match_membership(
    authority_peers: &[MachineStorageAuthorityPeer],
    machines: &[MachineMembership],
    local_machine_id: &ployz_model::MachineId,
) -> Result<(), String> {
    let Some(local_peer) = authority_peers
        .iter()
        .find(|peer| peer.machine_id == *local_machine_id)
    else {
        return Err(format!(
            "storage promotion self payload must include local authority peer '{}'",
            local_machine_id
        ));
    };
    let Some(machine) = machines
        .iter()
        .find(|machine| machine.id == local_peer.machine_id)
    else {
        return Err(format!(
            "storage promotion self payload local authority peer '{}' is not in machine membership",
            local_peer.machine_id
        ));
    };
    if machine.public_key != local_peer.public_key
        || machine.overlay_ip != local_peer.overlay_ip
        || machine.subnet != local_peer.subnet
        || machine.bridge_ip != local_peer.bridge_ip
        || machine.region_role != local_peer.region_role
        || machine.endpoints != local_peer.endpoints
    {
        return Err(format!(
            "storage promotion self payload local authority peer '{}' does not match machine membership",
            local_peer.machine_id
        ));
    }
    if machine.lifecycle != MachineLifecycle::Active || !machine.storage() {
        return Err(format!(
            "storage promotion self payload local authority peer '{}' is not active storage membership",
            local_peer.machine_id
        ));
    }
    Ok(())
}

pub(super) fn restore_storage_config(
    path: &std::path::Path,
    config: &mut NetworkConfig,
    storage: bool,
    participation: StorageParticipation,
    replicas: StorageReplicaPolicy,
) -> Result<(), String> {
    config.storage = storage;
    config.storage_participation = participation;
    config.storage_replicas = replicas;
    config
        .save(path)
        .map_err(|error| format!("restore network config: {error}"))
}

pub(super) fn promote_self_rollback_message(
    error: String,
    config_rollback_error: Option<String>,
    peer_rollback_error: Option<String>,
) -> String {
    let message = append_rollback_error(
        format!("failed to promote storage authority: {error}"),
        config_rollback_error.as_deref(),
    );
    append_rollback_error(message, peer_rollback_error.as_deref())
}

pub(super) fn promote_self_record_rollback_message(
    config_rollback_error: Option<String>,
    peer_rollback_error: Option<String>,
    restart_rollback_error: Option<String>,
) -> String {
    let message = append_rollback_error(
        String::from("mesh self record unavailable after storage promotion restart"),
        config_rollback_error.as_deref(),
    );
    let message = append_rollback_error(message, peer_rollback_error.as_deref());
    append_rollback_error(message, restart_rollback_error.as_deref())
}
