//! Moves Corrosion's store to a new actor through a backup of it, crash-safely: each
//! step leaves either the old store or a complete, synced replacement in place.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::Error;

/// Clears what an earlier interrupted rotation left and returns where `corrosion backup`
/// writes the copy.
pub(super) fn staged(data_dir: &Path) -> Result<PathBuf, Error> {
    let staged = data_dir.join(STAGED_STORE);
    remove_store_files(&staged)?;
    Ok(staged)
}

/// Turns the finished backup into the store, the agent stopped.
pub(super) fn hand_off(data_dir: &Path) -> Result<(), Error> {
    let staged = data_dir.join(STAGED_STORE);
    keep_members(&staged, &data_dir.join("store.db"))?;
    fs::File::open(&staged)?.sync_all()?;
    // `backup` checkpoints the copy's WAL into it and truncates it.
    if fs::metadata(sibling(&staged, "-wal")).is_ok_and(|wal| wal.len() > 0) {
        return Err(Error::Api(
            "`corrosion backup` left frames in its WAL".into(),
        ));
    }
    for suffix in ["-wal", "-shm"] {
        remove_file(&sibling(&staged, suffix))?;
    }
    fs::rename(&staged, data_dir.join(ROTATED_STORE))?;
    fs::File::open(data_dir)?.sync_all()?;
    finish(data_dir)
}

/// A replacement waits while the old store may already have lost its WAL.
pub(super) fn half_done(data_dir: &Path) -> bool {
    data_dir.join(ROTATED_STORE).exists()
}

/// `corrosion backup` writes here; a crash leaves only a partial copy to discard.
const STAGED_STORE: &str = "rotate.db.tmp";
/// A complete, synced backup with no own actor, waiting to replace `store.db`.
const ROTATED_STORE: &str = "rotate.db";

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_file(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn remove_store_files(path: &Path) -> Result<(), Error> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        remove_file(&sibling(path, suffix))?;
    }
    Ok(())
}

/// `backup` empties the gossip member list, which is where Corrosion finds peers that
/// its config does not name. A founder's config names none, so it gets the old list.
fn keep_members(staged: &Path, store: &Path) -> Result<(), Error> {
    let copy = || -> rusqlite::Result<()> {
        let connection = rusqlite::Connection::open(staged)?;
        connection.execute("ATTACH DATABASE ?1 AS old", [store.to_string_lossy()])?;
        connection.execute(
            "INSERT OR IGNORE INTO __corro_members SELECT * FROM old.__corro_members",
            [],
        )?;
        connection.execute_batch("DETACH DATABASE old; PRAGMA wal_checkpoint(TRUNCATE);")
    };
    copy().map_err(|error| {
        Error::Api(format!(
            "could not keep the gossip members: {}",
            ployz_core::error_chain::inline(&error)
        ))
    })
}

/// Puts a synced rotated store in place of `store.db`, the two steps `corrosion restore`
/// takes, without its in-place copy that a power cut can leave half done. The old WAL
/// would replay onto the new file, and subscriptions belong to the old actor, so both
/// go first. Runs again at the next start when a crash interrupts it.
pub(super) fn finish(data_dir: &Path) -> Result<(), Error> {
    remove_store_files(&data_dir.join(STAGED_STORE))?;
    let rotated = data_dir.join(ROTATED_STORE);
    if !rotated.exists() {
        return Ok(());
    }
    let store = data_dir.join("store.db");
    for suffix in ["-wal", "-shm"] {
        remove_file(&sibling(&store, suffix))?;
    }
    match fs::remove_dir_all(data_dir.join("subscriptions")) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    // A power cut must not keep the old WAL beside the new store.
    fs::File::open(data_dir)?.sync_all()?;
    fs::rename(&rotated, &store)?;
    fs::File::open(data_dir)?.sync_all()?;
    tracing::info!("Corrosion's store now runs under a new actor");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rotated_store_replaces_the_old_one_with_its_wal_and_subscriptions() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        for (name, body) in [
            ("store.db", "old"),
            ("store.db-wal", "old frames"),
            ("store.db-shm", ""),
            ("rotate.db", "rotated"),
            ("rotate.db.tmp", "partial"),
        ] {
            fs::write(dir.join(name), body).unwrap();
        }
        fs::create_dir(dir.join("subscriptions")).unwrap();

        finish(dir).unwrap();

        assert_eq!(fs::read_to_string(dir.join("store.db")).unwrap(), "rotated");
        for gone in [
            "store.db-wal",
            "store.db-shm",
            "rotate.db",
            "rotate.db.tmp",
            "subscriptions",
        ] {
            assert!(!dir.join(gone).exists(), "{gone} is still there");
        }
    }

    #[test]
    fn a_rotated_store_keeps_the_gossip_members_backup_cleared() {
        let root = tempfile::tempdir().unwrap();
        let (staged, store) = (
            root.path().join("rotate.db.tmp"),
            root.path().join("store.db"),
        );
        let members = "CREATE TABLE __corro_members (actor_id BLOB PRIMARY KEY NOT NULL, \
            address TEXT NOT NULL, foca_state JSON, rtt_min INTEGER, \
            updated_at DATETIME NOT NULL DEFAULT 0) WITHOUT ROWID;";
        for path in [&staged, &store] {
            rusqlite::Connection::open(path)
                .unwrap()
                .execute_batch(&format!("PRAGMA journal_mode = WAL; {members}"))
                .unwrap();
        }
        rusqlite::Connection::open(&store)
            .unwrap()
            .execute(
                "INSERT INTO __corro_members (actor_id, address) VALUES (x'01', '[fdcc::2]:7570')",
                [],
            )
            .unwrap();

        keep_members(&staged, &store).unwrap();

        let address: String = rusqlite::Connection::open(&staged)
            .unwrap()
            .query_row("SELECT address FROM __corro_members", [], |row| row.get(0))
            .unwrap();
        assert_eq!(address, "[fdcc::2]:7570");
    }

    #[test]
    fn without_a_rotated_store_only_a_partial_backup_goes() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        for name in ["store.db", "store.db-wal", "rotate.db.tmp"] {
            fs::write(dir.join(name), name).unwrap();
        }
        fs::create_dir(dir.join("subscriptions")).unwrap();

        finish(dir).unwrap();

        for kept in ["store.db", "store.db-wal", "subscriptions"] {
            assert!(dir.join(kept).exists(), "{kept} is gone");
        }
        assert!(!dir.join("rotate.db.tmp").exists());
    }
}
