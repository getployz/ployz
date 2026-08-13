use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use ployz_core::{LocalMachinePhase, MachineId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_DATA_DIR: &str = "/var/lib/ployz";
const STATE_FILE_NAME: &str = "machine.json";
const TEMPORARY_FILE_NAME: &str = ".machine.json.tmp";
const LOCK_FILE_NAME: &str = ".ployzd.lock";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalMachineRecord {
    pub id: MachineId,
    pub phase: LocalMachinePhase,
}

pub struct LocalMachineStore {
    data_dir: PathBuf,
    record: LocalMachineRecord,
    _lock: File,
}

impl LocalMachineStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_owned();
        validate_data_dir(&data_dir)?;
        claim_data_dir(&data_dir)?;
        fs::create_dir_all(&data_dir)?;
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o711))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(data_dir.join(LOCK_FILE_NAME))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                StoreError::AlreadyRunning(data_dir.clone())
            } else {
                StoreError::Io(error)
            }
        })?;
        let path = data_dir.join(STATE_FILE_NAME);
        let record = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::remove_file(data_dir.join(TEMPORARY_FILE_NAME)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                let record = LocalMachineRecord {
                    id: MachineId::random(),
                    phase: LocalMachinePhase::Uninitialized,
                };
                save(&data_dir, &record)?;
                record
            }
            Err(error) => return Err(error.into()),
        };

        if matches!(record.phase, LocalMachinePhase::Unrecognized(_)) {
            return Err(StoreError::InvalidPhase);
        }

        let store = Self {
            data_dir,
            record,
            _lock: lock,
        };
        if store.record.phase == LocalMachinePhase::Resetting {
            let data_dir = store.data_dir.clone();
            store.complete_reset()?;
            drop(store);
            return Self::open(data_dir);
        }
        Ok(store)
    }

    #[must_use]
    pub fn record(&self) -> &LocalMachineRecord {
        &self.record
    }

    pub fn begin_reset(&mut self) -> Result<(), StoreError> {
        if self.record.phase == LocalMachinePhase::Resetting {
            return Err(StoreError::AlreadyResetting);
        }
        let mut resetting = self.record.clone();
        resetting.phase = LocalMachinePhase::Resetting;
        save(&self.data_dir, &resetting)?;
        self.record = resetting;
        Ok(())
    }

    pub fn complete_reset(&self) -> Result<(), StoreError> {
        if self.record.phase != LocalMachinePhase::Resetting {
            return Err(StoreError::NotResetting);
        }
        let persisted: LocalMachineRecord =
            serde_json::from_slice(&fs::read(self.data_dir.join(STATE_FILE_NAME))?)?;
        if persisted != self.record {
            return Err(StoreError::OwnershipLost(self.data_dir.clone()));
        }
        fs::remove_dir_all(&self.data_dir).map_err(StoreError::Io)
    }
}

fn validate_data_dir(path: &Path) -> Result<(), StoreError> {
    if path.file_name().is_none()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(StoreError::UnsafeDataDirectory(path.to_owned()));
    }
    Ok(())
}

fn claim_data_dir(data_dir: &Path) -> Result<(), StoreError> {
    match fs::read_dir(data_dir) {
        Ok(entries) => {
            let names = entries
                .map(|entry| Ok(entry?.file_name()))
                .collect::<io::Result<Vec<_>>>()?;
            let owns_directory = names.iter().any(|name| name == STATE_FILE_NAME)
                || names
                    .iter()
                    .all(|name| name == TEMPORARY_FILE_NAME || name == LOCK_FILE_NAME);
            if owns_directory {
                Ok(())
            } else {
                Err(StoreError::UnownedDataDirectory(data_dir.to_owned()))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn save(data_dir: &Path, record: &LocalMachineRecord) -> Result<(), StoreError> {
    fs::create_dir_all(data_dir)?;
    fs::set_permissions(data_dir, fs::Permissions::from_mode(0o711))?;

    let path = data_dir.join(STATE_FILE_NAME);
    let temporary = data_dir.join(TEMPORARY_FILE_NAME);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, record)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(data_dir)?.sync_all()?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("local Machine record I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("local Machine record JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local Machine record contains an unrecognized phase")]
    InvalidPhase,
    #[error("machine is already resetting")]
    AlreadyResetting,
    #[error("machine is not resetting")]
    NotResetting,
    #[error("another daemon already owns data directory {0:?}")]
    AlreadyRunning(PathBuf),
    #[error("refusing to clear broad data directory {0:?}")]
    UnsafeDataDirectory(PathBuf),
    #[error("refusing to claim nonempty data directory {0:?}")]
    UnownedDataDirectory(PathBuf),
    #[error("local Machine record changed before clearing data directory {0:?}")]
    OwnershipLost(PathBuf),
}
