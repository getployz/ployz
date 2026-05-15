use ployz_api::{
    MachineStorageAuthorityPeer, MachineStoragePromotionFailure,
    MachineStoragePromotionFailureCause, MachineStoragePromotionPayload,
};
use ployz_model::{MachineId, StorageParticipation, StorageReplicaPolicy};

mod remote;
pub(super) use remote::{
    preflight_remote_storage_promotion, promote_remote_storage, rollback_remote_storage_promotions,
};

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

pub(in crate::daemon::handlers::machine::storage) struct RemoteStorageRollback {
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
