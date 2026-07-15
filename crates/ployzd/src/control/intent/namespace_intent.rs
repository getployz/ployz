//! Core-local namespace intent (route bindings and serving targets), in SQLite.

use crate::control::store::{CoreStore, CoreStoreError, query_json_list, to_json};
use ployz_core::deploy::VolumeName;
use ployz_core::ids::NamespaceId;
use ployz_core::intent::RouteBindingState;
use ployz_core::intent::ServingTargetEntry;
use ployz_core::intent::VolumePinState;
use ployz_core::operation::RouteTarget;

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
            .call(move |conn| upsert_route_binding(conn, &state))
            .await
            .map_err(store_error)
    }

    /// Promote one deploy phase as one intent decision. Route bindings and
    /// serving entries become visible together or not at all.
    pub async fn commit_deploy_phase(
        &self,
        route_bindings: Vec<RouteBindingState>,
        route_binding_removals: Vec<RouteTarget>,
        serving_target_entries: Vec<ServingTargetEntry>,
    ) -> Result<(), NamespaceIntentStoreError> {
        self.store
            .call(move |conn| {
                let transaction = conn.transaction()?;
                for state in route_bindings {
                    upsert_route_binding(&transaction, &state)?;
                }
                for target in route_binding_removals {
                    transaction.execute(
                        "DELETE FROM route_bindings WHERE hostname = ?1",
                        [target.hostname.as_str()],
                    )?;
                }
                for state in serving_target_entries {
                    transaction.execute(
                        "INSERT INTO serving_targets (namespace_id, service_id, json) VALUES (?1, ?2, ?3)
                         ON CONFLICT(namespace_id, service_id) DO UPDATE SET json = excluded.json",
                        params![
                            state.namespace_id.as_str(),
                            state.service_id.as_str(),
                            to_json(&state)?
                        ],
                    )?;
                }
                transaction.commit()
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
                    "DELETE FROM route_bindings WHERE hostname = ?1",
                    [target.hostname.as_str()],
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

    pub async fn replace_volume_pin(
        &self,
        state: VolumePinState,
    ) -> Result<(), NamespaceIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO volume_pins (namespace_id, volume_name, json) VALUES (?1, ?2, ?3)
                     ON CONFLICT(namespace_id, volume_name) DO UPDATE SET json = excluded.json",
                    params![
                        state.namespace_id.as_str(),
                        state.volume_name.as_str(),
                        to_json(&state)?
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_volume_pin(
        &self,
        namespace_id: &NamespaceId,
        volume_name: &VolumeName,
    ) -> Result<(), NamespaceIntentStoreError> {
        let namespace_id = namespace_id.clone();
        let volume_name = volume_name.clone();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM volume_pins WHERE namespace_id = ?1 AND volume_name = ?2",
                    params![namespace_id.as_str(), volume_name.as_str()],
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

fn upsert_route_binding(
    conn: &Connection,
    state: &RouteBindingState,
) -> Result<(), rusqlite::Error> {
    let changed = conn.execute(
        "INSERT INTO route_bindings (hostname, route_binding_id, json) VALUES (?1, ?2, ?3)
         ON CONFLICT(hostname) DO UPDATE SET json = excluded.json
         WHERE route_bindings.route_binding_id = excluded.route_binding_id",
        params![
            state.target.hostname.as_str(),
            state.id.as_str(),
            to_json(state)?
        ],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::other(format!(
                "route binding hostname {} already belongs to another identity",
                state.target.hostname.as_str()
            )),
        )));
    }
    Ok(())
}

fn load_evidence(conn: &mut Connection) -> Result<NamespaceIntentEvidence, rusqlite::Error> {
    Ok(NamespaceIntentEvidence {
        route_bindings: query_json_list(conn, "SELECT json FROM route_bindings ORDER BY hostname")?,
        serving_target_entries: query_json_list(
            conn,
            "SELECT json FROM serving_targets ORDER BY namespace_id, service_id",
        )?,
        volume_pins: query_json_list(
            conn,
            "SELECT json FROM volume_pins ORDER BY namespace_id, volume_name",
        )?,
    })
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceIntentEvidence {
    pub route_bindings: Vec<RouteBindingState>,
    pub serving_target_entries: Vec<ServingTargetEntry>,
    #[serde(default)]
    pub volume_pins: Vec<VolumePinState>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{ImageReference, ReplicaCount};
    use ployz_core::ids::{NamespaceRevisionEntryId, RouteBindingId, ServiceId};
    use ployz_core::ingress::RouteBindingOrigin;
    use ployz_core::operation::{RouteHostname, RoutePort};

    #[tokio::test]
    async fn same_hostname_reuses_its_route_binding_identity() {
        let store = NamespaceIntentStore::new(CoreStore::open_in_memory().await.expect("store"));
        let first = route("route_1", "service_a");
        store
            .replace_route_binding(first.clone())
            .await
            .expect("insert route");
        let updated = RouteBindingState {
            service_id: service_id("service_b"),
            ..first.clone()
        };
        store
            .replace_route_binding(updated.clone())
            .await
            .expect("update route");

        assert_eq!(
            store.load().await.expect("load").route_bindings.as_slice(),
            std::slice::from_ref(&updated)
        );
        assert!(
            store
                .replace_route_binding(route("route_2", "service_c"))
                .await
                .is_err()
        );
        assert_eq!(
            store.load().await.expect("reload").route_bindings,
            [updated]
        );
    }

    #[tokio::test]
    async fn detached_hostname_accepts_a_fresh_route_binding_identity() {
        let store = NamespaceIntentStore::new(CoreStore::open_in_memory().await.expect("store"));
        let first = route("route_1", "service_a");
        store
            .replace_route_binding(first.clone())
            .await
            .expect("insert route");
        store
            .remove_route_binding(&first.target)
            .await
            .expect("detach route");

        let recreated = route("route_2", "service_a");
        store
            .replace_route_binding(recreated.clone())
            .await
            .expect("recreate route");

        assert_eq!(
            store.load().await.expect("load").route_bindings,
            [recreated]
        );
    }

    #[tokio::test]
    async fn route_identity_conflict_rolls_back_the_phase_commit() {
        let store = NamespaceIntentStore::new(CoreStore::open_in_memory().await.expect("store"));
        let existing = route("route_1", "service_a");
        store
            .replace_route_binding(existing.clone())
            .await
            .expect("insert route");

        assert!(
            store
                .commit_deploy_phase(
                    vec![
                        route_at("route_3", "other.example.com", "service_b"),
                        route("route_2", "service_b"),
                    ],
                    Vec::new(),
                    vec![serving_entry()],
                )
                .await
                .is_err()
        );
        let evidence = store.load().await.expect("load");
        assert_eq!(evidence.route_bindings, [existing]);
        assert!(evidence.serving_target_entries.is_empty());
    }

    fn route(id: &str, service: &str) -> RouteBindingState {
        route_at(id, "app.example.com", service)
    }

    fn route_at(id: &str, hostname: &str, service: &str) -> RouteBindingState {
        RouteBindingState {
            id: RouteBindingId::try_new(id).expect("route binding id"),
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            target: RouteTarget::new(RouteHostname::try_new(hostname).expect("hostname")),
            endpoint_port: RoutePort::try_new(8080).expect("endpoint port"),
            service_id: service_id(service),
            origin: RouteBindingOrigin::Declared,
        }
    }

    fn serving_entry() -> ServingTargetEntry {
        ServingTargetEntry {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            service_id: service_id("service_b"),
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("revision_entry_b")
                .expect("revision entry id"),
            image: ImageReference::try_new("nginx:latest").expect("image"),
            desired_replicas: ReplicaCount::try_new(1).expect("replicas"),
            volume_names: Vec::new(),
        }
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("service id")
    }
}
