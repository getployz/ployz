//! Core-local namespace intent evidence.

use crate::evidence_file::{EvidenceFileError, read_json_or_default, write_json};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use serde::{Deserialize, Serialize};
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
        let mut evidence: NamespaceIntentEvidence = read_json_or_default(&self.path)?;
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
        let mut evidence: NamespaceIntentEvidence = read_json_or_default(&self.path)?;
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
        let mut evidence: NamespaceIntentEvidence = read_json_or_default(&self.path)?;
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
        let mut evidence: NamespaceIntentEvidence = read_json_or_default(&self.path)?;
        evidence.serving_target_entries.retain(|current| {
            current.namespace_id != entry.namespace_id || current.service_id != entry.service_id
        });
        write_evidence(&self.path, &evidence)
    }

    pub fn load(&self) -> Result<NamespaceIntentEvidence, NamespaceIntentStoreError> {
        read_json_or_default(&self.path).map_err(NamespaceIntentStoreError::from)
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
        }
    }
}

impl std::error::Error for NamespaceIntentStoreError {}

fn write_evidence(
    path: &Path,
    evidence: &NamespaceIntentEvidence,
) -> Result<(), NamespaceIntentStoreError> {
    write_json(path, evidence).map_err(NamespaceIntentStoreError::from)
}

impl From<EvidenceFileError> for NamespaceIntentStoreError {
    fn from(error: EvidenceFileError) -> Self {
        match error {
            EvidenceFileError::Read { message } => Self::Read { message },
            EvidenceFileError::Decode { message } => Self::Decode { message },
            EvidenceFileError::Encode { message } => Self::Encode { message },
            EvidenceFileError::Write { message } => Self::Write { message },
        }
    }
}
