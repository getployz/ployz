use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("create runtime directory '{path}': {source}")]
    CreateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("read runtime metadata '{path}': {source}")]
    ReadMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write runtime metadata '{path}': {source}")]
    WriteMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("decode runtime metadata '{path}': {source}")]
    DecodeMetadata {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("encode runtime metadata: {source}")]
    EncodeMetadata { source: serde_json::Error },
    #[error("spawn instance '{instance_id}': {source}")]
    Spawn {
        instance_id: String,
        source: std::io::Error,
    },
    #[error("instance '{instance_id}' did not become ready at {address}")]
    ReadinessTimeout {
        instance_id: String,
        address: String,
    },
    #[error("runtime HTTP server failed: {source}")]
    HttpServer { source: std::io::Error },
    #[error("runtime command path is unavailable: {source}")]
    CurrentExe { source: std::io::Error },
    #[error("stop instance '{instance_id}' pid {pid}: {source}")]
    Stop {
        instance_id: String,
        pid: u32,
        source: std::io::Error,
    },
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;
