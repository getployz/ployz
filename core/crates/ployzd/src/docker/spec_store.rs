use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write as _,
    os::unix::fs::{self as unix_fs, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use hex::encode as hex_encode;
use ployz_core::{
    ConfigMount, ConfigSpec, ContainerId, ContainerKind, ContainerRun, CopyRotation,
    DockerVolumeName, Namespace, ResolvedServiceSpec, VolumeGen,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, MutexGuard, watch};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS containers (
    id           TEXT NOT NULL PRIMARY KEY,
    service_spec TEXT NOT NULL CHECK (json_valid(service_spec))
);
CREATE TABLE IF NOT EXISTS volume_floors (
    name  TEXT NOT NULL PRIMARY KEY,
    floor INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rotations (
    id       TEXT NOT NULL PRIMARY KEY,
    rotation TEXT NOT NULL CHECK (json_valid(rotation))
);
CREATE TABLE IF NOT EXISTS stops (
    id  TEXT NOT NULL PRIMARY KEY,
    run TEXT NOT NULL CHECK (json_valid(run))
);
CREATE TABLE IF NOT EXISTS creations (
    slot TEXT NOT NULL PRIMARY KEY,
    id   TEXT NOT NULL
);
";
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The namespace, kind and creation key a keyed create reserved. A container-level
/// restore keeps a Container's labels and so its slot, but mints a new Docker ID.
#[derive(Clone, Debug)]
pub(crate) struct CreationSlot(String);

impl CreationSlot {
    pub(crate) fn new(namespace: &Namespace, kind: ContainerKind, key: &str) -> Self {
        Self(serde_json::to_string(&(namespace, kind, key)).expect("creation slot serializes"))
    }
}

#[derive(Clone)]
pub struct MachineSpecStore {
    path: PathBuf,
    config_operations: Arc<Mutex<()>>,
    floors: Arc<Mutex<()>>,
    rotations: watch::Sender<()>,
    /// Never used: keeps SQLite from checkpointing the WAL each time a call's own
    /// connection closes as the last one.
    _open: Arc<std::sync::Mutex<Connection>>,
}

pub(super) struct ConfigOperation<'a> {
    store: &'a MachineSpecStore,
    _guard: MutexGuard<'a, ()>,
}

/// The floor lock, held. Each Volume's floor is the newest lane `gen` this Machine
/// accepted; it is read and raised only through this.
pub struct FloorTurn<'a> {
    store: &'a MachineSpecStore,
    _guard: MutexGuard<'a, ()>,
}

impl MachineSpecStore {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let init_path = path.clone();
        let open = blocking(move || initialize(&init_path)).await?;
        Ok(Self {
            path,
            config_operations: Arc::new(Mutex::new(())),
            floors: Arc::new(Mutex::new(())),
            rotations: watch::Sender::new(()),
            _open: Arc::new(std::sync::Mutex::new(open)),
        })
    }

    /// This Machine's record of a copy's rotation, `None` when it never left Pending.
    pub async fn rotation(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<CopyRotation>, Error> {
        let path = self.path.clone();
        let container_id = container_id.to_string();
        let encoded = blocking(move || {
            connect(&path)?
                .query_row(
                    "SELECT rotation FROM rotations WHERE id = ?1",
                    [container_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(Error::from)
        })
        .await?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(Error::from))
            .transpose()
    }

    pub async fn rotations(
        &self,
        container_ids: Vec<ContainerId>,
    ) -> Result<BTreeMap<ContainerId, CopyRotation>, Error> {
        let path = self.path.clone();
        let encoded = blocking(move || {
            let connection = connect(&path)?;
            let mut statement =
                connection.prepare("SELECT rotation FROM rotations WHERE id = ?1")?;
            let mut encoded = Vec::new();
            for container_id in container_ids {
                if let Some(rotation) = statement
                    .query_row([container_id.to_string()], |row| row.get::<_, String>(0))
                    .optional()?
                {
                    encoded.push((container_id, rotation));
                }
            }
            Ok(encoded)
        })
        .await?;
        encoded
            .into_iter()
            .map(|(container_id, encoded)| Ok((container_id, serde_json::from_str(&encoded)?)))
            .collect()
    }

    /// The run this Machine last stopped for a `Stopped` want, if any.
    pub async fn stopped_run(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<ContainerRun>, Error> {
        let path = self.path.clone();
        let container_id = container_id.to_string();
        let encoded = blocking(move || {
            connect(&path)?
                .query_row(
                    "SELECT run FROM stops WHERE id = ?1",
                    [container_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(Error::from)
        })
        .await?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(Error::from))
            .transpose()
    }

    #[must_use]
    pub fn subscribe_rotations(&self) -> watch::Receiver<()> {
        self.rotations.subscribe()
    }

    /// Take the floor lock. It comes after the switch lock and the admission turn, and
    /// nothing waits on either of those while holding it. A holder may wait on Docker
    /// or the volume plugin: a fenced stop holds it through the Docker stop, a cut
    /// through `zfs`.
    pub async fn floor_turn(&self) -> FloorTurn<'_> {
        FloorTurn {
            store: self,
            _guard: self.floors.lock().await,
        }
    }

    /// Forget every floor, for a Machine leaving its cluster.
    pub async fn clear_floors(&self) -> Result<(), Error> {
        let _turn = self.floor_turn().await;
        let path = self.path.clone();
        blocking(move || {
            connect(&path)?.execute("DELETE FROM volume_floors", [])?;
            Ok(())
        })
        .await
    }

    pub(super) async fn config_operation(&self) -> ConfigOperation<'_> {
        ConfigOperation {
            store: self,
            _guard: self.config_operations.lock().await,
        }
    }

    /// Point each listed slot with no holder at the listed Container holding its spec.
    /// Returns whether a listed Container looks restored, which
    /// [`ConfigOperation::adopt_restored`] then adopts under the lock.
    pub(crate) async fn record_creations(
        &self,
        listed: Vec<(ContainerId, Option<CreationSlot>)>,
    ) -> Result<bool, Error> {
        let path = self.path.clone();
        blocking(move || Ok(!reconcile_creations(&path, &listed, false)?.is_empty())).await
    }

    pub async fn get(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<ResolvedServiceSpec>, Error> {
        let path = self.path.clone();
        let container_id = container_id.to_string();
        let encoded = blocking(move || {
            connect(&path)?
                .query_row(
                    "SELECT service_spec FROM containers WHERE id = ?1",
                    [container_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(Error::from)
        })
        .await?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(Error::from))
            .transpose()
    }
}

impl ConfigOperation<'_> {
    /// Also wakes the observation publisher. The caller checks the edge under this
    /// same lock, so no removal lands between check and write.
    pub async fn set_rotation(
        &mut self,
        container_id: &ContainerId,
        rotation: &CopyRotation,
    ) -> Result<(), Error> {
        let path = self.store.path.clone();
        let container_id = container_id.to_string();
        let rotation = serde_json::to_string(rotation)?;
        blocking(move || {
            connect(&path)?.execute(
                "INSERT INTO rotations (id, rotation) VALUES (?1, ?2) ON CONFLICT (id) DO UPDATE SET rotation = excluded.rotation",
                params![container_id, rotation],
            )?;
            Ok(())
        })
        .await?;
        self.store.rotations.send_replace(());
        Ok(())
    }

    /// Record In for each listed Container that has a spec and no rotation yet.
    pub(super) async fn adopt_unrotated(
        &mut self,
        container_ids: Vec<ContainerId>,
    ) -> Result<(), Error> {
        let path = self.store.path.clone();
        let rotation = serde_json::to_string(&CopyRotation::In)?;
        blocking(move || {
            let mut connection = connect(&path)?;
            let transaction = connection.transaction()?;
            for container_id in container_ids {
                transaction.execute(
                    "INSERT INTO rotations (id, rotation) SELECT id, ?2 FROM containers WHERE id = ?1 ON CONFLICT (id) DO NOTHING",
                    params![container_id.to_string(), rotation],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    /// Record that this Machine stopped `run` for a `Stopped` want, and republish
    /// observations as [`Self::set_rotation`] does. Records nothing once the
    /// Container's spec is gone: a removal that won the race already took its row,
    /// and the stop must not bring one back.
    pub async fn set_stopped(
        &mut self,
        container_id: &ContainerId,
        run: &ContainerRun,
    ) -> Result<(), Error> {
        let path = self.store.path.clone();
        let container_id = container_id.to_string();
        let run = serde_json::to_string(run)?;
        blocking(move || {
            connect(&path)?.execute(
                "INSERT INTO stops (id, run) SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM containers WHERE id = ?1) ON CONFLICT (id) DO UPDATE SET run = excluded.run",
                params![container_id, run],
            )?;
            Ok(())
        })
        .await?;
        self.store.rotations.send_replace(());
        Ok(())
    }

    /// Republish observations as [`Self::set_rotation`] does, for a spec write a
    /// Docker event may have raced.
    pub fn republish(&self) {
        self.store.rotations.send_replace(());
    }

    pub async fn put(
        &mut self,
        container_id: &ContainerId,
        slot: Option<&CreationSlot>,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        let path = self.store.path.clone();
        let container_id = container_id.to_string();
        let slot = slot.cloned();
        let spec = serde_json::to_string(spec)?;
        blocking(move || {
            let mut connection = connect(&path)?;
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO containers (id, service_spec) VALUES (?1, ?2) ON CONFLICT (id) DO UPDATE SET service_spec = excluded.service_spec",
                params![container_id, spec],
            )?;
            if let Some(CreationSlot(slot)) = slot {
                transaction.execute(
                    "INSERT INTO creations (slot, id) VALUES (?1, ?2) ON CONFLICT (slot) DO UPDATE SET id = excluded.id",
                    params![slot, container_id],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    /// Move the records of each listed Container's restored-over ID onto it, so it is
    /// observed as it was before the restore. `listed` is Docker's inventory read under
    /// this lock. Returns each `(old, new)` pair moved.
    pub(crate) async fn adopt_restored(
        &mut self,
        listed: Vec<(ContainerId, Option<CreationSlot>)>,
    ) -> Result<Vec<(String, ContainerId)>, Error> {
        let path = self.store.path.clone();
        blocking(move || reconcile_creations(&path, &listed, true)).await
    }

    pub async fn remove(&mut self, container_id: &ContainerId) -> Result<bool, Error> {
        let path = self.store.path.clone();
        let container_id = container_id.to_string();
        blocking(move || {
            let connection = connect(&path)?;
            connection.execute("DELETE FROM rotations WHERE id = ?1", [&container_id])?;
            connection.execute("DELETE FROM stops WHERE id = ?1", [&container_id])?;
            let removed =
                connection.execute("DELETE FROM containers WHERE id = ?1", [&container_id])? > 0;
            connection.execute("DELETE FROM creations WHERE id = ?1", [container_id])?;
            if removed
                && let Err(error) = referenced_config_names(&connection)
                    .and_then(|retained| garbage_collect_config_files(&path, &retained))
            {
                eprintln!(
                    "failed to reclaim materialized configs: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
            }
            Ok(removed)
        })
        .await
    }

    pub async fn materialize_config(
        &mut self,
        config: &ConfigSpec,
        mount: &ConfigMount,
    ) -> Result<PathBuf, Error> {
        let database = self.store.path.clone();
        let config = config.clone();
        let mount = mount.clone();
        blocking(move || materialize_config(&database, &config, &mount)).await
    }

    pub async fn garbage_collect_configs(&mut self) -> Result<(), Error> {
        let path = self.store.path.clone();
        blocking(move || {
            let connection = connect(&path)?;
            let retained = referenced_config_names(&connection)?;
            garbage_collect_config_files(&path, &retained)
        })
        .await
    }
}

#[cfg(test)]
impl MachineSpecStore {
    pub(crate) fn path_for_test(&self) -> &std::path::Path {
        &self.path
    }
}

impl FloorTurn<'_> {
    /// Refuse a `gen` below the floor of `name` with that floor; otherwise raise the
    /// floor to `gen`. A Volume with no floor yet has floor 0.
    pub async fn admit(
        &mut self,
        name: &DockerVolumeName,
        r#gen: VolumeGen,
    ) -> Result<Result<(), VolumeGen>, super::Error> {
        let path = self.store.path.clone();
        let name = name.to_string();
        blocking(move || {
            let connection = connect(&path)?;
            let floor = connection
                .query_row(
                    "SELECT floor FROM volume_floors WHERE name = ?1",
                    [&name],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map_or(VolumeGen::default(), |floor| {
                    VolumeGen::new(u64::try_from(floor).unwrap_or_default())
                });
            if r#gen < floor {
                return Ok(Err(floor));
            }
            connection.execute(
                "INSERT INTO volume_floors (name, floor) VALUES (?1, ?2) ON CONFLICT (name) DO UPDATE SET floor = max(floor, excluded.floor)",
                // SQLite integers are signed. Cloud's gen is a Postgres bigint, so it
                // fits; saturating keeps the order if one ever does not.
                params![name, i64::try_from(r#gen.get()).unwrap_or(i64::MAX)],
            )?;
            Ok(Ok(()))
        })
        .await
        .map_err(super::Error::from)
    }
}

fn materialize_config(
    database: &Path,
    config: &ConfigSpec,
    mount: &ConfigMount,
) -> Result<PathBuf, Error> {
    let (name, uid, gid, mode) = config_identity(config, mount)?;
    let directory = config_directory(database)?;
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o711))?;
    let path = directory.join(name);
    let temporary = directory.join(format!(
        ".config-{}-{}.tmp",
        std::process::id(),
        TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&config.content)?;
        file.sync_all()?;
        unix_fs::fchown(&file, Some(uid), Some(gid))?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        drop(file);
        fs::rename(&temporary, &path)?;
        File::open(&directory)?.sync_all()?;
        Ok(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn config_identity(
    config: &ConfigSpec,
    mount: &ConfigMount,
) -> Result<(String, u32, u32, u32), Error> {
    let uid = u32::try_from(mount.uid.unwrap_or(0))
        .map_err(|_| Error::InvalidConfig("uid exceeds u32".into()))?;
    let gid = u32::try_from(mount.gid.unwrap_or(0))
        .map_err(|_| Error::InvalidConfig("gid exceeds u32".into()))?;
    let mode = mount.mode.unwrap_or(0o444);
    if mode & !0o7777 != 0 {
        return Err(Error::InvalidConfig(format!(
            "mode {mode:#o} exceeds Unix permission bits"
        )));
    }

    let mut digest = Sha256::new();
    digest.update((config.content.len() as u64).to_le_bytes());
    digest.update(&config.content);
    digest.update(uid.to_le_bytes());
    digest.update(gid.to_le_bytes());
    digest.update(mode.to_le_bytes());
    Ok((hex_encode(digest.finalize()), uid, gid, mode))
}

fn config_directory(database: &Path) -> Result<PathBuf, Error> {
    let parent = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(fs::canonicalize(parent)?.join("configs"))
}

fn referenced_config_names(connection: &Connection) -> Result<HashSet<String>, Error> {
    let mut referenced = HashSet::new();
    let mut statement = connection.prepare("SELECT service_spec FROM containers")?;
    let encoded = statement.query_map([], |row| row.get::<_, String>(0))?;
    for encoded in encoded {
        let spec: ResolvedServiceSpec = serde_json::from_str(&encoded?)?;
        for mount in spec.config_graph().mounts() {
            let config = spec.config_graph().config_for(mount);
            referenced.insert(config_identity(config, mount)?.0);
        }
    }

    Ok(referenced)
}

fn garbage_collect_config_files(
    database: &Path,
    referenced: &HashSet<String>,
) -> Result<(), Error> {
    let directory = config_directory(database)?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if entry.file_type()?.is_file()
            && name.len() == 64
            && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !referenced.contains(name)
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Point each listed slot that has no holder with a spec at the listed Container
/// holding its spec, and return each listed Container with no spec whose slot names
/// an ID absent from `listed` that holds a spec: one a restore recreated under a new
/// ID. With `adopt`, that old ID's records move to the new ID, so `listed` must be
/// read under the lock.
fn reconcile_creations(
    path: &Path,
    listed: &[(ContainerId, Option<CreationSlot>)],
    adopt: bool,
) -> Result<Vec<(String, ContainerId)>, Error> {
    let mut connection = connect(path)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let present = listed
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<HashSet<_>>();
    let has_spec = |id: &str| {
        transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM containers WHERE id = ?1)",
            [id],
            |row| row.get::<_, bool>(0),
        )
    };
    let mut restored = Vec::new();
    for (id, slot) in listed {
        let Some(CreationSlot(slot)) = slot else {
            continue;
        };
        if has_spec(id.as_str())? {
            transaction.execute(
                "INSERT INTO creations (slot, id) VALUES (?1, ?2) ON CONFLICT (slot) DO UPDATE SET id = excluded.id WHERE NOT EXISTS (SELECT 1 FROM containers WHERE containers.id = creations.id)",
                params![slot, id.as_str()],
            )?;
            continue;
        }
        let Some(old) = transaction
            .query_row("SELECT id FROM creations WHERE slot = ?1", [slot], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
        else {
            continue;
        };
        if present.contains(old.as_str()) || !has_spec(&old)? {
            continue;
        }
        if adopt {
            for table in ["containers", "rotations", "stops"] {
                transaction.execute(
                    &format!("UPDATE {table} SET id = ?2 WHERE id = ?1"),
                    params![old, id.as_str()],
                )?;
            }
            transaction.execute(
                "UPDATE creations SET id = ?2 WHERE slot = ?1",
                params![slot, id.as_str()],
            )?;
        }
        restored.push((old, *id));
    }
    transaction.commit()?;
    Ok(restored)
}

fn initialize(path: &Path) -> Result<Connection, Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    drop(file);

    let connection = connect(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.execute_batch(SCHEMA)?;
    Ok(connection)
}

fn connect(path: &Path) -> Result<Connection, Error> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(connection)
}

async fn blocking<T>(work: impl FnOnce() -> Result<T, Error> + Send + 'static) -> Result<T, Error>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await?
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("machine database I/O failed")]
    Io(#[from] std::io::Error),
    #[error("machine database failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("machine database JSON failed")]
    Json(#[from] serde_json::Error),
    #[error("invalid config metadata: {0}")]
    InvalidConfig(String),
    #[error("machine database task failed")]
    Task(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };

    use ployz_core::{ContainerId, DockerVolumeName, ResolvedServiceSpec, VolumeGen};
    use serde_json::json;

    use super::{MachineSpecStore, connect};

    #[tokio::test]
    async fn a_floor_starts_at_zero_only_rises_and_survives_a_reopen() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-machine-db-{}",
            ployz_core::MachineId::random()
        ));
        let store = MachineSpecStore::open(root.join("machine.db"))
            .await
            .unwrap();
        let data: DockerVolumeName = "data".parse().unwrap();
        let other: DockerVolumeName = "other".parse().unwrap();
        let mut turn = store.floor_turn().await;

        assert_eq!(turn.admit(&data, VolumeGen::new(0)).await.unwrap(), Ok(()));
        assert_eq!(turn.admit(&data, VolumeGen::new(7)).await.unwrap(), Ok(()));
        assert_eq!(turn.admit(&data, VolumeGen::new(7)).await.unwrap(), Ok(()));
        assert_eq!(
            turn.admit(&data, VolumeGen::new(6)).await.unwrap(),
            Err(VolumeGen::new(7))
        );
        assert_eq!(
            turn.admit(&data, VolumeGen::new(0)).await.unwrap(),
            Err(VolumeGen::new(7))
        );
        assert_eq!(turn.admit(&other, VolumeGen::new(1)).await.unwrap(), Ok(()));
        drop(turn);

        let reopened = MachineSpecStore::open(root.join("machine.db"))
            .await
            .unwrap();
        assert_eq!(
            reopened
                .floor_turn()
                .await
                .admit(&data, VolumeGen::new(6))
                .await
                .unwrap(),
            Err(VolumeGen::new(7))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn resolved_spec_round_trips_and_removal_reclaims_unreferenced_configs() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-machine-db-{}",
            ployz_core::MachineId::random()
        ));
        let path = root.join("machine.db");
        let store = MachineSpecStore::open(&path).await.unwrap();
        let metadata = fs::metadata(&root).unwrap();
        let uid = metadata.uid();
        let gid = metadata.gid();
        let id = ContainerId::parse("a".repeat(64)).unwrap();
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "b".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "environment": { "TOKEN": "secret" },
                "pull_policy": "missing",
                "config_mounts": [{ "config_name": "settings", "uid": uid, "gid": gid }]
            },
            "configs": [{ "name": "settings", "content": [107, 61, 118, 10] }]
        }))
        .unwrap();

        let mut operation = store.config_operation().await;
        let config_path = operation
            .materialize_config(
                spec.configs().first().expect("fixture has a config"),
                spec.config_mounts()
                    .first()
                    .expect("fixture has a config mount"),
            )
            .await
            .unwrap();
        let second_id = ContainerId::parse("c".repeat(64)).unwrap();
        let incompatible_id = ContainerId::parse("d".repeat(64)).unwrap();

        operation.put(&id, None, &spec).await.unwrap();
        operation.put(&second_id, None, &spec).await.unwrap();
        connect(&path)
            .unwrap()
            .execute(
                "INSERT INTO containers (id, service_spec) VALUES (?1, '{}')",
                [incompatible_id.as_str()],
            )
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(operation);
        drop(store);
        let store = MachineSpecStore::open(&path).await.unwrap();
        let mut operation = store.config_operation().await;
        assert_eq!(store.get(&id).await.unwrap(), Some(spec.clone()));
        assert!(operation.remove(&id).await.unwrap());
        assert!(config_path.exists());
        assert_eq!(store.get(&id).await.unwrap(), None);
        assert!(!operation.remove(&id).await.unwrap());
        assert!(operation.remove(&incompatible_id).await.unwrap());
        assert!(operation.remove(&second_id).await.unwrap());
        assert!(!config_path.exists());

        operation
            .materialize_config(
                spec.configs().first().expect("fixture has a config"),
                spec.config_mounts()
                    .first()
                    .expect("fixture has a config mount"),
            )
            .await
            .unwrap();
        operation.garbage_collect_configs().await.unwrap();
        assert!(!config_path.exists());

        drop(operation);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rotation_survives_observation_rebuild() {
        use crate::docker::{ContainerRuntime, test_support::runtime_with_running_container};
        use ployz_core::{CopyRotation, DeploymentLogId, Gen, MachineId};

        let (runtime, _fake, id) = runtime_with_running_container().await;
        let machine = MachineId::random();
        let observed = runtime.inspect_managed(&id, &machine).await.unwrap();
        assert_eq!(observed.rotation, Some(CopyRotation::Pending));
        assert_eq!(observed.r#gen, Some(Gen::FIRST));

        let held = CopyRotation::Held(DeploymentLogId::parse("d1").unwrap());
        runtime
            .rotation_turn(&id)
            .await
            .unwrap()
            .write(&held)
            .await
            .unwrap();
        let restarted = ContainerRuntime::new(
            runtime.local_docker(),
            MachineSpecStore::open(runtime.spec_store_path_for_test())
                .await
                .unwrap(),
        );
        let observed = restarted.inspect_managed(&id, &machine).await.unwrap();
        assert_eq!(observed.rotation(), &held);
        assert_eq!(observed.copy_gen(), Gen::FIRST);
    }

    #[tokio::test]
    async fn a_copy_created_before_rotation_existed_is_in_until_a_rotation_is_written() {
        use crate::docker::{LABEL_GEN, test_support::runtime_with_running_container};
        use ployz_core::{CopyRotation, MachineId};

        let (runtime, fake, id) = runtime_with_running_container().await;
        fake.existing_container
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .pointer_mut("/Config/Labels")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(LABEL_GEN)
            .unwrap();
        let machine = MachineId::random();
        let rotation = || async {
            let [observed] = runtime
                .list_managed(&machine)
                .await
                .unwrap()
                .try_into()
                .unwrap();
            observed.rotation().clone()
        };

        assert_eq!(rotation().await, CopyRotation::In);
        assert_eq!(
            runtime.rotation_turn(&id).await.unwrap().rotation(),
            &CopyRotation::In
        );
        runtime
            .rotation_turn(&id)
            .await
            .unwrap()
            .write(&CopyRotation::Out)
            .await
            .unwrap();
        assert_eq!(rotation().await, CopyRotation::Out);
    }

    struct Restorable {
        runtime: crate::docker::ContainerRuntime,
        fake: crate::docker::test_support::FakeDocker,
        containers:
            std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
        machine: ployz_core::MachineId,
        id: ContainerId,
        spec: ResolvedServiceSpec,
        rotation: ployz_core::CopyRotation,
    }

    async fn keyed_copy_in_rotation() -> Restorable {
        use crate::docker::test_support::{
            FakeDocker, container_request, fake_runtime_with, machine, spec_with_sources,
        };
        use ployz_core::{ContainerKind, CopyRotation, DeploymentLogId, Namespace};

        let containers = std::sync::Arc::default();
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(std::sync::Arc::clone(&containers)),
            list_named: true,
            ..Default::default()
        })
        .await;
        let machine = machine();
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let mut request = container_request(
            ContainerKind::ServiceContainer,
            &namespace,
            &spec,
            std::future::ready(None),
        );
        request.creation_key = Some("restore-1");
        let id = runtime
            .create_with_admission::<_, _, crate::docker::Error>(&machine, request)
            .await
            .unwrap()
            .container_id;
        let rotation = CopyRotation::Held(DeploymentLogId::parse("d1").unwrap());
        runtime
            .rotation_turn(&id)
            .await
            .unwrap()
            .write(&rotation)
            .await
            .unwrap();
        Restorable {
            runtime,
            fake,
            containers,
            machine: machine.id,
            id,
            spec,
            rotation,
        }
    }

    fn other_id(id: &ContainerId) -> ContainerId {
        let flipped = if id.as_str().starts_with('f') {
            'e'
        } else {
            'f'
        };
        ContainerId::parse(format!("{flipped}{}", &id.as_str()[1..])).unwrap()
    }

    #[tokio::test]
    async fn a_restored_copy_keeps_its_spec_and_rotation_under_its_new_id() {
        let copy = keyed_copy_in_rotation().await;
        let restored = other_id(&copy.id);
        for container in copy.containers.lock().unwrap().values_mut() {
            container
                .as_object_mut()
                .unwrap()
                .insert("Id".into(), restored.as_str().into());
        }

        let listed = copy.runtime.list_managed(&copy.machine).await.unwrap();

        let observed = listed
            .iter()
            .find(|observed| observed.container_id == restored)
            .expect("the restored copy is observed");
        assert_eq!(observed.resolved_spec, copy.spec);
        assert_eq!(observed.rotation(), &copy.rotation);
        let store = MachineSpecStore::open(copy.runtime.spec_store_path_for_test())
            .await
            .unwrap();
        assert_eq!(store.get(&restored).await.unwrap(), Some(copy.spec));
        assert_eq!(store.get(&copy.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_scan_listed_before_a_replacement_leaves_the_replacements_records() {
        use crate::docker::test_support::{container_request, machine};
        use ployz_core::{ContainerKind, Namespace};

        let copy = keyed_copy_in_rotation().await;
        let restored = other_id(&copy.id);
        for container in copy.containers.lock().unwrap().values_mut() {
            container
                .as_object_mut()
                .unwrap()
                .insert("Id".into(), restored.as_str().into());
        }
        let listed = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        *copy.fake.listed_hold.lock().unwrap() = Some(std::sync::Arc::clone(&listed));
        let stale = tokio::spawn({
            let (runtime, machine) = (copy.runtime.clone(), copy.machine);
            async move { runtime.list_managed(&machine).await }
        });
        listed.wait().await;

        copy.runtime.list_managed(&copy.machine).await.unwrap();
        copy.runtime.remove(&restored, false, true).await.unwrap();
        let mut machine = machine();
        machine.id = copy.machine;
        let namespace = Namespace::parse("app").unwrap();
        let mut request = container_request(
            ContainerKind::ServiceContainer,
            &namespace,
            &copy.spec,
            std::future::ready(None),
        );
        request.creation_key = Some("restore-1");
        let replacement = copy
            .runtime
            .create_with_admission::<_, _, crate::docker::Error>(&machine, request)
            .await
            .unwrap()
            .container_id;
        listed.wait().await;
        let _ = stale.await.unwrap();

        let store = MachineSpecStore::open(copy.runtime.spec_store_path_for_test())
            .await
            .unwrap();
        assert_eq!(store.get(&replacement).await.unwrap(), Some(copy.spec));
        assert_eq!(store.get(&restored).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_stale_listing_never_points_a_slot_away_from_a_container_with_a_spec() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-machine-db-{}",
            ployz_core::MachineId::random()
        ));
        let store = MachineSpecStore::open(root.join("machine.db"))
            .await
            .unwrap();
        let spec = crate::docker::test_support::spec_with_sources(Vec::new());
        let slot = super::CreationSlot::new(
            &ployz_core::Namespace::parse("app").unwrap(),
            ployz_core::ContainerKind::ServiceContainer,
            "restore-1",
        );
        let live = ContainerId::parse("a".repeat(64)).unwrap();
        let stale = ContainerId::parse("b".repeat(64)).unwrap();
        let restored = ContainerId::parse("c".repeat(64)).unwrap();
        let mut operation = store.config_operation().await;
        operation.put(&live, Some(&slot), &spec).await.unwrap();
        operation.put(&stale, None, &spec).await.unwrap();
        drop(operation);

        store
            .record_creations(vec![(stale, Some(slot.clone()))])
            .await
            .unwrap();

        assert_eq!(
            store
                .config_operation()
                .await
                .adopt_restored(vec![(restored, Some(slot))])
                .await
                .unwrap(),
            vec![(live.to_string(), restored)]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_copy_with_its_slot_whose_old_id_still_runs_is_not_rehomed() {
        let copy = keyed_copy_in_rotation().await;
        let foreign = other_id(&copy.id);
        {
            let mut containers = copy.containers.lock().unwrap();
            let mut twin = containers.values().next().unwrap().clone();
            let fields = twin.as_object_mut().unwrap();
            fields.insert("Id".into(), foreign.as_str().into());
            fields.insert("Name".into(), "/twin".into());
            containers.insert("twin".into(), twin);
        }

        let listed = copy.runtime.list_managed(&copy.machine).await.unwrap();

        assert_eq!(
            listed
                .iter()
                .map(|observed| observed.container_id)
                .collect::<Vec<_>>(),
            vec![copy.id]
        );
        let store = MachineSpecStore::open(copy.runtime.spec_store_path_for_test())
            .await
            .unwrap();
        assert_eq!(store.get(&foreign).await.unwrap(), None);
        assert_eq!(store.rotation(&foreign).await.unwrap(), None);
        assert_eq!(store.rotation(&copy.id).await.unwrap(), Some(copy.rotation));
    }

    #[tokio::test]
    async fn rotation_entry_deleted_on_remove() {
        use crate::docker::test_support::runtime_with_running_container;
        use ployz_core::CopyRotation;

        let (runtime, _fake, id) = runtime_with_running_container().await;
        runtime
            .rotation_turn(&id)
            .await
            .unwrap()
            .write(&CopyRotation::Out)
            .await
            .unwrap();
        runtime.remove(&id, false, true).await.unwrap();
        let store = MachineSpecStore::open(runtime.spec_store_path_for_test())
            .await
            .unwrap();
        assert_eq!(store.rotation(&id).await.unwrap(), None);
    }
}
