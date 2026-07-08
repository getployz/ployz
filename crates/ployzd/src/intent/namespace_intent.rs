//! Core-local namespace intent (route bindings and serving targets), in SQLite.

use crate::core_store::{CoreStore, CoreStoreError, query_json_list, to_json};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct NamespaceIntentStore {
    store: CoreStore,
}

impl NamespaceIntentStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn replace_route_binding(
        &self,
        state: RouteBindingState,
    ) -> Result<(), NamespaceIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO route_bindings (hostname, port, json) VALUES (?1, ?2, ?3)
                     ON CONFLICT(hostname, port) DO UPDATE SET json = excluded.json",
                    params![
                        state.target.hostname.as_str(),
                        state.target.port.get(),
                        to_json(&state)?
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_route_binding(
        &self,
        target: &RouteTarget,
    ) -> Result<(), NamespaceIntentStoreError> {
        let target = target.clone();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM route_bindings WHERE hostname = ?1 AND port = ?2",
                    params![target.hostname.as_str(), target.port.get()],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn replace_serving_target_entry(
        &self,
        state: ServingTargetEntry,
    ) -> Result<(), NamespaceIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO serving_targets (namespace_id, service_id, json) VALUES (?1, ?2, ?3)
                     ON CONFLICT(namespace_id, service_id) DO UPDATE SET json = excluded.json",
                    params![
                        state.namespace_id.as_str(),
                        state.service_id.as_str(),
                        to_json(&state)?
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_serving_target_entry(
        &self,
        entry: &ServingTargetEntry,
    ) -> Result<(), NamespaceIntentStoreError> {
        let entry = entry.clone();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM serving_targets WHERE namespace_id = ?1 AND service_id = ?2",
                    params![entry.namespace_id.as_str(), entry.service_id.as_str()],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn load(&self) -> Result<NamespaceIntentEvidence, NamespaceIntentStoreError> {
        self.store.call(load_evidence).await.map_err(store_error)
    }
}

fn load_evidence(conn: &mut Connection) -> Result<NamespaceIntentEvidence, rusqlite::Error> {
    Ok(NamespaceIntentEvidence {
        route_bindings: query_json_list(
            conn,
            "SELECT json FROM route_bindings ORDER BY hostname, port",
        )?,
        serving_target_entries: query_json_list(
            conn,
            "SELECT json FROM serving_targets ORDER BY namespace_id, service_id",
        )?,
    })
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceIntentEvidence {
    pub route_bindings: Vec<RouteBindingState>,
    pub serving_target_entries: Vec<ServingTargetEntry>,
}

#[derive(Debug, thiserror::Error)]
#[error("namespace intent store: {message}")]
pub struct NamespaceIntentStoreError {
    message: String,
}

fn store_error(error: CoreStoreError) -> NamespaceIntentStoreError {
    NamespaceIntentStoreError {
        message: error.to_string(),
    }
}
