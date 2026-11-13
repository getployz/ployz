//! Where this host keeps the check-in's files, and the lock that keeps one loop running.

use std::{
    fs, io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use ployz_core::MachineLayout;
use serde::Deserialize;

pub(crate) struct Paths {
    pub(crate) checkin_dir: PathBuf,
    pub(crate) prepare_state: PathBuf,
    /// The lock ployzd holds while it prepares this host.
    pub(crate) prepare_lock: PathBuf,
    pub(crate) token_file: PathBuf,
}

impl Paths {
    /// This host's check-in files, reading the Fleet Token from `token_file`.
    pub(crate) fn system(token_file: PathBuf) -> Self {
        let layout = MachineLayout::production();
        Self {
            checkin_dir: layout.checkin_dir(),
            prepare_state: layout.prepare_state(),
            prepare_lock: layout.prepare_lock(),
            token_file,
        }
    }

    /// This host's check-in files, reading the Fleet Token from where `ployz server checkin`
    /// defaults to.
    pub(crate) fn production() -> Self {
        Self::system(MachineLayout::production().fleet_token())
    }

    pub(crate) fn key(&self) -> PathBuf {
        self.checkin_dir.join("key")
    }

    pub(crate) fn status(&self) -> PathBuf {
        self.checkin_dir.join("status.json")
    }

    pub(super) fn probe(&self) -> PathBuf {
        self.checkin_dir.join("probe.json")
    }

    pub(crate) fn loop_lock(&self) -> PathBuf {
        self.checkin_dir.join(LOOP_LOCK)
    }
}

const LOOP_LOCK: &str = "loop.lock";
/// Only root may lock it: a lock any user could take would let them hold the loop back from starting.
const LOOP_LOCK_MODE: u32 = 0o600;

/// Held for as long as the check-in loop runs, so `status` can tell a record no loop updates any more.
pub(crate) struct LoopRunning {
    _lock: fs::File,
    checkin_dir: PathBuf,
}

/// The check-in directories whose lock this process holds, so the records it writes there say so.
static HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

pub(super) fn holds(checkin_dir: &Path) -> bool {
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|held| held == checkin_dir)
}

impl Drop for LoopRunning {
    fn drop(&mut self) {
        HELD.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|held| held != &self.checkin_dir);
    }
}

/// The loop's lock, or none when another loop already holds it.
pub(crate) fn hold_loop(paths: &Paths) -> io::Result<Option<LoopRunning>> {
    fs::create_dir_all(&paths.checkin_dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(LOOP_LOCK_MODE)
        .open(paths.loop_lock())?;
    // A lock an earlier release made readable by all is narrowed too.
    lock.set_permissions(fs::Permissions::from_mode(LOOP_LOCK_MODE))?;
    match lock.try_lock() {
        Ok(()) => {
            HELD.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(paths.checkin_dir.clone());
            Ok(Some(LoopRunning {
                _lock: lock,
                checkin_dir: paths.checkin_dir.clone(),
            }))
        }
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Whether the loop that wrote this host's record held the lock, and none holds it now. A record
/// from a loop older than the lock, or one that couldn't take it, claims nothing; so does a lock
/// a user can't open (anyone but root).
pub(crate) fn loop_stopped(checkin_dir: &Path) -> bool {
    #[derive(Deserialize)]
    struct Writer {
        #[serde(default)]
        locked: bool,
    }
    let locked = fs::read(checkin_dir.join("status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Writer>(&bytes).ok())
        .is_some_and(|writer| writer.locked);
    locked
        && fs::File::open(checkin_dir.join(LOOP_LOCK))
            .is_ok_and(|lock| lock.try_lock_shared().is_ok())
}
