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
    #[error("node ticket is not available; start daemon before creating invite")]
    MissingNodeTicket,
    #[error("decode invite token: {source}")]
    DecodeInviteToken { source: serde_json::Error },
    #[error("encode invite token: {source}")]
    EncodeInviteToken { source: serde_json::Error },
    #[error("invite token expired at {expires_at_ms}, now {now_ms}")]
    InviteExpired { expires_at_ms: u64, now_ms: u64 },
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
    #[error("p2panda transport failed: {source}")]
    Transport {
        source: mvp_p2panda_transport::PandaNetTransportError,
    },
    #[error("mesh command failed: {source}")]
    Mesh { source: mvp_mesh::MeshError },
    #[error("create runtime: {source}")]
    Runtime { source: std::io::Error },
}

pub type NodeResult<T> = Result<T, NodeError>;
