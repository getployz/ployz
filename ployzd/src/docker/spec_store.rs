use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use ployz_core::{ContainerId, ResolvedServiceSpec};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS containers (
    id           TEXT NOT NULL PRIMARY KEY,
    service_spec TEXT NOT NULL CHECK (json_valid(service_spec))
);
";

#[derive(Clone)]
pub struct MachineSpecStore {
    path: PathBuf,
}

impl MachineSpecStore {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let init_path = path.clone();
        blocking(move || initialize(&init_path)).await?;
        Ok(Self { path })
    }

    pub async fn put(
        &self,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        let path = self.path.clone();
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

    pub async fn remove(&self, container_id: &ContainerId) -> Result<bool, Error> {
        let path = self.path.clone();
        let container_id = container_id.to_string();
        blocking(move || {
            Ok(
                connect(&path)?.execute("DELETE FROM containers WHERE id = ?1", [container_id])?
                    > 0,
            )
        })
        .await
    }
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
    #[error("machine database task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use ployz_core::{ContainerId, ResolvedServiceSpec};
    use serde_json::json;

    use super::MachineSpecStore;

    #[tokio::test]
    async fn resolved_spec_round_trips_and_removal_is_local() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-machine-db-{}",
            ployz_core::MachineId::random()
        ));
        let path = root.join("machine.db");
        let store = MachineSpecStore::open(&path).await.unwrap();
        let id = ContainerId::parse("a".repeat(64)).unwrap();
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": "b".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "environment": { "TOKEN": "secret" },
                "pull_policy": "missing"
            }
        }))
        .unwrap();

        store.put(&id, &spec).await.unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(store);
        let store = MachineSpecStore::open(&path).await.unwrap();
        assert_eq!(store.get(&id).await.unwrap(), Some(spec));
        assert!(store.remove(&id).await.unwrap());
        assert_eq!(store.get(&id).await.unwrap(), None);
        assert!(!store.remove(&id).await.unwrap());

        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
