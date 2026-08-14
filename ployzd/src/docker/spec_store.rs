use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write as _,
    os::unix::fs::{self as unix_fs, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use ployz_core::{ConfigMount, ConfigSpec, ContainerId, ResolvedServiceSpec};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, MutexGuard};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS containers (
    id           TEXT NOT NULL PRIMARY KEY,
    service_spec TEXT NOT NULL CHECK (json_valid(service_spec))
);
";
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct MachineSpecStore {
    path: PathBuf,
    config_operations: Arc<Mutex<()>>,
}

pub(super) struct ConfigOperation<'a> {
    store: &'a MachineSpecStore,
    _guard: MutexGuard<'a, ()>,
}

impl MachineSpecStore {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let init_path = path.clone();
        blocking(move || initialize(&init_path)).await?;
        Ok(Self {
            path,
            config_operations: Arc::new(Mutex::new(())),
        })
    }

    pub(super) async fn config_operation(&self) -> ConfigOperation<'_> {
        ConfigOperation {
            store: self,
            _guard: self.config_operations.lock().await,
        }
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
    pub async fn put(
        &mut self,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        let path = self.store.path.clone();
        let container_id = container_id.to_string();
        let spec = serde_json::to_string(spec)?;
        blocking(move || {
            connect(&path)?.execute(
                "INSERT INTO containers (id, service_spec) VALUES (?1, ?2) ON CONFLICT (id) DO UPDATE SET service_spec = excluded.service_spec",
                params![container_id, spec],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn remove(&mut self, container_id: &ContainerId) -> Result<bool, Error> {
        let path = self.store.path.clone();
        let container_id = container_id.to_string();
        blocking(move || {
            let connection = connect(&path)?;
            let removed =
                connection.execute("DELETE FROM containers WHERE id = ?1", [container_id])? > 0;
            if removed
                && let Err(error) = referenced_config_names(&connection)
                    .and_then(|retained| garbage_collect_config_files(&path, &retained))
            {
                eprintln!("failed to reclaim materialized configs: {error}");
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
    Ok((format!("{:x}", digest.finalize()), uid, gid, mode))
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
        for mount in &spec.container.config_mounts {
            let config = spec
                .configs
                .iter()
                .find(|config| config.name == mount.config_name)
                .ok_or_else(|| Error::InvalidConfig(format!("missing {:?}", mount.config_name)))?;
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

fn initialize(path: &Path) -> Result<(), Error> {
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
    Ok(())
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
    #[error("machine database I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("machine database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("machine database JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid config metadata: {0}")]
    InvalidConfig(String),
    #[error("machine database task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };

    use ployz_core::{ContainerId, ResolvedServiceSpec};
    use serde_json::json;

    use super::{MachineSpecStore, connect};

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
                spec.configs.first().expect("fixture has a config"),
                spec.container
                    .config_mounts
                    .first()
                    .expect("fixture has a config mount"),
            )
            .await
            .unwrap();
        let second_id = ContainerId::parse("c".repeat(64)).unwrap();
        let incompatible_id = ContainerId::parse("d".repeat(64)).unwrap();

        operation.put(&id, &spec).await.unwrap();
        operation.put(&second_id, &spec).await.unwrap();
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
                spec.configs.first().expect("fixture has a config"),
                spec.container
                    .config_mounts
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
}
