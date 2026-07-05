//! Core-local active-machine roster evidence.

use ployz_core::ids::MachineId;
use ployz_core::state::ActiveMachineState;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct MachineRosterStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl MachineRosterStore {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn replace_active_machine(
        &self,
        state: &ActiveMachineState,
    ) -> Result<(), MachineRosterStoreError> {
        let _guard = self.lock.lock().await;
        let mut evidence = read_evidence(&self.path)?;
        evidence
            .active_machines
            .retain(|active| active.machine_id != state.machine_id);
        evidence.active_machines.push(state.clone());
        evidence
            .active_machines
            .sort_by(|left, right| left.machine_id.as_str().cmp(right.machine_id.as_str()));
        write_evidence(&self.path, &evidence)
    }

    pub fn active_machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<ActiveMachineState>, MachineRosterStoreError> {
        Ok(read_evidence(&self.path)?
            .active_machines
            .into_iter()
            .find(|active| &active.machine_id == machine_id))
    }

    pub fn active_machines(&self) -> Result<Vec<ActiveMachineState>, MachineRosterStoreError> {
        Ok(read_evidence(&self.path)?.active_machines)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineRosterEvidence {
    pub active_machines: Vec<ActiveMachineState>,
}

#[derive(Debug)]
pub enum MachineRosterStoreError {
    Read { message: String },
    Decode { message: String },
    Encode { message: String },
    Write { message: String },
    Commit { message: String },
}

impl std::fmt::Display for MachineRosterStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { message } => write!(formatter, "read machine roster evidence: {message}"),
            Self::Decode { message } => {
                write!(formatter, "decode machine roster evidence: {message}")
            }
            Self::Encode { message } => {
                write!(formatter, "encode machine roster evidence: {message}")
            }
            Self::Write { message } => {
                write!(formatter, "write machine roster evidence: {message}")
            }
            Self::Commit { message } => {
                write!(formatter, "commit machine roster evidence: {message}")
            }
        }
    }
}

impl std::error::Error for MachineRosterStoreError {}

fn read_evidence(path: &Path) -> Result<MachineRosterEvidence, MachineRosterStoreError> {
    let payload = match std::fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MachineRosterEvidence::default());
        }
        Err(error) => {
            return Err(MachineRosterStoreError::Read {
                message: error.to_string(),
            });
        }
    };
    serde_json::from_slice(&payload).map_err(|error| MachineRosterStoreError::Decode {
        message: error.to_string(),
    })
}

fn write_evidence(
    path: &Path,
    evidence: &MachineRosterEvidence,
) -> Result<(), MachineRosterStoreError> {
    let payload =
        serde_json::to_vec_pretty(evidence).map_err(|error| MachineRosterStoreError::Encode {
            message: error.to_string(),
        })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| MachineRosterStoreError::Write {
            message: error.to_string(),
        })?;
    }
    let temp_path = path.with_extension("tmp");
    let mut file =
        std::fs::File::create(&temp_path).map_err(|error| MachineRosterStoreError::Write {
            message: error.to_string(),
        })?;
    file.write_all(&payload)
        .and_then(|()| file.sync_all())
        .map_err(|error| MachineRosterStoreError::Write {
            message: error.to_string(),
        })?;
    std::fs::rename(&temp_path, path).map_err(|error| MachineRosterStoreError::Commit {
        message: error.to_string(),
    })
}
