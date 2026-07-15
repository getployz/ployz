//! Daemon-local mirror of core intent.
//!
//! A Reachable Machine persists the latest [`IntentSnapshot`] off the drumbeat
//! so a future promotion (ADR 0031) can seed a new core from it without a backup
//! restore. Writes are epoch-gated: a snapshot from a lower Control-Plane Epoch
//! — a healed, stale core — never overwrites a higher one.

use ployz_core::install::INTENT_MIRROR_FILE_NAME;
use ployz_core::intent::IntentSnapshot;
use ployz_core::intent::recovery::ControlPlaneEpoch;
use ployz_core::intent::recovery::PendingMachineJoinRecoverySnapshot;

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// How the epoch-gated mirror handled a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentMirrorStoreOutcome {
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
pub struct IntentMirror {
    path: PathBuf,
}

impl IntentMirror {
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
    pub fn store(&self, snapshot: &IntentSnapshot) -> io::Result<IntentMirrorStoreOutcome> {
        let _transaction_lock = lock_mirror_transaction(&self.path)?;
        let stored_epoch = self
            .load()
            .map_or_else(ControlPlaneEpoch::initial, |current| current.epoch);
        #[cfg(test)]
        pause_after_mirror_load_for_test();
        if snapshot.epoch < stored_epoch {
            return Ok(IntentMirrorStoreOutcome::RejectedStale);
        }
        let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
        write_atomic(&self.path, &bytes)?;
        Ok(if snapshot.epoch > stored_epoch {
            IntentMirrorStoreOutcome::AcceptedHigher
        } else {
            IntentMirrorStoreOutcome::AcceptedCurrent
        })
    }
}

/// Lock a stable sidecar rather than the snapshot itself: atomic replacement
/// changes the snapshot inode, while every role process continues contending
/// on this file for the complete load, compare, and replace transaction.
fn lock_mirror_transaction(path: &Path) -> io::Result<File> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "intent mirror path has no parent directory",
        ));
    };
    std::fs::create_dir_all(parent)?;
    let mut lock_name = path
        .file_name()
        .map(OsString::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "intent mirror has no name"))?;
    lock_name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join(lock_name))?;
    lock.lock()?;
    Ok(lock)
}

#[cfg(test)]
fn pause_after_mirror_load_for_test() {
    let Some(loaded) = std::env::var_os("PLOYZ_MIRROR_TEST_LOADED") else {
        return;
    };
    std::fs::write(loaded, b"loaded").expect("signal mirror load");
    let release = std::env::var_os("PLOYZ_MIRROR_TEST_RELEASE").expect("mirror release path");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !Path::new(&release).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out awaiting mirror writer release"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[derive(Debug, Clone)]
pub struct PendingMachineJoinMirror {
    path: PathBuf,
}

impl PendingMachineJoinMirror {
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
    use std::process::Command;
    use std::time::{Duration, Instant};

    const WRITER_TEST: &str = "recovery::intent_mirror::tests::mirror_writer_process";

    fn snapshot(epoch: u64) -> IntentSnapshot {
        serde_json::from_value(serde_json::json!({
            "epoch": epoch,
            "core_machine_id": "machine_a",
            "active_machines": [],
            "dataplane_projection": { "declared_members": [], "staged_member": null },
            "route_bindings": [],
            "serving_target_entries": [],
            "nats_authorizations": [],
            "automatic_hostname_configuration": { "mode": "ployz" },
            "ployz_dns_target": "enabled",
            "active_certificates": [],
        }))
        .expect("snapshot deserializes")
    }

    #[test]
    fn stores_and_reloads_the_snapshot() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = IntentMirror::new(dir.path().join("intent-mirror.json"));
        assert!(mirror.load().is_none());
        // A first snapshot at the initial epoch is a same-epoch drumbeat.
        assert_eq!(
            mirror.store(&snapshot(1)).expect("store"),
            IntentMirrorStoreOutcome::AcceptedCurrent
        );
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 1);
    }

    #[test]
    fn rejects_a_lower_epoch_and_accepts_equal_or_higher() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror = IntentMirror::new(dir.path().join("intent-mirror.json"));
        // A fresh mirror's baseline is the initial epoch, so epoch 2 is higher.
        assert_eq!(
            mirror.store(&snapshot(2)).expect("store 2"),
            IntentMirrorStoreOutcome::AcceptedHigher
        );
        // A healed, stale core advertising a lower epoch is dropped.
        assert_eq!(
            mirror.store(&snapshot(1)).expect("reject 1"),
            IntentMirrorStoreOutcome::RejectedStale
        );
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 2);
        // The same epoch re-stores (a drumbeat refresh), higher advances.
        assert_eq!(
            mirror.store(&snapshot(2)).expect("store 2 again"),
            IntentMirrorStoreOutcome::AcceptedCurrent
        );
        assert_eq!(
            mirror.store(&snapshot(3)).expect("store 3"),
            IntentMirrorStoreOutcome::AcceptedHigher
        );
        assert_eq!(mirror.load().expect("loaded").epoch.get(), 3);
    }

    #[test]
    fn competing_role_processes_cannot_regress_the_durable_epoch() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mirror_path = dir.path().join("intent-mirror.json");
        let lower_loaded = dir.path().join("lower-loaded");
        let release_lower = dir.path().join("release-lower");
        let higher_ready = dir.path().join("higher-ready");
        let higher_done = dir.path().join("higher-done");

        let mut lower = writer_process(&mirror_path, 2)
            .env("PLOYZ_MIRROR_TEST_LOADED", &lower_loaded)
            .env("PLOYZ_MIRROR_TEST_RELEASE", &release_lower)
            .spawn()
            .expect("spawn lower-epoch role writer");
        wait_for_file(&lower_loaded, Duration::from_secs(5));

        let mut higher = writer_process(&mirror_path, 3)
            .env("PLOYZ_MIRROR_TEST_READY", &higher_ready)
            .env("PLOYZ_MIRROR_TEST_DONE", &higher_done)
            .spawn()
            .expect("spawn higher-epoch role writer");
        wait_for_file(&higher_ready, Duration::from_secs(5));

        assert!(
            !file_appears_within(&higher_done, Duration::from_millis(500)),
            "a competing writer completed while another process held the mirror transaction lock"
        );
        std::fs::write(&release_lower, b"release").expect("release lower writer");

        assert!(lower.wait().expect("wait for lower writer").success());
        assert!(higher.wait().expect("wait for higher writer").success());
        assert!(
            writer_process(&mirror_path, 2)
                .status()
                .expect("run restarted stale writer")
                .success()
        );
        assert_eq!(
            IntentMirror::new(mirror_path)
                .load()
                .expect("durable mirror")
                .epoch
                .get(),
            3
        );
    }

    fn writer_process(path: &Path, epoch: u64) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .arg("--exact")
            .arg(WRITER_TEST)
            .arg("--ignored")
            .env("PLOYZ_MIRROR_TEST_PATH", path)
            .env("PLOYZ_MIRROR_TEST_EPOCH", epoch.to_string());
        command
    }

    fn wait_for_file(path: &Path, timeout: Duration) {
        assert!(
            file_appears_within(path, timeout),
            "timed out waiting for {}",
            path.display()
        );
    }

    fn file_appears_within(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        path.exists()
    }

    #[test]
    #[ignore = "cross-process helper invoked by competing_role_processes_cannot_regress_the_durable_epoch"]
    fn mirror_writer_process() {
        let path = std::env::var_os("PLOYZ_MIRROR_TEST_PATH")
            .map(PathBuf::from)
            .expect("mirror test path");
        let epoch = std::env::var("PLOYZ_MIRROR_TEST_EPOCH")
            .expect("mirror test epoch")
            .parse()
            .expect("numeric mirror test epoch");
        if let Some(ready) = std::env::var_os("PLOYZ_MIRROR_TEST_READY") {
            std::fs::write(ready, b"ready").expect("signal writer ready");
        }
        IntentMirror::new(path)
            .store(&snapshot(epoch))
            .expect("store mirror from role writer process");
        if let Some(done) = std::env::var_os("PLOYZ_MIRROR_TEST_DONE") {
            std::fs::write(done, b"done").expect("signal writer done");
        }
    }

    #[test]
    fn legacy_ingress_projection_is_not_accepted() {
        let legacy = serde_json::json!({
            "epoch": 1,
            "core_machine_id": "machine_a",
            "active_machines": [],
            "dataplane_projection": { "declared_members": [], "staged_member": null },
            "route_bindings": [],
            "serving_target_entries": [],
            "nats_authorizations": [],
            "public_url": { "mode": "auto", "managed_lease": { "state": "unacquired" } },
            "custom_certificates": [],
            "acme_http01_challenges": [],
        });

        assert!(serde_json::from_value::<IntentSnapshot>(legacy).is_err());
    }
}
