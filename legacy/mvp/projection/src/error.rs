use std::path::PathBuf;

use thiserror::Error;

use crate::source::FactSourceError;

pub type ProjectionResult<T> = std::result::Result<T, ProjectionError>;

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error(transparent)]
    FactSource(#[from] FactSourceError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("snapshot path {path} is invalid: {reason}")]
    SnapshotPath { path: PathBuf, reason: String },
    #[error("projection store {path} is invalid: {reason}")]
    ProjectionStore { path: PathBuf, reason: String },
    #[error("projection operation {operation} timed out")]
    Timeout { operation: &'static str },
    #[error("projection worker failed: {0}")]
    WorkerJoin(String),
}
