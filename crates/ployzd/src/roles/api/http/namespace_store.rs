//! Direct Corrosion access for the two synchronous namespace primitives.

use ployz_core::corrosion::{
    CorrosionNamespaceName, CorrosionTable, NamespaceDocument, ServiceDocument, SqliteParameter,
    Statement, StoredRow, TransactionResponse, TransactionResult, read_named_rows, read_rows,
};
use ployz_core::ids::{ClusterId, NamespaceRowId, ServiceRowId};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};

use super::store::{MutationStoreError, insert_document};

const MAX_KEYED_ROWS: usize = 2;
const MAX_NAMESPACE_ROWS: usize = 1_000;

#[derive(Debug)]
pub(super) enum NamespaceCreateOutcome {
    Created { namespace_id: NamespaceRowId },
    NameUnavailable { winner: NamespaceRowId },
}

#[derive(Debug)]
pub(super) struct ObservedNamespace {
    pub(super) id: NamespaceRowId,
    pub(super) exact_document: String,
    pub(super) document: NamespaceDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NamespaceDeleteOutcome {
    Removed,
    AlreadyAbsent,
    Changed,
    NotEmpty {
        service_ids: Vec<ServiceRowId>,
        route_binding_count: usize,
    },
}

#[derive(Clone)]
pub(super) struct NamespaceStore {
    client: CorrosionClient,
    cluster_id: ClusterId,
}

impl NamespaceStore {
    #[must_use]
    pub(super) const fn new(client: CorrosionClient, cluster_id: ClusterId) -> Self {
        Self { client, cluster_id }
    }

    pub(super) async fn create(
        &self,
        namespace_id: &NamespaceRowId,
        document: &NamespaceDocument,
    ) -> Result<NamespaceCreateOutcome, NamespaceStoreError> {
        if document.cluster_id != self.cluster_id {
            return Err(NamespaceStoreError::ForeignNamespace);
        }
        let existing = self.named(&document.name).await?;
        if let Some(winner) = existing.into_iter().next() {
            return Ok(NamespaceCreateOutcome::NameUnavailable { winner: winner.id });
        }
        insert_document(
            &self.client,
            CorrosionTable::Namespaces,
            namespace_id.as_str(),
            document,
        )
        .await?;
        Ok(NamespaceCreateOutcome::Created {
            namespace_id: namespace_id.clone(),
        })
    }

    pub(super) async fn named(
        &self,
        name: &CorrosionNamespaceName,
    ) -> Result<Vec<ObservedNamespace>, NamespaceStoreError> {
        let rows = self
            .query(
                Statement::with_params(
                    "SELECT id, document FROM namespaces WHERE json_extract(document, '$.cluster_id') = ? AND json_extract(document, '$.name') = ?",
                    vec![
                        SqliteParameter::Text(self.cluster_id.as_str().to_owned()),
                        SqliteParameter::Text(name.as_str().to_owned()),
                    ],
                ),
                MAX_NAMESPACE_ROWS,
            )
            .await?;
        accepted_namespaces(&self.cluster_id, rows)
    }

    pub(super) async fn by_id(
        &self,
        namespace_id: &NamespaceRowId,
    ) -> Result<Option<ObservedNamespace>, NamespaceStoreError> {
        let rows = self
            .query(select_by_id(namespace_id), MAX_KEYED_ROWS)
            .await?;
        let mut namespaces = accepted_namespaces(&self.cluster_id, rows)?;
        match namespaces.as_slice() {
            [] => Ok(None),
            [_] => Ok(namespaces.pop()),
            [_, _, ..] => Err(NamespaceStoreError::AmbiguousNamespace),
        }
    }

    pub(super) async fn delete_if_empty(
        &self,
        observed: &ObservedNamespace,
    ) -> Result<NamespaceDeleteOutcome, NamespaceStoreError> {
        let response = self
            .client
            .execute(&[delete_empty_statement(
                &observed.id,
                &observed.exact_document,
            )])
            .await?;
        match affected_rows(&observed.id, &response)? {
            1 => Ok(NamespaceDeleteOutcome::Removed),
            0 => self.classify_delete_miss(observed).await,
            rows_affected => Err(NamespaceStoreError::UnexpectedWriteCount {
                id: observed.id.clone(),
                rows_affected,
            }),
        }
    }

    async fn classify_delete_miss(
        &self,
        observed: &ObservedNamespace,
    ) -> Result<NamespaceDeleteOutcome, NamespaceStoreError> {
        let namespace = self.query(select_by_id(&observed.id), MAX_KEYED_ROWS);
        let services = self.query(
            dependents_statement(CorrosionTable::Services, &observed.id),
            MAX_NAMESPACE_ROWS,
        );
        let routes = self.query(
            dependents_statement(CorrosionTable::RouteBindings, &observed.id),
            MAX_NAMESPACE_ROWS,
        );
        let (namespace, services, routes) = tokio::try_join!(namespace, services, routes)?;
        if namespace.is_empty() {
            return Ok(NamespaceDeleteOutcome::AlreadyAbsent);
        }
        let [namespace] = namespace.as_slice() else {
            return Ok(NamespaceDeleteOutcome::Changed);
        };
        if namespace.document != observed.exact_document {
            return Ok(NamespaceDeleteOutcome::Changed);
        }

        let report = read_rows::<ServiceDocument>(&self.cluster_id, services);
        if !report.skipped.is_empty() {
            return Err(NamespaceStoreError::RejectedRows {
                table: CorrosionTable::Services,
                count: report.skipped.len(),
            });
        }
        let mut service_ids = report
            .accepted
            .into_iter()
            .map(|row| {
                ServiceRowId::try_new(row.source.key.clone()).map_err(|_| {
                    NamespaceStoreError::InvalidRowId {
                        table: CorrosionTable::Services,
                        id: row.source.key,
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        service_ids.sort();
        Ok(NamespaceDeleteOutcome::NotEmpty {
            service_ids,
            route_binding_count: routes.len(),
        })
    }

    async fn query(
        &self,
        statement: Statement,
        limit: usize,
    ) -> Result<Vec<StoredRow>, NamespaceStoreError> {
        let mut stream = self.client.query(&statement).await?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(limit))
            .await
            .map_err(NamespaceStoreError::Rows)
    }
}

fn accepted_namespaces(
    cluster_id: &ClusterId,
    rows: Vec<StoredRow>,
) -> Result<Vec<ObservedNamespace>, NamespaceStoreError> {
    let report = read_named_rows::<NamespaceDocument>(cluster_id, rows);
    report
        .accepted
        .into_iter()
        .map(|row| {
            let id = NamespaceRowId::try_new(row.source.key.clone()).map_err(|_| {
                NamespaceStoreError::InvalidRowId {
                    table: CorrosionTable::Namespaces,
                    id: row.source.key.clone(),
                }
            })?;
            Ok(ObservedNamespace {
                id,
                exact_document: row.source.document,
                document: row.value,
            })
        })
        .collect()
}

fn select_by_id(namespace_id: &NamespaceRowId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM namespaces WHERE id = ?",
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

fn delete_empty_statement(namespace_id: &NamespaceRowId, exact_document: &str) -> Statement {
    Statement::with_params(
        "DELETE FROM namespaces WHERE id = ? AND document = ? AND NOT EXISTS (SELECT 1 FROM services WHERE namespace_id = ?) AND NOT EXISTS (SELECT 1 FROM route_bindings WHERE namespace_id = ?)",
        vec![
            SqliteParameter::Text(namespace_id.as_str().to_owned()),
            SqliteParameter::Text(exact_document.to_owned()),
            SqliteParameter::Text(namespace_id.as_str().to_owned()),
            SqliteParameter::Text(namespace_id.as_str().to_owned()),
        ],
    )
}

fn dependents_statement(table: CorrosionTable, namespace_id: &NamespaceRowId) -> Statement {
    Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE namespace_id = ?",
            table.as_str()
        ),
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

fn affected_rows(
    namespace_id: &NamespaceRowId,
    response: &TransactionResponse,
) -> Result<usize, NamespaceStoreError> {
    match response.results.as_slice() {
        [TransactionResult::Success(result)] => Ok(result.rows_affected),
        [TransactionResult::Error(_)] | [] | [_, _, ..] => {
            Err(NamespaceStoreError::UnexpectedWriteResult {
                id: namespace_id.clone(),
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum NamespaceStoreError {
    #[error(transparent)]
    Client(#[from] CorrosionClientError),
    #[error(transparent)]
    Rows(StoredRowCollectionError),
    #[error(transparent)]
    Store(#[from] MutationStoreError),
    #[error("namespace belongs to another cluster")]
    ForeignNamespace,
    #[error("Corrosion returned rejected {table:?} rows: {count}")]
    RejectedRows { table: CorrosionTable, count: usize },
    #[error("Corrosion returned invalid {table:?} row id {id}")]
    InvalidRowId { table: CorrosionTable, id: String },
    #[error("Corrosion returned ambiguous namespace rows")]
    AmbiguousNamespace,
    #[error("Corrosion returned an unexpected namespace write result for {id}")]
    UnexpectedWriteResult { id: NamespaceRowId },
    #[error("Corrosion namespace write for {id} affected {rows_affected} rows")]
    UnexpectedWriteCount {
        id: NamespaceRowId,
        rows_affected: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_is_one_exact_conditional_statement() {
        let id = NamespaceRowId::try_new("01J00000000000000000000002").expect("namespace");
        let Statement::WithParams(sql, parameters) =
            delete_empty_statement(&id, r#"{"name":"production"}"#)
        else {
            panic!("parameterized statement");
        };
        assert!(sql.contains("id = ? AND document = ?"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM services"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM route_bindings"));
        assert_eq!(parameters.len(), 4);
    }
}
