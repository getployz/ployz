use std::path::PathBuf;
use std::time::Duration;

use ployz_model::MachineId;
use ployz_operation_store::{FileOperationStore, try_unique_operation_id, validate_operation_id};
use ployz_spec::Namespace;
use ployz_time::now_unix_secs;
use serde::{Deserialize, Serialize};

const TRANSFERS_DIR_NAME: &str = "zfs-transfers";
const MOVE_CLAIM_RECORD_WAIT: Duration = Duration::from_millis(20);
const MOVE_CLAIM_RECORD_WAIT_ATTEMPTS: usize = 5;

pub struct SendResult {
    pub bytes_transferred: u64,
    pub snapshot_guid: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferTransition {
    status: TransferStatus,
    last_error: Option<String>,
    at_unix_secs: u64,
}

impl TransferTransition {
    pub fn succeeded(at_unix_secs: u64) -> Self {
        Self {
            status: TransferStatus::Succeeded,
            last_error: None,
            at_unix_secs,
        }
    }

    pub fn failed(last_error: String, at_unix_secs: u64) -> Self {
        Self {
            status: TransferStatus::Failed,
            last_error: Some(last_error),
            at_unix_secs,
        }
    }

    pub fn interrupted(at_unix_secs: u64) -> Self {
        Self {
            status: TransferStatus::Interrupted,
            last_error: None,
            at_unix_secs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TransferState {
    Running {
        stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Succeeded {
        stage: String,
        snapshot_guid: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        bytes_transferred: u64,
    },
    Failed {
        stage: String,
        last_error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
    },
    Interrupted {
        stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
    },
}

impl TransferState {
    fn running(stage: impl Into<String>) -> Self {
        Self::Running {
            stage: stage.into(),
            snapshot_guid: None,
            from_snapshot_guid: None,
            bytes_transferred: None,
            last_error: None,
        }
    }

    pub fn status(&self) -> TransferStatus {
        match self {
            Self::Running { .. } => TransferStatus::Running,
            Self::Succeeded { .. } => TransferStatus::Succeeded,
            Self::Failed { .. } => TransferStatus::Failed,
            Self::Interrupted { .. } => TransferStatus::Interrupted,
        }
    }

    pub fn stage(&self) -> &str {
        match self {
            Self::Running { stage, .. }
            | Self::Succeeded { stage, .. }
            | Self::Failed { stage, .. }
            | Self::Interrupted { stage, .. } => stage,
        }
    }

    fn set_stage(&mut self, next_stage: String) {
        match self {
            Self::Running { stage, .. }
            | Self::Succeeded { stage, .. }
            | Self::Failed { stage, .. }
            | Self::Interrupted { stage, .. } => *stage = next_stage,
        }
    }

    pub fn snapshot_guid(&self) -> Option<u64> {
        match self {
            Self::Running { snapshot_guid, .. }
            | Self::Failed { snapshot_guid, .. }
            | Self::Interrupted { snapshot_guid, .. } => *snapshot_guid,
            Self::Succeeded { snapshot_guid, .. } => Some(*snapshot_guid),
        }
    }

    pub fn from_snapshot_guid(&self) -> Option<u64> {
        match self {
            Self::Running {
                from_snapshot_guid, ..
            }
            | Self::Succeeded {
                from_snapshot_guid, ..
            }
            | Self::Failed {
                from_snapshot_guid, ..
            }
            | Self::Interrupted {
                from_snapshot_guid, ..
            } => *from_snapshot_guid,
        }
    }

    fn bytes_transferred(&self) -> Option<u64> {
        match self {
            Self::Running {
                bytes_transferred, ..
            }
            | Self::Failed {
                bytes_transferred, ..
            }
            | Self::Interrupted {
                bytes_transferred, ..
            } => *bytes_transferred,
            Self::Succeeded {
                bytes_transferred, ..
            } => Some(*bytes_transferred),
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Running { last_error, .. } | Self::Interrupted { last_error, .. } => {
                last_error.as_deref()
            }
            Self::Failed { last_error, .. } => Some(last_error.as_str()),
            Self::Succeeded { .. } => None,
        }
    }

    pub fn with_snapshot_guid(&mut self, guid: u64) {
        match self {
            Self::Running { snapshot_guid, .. }
            | Self::Failed { snapshot_guid, .. }
            | Self::Interrupted { snapshot_guid, .. } => *snapshot_guid = Some(guid),
            Self::Succeeded { snapshot_guid, .. } => *snapshot_guid = guid,
        }
    }

    pub fn with_from_snapshot_guid(&mut self, guid: u64) {
        match self {
            Self::Running {
                from_snapshot_guid, ..
            }
            | Self::Succeeded {
                from_snapshot_guid, ..
            }
            | Self::Failed {
                from_snapshot_guid, ..
            }
            | Self::Interrupted {
                from_snapshot_guid, ..
            } => *from_snapshot_guid = Some(guid),
        }
    }

    pub fn with_bytes_transferred(&mut self, bytes: u64) {
        match self {
            Self::Running {
                bytes_transferred, ..
            }
            | Self::Failed {
                bytes_transferred, ..
            }
            | Self::Interrupted {
                bytes_transferred, ..
            } => *bytes_transferred = Some(bytes),
            Self::Succeeded {
                bytes_transferred, ..
            } => *bytes_transferred = bytes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRecord {
    pub id: String,
    pub namespace: String,
    pub volume: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub snapshot_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_name: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
    pub state: TransferState,
}

impl TransferRecord {
    pub fn status(&self) -> TransferStatus {
        self.state.status()
    }

    pub fn stage(&self) -> &str {
        self.state.stage()
    }

    pub fn snapshot_guid(&self) -> Option<u64> {
        self.state.snapshot_guid()
    }

    pub fn from_snapshot_guid(&self) -> Option<u64> {
        self.state.from_snapshot_guid()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.state.last_error()
    }

    pub fn apply_transition(&mut self, transition: TransferTransition) {
        let TransferTransition {
            status,
            last_error,
            at_unix_secs,
        } = transition;
        let stage = self.stage().to_string();
        let snapshot_guid = self.snapshot_guid();
        let from_snapshot_guid = self.from_snapshot_guid();
        let bytes_transferred = self.state.bytes_transferred();
        let current_error = self.last_error().map(str::to_string);
        self.state = match status {
            TransferStatus::Running => TransferState::Running {
                stage,
                snapshot_guid,
                from_snapshot_guid,
                bytes_transferred,
                last_error: last_error.or(current_error),
            },
            TransferStatus::Succeeded => TransferState::Succeeded {
                stage,
                snapshot_guid: snapshot_guid.unwrap_or_default(),
                from_snapshot_guid,
                bytes_transferred: bytes_transferred.unwrap_or_default(),
            },
            TransferStatus::Failed => TransferState::Failed {
                stage,
                last_error: last_error.unwrap_or_else(|| "zfs transfer failed".into()),
                snapshot_guid,
                from_snapshot_guid,
                bytes_transferred,
            },
            TransferStatus::Interrupted => TransferState::Interrupted {
                stage,
                last_error: last_error.or(current_error),
                snapshot_guid,
                from_snapshot_guid,
                bytes_transferred,
            },
        };
        self.updated_at = at_unix_secs;
    }
}

#[derive(Debug, Clone)]
pub struct TransferStore {
    records: FileOperationStore,
}

impl TransferStore {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self {
            records: FileOperationStore::new(root, TRANSFERS_DIR_NAME, "zfs transfer"),
        }
    }

    pub fn begin(
        &self,
        namespace: &Namespace,
        volume: &str,
        source_machine: MachineId,
        target_machine: MachineId,
        snapshot_name: String,
        from_snapshot_name: Option<String>,
    ) -> Result<TransferRecord, String> {
        let now = now_unix_secs();
        self.begin_with_id(
            unique_transfer_id(now)?,
            namespace,
            volume,
            source_machine,
            target_machine,
            snapshot_name,
            from_snapshot_name,
            now,
        )
    }

    pub fn begin_with_id(
        &self,
        id: String,
        namespace: &Namespace,
        volume: &str,
        source_machine: MachineId,
        target_machine: MachineId,
        snapshot_name: String,
        from_snapshot_name: Option<String>,
        now: u64,
    ) -> Result<TransferRecord, String> {
        let record = TransferRecord {
            id,
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            source_machine,
            target_machine,
            snapshot_name,
            from_snapshot_name,
            started_at: now,
            updated_at: now,
            state: TransferState::running("preflight"),
        };
        self.save(&record)?;
        Ok(record)
    }

    pub fn new_transfer_id(&self) -> Result<String, String> {
        unique_transfer_id(now_unix_secs())
    }

    pub fn find_reusable(
        &self,
        namespace: &Namespace,
        volume: &str,
        source_machine: &MachineId,
        target_machine: &MachineId,
        snapshot_name: &str,
        from_snapshot_name: Option<&str>,
    ) -> Result<Option<TransferRecord>, String> {
        Ok(self.list()?.into_iter().find(|record| {
            record.namespace == namespace.as_str()
                && record.volume == volume
                && record.source_machine == *source_machine
                && record.target_machine == *target_machine
                && record.snapshot_name == snapshot_name
                && record.from_snapshot_name.as_deref() == from_snapshot_name
                && record.status() == TransferStatus::Running
        }))
    }

    pub fn create_move_claim(
        &self,
        namespace: &Namespace,
        volume: &str,
        source_machine: &MachineId,
        target_machine: &MachineId,
        snapshot_name: &str,
        from_snapshot_name: Option<&str>,
        transfer_id: &str,
    ) -> Result<MoveClaimOutcome, String> {
        std::fs::create_dir_all(self.claim_dir()).map_err(|err| {
            format!(
                "create zfs transfer claim dir '{}': {err}",
                self.claim_dir().display()
            )
        })?;
        let key = move_claim_key(
            namespace,
            volume,
            source_machine,
            target_machine,
            snapshot_name,
            from_snapshot_name,
        );
        let path = self.claim_path(&key);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                if let Err(err) = writeln!(file, "{transfer_id}") {
                    if let Err(remove_err) = std::fs::remove_file(&path)
                        && remove_err.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(
                            %remove_err,
                            path = %path.display(),
                            "failed to delete partially written zfs transfer claim"
                        );
                    }
                    return Err(format!(
                        "write zfs transfer claim '{}': {err}",
                        path.display()
                    ));
                }
                Ok(MoveClaimOutcome::Created)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing_id = std::fs::read_to_string(&path)
                    .map_err(|err| format!("read zfs transfer claim '{}': {err}", path.display()))?
                    .trim()
                    .to_string();
                Ok(MoveClaimOutcome::Exists(existing_id))
            }
            Err(err) => Err(format!(
                "create zfs transfer claim '{}': {err}",
                path.display()
            )),
        }
    }

    pub fn delete_move_claim(
        &self,
        namespace: &Namespace,
        volume: &str,
        source_machine: &MachineId,
        target_machine: &MachineId,
        snapshot_name: &str,
        from_snapshot_name: Option<&str>,
    ) {
        let key = move_claim_key(
            namespace,
            volume,
            source_machine,
            target_machine,
            snapshot_name,
            from_snapshot_name,
        );
        self.delete_claim_path(&self.claim_path(&key), None);
    }

    pub fn update_stage(
        &self,
        record: &mut TransferRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.state.set_stage(stage.into());
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub fn update_status(
        &self,
        record: &mut TransferRecord,
        status: TransferStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        let at_unix_secs = now_unix_secs();
        let transition = match status {
            TransferStatus::Running => TransferTransition {
                status,
                last_error,
                at_unix_secs,
            },
            TransferStatus::Succeeded => TransferTransition::succeeded(at_unix_secs),
            TransferStatus::Failed => {
                TransferTransition::failed(last_error.unwrap_or_default(), at_unix_secs)
            }
            TransferStatus::Interrupted => TransferTransition::interrupted(at_unix_secs),
        };
        record.apply_transition(transition);
        self.save(record)
    }

    pub fn save(&self, record: &TransferRecord) -> Result<(), String> {
        self.records.save(&record.id, record)
    }

    pub fn load(&self, id: &str) -> Result<Option<TransferRecord>, String> {
        self.records.load(id)
    }

    pub fn list(&self) -> Result<Vec<TransferRecord>, String> {
        self.records.list(
            |record: &TransferRecord| record.started_at,
            |record| &record.id,
        )
    }

    pub fn recover_startup(&self) -> Result<usize, String> {
        let mut count = 0;
        for mut record in self.list()? {
            if record.status() == TransferStatus::Running {
                self.update_status(&mut record, TransferStatus::Interrupted, None)?;
                self.delete_claim_for(&record);
                count += 1;
            }
        }
        Ok(count)
    }

    fn dir(&self) -> PathBuf {
        self.records.dir()
    }

    pub fn claim_dir(&self) -> PathBuf {
        self.dir().join("claims")
    }

    pub fn claim_path(&self, key: &str) -> PathBuf {
        self.claim_dir().join(format!("{key}.claim"))
    }

    pub fn delete_claim_for(&self, record: &TransferRecord) {
        let key = move_claim_key(
            &Namespace::new(record.namespace.clone()),
            &record.volume,
            &record.source_machine,
            &record.target_machine,
            &record.snapshot_name,
            record.from_snapshot_name.as_deref(),
        );
        let path = self.claim_path(&key);
        self.delete_claim_path(&path, Some(&record.id));
    }

    fn delete_claim_path(&self, path: &std::path::Path, transfer_id: Option<&str>) {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                %error,
                transfer_id = transfer_id.unwrap_or("<unknown>"),
                path = %path.display(),
                "failed to delete zfs transfer claim"
            );
        }
    }
}

pub enum MoveClaimOutcome {
    Created,
    Exists(String),
}

pub enum ClaimedTransfer {
    Running(TransferRecord),
    Terminal(TransferRecord),
    MissingAfterWait,
}

pub async fn wait_for_claimed_transfer_record(
    store: &TransferStore,
    transfer_id: &str,
) -> Result<ClaimedTransfer, String> {
    for attempt in 0..MOVE_CLAIM_RECORD_WAIT_ATTEMPTS {
        match store.load(transfer_id)? {
            Some(record) if record.status() == TransferStatus::Running => {
                return Ok(ClaimedTransfer::Running(record));
            }
            Some(record) => return Ok(ClaimedTransfer::Terminal(record)),
            None if attempt + 1 < MOVE_CLAIM_RECORD_WAIT_ATTEMPTS => {
                tokio::time::sleep(MOVE_CLAIM_RECORD_WAIT).await;
            }
            None => return Ok(ClaimedTransfer::MissingAfterWait),
        }
    }

    Ok(ClaimedTransfer::MissingAfterWait)
}

pub fn move_claim_key(
    namespace: &Namespace,
    volume: &str,
    source_machine: &MachineId,
    target_machine: &MachineId,
    snapshot_name: &str,
    from_snapshot_name: Option<&str>,
) -> String {
    let raw = format!(
        "{}\n{volume}\n{}\n{}\n{snapshot_name}\n{}",
        namespace.as_str(),
        source_machine.as_str(),
        target_machine.as_str(),
        from_snapshot_name.unwrap_or("")
    );
    ployz_spec::stable_hash_hex(raw.as_bytes())
}

pub fn validate_transfer_id(id: &str) -> Result<(), String> {
    validate_operation_id("zfs transfer", id)
}

pub fn unique_transfer_id(now: u64) -> Result<String, String> {
    try_unique_operation_id("zfs-transfer", std::process::id(), now)
}
