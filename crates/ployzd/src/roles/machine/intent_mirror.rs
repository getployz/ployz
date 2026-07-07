//! Machine-local mirror of core intent.
//!
//! A Reachable Machine persists the latest [`IntentSnapshot`] off the drumbeat
//! so a future promotion (ADR 0031) can seed a new core from it without a backup
//! restore. Writes are epoch-gated: a snapshot from a lower Control-Plane Epoch
//! — a healed, stale core — never overwrites a higher one.

use ployz_core::state::IntentSnapshot;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct MachineIntentMirror {
    path: PathBuf,
}

impl MachineIntentMirror {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
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
    /// core's lower epoch is dropped. Returns whether the write happened.
    pub fn store(&self, snapshot: &IntentSnapshot) -> io::Result<bool> {
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

/// Write via a temp file + rename so a crash mid-write cannot leave a truncated
/// mirror behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(epoch: u64) -> IntentSnapshot {
        serde_json::from_value(serde_json::json!({
            "epoch": epoch,
            "active_machines": [],
            "route_bindings": [],
            "serving_target_entries": [],
            "authorized_users": [],
        }))
        .expect("snapshot deserializes")
    }

    #[test]
    fn stores_and_reloads_the_snapshot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        assert!(mirror.load().is_none());
        assert!(mirror.store(&snapshot(1)).expect("store"));
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 1);
    }

    #[test]
    fn rejects_a_lower_epoch_and_accepts_equal_or_higher() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        assert!(mirror.store(&snapshot(2)).expect("store 2"));
        // A healed, stale core advertising a lower epoch is dropped.
        assert!(!mirror.store(&snapshot(1)).expect("reject 1"));
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 2);
        // Equal or higher advances the mirror.
        assert!(mirror.store(&snapshot(3)).expect("store 3"));
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 3);
    }
}
