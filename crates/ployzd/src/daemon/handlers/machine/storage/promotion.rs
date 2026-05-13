use super::*;

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
    pub(super) authority_peers: Vec<MachineMembership>,
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

pub(super) async fn rollback_remote_storage_promotions(
    client: &NatsNodeRpcClient,
    rollbacks: &[RemoteStorageRollback],
) -> Vec<MachineStoragePromotionFailure> {
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

pub(super) async fn restore_remote_storage(
    client: &NatsNodeRpcClient,
    rollback: &RemoteStorageRollback,
) -> Result<(), String> {
    let response = client
        .request(
            NodeCommandSubject::machine_storage_restore_self(&rollback.machine_id),
            &NodeRequest::MachineStorageRestoreSelf {
                participation: rollback.participation.clone(),
                replicas: rollback.replicas,
                authority_peers: rollback.authority_peers.clone(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if response.is_ok() {
        Ok(())
    } else {
        Err(format!("{}: {}", response.code(), response.message()))
    }
}

pub(super) async fn promote_remote_storage(
    client: &NatsNodeRpcClient,
    target: &MachineMembership,
    replicas: StorageReplicaPolicy,
    authority_peers: &[MachineMembership],
) -> Result<(), String> {
    let response = client
        .request(
            NodeCommandSubject::machine_storage_promote_self(&target.id),
            &NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers: authority_peers
                    .iter()
                    .map(MachineStorageAuthorityPeer::from)
                    .collect(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if response.is_ok() {
        Ok(())
    } else {
        Err(format!("{}: {}", response.code(), response.message()))
    }
}

pub(super) async fn preflight_remote_storage_promotion(
    client: &NatsNodeRpcClient,
    targets: &[&MachineMembership],
) -> Result<(), Vec<MachineStoragePromotionFailure>> {
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

pub(super) async fn remote_status(
    client: &NatsNodeRpcClient,
    target: &MachineMembership,
) -> Result<StatusPayload, String> {
    let response = client
        .request(NodeCommandSubject::status(&target.id), &NodeRequest::Status)
        .await
        .map_err(|error| error.to_string())?;
    if !response.is_ok() {
        return Err(format!("{}: {}", response.code(), response.message()));
    }
    match response.payload() {
        Some(DaemonPayload::Status(status)) if status.machine_id == target.id.as_str() => {
            Ok(status)
        }
        Some(DaemonPayload::Status(status)) => Err(format!(
            "remote status reported machine {} instead of {}",
            status.machine_id, target.id
        )),
        Some(payload) => Err(format!(
            "remote status returned unexpected payload: {payload:?}"
        )),
        None => Err("remote status returned no payload".into()),
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
