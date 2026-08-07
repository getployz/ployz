use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    ContainerDocument, CorrosionNamespaceName, CorrosionPromotionRowObservation,
    CorrosionServiceName, CorrosionTable, NameClaim, NamedCorrosionDocument, NamespaceDocument,
    RouteBindingDocument, ServiceDocument, SqliteParameter, Statement, StoredRow,
    TransactionResponse, TransactionResult, read_named_rows, read_rows,
};
use ployz_core::ids::{ClusterId, ContainerId, NamespaceRowId, ServiceRowId};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};

use super::operation_evidence::{
    PreparedDeployContainer, PreparedPromotion, PreparedRedeployIntent,
};
use super::operation_finalizer::{
    PreparedPromotionStore, PromotionClaimOutcome, PromotionFinalizerStoreError,
    PromotionRequestDisposition, PromotionRowsObservation,
};

const MAX_SERVICE_CLAIM_ROWS: usize = 10_000;
const CLAIM_COURTESY_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(super) struct ResolvedNamespace {
    pub(super) id: NamespaceRowId,
    pub(super) exact_document: String,
    pub(super) document: NamespaceDocument,
}

/// One service row with the exact Corrosion document used for conditional writes.
#[derive(Debug)]
pub(super) struct ObservedService {
    pub(super) id: ServiceRowId,
    pub(super) exact_document: String,
    pub(super) document: ServiceDocument,
}

/// One container row with the exact Corrosion document used for exact deletes.
#[derive(Debug, Clone)]
pub(super) struct ObservedContainer {
    pub(super) id: ContainerId,
    pub(super) exact_document: String,
    pub(super) document: ContainerDocument,
}

/// Typed admission triage for one requested (namespace name, service name) deploy.
///
/// The deploy driver resolves this once at admission, then carries the exact
/// documents forward: `FirstDeploy` feeds the existing first-deploy
/// convergence, `Redeploy` feeds [`CorrosionPreparedPromotionStore::converge_redeploy_rows`]
/// with the incumbent's exact document as the CAS guard, and every other
/// variant is a refusal the driver maps to a typed deploy failure.
#[derive(Debug)]
pub(super) enum DeployAdmission {
    /// The namespace exists and holds no services and no route bindings.
    FirstDeploy {
        namespace: ResolvedNamespace,
    },
    /// The namespace's only service matches the requested name exactly.
    Redeploy {
        namespace: ResolvedNamespace,
        incumbent: Box<ObservedService>,
    },
    NamespaceMissing,
    NamespaceAmbiguous {
        namespace_ids: Vec<NamespaceRowId>,
    },
    /// The namespace holds one service whose name differs from the request;
    /// deploying a new service into a populated namespace is refused, and the
    /// incumbent's name lets the caller say which service holds it.
    DifferentService {
        namespace_id: NamespaceRowId,
        incumbent_name: CorrosionServiceName,
    },
    /// More than one service occupies the namespace. v2 writes cannot produce
    /// this today, but the reader stays tolerant of rows it did not write.
    MultipleServices {
        namespace_id: NamespaceRowId,
        service_ids: Vec<ServiceRowId>,
    },
    /// Route bindings exist without any service; the namespace is neither
    /// empty nor redeployable, so admission refuses rather than guesses.
    RoutesWithoutServices {
        namespace_id: NamespaceRowId,
    },
}

/// A namespace-name lookup shared by first-deploy preflight and admission triage.
#[derive(Debug)]
enum NamedNamespace {
    Missing,
    Ambiguous { namespace_ids: Vec<NamespaceRowId> },
    Found(ResolvedNamespace),
}

/// Corrosion's exact, restart-safe adapter for a prepared service/container promotion.
#[derive(Clone)]
pub(super) struct CorrosionPreparedPromotionStore {
    client: CorrosionClient,
    cluster_id: ClusterId,
    claim_courtesy_wait: Duration,
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
        let service = self
            .query_one(CorrosionTable::Services, prepared.service_id.as_str())
            .await?;
        Ok(PromotionRowsObservation {
            service_row: observe_exact_row(service, &prepared.service_document, &self.cluster_id)?,
            container_row: self.observe_container_rows(&prepared.containers).await?,
        })
    }

    /// Reads every prepared container row concurrently and folds the group
    /// into one observation: `Exact` only when all rows are exact, `Absent`
    /// only when all are absent, otherwise `Mismatch`.
    async fn observe_container_rows(
        &self,
        containers: &[PreparedDeployContainer],
    ) -> Result<CorrosionPromotionRowObservation, PromotionFinalizerStoreError> {
        let observations =
            futures_util::future::try_join_all(containers.iter().map(|container| async {
                let rows = self
                    .query_one(CorrosionTable::Containers, container.id.as_str())
                    .await?;
                observe_exact_row(rows, &container.document, &self.cluster_id)
            }))
            .await?;
        Ok(fold_row_observations(&observations))
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

    async fn namespace_named(
        &self,
        name: &CorrosionNamespaceName,
    ) -> Result<NamedNamespace, PromotionFinalizerStoreError> {
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
            return Ok(NamedNamespace::Ambiguous { namespace_ids });
        }
        let Some(namespace) = accepted.pop() else {
            return Ok(NamedNamespace::Missing);
        };
        let namespace_id =
            NamespaceRowId::try_new(namespace.id.into_string()).map_err(|error| {
                PromotionFinalizerStoreError::Protocol(format!(
                    "namespace row id was invalid: {error}"
                ))
            })?;
        Ok(NamedNamespace::Found(ResolvedNamespace {
            id: namespace_id,
            exact_document: namespace.source.document,
            document: namespace.value,
        }))
    }

    /// Classifies a requested (namespace name, service name) deploy at admission.
    ///
    /// The deploy driver calls this once before creating rows, then holds the
    /// returned exact documents: the `Redeploy` incumbent's `exact_document`
    /// becomes the CAS guard for
    /// [`CorrosionPreparedPromotionStore::converge_redeploy_rows`].
    pub(super) async fn resolve_deploy_admission(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
    ) -> Result<DeployAdmission, PromotionFinalizerStoreError> {
        let namespace = match self.namespace_named(namespace_name).await? {
            NamedNamespace::Missing => return Ok(DeployAdmission::NamespaceMissing),
            NamedNamespace::Ambiguous { namespace_ids } => {
                return Ok(DeployAdmission::NamespaceAmbiguous { namespace_ids });
            }
            NamedNamespace::Found(namespace) => namespace,
        };
        let services = self.query_many(
            namespace_services_statement(&namespace.id),
            MAX_SERVICE_CLAIM_ROWS,
        );
        let routes = self.query_many(namespace_has_route_statement(&namespace.id), 1);
        let (services, routes) = tokio::try_join!(services, routes)?;
        let route_report = read_rows::<RouteBindingDocument>(&self.cluster_id, routes);
        if !route_report.skipped.is_empty() {
            return Err(PromotionFinalizerStoreError::Protocol(
                "namespace route lookup contained a rejected row".to_owned(),
            ));
        }
        let services = observed_services(&self.cluster_id, services)?;
        Ok(classify_deploy_admission(
            namespace,
            services,
            !route_report.accepted.is_empty(),
            service_name,
        ))
    }

    /// Replaces the incumbent service document and inserts the new container
    /// row in one conditional transaction batch.
    ///
    /// The incumbent CAS is the whole guard: statement (a) rewrites the
    /// service row only while it still holds the exact incumbent document
    /// serialized at admission, and statement (b) inserts the container row
    /// only once the service row holds the new document. The write counts are
    /// a hint; the exact readback is the authority. A `Rejected` disposition
    /// whose readback is not exact means the incumbent changed underneath the
    /// driver, which maps it to `SupersededByOperation`.
    pub(super) async fn converge_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>
    {
        validate_redeploy_cluster(prepared, &self.cluster_id)?;
        let statements = redeploy_statements(prepared)?;
        let disposition = match self.client.execute(&statements).await {
            Ok(response) => classify_converge_response(&response, prepared.containers.len())?,
            Err(_) => PromotionRequestDisposition::Uncertain,
        };
        let rows = self.observe_redeploy_rows(prepared).await?;
        Ok((disposition, rows))
    }

    async fn observe_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<PromotionRowsObservation, PromotionFinalizerStoreError> {
        let service = self
            .query_one(CorrosionTable::Services, prepared.service_id.as_str())
            .await?;
        Ok(PromotionRowsObservation {
            service_row: observe_exact_row(service, &prepared.service_document, &self.cluster_id)?,
            container_row: self.observe_container_rows(&prepared.containers).await?,
        })
    }

    /// Lists every container row currently attributed to `service_id`,
    /// via the `service_id` generated column, with exact documents so the
    /// sweep and clean phases can later delete precisely what they observed.
    pub(super) async fn service_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError> {
        let rows = self
            .query_many(
                service_containers_statement(service_id),
                MAX_SERVICE_CLAIM_ROWS,
            )
            .await?;
        let report = read_rows::<ContainerDocument>(&self.cluster_id, rows);
        if !report.skipped.is_empty() {
            return Err(PromotionFinalizerStoreError::Protocol(format!(
                "service container lookup contained {} rejected rows",
                report.skipped.len()
            )));
        }
        let mut containers = report
            .accepted
            .into_iter()
            .map(|row| {
                let id = ContainerId::try_new(row.source.key.clone()).map_err(|error| {
                    PromotionFinalizerStoreError::Protocol(format!(
                        "container row id was invalid: {error}"
                    ))
                })?;
                Ok(ObservedContainer {
                    id,
                    exact_document: row.source.document,
                    document: row.value,
                })
            })
            .collect::<Result<Vec<_>, PromotionFinalizerStoreError>>()?;
        containers.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(containers)
    }

    /// Deletes container rows exactly as observed, one `DELETE ... WHERE
    /// id = ? AND document = ?` per row in a single batch, then rereads each
    /// row as the authority. A row that is already absent is a success; a row
    /// whose document changed belongs to another writer and is left alone.
    pub(super) async fn delete_exact_container_rows(
        &self,
        rows: &[ObservedContainer],
    ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let statements = rows
            .iter()
            .map(|row| {
                exact_delete_statement(
                    CorrosionTable::Containers,
                    row.id.as_str(),
                    row.exact_document.clone(),
                )
            })
            .collect::<Vec<_>>();
        if self.client.execute(&statements).await.is_err() {
            // A lost response is not a failed cleanup. Exact readback is the authority.
        }
        let mut outcomes = Vec::with_capacity(rows.len());
        for row in rows {
            let observed = self
                .query_one(CorrosionTable::Containers, row.id.as_str())
                .await?;
            let observation = observe_exact_row(observed, &row.document, &self.cluster_id)?;
            outcomes.push((row.id.clone(), cleanup_from_observation(observation)));
        }
        Ok(outcomes)
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
            Ok(response) => classify_converge_response(&response, prepared.containers.len())?,
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
        let mut statements = Vec::with_capacity(prepared.containers.len() + 1);
        for container in &prepared.containers {
            let document = serde_json::to_string(&container.document)
                .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
            statements.push(exact_delete_statement(
                CorrosionTable::Containers,
                container.id.as_str(),
                document,
            ));
        }
        statements.push(exact_delete_statement(
            CorrosionTable::Services,
            prepared.service_id.as_str(),
            service,
        ));
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
        || prepared
            .containers
            .iter()
            .any(|container| &container.document.cluster_id != expected)
    {
        return Err(PromotionFinalizerStoreError::Protocol(
            "prepared promotion belongs to another cluster".to_owned(),
        ));
    }
    if prepared.containers.is_empty() {
        return Err(PromotionFinalizerStoreError::Protocol(
            "prepared promotion carries no container rows".to_owned(),
        ));
    }
    Ok(())
}

/// Serializes the prepared container rows into `(id, exact document)` pairs.
fn exact_container_pairs(
    containers: &[PreparedDeployContainer],
) -> Result<Vec<(String, String)>, PromotionFinalizerStoreError> {
    containers
        .iter()
        .map(|container| {
            serde_json::to_string(&container.document)
                .map(|document| (container.id.as_str().to_owned(), document))
                .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))
        })
        .collect()
}

fn converge_statements(
    prepared: &PreparedPromotion,
) -> Result<Vec<Statement>, PromotionFinalizerStoreError> {
    let service = serde_json::to_string(&prepared.service_document)
        .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
    let containers = exact_container_pairs(&prepared.containers)?;
    let exact = ExactPromotionRows {
        namespace_id: prepared.namespace_id.as_str(),
        namespace_document: &prepared.exact_namespace_document,
        service_id: prepared.service_id.as_str(),
        service_document: &service,
        containers: &containers,
    };
    let mut statements = vec![conditional_service_insert_statement(
        CorrosionTable::Services,
        prepared.service_id.as_str(),
        service.clone(),
        &exact,
    )];
    for (container_id, container_document) in &containers {
        statements.push(conditional_container_insert_statement(
            CorrosionTable::Containers,
            container_id,
            container_document.clone(),
            prepared.namespace_id.as_str(),
            &prepared.exact_namespace_document,
            prepared.service_id.as_str(),
            &service,
        ));
    }
    Ok(statements)
}

struct ExactPromotionRows<'a> {
    namespace_id: &'a str,
    namespace_document: &'a str,
    service_id: &'a str,
    service_document: &'a str,
    containers: &'a [(String, String)],
}

fn conditional_service_insert_statement(
    table: CorrosionTable,
    id: &str,
    document: String,
    exact: &ExactPromotionRows<'_>,
) -> Statement {
    let mut sql = format!(
        "INSERT INTO {} (id, document) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM namespaces WHERE id = ? AND document = ?) AND NOT EXISTS (SELECT 1 FROM route_bindings WHERE namespace_id = ?) AND NOT EXISTS (SELECT 1 FROM services WHERE namespace_id = ? AND NOT (id = ? AND document = ?))",
        table.as_str()
    );
    let mut params = vec![
        SqliteParameter::Text(id.to_owned()),
        SqliteParameter::Text(document),
        SqliteParameter::Text(exact.namespace_id.to_owned()),
        SqliteParameter::Text(exact.namespace_document.to_owned()),
        SqliteParameter::Text(exact.namespace_id.to_owned()),
        SqliteParameter::Text(exact.namespace_id.to_owned()),
        SqliteParameter::Text(exact.service_id.to_owned()),
        SqliteParameter::Text(exact.service_document.to_owned()),
    ];
    for (container_id, container_document) in exact.containers {
        sql.push_str(
            " AND (NOT EXISTS (SELECT 1 FROM containers WHERE id = ?) OR EXISTS (SELECT 1 FROM containers WHERE id = ? AND document = ?))",
        );
        params.push(SqliteParameter::Text(container_id.clone()));
        params.push(SqliteParameter::Text(container_id.clone()));
        params.push(SqliteParameter::Text(container_document.clone()));
    }
    sql.push_str(" ON CONFLICT(id) DO NOTHING");
    Statement::with_params(sql, params)
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

/// Per-row outcome of an exact container-row delete, judged by readback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContainerRowCleanup {
    /// The row is gone — deleted now or already absent. Both are success.
    Removed,
    /// The exact observed row survived; the delete did not land. Retry.
    StillPresent,
    /// Another writer replaced the document; the row is no longer ours to delete.
    Changed,
}

fn cleanup_from_observation(observation: CorrosionPromotionRowObservation) -> ContainerRowCleanup {
    match observation {
        CorrosionPromotionRowObservation::Absent => ContainerRowCleanup::Removed,
        CorrosionPromotionRowObservation::Exact => ContainerRowCleanup::StillPresent,
        CorrosionPromotionRowObservation::Mismatch => ContainerRowCleanup::Changed,
    }
}

fn classify_deploy_admission(
    namespace: ResolvedNamespace,
    services: Vec<ObservedService>,
    has_route_bindings: bool,
    requested: &CorrosionServiceName,
) -> DeployAdmission {
    let mut services = services.into_iter();
    match (services.next(), services.next()) {
        (None, _) => {
            if has_route_bindings {
                DeployAdmission::RoutesWithoutServices {
                    namespace_id: namespace.id,
                }
            } else {
                DeployAdmission::FirstDeploy { namespace }
            }
        }
        (Some(incumbent), None) => {
            if incumbent.document.name == *requested {
                DeployAdmission::Redeploy {
                    namespace,
                    incumbent: Box::new(incumbent),
                }
            } else {
                DeployAdmission::DifferentService {
                    namespace_id: namespace.id,
                    incumbent_name: incumbent.document.name.clone(),
                }
            }
        }
        (Some(first), Some(second)) => {
            let mut service_ids = vec![first.id, second.id];
            service_ids.extend(services.map(|service| service.id));
            DeployAdmission::MultipleServices {
                namespace_id: namespace.id,
                service_ids,
            }
        }
    }
}

fn observed_services(
    cluster_id: &ClusterId,
    rows: Vec<StoredRow>,
) -> Result<Vec<ObservedService>, PromotionFinalizerStoreError> {
    let report = read_rows::<ServiceDocument>(cluster_id, rows);
    if !report.skipped.is_empty() {
        return Err(PromotionFinalizerStoreError::Protocol(format!(
            "namespace service lookup contained {} rejected rows",
            report.skipped.len()
        )));
    }
    let mut services = report
        .accepted
        .into_iter()
        .map(|row| {
            let id = ServiceRowId::try_new(row.source.key.clone()).map_err(|error| {
                PromotionFinalizerStoreError::Protocol(format!(
                    "service row id was invalid: {error}"
                ))
            })?;
            Ok(ObservedService {
                id,
                exact_document: row.source.document,
                document: row.value,
            })
        })
        .collect::<Result<Vec<_>, PromotionFinalizerStoreError>>()?;
    services.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(services)
}

fn validate_redeploy_cluster(
    prepared: &PreparedRedeployIntent,
    expected: &ClusterId,
) -> Result<(), PromotionFinalizerStoreError> {
    if &prepared.service_document.cluster_id != expected
        || prepared
            .containers
            .iter()
            .any(|container| &container.document.cluster_id != expected)
    {
        return Err(PromotionFinalizerStoreError::Protocol(
            "prepared redeploy belongs to another cluster".to_owned(),
        ));
    }
    if prepared.containers.is_empty() {
        return Err(PromotionFinalizerStoreError::Protocol(
            "prepared redeploy carries no container rows".to_owned(),
        ));
    }
    Ok(())
}

fn redeploy_statements(
    prepared: &PreparedRedeployIntent,
) -> Result<Vec<Statement>, PromotionFinalizerStoreError> {
    let service = serde_json::to_string(&prepared.service_document)
        .map_err(|error| PromotionFinalizerStoreError::Protocol(error.to_string()))?;
    let containers = exact_container_pairs(&prepared.containers)?;
    let mut statements = vec![incumbent_service_update_statement(
        prepared.service_id.as_str(),
        &prepared.exact_incumbent_document,
        service.clone(),
    )];
    for (container_id, container_document) in containers {
        statements.push(redeploy_container_insert_statement(
            &container_id,
            container_document,
            prepared.service_id.as_str(),
            &service,
        ));
    }
    Ok(statements)
}

fn incumbent_service_update_statement(
    service_id: &str,
    exact_incumbent_document: &str,
    replacement: String,
) -> Statement {
    Statement::with_params(
        "UPDATE services SET document = ? WHERE id = ? AND document = ?",
        vec![
            SqliteParameter::Text(replacement),
            SqliteParameter::Text(service_id.to_owned()),
            SqliteParameter::Text(exact_incumbent_document.to_owned()),
        ],
    )
}

fn redeploy_container_insert_statement(
    container_id: &str,
    document: String,
    service_id: &str,
    exact_service_document: &str,
) -> Statement {
    Statement::with_params(
        "INSERT INTO containers (id, document) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM services WHERE id = ? AND document = ?) ON CONFLICT(id) DO NOTHING",
        vec![
            SqliteParameter::Text(container_id.to_owned()),
            SqliteParameter::Text(document),
            SqliteParameter::Text(service_id.to_owned()),
            SqliteParameter::Text(exact_service_document.to_owned()),
        ],
    )
}

fn namespace_services_statement(namespace_id: &NamespaceRowId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM services WHERE namespace_id = ?",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

fn service_containers_statement(service_id: &ServiceRowId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM containers WHERE service_id = ?",
        vec![SqliteParameter::Text(service_id.as_str().to_owned())],
    )
}

/// Classifies one service statement followed by exactly one statement per
/// prepared container row. Every statement writes zero or one row; any
/// landed write is an accepted request (missing halves heal on retry), and
/// all-zero is the definite rejection.
fn classify_converge_response(
    response: &TransactionResponse,
    container_count: usize,
) -> Result<PromotionRequestDisposition, PromotionFinalizerStoreError> {
    let [service, containers @ ..] = response.results.as_slice() else {
        return Err(PromotionFinalizerStoreError::Protocol(
            "promotion transaction returned no results".to_owned(),
        ));
    };
    if containers.len() != container_count {
        return Err(PromotionFinalizerStoreError::Protocol(format!(
            "atomic promotion answered {} container statements for {container_count} prepared rows",
            containers.len()
        )));
    }
    let mut total = rows_affected(service)?;
    let mut counts = vec![total];
    for container in containers {
        let count = rows_affected(container)?;
        counts.push(count);
        total += count;
    }
    if counts.iter().any(|count| *count > 1) {
        return Err(PromotionFinalizerStoreError::Protocol(format!(
            "atomic promotion reported invalid write counts: {counts:?}"
        )));
    }
    if total == 0 {
        Ok(PromotionRequestDisposition::Rejected)
    } else {
        Ok(PromotionRequestDisposition::Accepted)
    }
}

/// Folds a group of exact row observations into one: `Exact` only when every
/// row is exact, `Absent` only when every row is absent, otherwise
/// `Mismatch` — the group is neither intact nor cleanly gone.
fn fold_row_observations(
    observations: &[CorrosionPromotionRowObservation],
) -> CorrosionPromotionRowObservation {
    let mut all_exact = true;
    let mut all_absent = true;
    for observation in observations {
        match observation {
            CorrosionPromotionRowObservation::Exact => all_absent = false,
            CorrosionPromotionRowObservation::Absent => all_exact = false,
            CorrosionPromotionRowObservation::Mismatch => {
                all_exact = false;
                all_absent = false;
            }
        }
    }
    if all_exact {
        CorrosionPromotionRowObservation::Exact
    } else if all_absent {
        CorrosionPromotionRowObservation::Absent
    } else {
        CorrosionPromotionRowObservation::Mismatch
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
        ContainerDocument, CorrosionPromotionRowObservation, CorrosionServiceName, CorrosionTable,
        NamespaceDocument, ServiceDocument, SqliteParameter, Statement, TransactionResult,
        TransactionSuccess,
    };
    use ployz_core::ids::{ClusterId, ContainerId, NamespaceRowId, ServiceRowId};

    use super::{
        ContainerRowCleanup, DeployAdmission, ExactPromotionRows, ObservedService,
        PreparedDeployContainer, PreparedRedeployIntent, PromotionRequestDisposition,
        ResolvedNamespace, classify_converge_response, classify_deploy_admission,
        cleanup_from_observation, conditional_container_insert_statement,
        conditional_service_insert_statement, exact_delete_statement, fold_row_observations,
        incumbent_service_update_statement, namespace_has_route_statement,
        namespace_services_statement, observe_exact_row, redeploy_container_insert_statement,
        redeploy_statements, service_containers_statement,
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
            &ExactPromotionRows {
                namespace_id: "namespace",
                namespace_document,
                service_id: "service",
                service_document,
                containers: &[("container".to_owned(), container_document.to_owned())],
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
            classify_converge_response(&response(1, 1), 1).expect("accepted"),
            PromotionRequestDisposition::Accepted
        );
        assert_eq!(
            classify_converge_response(&response(0, 0), 1).expect("rejected"),
            PromotionRequestDisposition::Rejected
        );
        assert_eq!(
            classify_converge_response(&response(1, 0), 1).expect("service healed"),
            PromotionRequestDisposition::Accepted
        );
        assert_eq!(
            classify_converge_response(&response(0, 1), 1).expect("container healed"),
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
            classify_converge_response(&response(1, 0), 1).expect("heal service half"),
            PromotionRequestDisposition::Accepted
        );
        assert_eq!(
            classify_converge_response(&response(0, 1), 1).expect("heal container half"),
            PromotionRequestDisposition::Accepted
        );
    }

    #[test]
    fn a_response_with_the_wrong_container_statement_count_is_a_protocol_error() {
        assert!(classify_converge_response(&response(1, 1), 2).is_err());
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
    fn admission_classifies_an_empty_namespace_as_first_deploy() {
        let admission =
            classify_deploy_admission(namespace_fixture(), Vec::new(), false, &requested("api"));
        let DeployAdmission::FirstDeploy { namespace } = admission else {
            panic!("empty namespace must be a first deploy");
        };
        assert_eq!(namespace.id, namespace_id());
    }

    #[test]
    fn admission_refuses_route_bindings_without_services() {
        let admission =
            classify_deploy_admission(namespace_fixture(), Vec::new(), true, &requested("api"));
        let DeployAdmission::RoutesWithoutServices { namespace_id: id } = admission else {
            panic!("routes without services must be refused");
        };
        assert_eq!(id, namespace_id());
    }

    #[test]
    fn admission_classifies_the_matching_sole_incumbent_as_redeploy() {
        let incumbent = observed_service("01J00000000000000000000014", "api");
        let admission = classify_deploy_admission(
            namespace_fixture(),
            vec![incumbent],
            true,
            &requested("api"),
        );
        let DeployAdmission::Redeploy { incumbent, .. } = admission else {
            panic!("matching sole incumbent must be a redeploy");
        };
        assert_eq!(incumbent.id.as_str(), "01J00000000000000000000014");
    }

    #[test]
    fn admission_refuses_a_namespace_held_by_a_different_service() {
        let incumbent = observed_service("01J00000000000000000000014", "web");
        let admission = classify_deploy_admission(
            namespace_fixture(),
            vec![incumbent],
            false,
            &requested("api"),
        );
        let DeployAdmission::DifferentService { incumbent_name, .. } = admission else {
            panic!("a differently named incumbent must be refused");
        };
        assert_eq!(incumbent_name, requested("web"));
    }

    #[test]
    fn admission_refuses_multiple_services_in_one_namespace() {
        let admission = classify_deploy_admission(
            namespace_fixture(),
            vec![
                observed_service("01J00000000000000000000014", "api"),
                observed_service("01J00000000000000000000016", "web"),
            ],
            false,
            &requested("api"),
        );
        let DeployAdmission::MultipleServices { service_ids, .. } = admission else {
            panic!("multiple services must be refused");
        };
        assert_eq!(
            service_ids,
            vec![
                ServiceRowId::try_new("01J00000000000000000000014").expect("service"),
                ServiceRowId::try_new("01J00000000000000000000016").expect("service"),
            ]
        );
    }

    #[test]
    fn incumbent_redeploy_cas_compares_the_exact_admission_document() {
        let statement = incumbent_service_update_statement(
            "01J00000000000000000000014",
            r#"{"name":"api", "spacing":"preserved"}"#,
            r#"{"name":"api","image":"next"}"#.to_owned(),
        );
        assert_eq!(
            statement,
            Statement::with_params(
                "UPDATE services SET document = ? WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(r#"{"name":"api","image":"next"}"#.to_owned()),
                    SqliteParameter::Text("01J00000000000000000000014".to_owned()),
                    SqliteParameter::Text(r#"{"name":"api", "spacing":"preserved"}"#.to_owned()),
                ],
            )
        );
    }

    #[test]
    fn redeploy_container_insert_requires_the_new_service_document() {
        let Statement::WithParams(sql, parameters) = redeploy_container_insert_statement(
            "docker-container-1",
            "container-document".to_owned(),
            "service",
            "new-service-document",
        ) else {
            panic!("container insert must be parameterized");
        };
        assert!(sql.contains("EXISTS (SELECT 1 FROM services WHERE id = ? AND document = ?)"));
        assert!(sql.contains("ON CONFLICT(id) DO NOTHING"));
        assert_eq!(
            parameters.last(),
            Some(&SqliteParameter::Text("new-service-document".to_owned()))
        );
    }

    #[test]
    fn redeploy_batch_updates_the_incumbent_before_inserting_the_container() {
        let prepared = prepared_redeploy();
        let statements = redeploy_statements(&prepared).expect("statements");
        let [update, insert] = statements.as_slice() else {
            panic!("one update and one container insert expected");
        };
        let Statement::WithParams(update_sql, update_params) = update else {
            panic!("update must be parameterized");
        };
        assert!(update_sql.starts_with("UPDATE services SET document = ?"));
        assert_eq!(
            update_params.last(),
            Some(&SqliteParameter::Text(
                prepared.exact_incumbent_document.clone()
            ))
        );
        let Statement::WithParams(insert_sql, _) = insert else {
            panic!("insert must be parameterized");
        };
        assert!(insert_sql.starts_with("INSERT INTO containers"));

        // An incumbent CAS miss with no prior healed write is the rejection the
        // driver maps to SupersededByOperation.
        assert_eq!(
            classify_converge_response(&response(0, 0), 1).expect("miss"),
            PromotionRequestDisposition::Rejected
        );
    }

    #[test]
    fn container_cleanup_deletes_the_exact_observed_row() {
        let Statement::WithParams(sql, parameters) = exact_delete_statement(
            CorrosionTable::Containers,
            "docker-container-1",
            "exact-container-document".to_owned(),
        ) else {
            panic!("delete must be parameterized");
        };
        assert_eq!(sql, "DELETE FROM containers WHERE id = ? AND document = ?");
        assert_eq!(
            parameters,
            vec![
                SqliteParameter::Text("docker-container-1".to_owned()),
                SqliteParameter::Text("exact-container-document".to_owned()),
            ]
        );
    }

    #[test]
    fn deleting_an_already_absent_container_row_is_success() {
        let observation =
            observe_exact_row(Vec::new(), &container_document(), &cluster_id()).expect("absent");
        assert_eq!(observation, CorrosionPromotionRowObservation::Absent);
        assert_eq!(
            cleanup_from_observation(observation),
            ContainerRowCleanup::Removed
        );
        assert_eq!(
            cleanup_from_observation(CorrosionPromotionRowObservation::Exact),
            ContainerRowCleanup::StillPresent
        );
        assert_eq!(
            cleanup_from_observation(CorrosionPromotionRowObservation::Mismatch),
            ContainerRowCleanup::Changed
        );
    }

    #[test]
    fn container_and_service_listings_key_on_generated_columns() {
        let service = ServiceRowId::try_new("01J00000000000000000000014").expect("service");
        let Statement::WithParams(sql, _) = service_containers_statement(&service) else {
            panic!("container listing must be parameterized");
        };
        assert_eq!(
            sql,
            "SELECT id, document FROM containers WHERE service_id = ?"
        );

        let Statement::WithParams(sql, _) = namespace_services_statement(&namespace_id()) else {
            panic!("service listing must be parameterized");
        };
        assert_eq!(
            sql,
            "SELECT id, document FROM services WHERE namespace_id = ?"
        );
    }

    fn cluster_id() -> ClusterId {
        ClusterId::try_new("01J00000000000000000000010").expect("cluster")
    }

    fn namespace_id() -> NamespaceRowId {
        NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace")
    }

    fn requested(name: &str) -> CorrosionServiceName {
        CorrosionServiceName::try_new(name).expect("service name")
    }

    fn namespace_fixture() -> ResolvedNamespace {
        let value = serde_json::json!({
            "v": 1,
            "cluster_id": cluster_id(),
            "written_by": {
                "kind": "peer",
                "peer_id": "01J00000000000000000000015"
            },
            "written_at": "2026-08-05T10:00:00Z",
            "name": "production"
        });
        let document: NamespaceDocument =
            serde_json::from_value(value.clone()).expect("namespace document");
        ResolvedNamespace {
            id: namespace_id(),
            exact_document: value.to_string(),
            document,
        }
    }

    fn service_document(name: &str) -> ServiceDocument {
        serde_json::from_value(serde_json::json!({
            "v": 1,
            "cluster_id": cluster_id(),
            "written_by": {
                "kind": "peer",
                "peer_id": "01J00000000000000000000015"
            },
            "written_at": "2026-08-05T10:00:00Z",
            "namespace_id": namespace_id(),
            "name": name,
            "image": "ghcr.io/acme/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "env_fingerprints": {},
            "mode": "replicated",
            "replicas": 1,
            "pinned_machines": ["01J00000000000000000000012"],
            "active_deploy": "01J00000000000000000000011",
            "previous_image": null,
            "deployed_at": "2026-08-05T10:00:02Z",
            "operation_id": "01J00000000000000000000011"
        }))
        .expect("service document")
    }

    fn observed_service(id: &str, name: &str) -> ObservedService {
        let document = service_document(name);
        ObservedService {
            id: ServiceRowId::try_new(id).expect("service id"),
            exact_document: serde_json::to_string(&document).expect("service json"),
            document,
        }
    }

    fn container_document() -> ContainerDocument {
        serde_json::from_value(serde_json::json!({
            "v": 1,
            "cluster_id": cluster_id(),
            "machine_id": "01J00000000000000000000012",
            "service_id": "01J00000000000000000000014",
            "namespace_id": namespace_id(),
            "ip": "10.210.20.2",
            "deploy": "01J00000000000000000000011"
        }))
        .expect("container document")
    }

    fn prepared_redeploy() -> PreparedRedeployIntent {
        let incumbent = observed_service("01J00000000000000000000014", "api");
        PreparedRedeployIntent {
            service_id: incumbent.id,
            exact_incumbent_document: incumbent.exact_document,
            service_document: service_document("api"),
            containers: vec![PreparedDeployContainer {
                id: ContainerId::try_new("docker-container-1").expect("container id"),
                document: container_document(),
            }],
            health_gate: ployz_core::HealthGatePolicy::Enforce,
        }
    }

    #[test]
    fn container_row_group_folds_to_one_observation() {
        use CorrosionPromotionRowObservation::{Absent, Exact, Mismatch};

        assert_eq!(fold_row_observations(&[Exact, Exact]), Exact);
        assert_eq!(fold_row_observations(&[Absent, Absent]), Absent);
        assert_eq!(fold_row_observations(&[Exact, Absent]), Mismatch);
        assert_eq!(fold_row_observations(&[Exact, Mismatch]), Mismatch);
        assert_eq!(fold_row_observations(&[Absent, Mismatch]), Mismatch);
    }

    #[test]
    fn route_binding_preflight_is_a_bounded_existence_read() {
        let namespace = ployz_core::ids::NamespaceRowId::try_new("01J00000000000000000000001")
            .expect("namespace");
        let ployz_core::corrosion::Statement::WithParams(route_sql, _) =
            namespace_has_route_statement(&namespace)
        else {
            panic!("parameterized route statement");
        };
        assert!(route_sql.contains("FROM route_bindings"));
        assert!(route_sql.ends_with("LIMIT 1"));
    }
}
