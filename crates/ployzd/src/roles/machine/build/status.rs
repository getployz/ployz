use crate::adapters::atomic_file::write_secret_file_atomically;
use ployz_core::build::{BuildExecutorState, BuildExecutorStatus};
use ployz_core::ids::OperationId;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const RECORD_VERSION: u8 = 1;
const RUNNING_DIRECTORY: &str = "running";
const TERMINAL_DIRECTORY: &str = "terminal";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildStatusRecord {
    version: u8,
    pub(super) status: BuildExecutorStatus,
}

impl BuildStatusRecord {
    pub(super) fn new(status: BuildExecutorStatus) -> Self {
        Self {
            version: RECORD_VERSION,
            status,
        }
    }

    fn validate(&self, key: &OperationId) -> Result<(), String> {
        if self.version != RECORD_VERSION {
            return Err(format!("unsupported build status version {}", self.version));
        }
        if self.status.acceptance.operation_id != *key {
            return Err("build status evidence identity does not match its file key".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct BuildStatusRepository {
    root: PathBuf,
    writes: Arc<Mutex<()>>,
}

impl BuildStatusRepository {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            writes: Arc::new(Mutex::new(())),
        }
    }

    pub(super) fn read(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<BuildStatusRecord>, String> {
        if let Some(record) = read_record(&self.terminal_path(operation_id), operation_id)? {
            return Ok(Some(record));
        }
        read_record(&self.running_path(operation_id), operation_id)
    }

    pub(super) fn write(&self, record: &BuildStatusRecord) -> Result<(), String> {
        let _write = self
            .writes
            .lock()
            .map_err(|_| "build status write lock is poisoned".to_owned())?;
        ensure_private_directory(&self.root)?;
        let operation_id = &record.status.acceptance.operation_id;
        record.validate(operation_id)?;
        if let Some(existing) = self.read(operation_id)? {
            validate_transition(&existing, record)?;
            if existing.status == record.status {
                if record.status.is_terminal() {
                    remove_running_if_present(&self.running_path(operation_id))?;
                }
                return Ok(());
            }
        }
        let bytes = serde_json::to_vec(record)
            .map_err(|error| format!("encode build status evidence: {error}"))?;
        if record.status.is_terminal() {
            write_secret_file_atomically(&self.terminal_path(operation_id), &bytes)
                .map_err(|error| error.to_string())?;
            remove_running_if_present(&self.running_path(operation_id))?;
        } else {
            write_secret_file_atomically(&self.running_path(operation_id), &bytes)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Startup recovery is bounded by active builds; terminal history is read
    /// only by an exact operation lookup.
    pub(super) fn records(&self) -> Result<Vec<BuildStatusRecord>, String> {
        let root = self.root.join(RUNNING_DIRECTORY);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("read {}: {error}", root.display())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let Ok(operation_id) = OperationId::try_new(stem.to_owned()) else {
                continue;
            };
            let Ok(Some(record)) = self.read(&operation_id) else {
                continue;
            };
            if !record.status.is_terminal() {
                records.push(record);
            }
        }
        Ok(records)
    }

    fn running_path(&self, operation_id: &OperationId) -> PathBuf {
        self.root
            .join(RUNNING_DIRECTORY)
            .join(format!("{}.json", operation_id.as_str()))
    }

    fn terminal_path(&self, operation_id: &OperationId) -> PathBuf {
        self.root
            .join(TERMINAL_DIRECTORY)
            .join(format!("{}.json", operation_id.as_str()))
    }
}

fn ensure_private_directory(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| format!("create {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("restrict {}: {error}", path.display()))?;
    }
    Ok(())
}

fn read_record(
    path: &PathBuf,
    operation_id: &OperationId,
) -> Result<Option<BuildStatusRecord>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let record: BuildStatusRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decode {}: {error}", path.display()))?;
    record.validate(operation_id)?;
    Ok(Some(record))
}

fn remove_running_if_present(path: &PathBuf) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

fn validate_transition(
    existing: &BuildStatusRecord,
    next: &BuildStatusRecord,
) -> Result<(), String> {
    if existing.status.acceptance != next.status.acceptance {
        return Err("build status transition changed its accepted identity".to_owned());
    }
    match (&existing.status, &next.status) {
        (
            BuildExecutorStatus {
                state:
                    BuildExecutorState::Running {
                        log_summary: previous,
                    },
                ..
            },
            BuildExecutorStatus {
                state:
                    BuildExecutorState::Running {
                        log_summary: observed,
                    },
                ..
            },
        ) if dominates(*observed, *previous) => Ok(()),
        (
            BuildExecutorStatus {
                state:
                    BuildExecutorState::Running {
                        log_summary: previous,
                    },
                ..
            },
            terminal,
        ) if terminal.is_terminal() && dominates(terminal.log_summary(), *previous) => Ok(()),
        (terminal, candidate) if terminal == candidate => Ok(()),
        _ => Err(
            "build status transition regressed progress or changed terminal evidence".to_owned(),
        ),
    }
}

fn dominates(
    observed: ployz_core::build::BuildLogSummary,
    previous: ployz_core::build::BuildLogSummary,
) -> bool {
    observed == previous || observed.strictly_advances(&previous)
}

#[cfg(test)]
pub(super) fn raw_path(root: &Path, operation_id: &OperationId) -> PathBuf {
    root.join(RUNNING_DIRECTORY)
        .join(format!("{}.json", operation_id.as_str()))
}

#[cfg(test)]
pub(super) fn terminal_raw_path(root: &Path, operation_id: &OperationId) -> PathBuf {
    root.join(TERMINAL_DIRECTORY)
        .join(format!("{}.json", operation_id.as_str()))
}
