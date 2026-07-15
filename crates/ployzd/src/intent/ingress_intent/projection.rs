use ployz_core::ingress::IngressEndpointProjectionIdentity;
use rusqlite::{Connection, OptionalExtension};

use crate::core_store::{CoreStore, CoreStoreError, from_json, query_json, to_json};
use crate::ingress_endpoint::ProjectionEvidenceRecord;

const SINGLETON_ID: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressProjectionWrite {
    Stored,
    Unchanged,
    Conflict {
        current: Option<IngressEndpointProjectionIdentity>,
    },
}

#[derive(Debug, Clone)]
pub struct IngressProjectionStore {
    store: CoreStore,
}

impl IngressProjectionStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub(crate) async fn load(&self) -> Result<Option<ProjectionEvidenceRecord>, CoreStoreError> {
        self.store
            .call(|conn| {
                query_json(
                    conn,
                    "SELECT json FROM ingress_endpoint_projection WHERE id = ?1",
                    SINGLETON_ID,
                )
            })
            .await
    }

    pub(crate) async fn load_publishable_gateway_ids(
        &self,
    ) -> Result<Vec<ployz_core::ids::MachineId>, CoreStoreError> {
        Ok(self
            .load()
            .await?
            .map(|record| record.publishable_gateway_ids)
            .unwrap_or_default())
    }

    pub(crate) async fn compare_and_replace(
        &self,
        expected: Option<IngressEndpointProjectionIdentity>,
        next: ProjectionEvidenceRecord,
    ) -> Result<IngressProjectionWrite, CoreStoreError> {
        self.store
            .call(move |conn| compare_and_replace_projection(conn, expected, &next))
            .await
    }
}

fn compare_and_replace_projection(
    conn: &mut Connection,
    expected: Option<IngressEndpointProjectionIdentity>,
    next: &ProjectionEvidenceRecord,
) -> Result<IngressProjectionWrite, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let current: Option<ProjectionEvidenceRecord> = transaction
        .query_row(
            "SELECT json FROM ingress_endpoint_projection WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| from_json(&json))
        .transpose()?;
    let current_identity = current.as_ref().map(|record| record.projection.identity());
    if current_identity != expected {
        return Ok(IngressProjectionWrite::Conflict {
            current: current_identity,
        });
    }
    if current.as_ref() == Some(next) {
        return Ok(IngressProjectionWrite::Unchanged);
    }
    transaction.execute(
        "INSERT INTO ingress_endpoint_projection (id, json) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json",
        [to_json(next)?],
    )?;
    transaction.commit()?;
    Ok(IngressProjectionWrite::Stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ingress::{IngressEndpointProjection, IngressEndpointProjectionState};
    use ployz_core::state::ControlPlaneEpoch;

    fn projection(revision: u64) -> IngressEndpointProjection {
        IngressEndpointProjection {
            control_plane_epoch: ControlPlaneEpoch::initial(),
            revision,
            state: IngressEndpointProjectionState::Pending,
        }
    }

    fn projection_record(revision: u64) -> ProjectionEvidenceRecord {
        ProjectionEvidenceRecord {
            projection: projection(revision),
            candidate_outcomes: Vec::new(),
            publishable_gateway_ids: Vec::new(),
        }
    }

    #[tokio::test]
    async fn projection_compare_and_replace_rejects_stale_writer() {
        let store = IngressProjectionStore::new(CoreStore::open_in_memory().await.expect("store"));
        store
            .compare_and_replace(None, projection_record(1))
            .await
            .expect("initial write");

        let outcome = store
            .compare_and_replace(None, projection_record(2))
            .await
            .expect("compare");

        assert_eq!(
            outcome,
            IngressProjectionWrite::Conflict {
                current: Some(projection(1).identity())
            }
        );
    }
}
