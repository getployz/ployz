//! Machine-local mirror of core intent.
//!
//! A Reachable Machine persists the latest [`IntentSnapshot`] off the drumbeat
//! so a future promotion (ADR 0031) can seed a new core from it without a backup
//! restore. Writes are epoch-gated: a snapshot from a lower Control-Plane Epoch
//! — a healed, stale core — never overwrites a higher one.

use ployz_core::install::INTENT_MIRROR_FILE_NAME;
use ployz_core::state::{ControlPlaneEpoch, IntentSnapshot, PendingMachineJoinRecoverySnapshot};
use std::io;
use std::path::{Path, PathBuf};

/// How the epoch-gated mirror handled a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// The snapshot advanced the mirror beyond its previously stored epoch
    /// (or beyond the initial epoch when nothing was stored yet).
    AcceptedHigher,
    /// The snapshot matched the mirror's epoch: a same-epoch drumbeat.
    AcceptedCurrent,
    /// The snapshot's epoch is behind the mirror — a healed, stale core.
    /// Nothing was written.
    RejectedStale,
}

#[derive(Debug, Clone)]
pub struct MachineIntentMirror {
    path: PathBuf,
}

impl MachineIntentMirror {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The canonical mirror location: `intent-mirror.json` beside the
    /// machine's NKey seed file. Every role and Host Runner derives the path this
    /// way so promotion always finds the mirror the roles wrote.
    #[must_use]
    pub fn beside_seed_file(seed_file: &Path) -> Self {
        Self::new(seed_file.with_file_name(INTENT_MIRROR_FILE_NAME))
    }

    /// The mirrored snapshot, or `None` if none is persisted yet or the file is
    /// unreadable/corrupt — a corrupt mirror is repaired by the next drumbeat,
    /// never trusted.
    #[must_use]
    pub fn load(&self) -> Option<IntentSnapshot> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Persist `snapshot` unless a higher-epoch one is already stored; a stale
    /// core's lower epoch is dropped without a write.
    pub fn store(&self, snapshot: &IntentSnapshot) -> io::Result<StoreOutcome> {
        let stored_epoch = self
            .load()
            .map_or_else(ControlPlaneEpoch::initial, |current| current.epoch);
        if snapshot.epoch < stored_epoch {
            return Ok(StoreOutcome::RejectedStale);
        }
        let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
        write_atomic(&self.path, &bytes)?;
        Ok(if snapshot.epoch > stored_epoch {
            StoreOutcome::AcceptedHigher
        } else {
            StoreOutcome::AcceptedCurrent
        })
    }
}

#[derive(Debug, Clone)]
pub struct MachinePendingJoinMirror {
    path: PathBuf,
}

impl MachinePendingJoinMirror {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn load(&self) -> Option<PendingMachineJoinRecoverySnapshot> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn store(&self, snapshot: &PendingMachineJoinRecoverySnapshot) -> io::Result<bool> {
        if let Some(current) = self.load()
            && snapshot.epoch < current.epoch
        {
            return Ok(false);
        }
        let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
        write_atomic(&self.path, &bytes)?;
        Ok(true)
    }
}

/// Write via a uniquely named temp file + rename so a crash mid-write cannot
/// leave a truncated mirror behind, and concurrent writers (the machine,
/// gateway, and DNS processes all mirror the same file) cannot expose each
/// other's partial writes through a shared temp path.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::adapters::atomic_file::write_file_atomically(path, bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(epoch: u64) -> IntentSnapshot {
        serde_json::from_value(serde_json::json!({
            "epoch": epoch,
            "core_machine_id": "machine_a",
            "active_machines": [],
            "dataplane_projection": { "declared_members": [], "staged_member": null },
            "route_bindings": [],
            "serving_target_entries": [],
            "nats_authorizations": [],
            "custom_certificates": [],
            "acme_http01_challenges": [],
        }))
        .expect("snapshot deserializes")
    }

    #[test]
    fn stores_and_reloads_the_snapshot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        assert!(mirror.load().is_none());
        // A first snapshot at the initial epoch is a same-epoch drumbeat.
        assert_eq!(
            mirror.store(&snapshot(1)).expect("store"),
            StoreOutcome::AcceptedCurrent
        );
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 1);
    }

    #[test]
    fn rejects_a_lower_epoch_and_accepts_equal_or_higher() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        // A fresh mirror's baseline is the initial epoch, so epoch 2 is higher.
        assert_eq!(
            mirror.store(&snapshot(2)).expect("store 2"),
            StoreOutcome::AcceptedHigher
        );
        // A healed, stale core advertising a lower epoch is dropped.
        assert_eq!(
            mirror.store(&snapshot(1)).expect("reject 1"),
            StoreOutcome::RejectedStale
        );
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 2);
        // The same epoch re-stores (a drumbeat refresh), higher advances.
        assert_eq!(
            mirror.store(&snapshot(2)).expect("store 2 again"),
            StoreOutcome::AcceptedCurrent
        );
        assert_eq!(
            mirror.store(&snapshot(3)).expect("store 3"),
            StoreOutcome::AcceptedHigher
        );
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 3);
    }
}
