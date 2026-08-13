mod admin;
mod api;
mod service;
mod store;

pub use admin::AdminClient;
pub use api::{ApiClient, ExecResponse, QueryResult, Statement};
pub use service::{
    CorrosionConfig, DEFAULT_API_ADDRESS, DEFAULT_CONTAINER_NAME, DEFAULT_GOSSIP_ADDRESS,
    RunningCorrosion,
};
pub use store::{
    ContainerRecord, DockerSyncStatus, MissingChange, ReplicatedObservations, ReplicatedStore,
    wait_for_catch_up,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Corrosion I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Corrosion HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Corrosion JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Corrosion TOML failed: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("Docker operation for Corrosion failed: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("Corrosion API error: {0}")]
    Api(String),
    #[error("invalid Corrosion protocol response: {0}")]
    Protocol(String),
}
