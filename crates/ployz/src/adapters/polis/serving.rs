//! Serving route rows over Polis store primitives.

use crate::acme::Hostname;
use crate::error::{PrimitiveFailure, ServingFailure};
use crate::operation::{
    IdempotencyKey, MutationContext, MutationWriteIdentity, OperationId, ScopeId,
};
use crate::serving::{
    RouteId, ServingCommitId, ServingCommitIdentity, ServingCommitObservation,
    ServingCommitRequest, ServingCommittedSnapshot, ServingGeneration, ServingSnapshotPort,
    ServingTarget,
};

use super::scalars;

const SERVING_ROUTES_TABLE: &str = "ployz_serving_routes";
const SERVING_ROUTE_COLUMNS: &[polis::StoreTableColumn] = &[
    polis::StoreTableColumn::new(
        "scope",
        "TEXT",
        polis::StorePrimaryKey::order(1),
        Some("length(trim(scope)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "route",
        "TEXT",
        polis::StorePrimaryKey::order(2),
        Some("length(trim(route)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "hostname",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(hostname)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "target",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(target)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "generation",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("generation >= 0"),
    ),
    polis::StoreTableColumn::new(
        "commit_id",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(commit_id)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "operation",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(operation)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "idempotency",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(idempotency)) > 0"),
    ),
];

#[derive(Clone)]
pub(crate) struct CorrosionServingSnapshots {
    store: polis::CorrosionStore,
}

impl CorrosionServingSnapshots {
    #[must_use]
    pub(crate) fn new(store: polis::CorrosionStore) -> Self {
        Self { store }
    }
}

pub(crate) fn start_corrosion_serving_snapshots(
    store: polis::CorrosionStore,
) -> impl ServingSnapshotPort {
    CorrosionServingSnapshots::new(store)
}

#[cfg(test)]
fn serving_snapshot_schema_statements() -> Result<Vec<polis::StoreStatement>, polis::StoreError> {
    polis::schema_statements(&schema_sql())
}

#[cfg(test)]
fn schema_sql() -> String {
    polis::create_table_sql(SERVING_ROUTES_TABLE, SERVING_ROUTE_COLUMNS)
}

impl ServingSnapshotPort for CorrosionServingSnapshots {
    async fn commit_snapshot(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<(), ServingFailure> {
        let row = ServingRouteRow::from_request(context, request)?;
        self.replace_current_route(&row)
            .await
            .map_err(map_serving_store_error)
    }

    async fn commit_status(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<ServingCommitObservation, ServingFailure> {
        Ok(
            match self
                .current(context.authority().scope(), request.route())
                .await?
            {
                Some(row) => ServingCommitObservation::Current(row.snapshot()),
                None => ServingCommitObservation::Missing,
            },
        )
    }
}

impl CorrosionServingSnapshots {
    async fn current(
        &self,
        scope: &ScopeId,
        route: &RouteId,
    ) -> Result<Option<ServingRouteRow>, ServingFailure> {
        self.current_route(scope, route)
            .await
            .map_err(map_serving_store_error)
    }
}

impl CorrosionServingSnapshots {
    async fn current_route(
        &self,
        scope: &ScopeId,
        route: &RouteId,
    ) -> Result<Option<ServingRouteRow>, polis::StoreError> {
        let statement = polis::StoreStatement::with_params(
            format!(
                "SELECT {}
                FROM {SERVING_ROUTES_TABLE}
                WHERE scope = ?1
                  AND route = ?2",
                polis::select_columns(SERVING_ROUTE_COLUMNS)
            ),
            vec![
                polis::StoreParam::Text(scope.as_str().to_string()),
                polis::StoreParam::Text(route.as_str().to_string()),
            ],
        )?;
        let rows = self
            .store
            .query(&statement, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        let Some(row) = rows.rows().first() else {
            return Ok(None);
        };
        decode_serving_route_row(row).map(Some)
    }

    async fn replace_current_route(&self, row: &ServingRouteRow) -> Result<(), polis::StoreError> {
        self.store
            .execute_transaction(
                &[serving_route_replace_current_statement(row)?],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServingRouteRow {
    snapshot: ServingCommittedSnapshot,
    write: MutationWriteIdentity,
}

impl ServingRouteRow {
    fn from_request(
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<Self, ServingFailure> {
        Ok(Self {
            snapshot: ServingCommittedSnapshot::from_request(request)?,
            write: MutationWriteIdentity::from_context(context),
        })
    }

    fn snapshot(&self) -> ServingCommittedSnapshot {
        self.snapshot.clone()
    }
}

fn serving_route_replace_current_statement(
    row: &ServingRouteRow,
) -> Result<polis::StoreStatement, polis::StoreError> {
    polis::StoreStatement::with_params(
        format!(
            "INSERT INTO {SERVING_ROUTES_TABLE} (
            scope,
            route,
            hostname,
            target,
            generation,
            commit_id,
            operation,
            idempotency
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(scope, route) DO UPDATE SET
            hostname = excluded.hostname,
            target = excluded.target,
            generation = excluded.generation,
            commit_id = excluded.commit_id,
            operation = excluded.operation,
            idempotency = excluded.idempotency"
        ),
        vec![
            polis::StoreParam::Text(row.write.scope().as_str().to_string()),
            polis::StoreParam::Text(row.snapshot.identity().route().as_str().to_string()),
            polis::StoreParam::Text(row.snapshot.identity().hostname().as_str().to_string()),
            polis::StoreParam::Text(row.snapshot.identity().target().as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(
                row.snapshot.identity().generation().value(),
            )?),
            polis::StoreParam::Text(row.snapshot.commit_id().as_str().to_string()),
            polis::StoreParam::Text(row.write.operation().as_str().to_string()),
            polis::StoreParam::Text(row.write.idempotency().as_str().to_string()),
        ],
    )
}

fn decode_serving_route_row(row: &polis::StoreRow) -> Result<ServingRouteRow, polis::StoreError> {
    let route =
        RouteId::parse(row.text("route")?).map_err(|_| polis::StoreError::MalformedPayload)?;
    let hostname =
        Hostname::parse(row.text("hostname")?).map_err(|_| polis::StoreError::MalformedPayload)?;
    let target = ServingTarget::parse(row.text("target")?)
        .map_err(|_| polis::StoreError::MalformedPayload)?;
    let generation = scalars::u64_from_i64(row.integer("generation")?)?;
    let commit_id = ServingCommitId::parse(row.text("commit_id")?)
        .map_err(|_| polis::StoreError::MalformedPayload)?;
    let identity =
        ServingCommitIdentity::new(route, hostname, target, ServingGeneration::new(generation));
    Ok(ServingRouteRow {
        snapshot: ServingCommittedSnapshot::from_stored_parts(commit_id, identity)
            .map_err(|_| polis::StoreError::MalformedPayload)?,
        write: MutationWriteIdentity::new(
            ScopeId::parse(row.text("scope")?).map_err(map_primitive_payload_error)?,
            OperationId::parse(row.text("operation")?).map_err(map_primitive_payload_error)?,
            IdempotencyKey::parse(row.text("idempotency")?).map_err(map_primitive_payload_error)?,
        ),
    })
}

fn map_primitive_payload_error(_error: PrimitiveFailure) -> polis::StoreError {
    polis::StoreError::MalformedPayload
}

fn map_serving_store_error(error: polis::StoreError) -> ServingFailure {
    match error {
        polis::StoreError::MalformedPayload => ServingFailure::SnapshotRejected,
        polis::StoreError::Timeout
        | polis::StoreError::MissedChange { .. }
        | polis::StoreError::Stream { .. }
        | polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => ServingFailure::LiveObservationUnknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_row_rejects_mismatched_commit_id() {
        let row = polis::StoreRow::from_fields([
            ("route", polis::StoreValue::Text("route-a".to_string())),
            (
                "hostname",
                polis::StoreValue::Text("app.example.com".to_string()),
            ),
            ("target", polis::StoreValue::Text("gateway-a".to_string())),
            ("generation", polis::StoreValue::Integer(7)),
            (
                "commit_id",
                polis::StoreValue::Text("not-the-commit".to_string()),
            ),
            ("scope", polis::StoreValue::Text("cluster".to_string())),
            ("operation", polis::StoreValue::Text("deploy-1".to_string())),
            ("idempotency", polis::StoreValue::Text("idem-1".to_string())),
        ])
        .expect("row");

        assert_eq!(
            decode_serving_route_row(&row),
            Err(polis::StoreError::MalformedPayload)
        );
    }

    #[test]
    fn snapshot_match_checks_commit_id_invariant() {
        let request = request(7, "gateway-a");
        let snapshot = ServingCommittedSnapshot::from_request(&request).expect("snapshot");

        assert!(snapshot.matches_request(&request));
    }

    #[test]
    fn schema_creates_serving_route_table_only() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in serving_snapshot_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }

        let table: String = conn
            .query_row(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'ployz_serving_routes'",
                [],
                |row| row.get(0),
            )
            .expect("table");
        assert_eq!(table, "ployz_serving_routes");
    }

    fn request(generation: u64, target: &str) -> ServingCommitRequest {
        ServingCommitRequest::replace_current_route(
            RouteId::parse("route-a").expect("route"),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse(target).expect("target"),
            ServingGeneration::new(generation),
        )
    }
}
