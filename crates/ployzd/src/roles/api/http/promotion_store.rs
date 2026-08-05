use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    CorrosionNamespaceName, CorrosionPromotionRowObservation, CorrosionTable, NameClaim,
    NamedCorrosionDocument, NamespaceDocument, RouteBindingDocument, ServiceDocument,
    SqliteParameter, Statement, StoredRow, TransactionResponse, TransactionResult, read_named_rows,
    read_rows,
};
use ployz_core::ids::{ClusterId, NamespaceRowId, ServiceRowId};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};

use super::operation_evidence::PreparedPromotion;
use super::operation_finalizer::{
    PreparedPromotionStore, PromotionClaimOutcome, PromotionFinalizerStoreError,
    PromotionRequestDisposition, PromotionRowsObservation,
};

const MAX_SERVICE_CLAIM_ROWS: usize = 10_000;
const CLAIM_COURTESY_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(super) struct ResolvedFirstDeployNamespace {
    pub(super) id: NamespaceRowId,
    pub(super) exact_document: String,
    pub(super) document: NamespaceDocument,
}

#[derive(Debug)]
pub(super) enum FirstDeployNamespaceResolution {
    Missing,
    Ambiguous { namespace_ids: Vec<NamespaceRowId> },
    NotFirst { namespace_id: NamespaceRowId },
    Ready(ResolvedFirstDeployNamespace),
}

#[async_trait]
pub(super) trait FirstDeployPreflightStore: Send + Sync {
    async fn resolve_empty_namespace(
        &self,
        name: &CorrosionNamespaceName,
    ) -> Result<FirstDeployNamespaceResolution, PromotionFinalizerStoreError>;
}

/// Corrosion's exact, restart-safe adapter for a prepared service/container promotion.
#[derive(Clone)]
pub(super) struct CorrosionPreparedPromotionStore {
    client: CorrosionClient,
    cluster_id: ClusterId,
    claim_courtesy_wait: Duration,
}

#[async_trait]
impl FirstDeployPreflightStore for CorrosionPreparedPromotionStore {
    async fn resolve_empty_namespace(
        &self,
        name: &CorrosionNamespaceName,
    ) -> Result<FirstDeployNamespaceResolution, PromotionFinalizerStoreError> {
        let rows = self
            .query_many(
                Statement::with_params(
                    "SELECT id, document FROM namespaces WHERE json_extract(document, '$.cluster_id') = ? AND json_extract(document, '$.name') = ?",
                    vec![
                        SqliteParameter::Text(self.cluster_id.as_str().to_owned()),
                        SqliteParameter::Text(name.as_str().to_owned()),
                    ],
                ),
                MAX_SERVICE_CLAIM_ROWS,
            )
            .await?;
        let report = read_named_rows::<NamespaceDocument>(&self.cluster_id, rows);
        if !report.skipped.is_empty() {
            return Err(PromotionFinalizerStoreError::Protocol(format!(
                "namespace lookup contained {} rejected rows",
                report.skipped.len()
            )));
        }
        if report.accepted.is_empty() {
            return Ok(FirstDeployNamespaceResolution::Missing);
        }
        let mut accepted = report.accepted;
        accepted.sort_by(|left, right| left.id.cmp(&right.id));
        if accepted.len() > 1 {
            let namespace_ids = accepted
                .into_iter()
                .map(|row| {
                    NamespaceRowId::try_new(row.id.into_string()).map_err(|error| {
                        PromotionFinalizerStoreError::Protocol(format!(
                            "namespace row id was invalid: {error}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(FirstDeployNamespaceResolution::Ambiguous { namespace_ids });
        }
        let Some(namespace) = accepted.pop() else {
            return Ok(FirstDeployNamespaceResolution::Missing);
        };
        let namespace_id =
            NamespaceRowId::try_new(namespace.id.into_string()).map_err(|error| {
                PromotionFinalizerStoreError::Protocol(format!(
                    "namespace row id was invalid: {error}"
                ))
            })?;
        let services = self.query_many(namespace_has_service_statement(&namespace_id), 1);
        let routes = self.query_many(namespace_has_route_statement(&namespace_id), 1);
        let (services, routes) = tokio::try_join!(services, routes)?;
        let service_report = read_rows::<ServiceDocument>(&self.cluster_id, services);
        if !service_report.skipped.is_empty() {
            return Err(PromotionFinalizerStoreError::Protocol(
                "namespace service lookup contained a rejected row".to_owned(),
            ));
        }
        let route_report = read_rows::<RouteBindingDocument>(&self.cluster_id, routes);
        if !route_report.skipped.is_empty() {
            return Err(PromotionFinalizerStoreError::Protocol(
                "namespace route lookup contained a rejected row".to_owned(),
            ));
        }
        if !service_report.accepted.is_empty() || !route_report.accepted.is_empty() {
            return Ok(FirstDeployNamespaceResolution::NotFirst { namespace_id });
        }
        Ok(FirstDeployNamespaceResolution::Ready(
            ResolvedFirstDeployNamespace {
                id: namespace_id,
                exact_document: namespace.source.document,
                document: namespace.value,
            },
        ))
    }
}

fn namespace_has_service_statement(namespace_id: &NamespaceRowId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM services WHERE namespace_id = ? LIMIT 1",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

fn namespace_has_route_statement(namespace_id: &NamespaceRowId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM route_bindings WHERE namespace_id = ? LIMIT 1",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

impl CorrosionPreparedPromotionStore {
    #[must_use]
    pub(super) const fn new(client: CorrosionClient, cluster_id: ClusterId) -> Self {
        Self {
            client,
            cluster_id,
            claim_courtesy_wait: CLAIM_COURTESY_WAIT,
        }
    }

    async fn observe_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<PromotionRowsObservation, PromotionFinalizerStoreError> {
        let service = self.query_one(CorrosionTable::Services, prepared.service_id.as_str());
        let container = self.query_one(CorrosionTable::Containers, prepared.container_id.as_str());
        let (service, container) = tokio::try_join!(service, container)?;
        Ok(PromotionRowsObservation {
            service_row: observe_exact_row(service, &prepared.service_document, &self.cluster_id)?,
            container_row: observe_exact_row(
                container,
                &prepared.container_document,
                &self.cluster_id,
            )?,
        })
    }

    async fn query_one(
        &self,
        table: CorrosionTable,
        id: &str,
    ) -> Result<Vec<StoredRow>, PromotionFinalizerStoreError> {
        let statement = Statement::with_params(
            format!("SELECT id, document FROM {} WHERE id = ?", table.as_str()),
            vec![SqliteParameter::Text(id.to_owned())],
        );
        let mut stream = self.client.query(&statement).await.map_err(transport)?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(2))
            .await
            .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))
    }

    async fn query_many(
        &self,
        statement: Statement,
        limit: usize,
    ) -> Result<Vec<StoredRow>, PromotionFinalizerStoreError> {
        let mut stream = self.client.query(&statement).await.map_err(transport)?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(limit))
            .await
            .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))
    }

    async fn service_claim_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<Vec<StoredRow>, PromotionFinalizerStoreError> {
        let NameClaim::Service { namespace_id, name } = prepared.service_document.name_claim()
        else {
            return Err(PromotionFinalizerStoreError::Protocol(
                "prepared service document did not produce a service claim".to_owned(),
            ));
        };
        let statement = Statement::with_params(
            "SELECT id, document FROM services WHERE namespace_id = ? AND name = ?",
            vec![
                SqliteParameter::Text(namespace_id.as_str().to_owned()),
                SqliteParameter::Text(name),
            ],
        );
        let mut stream = self.client.query(&statement).await.map_err(transport)?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_SERVICE_CLAIM_ROWS))
            .await
            .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))
    }
}

#[async_trait]
impl PreparedPromotionStore for CorrosionPreparedPromotionStore {
    async fn converge_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>
    {
        validate_cluster(prepared, &self.cluster_id)?;
        let statements = converge_statements(prepared)?;
        let disposition = match self.client.execute(&statements).await {
            Ok(response) => classify_converge_response(&response)?,
            Err(_) => PromotionRequestDisposition::Uncertain,
        };
        let rows = self.observe_rows(prepared).await?;
        Ok((disposition, rows))
    }

    async fn adjudicate_service_claim(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<PromotionClaimOutcome, PromotionFinalizerStoreError> {
        validate_cluster(prepared, &self.cluster_id)?;
        tokio::time::sleep(self.claim_courtesy_wait).await;
        let report = read_named_rows::<ServiceDocument>(
            &self.cluster_id,
            self.service_claim_rows(prepared).await?,
        );
        if !report.skipped.is_empty() {
            return Err(PromotionFinalizerStoreError::Protocol(format!(
                "service claim contained {} rejected rows",
                report.skipped.len()
            )));
        }
        let Some(winner) = report.accepted.first() else {
            return Err(PromotionFinalizerStoreError::Protocol(
                "prepared service claim was not visible".to_owned(),
            ));
        };
        let winner_id = ServiceRowId::try_new(winner.id.as_str().to_owned()).map_err(|error| {
            PromotionFinalizerStoreError::Protocol(format!(
                "service claim winner id was invalid: {error}"
            ))
        })?;
        if winner_id == prepared.service_id {
            Ok(PromotionClaimOutcome::Won)
        } else {
            Ok(PromotionClaimOutcome::Lost { winner: winner_id })
        }
    }

    async fn delete_exact_losing_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<PromotionRowsObservation, PromotionFinalizerStoreError> {
        validate_cluster(prepared, &self.cluster_id)?;
        let service = serde_json::to_string(&prepared.service_document)
            .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
        let container = serde_json::to_string(&prepared.container_document)
            .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
        let statements = [
            exact_delete_statement(
                CorrosionTable::Containers,
                prepared.container_id.as_str(),
                container,
            ),
            exact_delete_statement(
                CorrosionTable::Services,
                prepared.service_id.as_str(),
                service,
            ),
        ];
        if self.client.execute(&statements).await.is_err() {
            // A lost response is not a failed cleanup. Exact readback is the authority.
        }
        self.observe_rows(prepared).await
    }
}

fn validate_cluster(
    prepared: &PreparedPromotion,
    expected: &ClusterId,
) -> Result<(), PromotionFinalizerStoreError> {
    if &prepared.service_document.cluster_id != expected
        || &prepared.container_document.cluster_id != expected
    {
        return Err(PromotionFinalizerStoreError::Protocol(
            "prepared promotion belongs to another cluster".to_owned(),
        ));
    }
    Ok(())
}

fn converge_statements(
    prepared: &PreparedPromotion,
) -> Result<[Statement; 2], PromotionFinalizerStoreError> {
    let service = serde_json::to_string(&prepared.service_document)
        .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
    let container = serde_json::to_string(&prepared.container_document)
        .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
    let exact_pair = ExactPromotionPair {
        namespace_id: prepared.namespace_id.as_str(),
        namespace_document: &prepared.exact_namespace_document,
        service_id: prepared.service_id.as_str(),
        service_document: &service,
        container_id: prepared.container_id.as_str(),
        container_document: &container,
    };
    Ok([
        conditional_service_insert_statement(
            CorrosionTable::Services,
            prepared.service_id.as_str(),
            service.clone(),
            &exact_pair,
        ),
        conditional_container_insert_statement(
            CorrosionTable::Containers,
            prepared.container_id.as_str(),
            container,
            prepared.namespace_id.as_str(),
            &prepared.exact_namespace_document,
            prepared.service_id.as_str(),
            &service,
        ),
    ])
}

struct ExactPromotionPair<'a> {
    namespace_id: &'a str,
    namespace_document: &'a str,
    service_id: &'a str,
    service_document: &'a str,
    container_id: &'a str,
    container_document: &'a str,
}

fn conditional_service_insert_statement(
    table: CorrosionTable,
    id: &str,
    document: String,
    exact: &ExactPromotionPair<'_>,
) -> Statement {
    Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM namespaces WHERE id = ? AND document = ?) AND NOT EXISTS (SELECT 1 FROM route_bindings WHERE namespace_id = ?) AND NOT EXISTS (SELECT 1 FROM services WHERE namespace_id = ? AND NOT (id = ? AND document = ?)) AND (NOT EXISTS (SELECT 1 FROM containers WHERE id = ?) OR EXISTS (SELECT 1 FROM containers WHERE id = ? AND document = ?)) ON CONFLICT(id) DO NOTHING",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(document),
            SqliteParameter::Text(exact.namespace_id.to_owned()),
            SqliteParameter::Text(exact.namespace_document.to_owned()),
            SqliteParameter::Text(exact.namespace_id.to_owned()),
            SqliteParameter::Text(exact.namespace_id.to_owned()),
            SqliteParameter::Text(exact.service_id.to_owned()),
            SqliteParameter::Text(exact.service_document.to_owned()),
            SqliteParameter::Text(exact.container_id.to_owned()),
            SqliteParameter::Text(exact.container_id.to_owned()),
            SqliteParameter::Text(exact.container_document.to_owned()),
        ],
    )
}

fn conditional_container_insert_statement(
    table: CorrosionTable,
    id: &str,
    document: String,
    namespace_id: &str,
    exact_namespace_document: &str,
    service_id: &str,
    exact_service_document: &str,
) -> Statement {
    Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM namespaces WHERE id = ? AND document = ?) AND NOT EXISTS (SELECT 1 FROM route_bindings WHERE namespace_id = ?) AND NOT EXISTS (SELECT 1 FROM services WHERE namespace_id = ? AND NOT (id = ? AND document = ?)) AND EXISTS (SELECT 1 FROM services WHERE id = ? AND document = ?) ON CONFLICT(id) DO NOTHING",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(document),
            SqliteParameter::Text(namespace_id.to_owned()),
            SqliteParameter::Text(exact_namespace_document.to_owned()),
            SqliteParameter::Text(namespace_id.to_owned()),
            SqliteParameter::Text(namespace_id.to_owned()),
            SqliteParameter::Text(service_id.to_owned()),
            SqliteParameter::Text(exact_service_document.to_owned()),
            SqliteParameter::Text(service_id.to_owned()),
            SqliteParameter::Text(exact_service_document.to_owned()),
        ],
    )
}

fn exact_delete_statement(table: CorrosionTable, id: &str, document: String) -> Statement {
    Statement::with_params(
        format!(
            "DELETE FROM {} WHERE id = ? AND document = ?",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(document),
        ],
    )
}

fn classify_converge_response(
    response: &TransactionResponse,
) -> Result<PromotionRequestDisposition, PromotionFinalizerStoreError> {
    let [service, container] = response.results.as_slice() else {
        return Err(PromotionFinalizerStoreError::Protocol(format!(
            "promotion transaction returned {} results",
            response.results.len()
        )));
    };
    let service = rows_affected(service)?;
    let container = rows_affected(container)?;
    match (service, container) {
        (1, 1) | (1, 0) | (0, 1) => Ok(PromotionRequestDisposition::Accepted),
        (0, 0) => Ok(PromotionRequestDisposition::Rejected),
        _ => Err(PromotionFinalizerStoreError::Protocol(format!(
            "atomic promotion reported invalid write counts: service={service}, container={container}"
        ))),
    }
}

fn rows_affected(result: &TransactionResult) -> Result<usize, PromotionFinalizerStoreError> {
    match result {
        TransactionResult::Success(success) => Ok(success.rows_affected),
        TransactionResult::Error(error) => Err(PromotionFinalizerStoreError::Protocol(format!(
            "promotion statement failed: {}",
            error.message
        ))),
    }
}

fn observe_exact_row<Document>(
    rows: Vec<StoredRow>,
    expected: &Document,
    cluster_id: &ClusterId,
) -> Result<CorrosionPromotionRowObservation, PromotionFinalizerStoreError>
where
    Document: ployz_core::corrosion::OrdinaryCorrosionDocument + PartialEq,
{
    if rows.is_empty() {
        return Ok(CorrosionPromotionRowObservation::Absent);
    }
    let report = read_rows::<Document>(cluster_id, rows);
    let [accepted] = report.accepted.as_slice() else {
        return Ok(CorrosionPromotionRowObservation::Mismatch);
    };
    if !report.skipped.is_empty() || &accepted.value != expected {
        return Ok(CorrosionPromotionRowObservation::Mismatch);
    }
    Ok(CorrosionPromotionRowObservation::Exact)
}

fn transport(error: crate::corrosion::CorrosionClientError) -> PromotionFinalizerStoreError {
    PromotionFinalizerStoreError::Transport(error.to_string())
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        CorrosionPromotionRowObservation, SqliteParameter, TransactionResult, TransactionSuccess,
    };

    use super::{
        ExactPromotionPair, PromotionRequestDisposition, classify_converge_response,
        conditional_container_insert_statement, conditional_service_insert_statement,
        namespace_has_route_statement, namespace_has_service_statement,
    };

    fn service_insert(
        service_document: &str,
        namespace_document: &str,
        container_document: &str,
    ) -> ployz_core::corrosion::Statement {
        conditional_service_insert_statement(
            ployz_core::corrosion::CorrosionTable::Services,
            "service",
            service_document.to_owned(),
            &ExactPromotionPair {
                namespace_id: "namespace",
                namespace_document,
                service_id: "service",
                service_document,
                container_id: "container",
                container_document,
            },
        )
    }

    fn response(service: usize, container: usize) -> ployz_core::corrosion::TransactionResponse {
        ployz_core::corrosion::TransactionResponse {
            results: vec![
                TransactionResult::Success(TransactionSuccess {
                    rows_affected: service,
                    time: 0.0,
                }),
                TransactionResult::Success(TransactionSuccess {
                    rows_affected: container,
                    time: 0.0,
                }),
            ],
            time: 0.0,
            version: None,
            actor_id: None,
        }
    }

    #[test]
    fn convergence_is_conditioned_on_the_exact_namespace_document() {
        let statement = service_insert(
            "service-document",
            "namespace-document",
            "container-document",
        );
        let ployz_core::corrosion::Statement::WithParams(sql, params) = statement else {
            panic!("parameterized statement");
        };
        assert_eq!(
            params,
            vec![
                SqliteParameter::Text("service".to_owned()),
                SqliteParameter::Text("service-document".to_owned()),
                SqliteParameter::Text("namespace".to_owned()),
                SqliteParameter::Text("namespace-document".to_owned()),
                SqliteParameter::Text("namespace".to_owned()),
                SqliteParameter::Text("namespace".to_owned()),
                SqliteParameter::Text("service".to_owned()),
                SqliteParameter::Text("service-document".to_owned()),
                SqliteParameter::Text("container".to_owned()),
                SqliteParameter::Text("container".to_owned()),
                SqliteParameter::Text("container-document".to_owned()),
            ]
        );
        assert!(sql.contains("WHERE EXISTS"));
        assert!(sql.contains("document = ?"));
        assert!(sql.contains("ON CONFLICT(id) DO NOTHING"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM containers"));
        assert!(sql.contains("OR EXISTS (SELECT 1 FROM containers"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM route_bindings"));
        assert!(sql.contains("NOT (id = ? AND document = ?)"));

        let ployz_core::corrosion::Statement::WithParams(container_sql, _) =
            conditional_container_insert_statement(
                ployz_core::corrosion::CorrosionTable::Containers,
                "container",
                "container-document".to_owned(),
                "namespace",
                "namespace-document",
                "service",
                "service-document",
            )
        else {
            panic!("parameterized statement");
        };
        assert!(container_sql.contains("EXISTS (SELECT 1 FROM services"));
    }

    #[test]
    fn only_an_exact_atomic_pair_is_accepted() {
        assert_eq!(
            classify_converge_response(&response(1, 1)).expect("accepted"),
            PromotionRequestDisposition::Accepted
        );
        assert_eq!(
            classify_converge_response(&response(0, 0)).expect("rejected"),
            PromotionRequestDisposition::Rejected
        );
        assert_eq!(
            classify_converge_response(&response(1, 0)).expect("service healed"),
            PromotionRequestDisposition::Accepted
        );
        assert_eq!(
            classify_converge_response(&response(0, 1)).expect("container healed"),
            PromotionRequestDisposition::Accepted
        );
        let _ = CorrosionPromotionRowObservation::Exact;
    }

    #[test]
    fn mismatched_half_blocks_the_absent_half_in_both_directions() {
        let ployz_core::corrosion::Statement::WithParams(service_sql, service_params) =
            service_insert("exact-service", "exact-namespace", "exact-container")
        else {
            panic!("service statement");
        };
        assert!(service_sql.contains("containers WHERE id = ? AND document = ?"));
        assert_eq!(
            service_params.last(),
            Some(&SqliteParameter::Text("exact-container".to_owned()))
        );

        let ployz_core::corrosion::Statement::WithParams(container_sql, container_params) =
            conditional_container_insert_statement(
                ployz_core::corrosion::CorrosionTable::Containers,
                "container",
                "exact-container".to_owned(),
                "namespace",
                "exact-namespace",
                "service",
                "exact-service",
            )
        else {
            panic!("container statement");
        };
        assert!(container_sql.contains("services WHERE id = ? AND document = ?"));
        assert_eq!(
            container_params.last(),
            Some(&SqliteParameter::Text("exact-service".to_owned()))
        );
    }

    #[test]
    fn exact_half_healing_is_a_valid_accepted_request() {
        assert_eq!(
            classify_converge_response(&response(1, 0)).expect("heal service half"),
            PromotionRequestDisposition::Accepted
        );
        assert_eq!(
            classify_converge_response(&response(0, 1)).expect("heal container half"),
            PromotionRequestDisposition::Accepted
        );
    }

    #[test]
    fn promotion_rechecks_namespace_emptiness_after_preflight() {
        let ployz_core::corrosion::Statement::WithParams(service_sql, _) =
            service_insert("exact-service", "exact-namespace", "exact-container")
        else {
            panic!("service statement");
        };
        assert!(service_sql.contains("route_bindings WHERE namespace_id = ?"));
        assert!(
            service_sql
                .contains("services WHERE namespace_id = ? AND NOT (id = ? AND document = ?)")
        );
    }

    #[test]
    fn nonempty_namespace_preflight_is_a_bounded_existence_read() {
        let namespace = ployz_core::ids::NamespaceRowId::try_new("01J00000000000000000000001")
            .expect("namespace");
        let ployz_core::corrosion::Statement::WithParams(sql, _) =
            namespace_has_service_statement(&namespace)
        else {
            panic!("parameterized statement");
        };
        assert!(sql.ends_with("LIMIT 1"));
        let ployz_core::corrosion::Statement::WithParams(route_sql, _) =
            namespace_has_route_statement(&namespace)
        else {
            panic!("parameterized route statement");
        };
        assert!(route_sql.contains("FROM route_bindings"));
        assert!(route_sql.ends_with("LIMIT 1"));
    }
}
