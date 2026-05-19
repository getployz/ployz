use std::net::AddrParseError;
use std::net::SocketAddr;
use std::time::Duration;

use mvp_identity::NodeId;
use thiserror::Error;

use crate::invite::InviteId;

#[derive(Debug, Error)]
pub enum MeshError {
    #[error("invite {invite_id} expired at {expires_at_ms}, checked at {now_ms}")]
    InviteExpired {
        invite_id: InviteId,
        expires_at_ms: u64,
        now_ms: u64,
    },
    #[error("invite {invite_id} secret did not match")]
    InviteSecretMismatch { invite_id: InviteId },
    #[error("node {node_id:?} is tombstoned and needs a fresh reinvite")]
    NodeTombstoned { node_id: NodeId },
    #[error("local node {node_id:?} is not live in projection")]
    LocalNodeNotLive { node_id: NodeId },
    #[error("full mesh supports at most {limit} live nodes, got {count}")]
    TooManyFullMeshNodes { count: usize, limit: usize },
    #[error("node {node_id:?} has invalid overlay ip {overlay_ip}: {source}")]
    InvalidOverlayIp {
        node_id: NodeId,
        overlay_ip: String,
        source: AddrParseError,
    },
    #[error("node {node_id:?} is missing {field}")]
    MissingMeshIdentity {
        node_id: NodeId,
        field: &'static str,
    },
    #[error("wireguard snapshot {operation} failed for {path}: {message}")]
    SnapshotIo {
        operation: &'static str,
        path: String,
        message: String,
    },
    #[error("wireguard snapshot {path} is invalid: {message}")]
    InvalidSnapshot { path: String, message: String },
    #[error("wireguard backend {operation} failed: {message}")]
    Backend {
        operation: &'static str,
        message: String,
    },
    #[error("wireguard backend {operation} timed out after {timeout:?}")]
    BackendTimeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("wireguard actor unavailable during {operation}: {reason}")]
    ActorUnavailable {
        operation: &'static str,
        reason: String,
    },
    #[error("host network endpoint {address} is invalid: {message}")]
    InvalidHostEndpoint { address: String, message: String },
    #[error("host network route {address} was unreachable during {operation}: {message}")]
    HostEndpointUnreachable {
        address: SocketAddr,
        operation: &'static str,
        message: String,
    },
}

pub type MeshResult<T> = std::result::Result<T, MeshError>;
