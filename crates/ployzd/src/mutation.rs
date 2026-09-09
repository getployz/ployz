//! Cross-process exclusion between Machine installation and local mutation.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};

use thiserror::Error;

const LOCK_FILE: &str = ".install.lock";
const ACTIVE_FILE: &str = ".upgrade-active";

/// Shared Machine mutation policy backed by an OS lock and durable upgrade evidence.
#[derive(Clone, Debug)]
pub struct MutationGate {
    lock_dir: PathBuf,
    data_dir: PathBuf,
}

/// A refusal or local I/O failure while claiming Machine mutation ownership.
#[derive(Debug, Error)]
pub enum Error {
    /// Another mutation, installation, or durable upgrade attempt owns the Machine.
    #[error("a Ployz installation or upgrade is active")]
    Busy,
    /// Local ownership evidence could not be inspected or changed.
    #[error("Machine mutation admission: {0}")]
    Io(#[from] io::Error),
}

/// Shared ownership that excludes Machine installation while a mutation finishes.
pub struct MutationGuard(File);

/// Exclusive ownership that excludes every other Machine installation or mutation.
pub struct InstallationGuard(File);

impl MutationGate {
    /// Bind one gate to its runtime lock and durable data directories.
    #[must_use]
    pub fn new(lock_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            lock_dir: lock_dir.into(),
            data_dir: data_dir.into(),
        }
    }

    /// Claim shared mutation ownership when no installation evidence exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] when an installation lock or any durable active-marker entry
    /// exists. Returns [`Error::Io`] when either ownership source cannot be inspected.
    pub fn try_mutation(&self) -> Result<MutationGuard, Error> {
        let file = self.open_lock()?;
        file.try_lock_shared().map_err(lock_error)?;
        match fs::symlink_metadata(self.active_path()) {
            Ok(_) => {
                let _ = file.unlock();
                Err(Error::Busy)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(MutationGuard(file)),
            Err(error) => {
                let _ = file.unlock();
                Err(Error::Io(error))
            }
        }
    }

    /// Try to claim exclusive installation ownership without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] when another owner holds the lock and [`Error::Io`] when the
    /// lock directory or file cannot be prepared.
    pub fn try_installation(&self) -> Result<InstallationGuard, Error> {
        let file = self.open_lock()?;
        file.try_lock().map_err(lock_error)?;
        Ok(InstallationGuard(file))
    }

    /// Wait for exclusive installation ownership.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the lock directory or file cannot be prepared or locked.
    pub fn lock_installation(&self) -> Result<InstallationGuard, Error> {
        let file = self.open_lock()?;
        file.lock()?;
        Ok(InstallationGuard(file))
    }

    /// Whether any filesystem entry exists at the durable active-marker path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when absence cannot be established from filesystem metadata.
    pub fn active(&self) -> Result<bool, Error> {
        match fs::symlink_metadata(self.active_path()) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(Error::Io(error)),
        }
    }

    pub(crate) fn mark_active(&self, attempt_id: &str) -> Result<(), Error> {
        fs::create_dir_all(&self.data_dir)?;
        crate::filesystem::atomic_write(&self.active_path(), attempt_id.as_bytes(), 0o600)?;
        Ok(())
    }

    pub(crate) fn clear_active(&self, attempt_id: &str) -> Result<(), Error> {
        let path = self.active_path();
        match fs::read_to_string(&path) {
            Ok(active) if active == attempt_id => fs::remove_file(path)?,
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        File::open(&self.data_dir)?.sync_all()?;
        Ok(())
    }

    fn open_lock(&self) -> Result<File, Error> {
        fs::create_dir_all(&self.lock_dir)?;
        fs::set_permissions(&self.lock_dir, fs::Permissions::from_mode(0o750))?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.lock_dir.join(LOCK_FILE))
            .map_err(Into::into)
    }

    fn active_path(&self) -> PathBuf {
        self.data_dir.join(ACTIVE_FILE)
    }
}

fn lock_error(error: TryLockError) -> Error {
    match error {
        TryLockError::WouldBlock => Error::Busy,
        TryLockError::Error(error) => Error::Io(error),
    }
}

impl Drop for MutationGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Drop for InstallationGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_and_active_handoff_exclude_mutations_across_instances() {
        let root = crate::test_dir::TestDir::new("mutation-gate");
        let first = MutationGate::new(root.0.join("run"), root.0.join("data"));
        let second = MutationGate::new(root.0.join("run"), root.0.join("data"));

        let install = first.try_installation().unwrap();
        assert!(matches!(second.try_mutation(), Err(Error::Busy)));
        first.mark_active("attempt").unwrap();
        drop(install);
        assert!(matches!(second.try_mutation(), Err(Error::Busy)));

        let install = second.try_installation().unwrap();
        second.clear_active("attempt").unwrap();
        drop(install);
        assert!(second.try_mutation().is_ok());
    }

    #[test]
    fn every_existing_marker_type_fails_closed() {
        let root = crate::test_dir::TestDir::new("mutation-marker-type");
        let gate = MutationGate::new(root.0.join("run"), root.0.join("data"));
        fs::create_dir_all(gate.active_path()).unwrap();

        assert!(matches!(gate.try_mutation(), Err(Error::Busy)));
        assert!(gate.active().unwrap());
    }

    #[test]
    fn marker_metadata_errors_are_not_absence() {
        let root = crate::test_dir::TestDir::new("mutation-marker-error");
        fs::create_dir_all(&root.0).unwrap();
        let data = root.0.join("data-is-a-file");
        fs::write(&data, "not a directory").unwrap();
        let gate = MutationGate::new(root.0.join("run"), data);

        assert!(matches!(gate.try_mutation(), Err(Error::Io(_))));
        assert!(matches!(gate.active(), Err(Error::Io(_))));
    }
}
