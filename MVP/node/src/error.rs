use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("state directory is already initialized: {path}")]
    AlreadyInitialized { path: PathBuf },
    #[error("state directory is not initialized: {path}")]
    NotInitialized { path: PathBuf },
    #[error("unsupported command '{command}'")]
    UnsupportedCommand { command: String },
    #[error("missing value for {flag}")]
    MissingFlagValue { flag: &'static str },
    #[error("unknown argument '{argument}'")]
    UnknownArgument { argument: String },
    #[error("command '{command}' is not wired yet")]
    CommandNotWired { command: String },
    #[error("decode invite token: {source}")]
    DecodeInviteToken { source: serde_json::Error },
    #[error("encode invite token: {source}")]
    EncodeInviteToken { source: serde_json::Error },
    #[error("decode admission request: {source}")]
    DecodeAdmissionRequest { source: serde_json::Error },
    #[error("encode admission request: {source}")]
    EncodeAdmissionRequest { source: serde_json::Error },
    #[error("invite token expired at {expires_at_ms}, now {now_ms}")]
    InviteExpired { expires_at_ms: u64, now_ms: u64 },
    #[error("invite '{invite_id}' was not issued by this node")]
    InviteNotFound { invite_id: String },
    #[error("invite '{invite_id}' secret did not match")]
    InviteSecretMismatch { invite_id: String },
    #[error("admission island '{request_island}' does not match local island '{local_island}'")]
    AdmissionIslandMismatch {
        local_island: String,
        request_island: String,
    },
    #[error(
        "admission principal '{principal_id}' does not match expected '{expected}' for node '{node_id}'"
    )]
    InvalidAdmissionPrincipal {
        node_id: String,
        principal_id: String,
        expected: String,
    },
    #[error("admission conflicts with existing peer node='{node_id}' principal='{principal_id}'")]
    AdmissionPeerConflict {
        node_id: String,
        principal_id: String,
    },
    #[error("joined node has no stored invite credentials for admission")]
    MissingJoinInvite,
    #[error("unsupported node state schema version {found}, expected {expected}")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    #[error("create state directory '{path}': {source}")]
    CreateStateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("read node state '{path}': {source}")]
    ReadState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write node state '{path}': {source}")]
    WriteState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("persist node state '{path}': {source}")]
    PersistState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("sync node state directory '{path}': {source}")]
    SyncStateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("decode node state '{path}': {source}")]
    DecodeState {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("encode node state: {source}")]
    EncodeState { source: serde_json::Error },
    #[error("invalid stored author key: {source}")]
    InvalidAuthorKey {
        source: mvp_p2panda_facts::PandaFactError,
    },
    #[error("invalid stored p2panda network id: {source}")]
    InvalidNetworkId {
        source: mvp_p2panda_transport::PandaNetConfigError,
    },
    #[error("invalid stored p2panda node seed: {source}")]
    InvalidNodeSeed {
        source: mvp_p2panda_transport::PandaNetConfigError,
    },
    #[error("invalid stored p2panda topic: {source}")]
    InvalidTopic {
        source: mvp_p2panda_transport::PandaNetConfigError,
    },
    #[error("invalid stored p2panda bootstrap ticket: {source}")]
    InvalidBootstrapTicket {
        source: mvp_p2panda_transport::PandaNetConfigError,
    },
    #[error("p2panda fact store failed: {source}")]
    FactStore {
        source: mvp_p2panda_facts::PandaFactError,
    },
    #[error("fact source failed: {source}")]
    FactSource {
        source: mvp_projection::FactSourceError,
    },
    #[error("p2panda transport failed: {source}")]
    Transport {
        source: mvp_p2panda_transport::PandaNetTransportError,
    },
    #[error("mesh command failed: {source}")]
    Mesh { source: mvp_mesh::MeshError },
    #[error("bus operation failed: {source}")]
    Bus { source: mvp_bus::BusError },
    #[error("bus subject is invalid: {source}")]
    BusSubject { source: mvp_bus::SubjectParseError },
    #[error("create runtime: {source}")]
    Runtime { source: std::io::Error },
}

pub type NodeResult<T> = Result<T, NodeError>;
