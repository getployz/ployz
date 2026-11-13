//! The Servers this computer pinned: each Machine's ID, WireGuard key and subnet as this
//! Device learned them itself, never on Cloud's word. A join this Device approved records
//! its joiner, and `ployz device enforce` records a Server read over SSH or confirmed by
//! fingerprint. Activation signs a Member only as pinned here.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use ployz_core::{MachineId, authority::Member};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum KnownError {
    #[error("couldn't read {path}")]
    Read { path: PathBuf, source: io::Error },
    #[error("couldn't write {path}")]
    Write { path: PathBuf, source: io::Error },
    #[error("{path} isn't a list of known Servers. Delete it, then run ployz device enforce again")]
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl From<KnownError> for crate::failure::Failure {
    fn from(error: KnownError) -> Self {
        Self::command(error)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct KnownFile {
    #[serde(default)]
    machines: BTreeMap<MachineId, Member>,
}

/// This computer's known-Machines file, beside its Device key.
pub(crate) struct KnownMachines {
    path: PathBuf,
}

impl KnownMachines {
    pub(crate) fn beside(config: &Path) -> Self {
        Self {
            path: crate::config_dir::ConfigDir::new(config).known_machines(),
        }
    }

    /// Every pinned Member, by Machine.
    ///
    /// # Errors
    /// Fails when the file can't be read or isn't a known-Machines file.
    pub(crate) fn load(&self) -> Result<BTreeMap<MachineId, Member>, KnownError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<KnownFile>(&bytes)
                .map(|file| file.machines)
                .map_err(|source| KnownError::Malformed {
                    path: self.path.clone(),
                    source,
                }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(source) => Err(KnownError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Pin `member`, replacing what this computer had for its Machine.
    ///
    /// # Errors
    /// Fails when the file can't be read or written.
    pub(crate) fn record(&self, member: Member) -> Result<(), KnownError> {
        let mut machines = self.load()?;
        machines.insert(member.id, member);
        let write = |source| KnownError::Write {
            path: self.path.clone(),
            source,
        };
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(write)?;
        let mut temporary = tempfile::NamedTempFile::new_in(dir).map_err(write)?;
        serde_json::to_writer_pretty(&mut temporary, &KnownFile { machines })
            .map_err(|error| write(io::Error::other(error)))?;
        temporary.as_file().sync_all().map_err(write)?;
        temporary
            .persist(&self.path)
            .map_err(|error| write(error.error))?;
        Ok(())
    }
}
