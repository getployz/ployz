use std::{collections::BTreeMap, time::Duration};

use ployz_core::{ContainerObservation, Machine};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{ApiClient, Error, Statement};

#[derive(Clone)]
pub struct ReplicatedStore {
    api: ApiClient,
}

impl ReplicatedStore {
    #[must_use]
    pub fn new(api: ApiClient) -> Self {
        Self { api }
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
        let Some(row) = query.rows.first() else {
            return Ok(None);
        };
        let info = text(row.first(), "machine info")?;
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
        decode_observations(query.rows, 1)
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
        decode_observations(query.rows, 1)
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
            .rows
            .iter()
            .map(|row| {
                let actor = actor_id(row.first())?;
                let version = row
                    .get(1)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| Error::Protocol("invalid actor version".into()))?;
                Ok((actor, version))
            })
            .collect()
    }

    pub async fn known_missing_changes(&self) -> Result<Vec<MissingChange>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT actor_id, start, end FROM __corro_bookkeeping_gaps",
                [],
            ))
            .await?;
        query
            .rows
            .iter()
            .map(|row| {
                Ok(MissingChange {
                    actor_id: actor_id(row.first())?,
                    start_version: integer(row.get(1), "gap start")?,
                    end_version: integer(row.get(2), "gap end")?,
                })
            })
            .collect()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReplicatedObservations<T> {
    pub observations: Vec<T>,
    pub incomplete_ids: Vec<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct MissingChange {
    pub actor_id: String,
    pub start_version: i64,
    pub end_version: i64,
}

pub async fn wait_for_catch_up(
    store: &ReplicatedStore,
    target: &BTreeMap<String, i64>,
) -> Result<(), Error> {
    loop {
        let local = store.version().await?;
        let reached = target
            .iter()
            .all(|(actor, target)| local.get(actor).copied().unwrap_or_default() >= *target);
        if reached && store.known_missing_changes().await?.is_empty() {
            return Ok(());
        }
        // No deadline by design: Joining includes this potentially unbounded catch-up barrier.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn decode_observations<T: DeserializeOwned>(
    rows: Vec<Vec<Value>>,
    value_index: usize,
) -> Result<ReplicatedObservations<T>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for row in rows {
        let id = text(row.first(), "row ID")?.to_owned();
        let encoded = text(row.get(value_index), "replicated JSON")?;
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

fn text<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a str, Error> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol(format!("invalid {field}")))
}

fn integer(value: Option<&Value>, field: &str) -> Result<i64, Error> {
    value
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Protocol(format!("invalid {field}")))
}

fn actor_id(value: Option<&Value>) -> Result<String, Error> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(Value::Array(bytes)) => bytes
            .iter()
            .map(|byte| {
                byte.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .map(|byte| format!("{byte:02x}"))
                    .ok_or_else(|| Error::Protocol("invalid actor ID byte".into()))
            })
            .collect(),
        _ => Err(Error::Protocol("invalid actor ID".into())),
    }
}
