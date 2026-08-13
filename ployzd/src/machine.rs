use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use ployz_core::{LocalMachinePhase, MachineId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_DATA_DIR: &str = "/var/lib/ployz";
const STATE_FILE_NAME: &str = "machine.json";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalMachineState {
    pub id: MachineId,
    pub phase: LocalMachinePhase,
}

pub struct StateStore {
    data_dir: PathBuf,
    local_state: LocalMachineState,
}

impl StateStore {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StateError> {
        let data_dir = data_dir.as_ref().to_owned();
        validate_data_dir(&data_dir)?;
        let path = data_dir.join(STATE_FILE_NAME);
        let state = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let state = LocalMachineState {
                    id: MachineId::random(),
                    phase: LocalMachinePhase::Uninitialized,
                };
                save(&data_dir, &state)?;
                state
            }
            Err(error) => return Err(error.into()),
        };

        if matches!(state.phase, LocalMachinePhase::Unrecognized(_)) {
            return Err(StateError::InvalidPhase);
        }

        let store = Self {
            data_dir,
            local_state: state,
        };
        if store.local_state.phase == LocalMachinePhase::Resetting {
            store.complete_reset()?;
            return Self::open(&store.data_dir);
        }
        Ok(store)
    }

    #[must_use]
    pub fn local_state(&self) -> &LocalMachineState {
        &self.local_state
    }

    pub fn begin_reset(&mut self) -> Result<(), StateError> {
        if self.local_state.phase == LocalMachinePhase::Resetting {
            return Err(StateError::AlreadyResetting);
        }
        let mut resetting = self.local_state.clone();
        resetting.phase = LocalMachinePhase::Resetting;
        save(&self.data_dir, &resetting)?;
        self.local_state = resetting;
        Ok(())
    }

    pub fn complete_reset(&self) -> Result<(), StateError> {
        if self.local_state.phase != LocalMachinePhase::Resetting {
            return Err(StateError::NotResetting);
        }
        fs::remove_dir_all(&self.data_dir).map_err(StateError::Io)
    }
}

fn validate_data_dir(path: &Path) -> Result<(), StateError> {
    if path.file_name().is_none()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(StateError::UnsafeDataDirectory(path.to_owned()));
    }
    Ok(())
}

fn save(data_dir: &Path, state: &LocalMachineState) -> Result<(), StateError> {
    fs::create_dir_all(data_dir)?;
    fs::set_permissions(data_dir, fs::Permissions::from_mode(0o711))?;

    let path = data_dir.join(STATE_FILE_NAME);
    let temporary = data_dir.join(".machine.json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, state)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("machine state I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("machine state JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("machine state contains an unrecognized local phase")]
    InvalidPhase,
    #[error("machine is already resetting")]
    AlreadyResetting,
    #[error("machine is not resetting")]
    NotResetting,
    #[error("refusing to clear broad data directory {0:?}")]
    UnsafeDataDirectory(PathBuf),
}
