//! Core-local namespace intent evidence.

use ployz_core::ops::RouteTarget;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct NamespaceIntentStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl NamespaceIntentStore {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn replace_route_binding(
        &self,
        state: RouteBindingState,
    ) -> Result<(), NamespaceIntentStoreError> {
        let _guard = self.lock.lock().await;
        let mut evidence = read_evidence(&self.path)?;
        evidence
            .route_bindings
            .retain(|route| route.target != state.target);
        evidence.route_bindings.push(state);
        evidence.route_bindings.sort_by(|left, right| {
            left.target
                .hostname
                .as_str()
                .cmp(right.target.hostname.as_str())
                .then(left.target.port.get().cmp(&right.target.port.get()))
        });
        write_evidence(&self.path, &evidence)
    }

    pub async fn remove_route_binding(
        &self,
        target: &RouteTarget,
    ) -> Result<(), NamespaceIntentStoreError> {
        let _guard = self.lock.lock().await;
        let mut evidence = read_evidence(&self.path)?;
        evidence
            .route_bindings
            .retain(|route| &route.target != target);
        write_evidence(&self.path, &evidence)
    }

    pub async fn replace_serving_target_entry(
        &self,
        state: ServingTargetEntry,
    ) -> Result<(), NamespaceIntentStoreError> {
        let _guard = self.lock.lock().await;
        let mut evidence = read_evidence(&self.path)?;
        evidence.serving_target_entries.retain(|entry| {
            entry.namespace_id != state.namespace_id || entry.service_id != state.service_id
        });
        evidence.serving_target_entries.push(state);
        evidence.serving_target_entries.sort_by(|left, right| {
            left.namespace_id
                .as_str()
                .cmp(right.namespace_id.as_str())
                .then(left.service_id.as_str().cmp(right.service_id.as_str()))
        });
        write_evidence(&self.path, &evidence)
    }

    pub async fn remove_serving_target_entry(
        &self,
        entry: &ServingTargetEntry,
    ) -> Result<(), NamespaceIntentStoreError> {
        let _guard = self.lock.lock().await;
        let mut evidence = read_evidence(&self.path)?;
        evidence.serving_target_entries.retain(|current| {
            current.namespace_id != entry.namespace_id || current.service_id != entry.service_id
        });
        write_evidence(&self.path, &evidence)
    }

    pub fn load(&self) -> Result<NamespaceIntentEvidence, NamespaceIntentStoreError> {
        read_evidence(&self.path)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceIntentEvidence {
    pub route_bindings: Vec<RouteBindingState>,
    pub serving_target_entries: Vec<ServingTargetEntry>,
}

#[derive(Debug)]
pub enum NamespaceIntentStoreError {
    Read { message: String },
    Decode { message: String },
    Encode { message: String },
    Write { message: String },
    Commit { message: String },
}

impl std::fmt::Display for NamespaceIntentStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { message } => {
                write!(formatter, "read namespace intent evidence: {message}")
            }
            Self::Decode { message } => {
                write!(formatter, "decode namespace intent evidence: {message}")
            }
            Self::Encode { message } => {
                write!(formatter, "encode namespace intent evidence: {message}")
            }
            Self::Write { message } => {
                write!(formatter, "write namespace intent evidence: {message}")
            }
            Self::Commit { message } => {
                write!(formatter, "commit namespace intent evidence: {message}")
            }
        }
    }
}

impl std::error::Error for NamespaceIntentStoreError {}

fn read_evidence(path: &Path) -> Result<NamespaceIntentEvidence, NamespaceIntentStoreError> {
    let payload = match std::fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NamespaceIntentEvidence::default());
        }
        Err(error) => {
            return Err(NamespaceIntentStoreError::Read {
                message: error.to_string(),
            });
        }
    };
    serde_json::from_slice(&payload).map_err(|error| NamespaceIntentStoreError::Decode {
        message: error.to_string(),
    })
}

fn write_evidence(
    path: &Path,
    evidence: &NamespaceIntentEvidence,
) -> Result<(), NamespaceIntentStoreError> {
    let payload =
        serde_json::to_vec_pretty(evidence).map_err(|error| NamespaceIntentStoreError::Encode {
            message: error.to_string(),
        })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| NamespaceIntentStoreError::Write {
            message: error.to_string(),
        })?;
    }
    let temp_path = path.with_extension("tmp");
    let mut file =
        std::fs::File::create(&temp_path).map_err(|error| NamespaceIntentStoreError::Write {
            message: error.to_string(),
        })?;
    file.write_all(&payload)
        .and_then(|()| file.sync_all())
        .map_err(|error| NamespaceIntentStoreError::Write {
            message: error.to_string(),
        })?;
    std::fs::rename(&temp_path, path).map_err(|error| NamespaceIntentStoreError::Commit {
        message: error.to_string(),
    })
}
