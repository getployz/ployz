//! Core-local active-machine roster evidence.

use crate::evidence_file::{EvidenceFileError, read_json_or_default, write_json};
use ployz_core::ids::MachineId;
use ployz_core::state::ActiveMachineState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
        let mut evidence: MachineRosterEvidence = read_json_or_default(&self.path)?;
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
        Ok(read_json_or_default::<MachineRosterEvidence>(&self.path)?
            .active_machines
            .into_iter()
            .find(|active| &active.machine_id == machine_id))
    }

    pub fn active_machines(&self) -> Result<Vec<ActiveMachineState>, MachineRosterStoreError> {
        Ok(read_json_or_default::<MachineRosterEvidence>(&self.path)?.active_machines)
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
        }
    }
}

impl std::error::Error for MachineRosterStoreError {}

fn write_evidence(
    path: &std::path::Path,
    evidence: &MachineRosterEvidence,
) -> Result<(), MachineRosterStoreError> {
    write_json(path, evidence).map_err(MachineRosterStoreError::from)
}

impl From<EvidenceFileError> for MachineRosterStoreError {
    fn from(error: EvidenceFileError) -> Self {
        match error {
            EvidenceFileError::Read { message } => Self::Read { message },
            EvidenceFileError::Decode { message } => Self::Decode { message },
            EvidenceFileError::Encode { message } => Self::Encode { message },
            EvidenceFileError::Write { message } => Self::Write { message },
        }
    }
}
