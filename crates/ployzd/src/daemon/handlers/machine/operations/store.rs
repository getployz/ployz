use std::path::PathBuf;

use ployz_operation_store::FileOperationStore;
use ployz_time::now_unix_secs;

use super::types::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationRecord, MachineOperationState,
    MachineOperationStatus, MachineOperationTransition,
};

const OPERATIONS_DIR_NAME: &str = "machine-operations";

#[derive(Debug, Clone)]
pub(in crate::daemon::handlers::machine) struct MachineOperationStore {
    files: FileOperationStore,
}

impl MachineOperationStore {
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn new(root: PathBuf) -> Self {
        Self {
            files: FileOperationStore::new(root, OPERATIONS_DIR_NAME, "machine operation"),
        }
    }

    pub(in crate::daemon::handlers::machine) fn begin(
        &self,
        kind: MachineOperationKind,
        network_name: Option<String>,
        targets: Vec<String>,
        stage: impl Into<String>,
        artifacts: MachineOperationArtifacts,
    ) -> Result<MachineOperationRecord, String> {
        let now = now_unix_secs();
        let record = MachineOperationRecord {
            id: unique_operation_id(kind, now),
            kind,
            network_name,
            targets,
            stage: stage.into(),
            started_at: now,
            updated_at: now,
            state: MachineOperationState::Running { last_error: None },
            artifacts,
        };
        self.save(&record)?;
        Ok(record)
    }

    pub(in crate::daemon::handlers::machine) fn begin_with_id(
        &self,
        id: String,
        kind: MachineOperationKind,
        network_name: Option<String>,
        targets: Vec<String>,
        stage: impl Into<String>,
        artifacts: MachineOperationArtifacts,
    ) -> Result<MachineOperationRecord, String> {
        validate_operation_id(&id)?;
        let now = now_unix_secs();
        let record = MachineOperationRecord {
            id,
            kind,
            network_name,
            targets,
            stage: stage.into(),
            started_at: now,
            updated_at: now,
            state: MachineOperationState::Running { last_error: None },
            artifacts,
        };
        self.save(&record)?;
        Ok(record)
    }

    pub(in crate::daemon::handlers::machine) fn update_stage(
        &self,
        record: &mut MachineOperationRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub(in crate::daemon::handlers::machine) fn update_status(
        &self,
        record: &mut MachineOperationRecord,
        status: MachineOperationStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        let at_unix_secs = now_unix_secs();
        let transition = match status {
            MachineOperationStatus::Running => {
                MachineOperationTransition::running(last_error, at_unix_secs)
            }
            MachineOperationStatus::Succeeded => MachineOperationTransition::succeed(at_unix_secs),
            MachineOperationStatus::Failed => {
                MachineOperationTransition::fail(last_error.unwrap_or_default(), at_unix_secs)
            }
            MachineOperationStatus::Interrupted => {
                MachineOperationTransition::interrupt(last_error, at_unix_secs)
            }
        };
        record.apply_transition(transition);
        self.save(record)
    }

    pub(in crate::daemon::handlers::machine) fn save(
        &self,
        record: &MachineOperationRecord,
    ) -> Result<(), String> {
        self.files.save(&record.id, record)
    }

    pub(in crate::daemon::handlers::machine) fn load(
        &self,
        id: &str,
    ) -> Result<Option<MachineOperationRecord>, String> {
        self.files.load(id)
    }

    pub(in crate::daemon::handlers::machine) fn list(
        &self,
    ) -> Result<Vec<MachineOperationRecord>, String> {
        self.files.list(
            |record: &MachineOperationRecord| record.started_at,
            |record| &record.id,
        )
    }
}

#[must_use]
pub(super) fn unique_operation_id(kind: MachineOperationKind, now: u64) -> String {
    ployz_operation_store::unique_operation_id("machine", kind.as_str(), now)
}

#[cfg(test)]
pub(super) const MAX_OPERATION_ID_LEN: usize = ployz_operation_store::MAX_OPERATION_ID_LEN;

pub(super) fn validate_operation_id(id: &str) -> Result<(), String> {
    ployz_operation_store::validate_operation_id("machine operation", id)
}
