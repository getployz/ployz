use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{ContainerObservation, LocalMachinePhase, Machine};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;

use super::{ApiClient, Error, Statement};
use crate::machine::LocalMachineStore;

#[derive(Clone)]
pub struct ReplicatedStore {
    api: ApiClient,
}

impl ReplicatedStore {
    #[must_use]
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    #[cfg(test)]
    pub(super) fn api(&self) -> &ApiClient {
        &self.api
    }

    pub async fn publish_local_machine(&self, machine: &Machine) -> Result<(), Error> {
        let current = self.machine(machine.id.as_str()).await?;
        if current.as_ref() == Some(machine) {
            return Ok(());
        }
        self.api
            .execute([Statement::new(
                "INSERT INTO machines (id, info, created_at, updated_at) VALUES (?, ?, datetime('now'), datetime('now')) ON CONFLICT (id) DO UPDATE SET info = excluded.info, updated_at = excluded.updated_at",
                [json!(machine.id), json!(serde_json::to_string(machine)?)],
            )])
            .await?;
        Ok(())
    }

    pub async fn machine(&self, id: &str) -> Result<Option<Machine>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT info FROM machines WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        let rows = query.rows(["info"])?;
        let Some([info]) = rows.first() else {
            return Ok(None);
        };
        let info = text(info, "machine info")?;
        if info.is_empty() || info == "{}" {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(info)?))
    }

    pub async fn machines(&self) -> Result<ReplicatedObservations<Machine>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, info FROM machines ORDER BY name",
                [],
            ))
            .await?;
        decode_observations(query.rows(["id", "info"])?)
    }

    pub async fn publish_container(&self, observation: &ContainerObservation) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO containers (id, container, machine_id, updated_at) VALUES (?, ?, ?, datetime('now')) ON CONFLICT (id) DO UPDATE SET container = excluded.container, machine_id = excluded.machine_id, updated_at = excluded.updated_at",
                [
                    json!(observation.container_id),
                    json!(serde_json::to_string(observation)?),
                    json!(observation.machine_id),
                ],
            )])
            .await?;
        Ok(())
    }

    pub async fn containers(&self) -> Result<ReplicatedObservations<ContainerObservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, container FROM containers ORDER BY id",
                [],
            ))
            .await?;
        decode_observations(query.rows(["id", "container"])?)
    }

    pub async fn version(&self) -> Result<BTreeMap<String, i64>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT site_id, db_version FROM crsql_db_versions",
                [],
            ))
            .await?;
        query
            .rows(["site_id", "db_version"])?
            .into_iter()
            .map(|[actor, version]| {
                let actor = actor_id(&actor)?;
                let version = version
                    .as_i64()
                    .ok_or_else(|| Error::Protocol("invalid actor version".into()))?;
                Ok((actor, version))
            })
            .collect()
    }

    pub async fn has_known_missing_changes(&self) -> Result<bool, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT EXISTS (SELECT 1 FROM __corro_bookkeeping_gaps) AS has_gaps",
                [],
            ))
            .await?;
        match query.rows(["has_gaps"])?.as_slice() {
            [[Value::Number(value)]] => value
                .as_u64()
                .filter(|value| *value <= 1)
                .map(|value| value == 1)
                .ok_or_else(|| Error::Protocol("invalid gap status".into())),
            _ => Err(Error::Protocol("invalid gap status row".into())),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReplicatedObservations<T> {
    pub observations: Vec<T>,
    pub incomplete_ids: Vec<String>,
}

pub async fn wait_for_catch_up(
    store: &ReplicatedStore,
    target: &BTreeMap<String, i64>,
) -> Result<(), Error> {
    if target.is_empty() {
        return Ok(());
    }
    let warning_interval = Duration::from_secs(5 * 60);
    let mut warning_at = tokio::time::Instant::now() + warning_interval;
    loop {
        let status = match store.version().await {
            Ok(local) => {
                let lagging = target
                    .iter()
                    .filter(|(actor, target)| {
                        local.get(*actor).copied().unwrap_or_default() < **target
                    })
                    .count();
                if lagging == 0 {
                    match store.has_known_missing_changes().await {
                        Ok(false) => return Ok(()),
                        Ok(true) => "known bookkeeping gaps remain".to_owned(),
                        Err(error) => error.to_string(),
                    }
                } else {
                    format!("{lagging} actor(s) remain behind the target")
                }
            }
            Err(error) => error.to_string(),
        };
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = tokio::time::sleep_until(warning_at) => {
                eprintln!("cluster store catch-up is still pending: {status}");
                warning_at = tokio::time::Instant::now() + warning_interval;
            }
        }
    }
}

pub async fn run_machine_publisher(
    replicated: Option<ReplicatedStore>,
    local: Arc<Mutex<LocalMachineStore>>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    if let Some(replicated) = &replicated {
        let (joining, target) = {
            let local = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
            (
                local.record().phase == LocalMachinePhase::Joining,
                local.record().min_store_version.clone(),
            )
        };
        if joining {
            tokio::select! {
                result = wait_for_catch_up(replicated, &target) => {
                    result.map_err(io::Error::other)?;
                }
                changed = shutdown.changed() => {
                    changed.map_err(io::Error::other)?;
                    return Ok(());
                }
            }
            local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
                .complete_catch_up()
                .map_err(io::Error::other)?;
        }
    }
    loop {
        if let Some(replicated) = &replicated {
            let machine = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
                .record()
                .machine
                .clone();
            if let Some(machine) = machine {
                if let Err(error) = replicated.publish_local_machine(&machine).await {
                    eprintln!("failed to publish local Machine: {error}");
                }
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            changed = shutdown.changed() => {
                changed.map_err(io::Error::other)?;
                return Ok(());
            }
        }
    }
}

fn decode_observations<T: DeserializeOwned>(
    rows: Vec<[Value; 2]>,
) -> Result<ReplicatedObservations<T>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for [id, encoded] in rows {
        let id = text(&id, "row ID")?.to_owned();
        let encoded = text(&encoded, "replicated JSON")?;
        if encoded.is_empty() || encoded == "{}" {
            incomplete_ids.push(id);
        } else {
            observations.push(serde_json::from_str(encoded)?);
        }
    }
    Ok(ReplicatedObservations {
        observations,
        incomplete_ids,
    })
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value
        .as_str()
        .ok_or_else(|| Error::Protocol(format!("invalid {field}")))
}

fn actor_id(value: &Value) -> Result<String, Error> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Array(bytes) => bytes
            .iter()
            .map(|byte| {
                byte.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .map(|byte| format!("{byte:02x}"))
                    .ok_or_else(|| Error::Protocol("invalid actor ID byte".into()))
            })
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            Err(Error::Protocol("invalid actor ID".into()))
        }
    }
}
