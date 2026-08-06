use ployz_core::corrosion::{
    CorrosionBasicTransition, CorrosionDeployState, CorrosionDeployTransition,
    CorrosionNamespaceName, CorrosionOperation, CorrosionOperationState, CorrosionTable,
    NamespaceDocument, OperationDocument, ServiceDocument, SqliteParameter, Statement, StoredRow,
    TransactionResponse, TransactionResult, read_rows,
};
use ployz_core::ids::{
    ClusterId, CorrosionUlid, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId,
};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, NameClaimError, NameClaimOutcome,
    StoredRowCollectionError, StoredRowLimit, collect_stored_rows,
};

const MAX_DEEP_OPERATION_ROWS: usize = 2;
const MAX_NAMESPACE_ROWS: usize = 1_000;
const MAX_LOCAL_STARTUP_OPERATIONS: usize = 10_000;

/// One operation row with the exact Corrosion document used for conditional writes.
#[derive(Debug)]
pub(super) struct ObservedOperation {
    pub(super) id: OperationRowId,
    pub(super) exact_document: String,
    pub(super) document: OperationDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConditionalOperationWrite {
    Written,
    Stale,
}

#[derive(Debug)]
pub(super) enum NamespaceClaim {
    Claimed { namespace_id: NamespaceRowId },
    Lost { winner: NamespaceRowId },
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

/// Deep, keyed Corrosion access for operation drivers and namespace primitives.
#[derive(Clone)]
pub(super) struct OperationStore {
    client: CorrosionClient,
    cluster_id: ClusterId,
}

impl OperationStore {
    #[must_use]
    pub(super) const fn new(client: CorrosionClient, cluster_id: ClusterId) -> Self {
        Self { client, cluster_id }
    }

    pub(super) async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, OperationStoreError> {
        let rows = self
            .query(
                select_by_id(CorrosionTable::Operations, operation_id.as_str()),
                MAX_DEEP_OPERATION_ROWS,
            )
            .await?;
        one_operation(&self.cluster_id, operation_id, rows)
    }

    pub(super) async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        document: &OperationDocument,
    ) -> Result<(), OperationStoreError> {
        if document.cluster_id != self.cluster_id {
            return Err(OperationStoreError::ForeignOperation);
        }
        let encoded = encode(CorrosionTable::Operations, document)?;
        let response = self
            .client
            .execute(&[insert_statement(
                CorrosionTable::Operations,
                operation_id.as_str(),
                encoded,
            )])
            .await?;
        require_one_write(CorrosionTable::Operations, operation_id.as_str(), &response)
    }

    pub(super) async fn transition_deploy(
        &self,
        observed: ObservedOperation,
        transition: CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        let replacement = observed
            .document
            .clone()
            .transition_deploy(transition)
            .map_err(OperationStoreError::Transition)?;
        self.replace_operation(&observed, &replacement).await
    }

    /// Replaces an operation only after the caller has obtained the replacement through Core's
    /// transition API. This narrower method exists for recovery paths that read a durable terminal
    /// document from the operation evidence file.
    pub(super) async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        replacement: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        if observed.document.cluster_id != self.cluster_id
            || replacement.cluster_id != self.cluster_id
            || !is_legal_terminal_recovery(&observed.document, replacement)
        {
            return Err(OperationStoreError::IllegalTerminalRecovery);
        }
        self.replace_operation(observed, replacement).await
    }

    async fn replace_operation(
        &self,
        observed: &ObservedOperation,
        replacement: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        let encoded = encode(CorrosionTable::Operations, replacement)?;
        let response = self
            .client
            .execute(&[replace_operation_statement(
                &observed.id,
                &observed.exact_document,
                encoded,
            )])
            .await?;
        conditional_write(&observed.id, &response)
    }

    pub(super) async fn local_nonterminal_operations(
        &self,
        machine_id: &MachineRowId,
    ) -> Result<Vec<ObservedOperation>, OperationStoreError> {
        let statement = Statement::with_params(
            "SELECT id, document FROM operations WHERE json_extract(document, '$.cluster_id') = ? AND json_extract(document, '$.machine_id') = ? AND json_extract(document, '$.state') IN ('created', 'running')",
            vec![
                SqliteParameter::Text(self.cluster_id.as_str().to_owned()),
                SqliteParameter::Text(machine_id.as_str().to_owned()),
            ],
        );
        let rows = self.query(statement, MAX_LOCAL_STARTUP_OPERATIONS).await?;
        let report = read_rows::<OperationDocument>(&self.cluster_id, rows);
        if !report.skipped.is_empty() {
            return Err(OperationStoreError::RejectedRows {
                table: CorrosionTable::Operations,
                count: report.skipped.len(),
            });
        }
        report
            .accepted
            .into_iter()
            .map(|row| {
                let id = OperationRowId::try_new(row.source.key.clone()).map_err(|_| {
                    OperationStoreError::InvalidRowId {
                        table: CorrosionTable::Operations,
                        id: row.source.key.clone(),
                    }
                })?;
                Ok(ObservedOperation {
                    id,
                    exact_document: row.source.document,
                    document: row.value,
                })
            })
            .collect()
    }

    pub(super) async fn create_namespace(
        &self,
        namespace_id: &NamespaceRowId,
        document: &NamespaceDocument,
    ) -> Result<NamespaceClaim, OperationStoreError> {
        if document.cluster_id != self.cluster_id {
            return Err(OperationStoreError::ForeignNamespace);
        }
        let mut existing = self.namespaces_named(&document.name).await?;
        existing.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(winner) = existing.into_iter().next() {
            return Ok(NamespaceClaim::Lost { winner: winner.id });
        }
        let claim_id = CorrosionUlid::try_new(namespace_id.as_str().to_owned())
            .map_err(|_| OperationStoreError::InvalidNamespaceId)?;
        match self.client.claim_named(claim_id, document).await? {
            NameClaimOutcome::Claimed { id, .. } => Ok(NamespaceClaim::Claimed {
                namespace_id: NamespaceRowId::try_new(id.into_string())
                    .map_err(|_| OperationStoreError::InvalidNamespaceId)?,
            }),
            NameClaimOutcome::Lost { winner, .. } => Ok(NamespaceClaim::Lost {
                winner: NamespaceRowId::try_new(winner.id.into_string())
                    .map_err(|_| OperationStoreError::InvalidNamespaceId)?,
            }),
        }
    }

    pub(super) async fn namespaces_named(
        &self,
        name: &CorrosionNamespaceName,
    ) -> Result<Vec<ObservedNamespace>, OperationStoreError> {
        let statement = Statement::with_params(
            "SELECT id, document FROM namespaces WHERE json_extract(document, '$.cluster_id') = ? AND json_extract(document, '$.name') = ?",
            vec![
                SqliteParameter::Text(self.cluster_id.as_str().to_owned()),
                SqliteParameter::Text(name.as_str().to_owned()),
            ],
        );
        let rows = self.query(statement, MAX_NAMESPACE_ROWS).await?;
        accepted_namespaces(&self.cluster_id, rows)
    }

    pub(super) async fn namespace(
        &self,
        namespace_id: &NamespaceRowId,
    ) -> Result<Option<ObservedNamespace>, OperationStoreError> {
        let rows = self
            .query(
                select_by_id(CorrosionTable::Namespaces, namespace_id.as_str()),
                MAX_DEEP_OPERATION_ROWS,
            )
            .await?;
        let mut namespaces = accepted_namespaces(&self.cluster_id, rows)?;
        match namespaces.as_slice() {
            [] => Ok(None),
            [_] => Ok(namespaces.pop()),
            _ => Err(OperationStoreError::AmbiguousNamespace),
        }
    }

    pub(super) async fn delete_namespace_if_empty(
        &self,
        observed: &ObservedNamespace,
    ) -> Result<NamespaceDeleteOutcome, OperationStoreError> {
        let response = self
            .client
            .execute(&[delete_empty_namespace_statement(
                &observed.id,
                &observed.exact_document,
            )])
            .await?;
        match affected_rows(CorrosionTable::Namespaces, observed.id.as_str(), &response)? {
            1 => Ok(NamespaceDeleteOutcome::Removed),
            0 => self.classify_namespace_delete_miss(observed).await,
            rows_affected => Err(OperationStoreError::UnexpectedWriteCount {
                table: CorrosionTable::Namespaces,
                id: observed.id.as_str().to_owned(),
                rows_affected,
            }),
        }
    }

    async fn classify_namespace_delete_miss(
        &self,
        observed: &ObservedNamespace,
    ) -> Result<NamespaceDeleteOutcome, OperationStoreError> {
        let namespace = self.query(
            select_by_id(CorrosionTable::Namespaces, observed.id.as_str()),
            MAX_DEEP_OPERATION_ROWS,
        );
        let services = self.query(
            namespace_dependents_statement(CorrosionTable::Services, &observed.id),
            MAX_NAMESPACE_ROWS,
        );
        let routes = self.query(
            namespace_dependents_statement(CorrosionTable::RouteBindings, &observed.id),
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

        let service_report = read_rows::<ServiceDocument>(&self.cluster_id, services);
        if !service_report.skipped.is_empty() {
            return Err(OperationStoreError::RejectedRows {
                table: CorrosionTable::Services,
                count: service_report.skipped.len(),
            });
        }
        let mut service_ids = service_report
            .accepted
            .into_iter()
            .map(|row| {
                ServiceRowId::try_new(row.source.key.clone()).map_err(|_| {
                    OperationStoreError::InvalidRowId {
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
    ) -> Result<Vec<StoredRow>, OperationStoreError> {
        let mut stream = self.client.query(&statement).await?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(limit))
            .await
            .map_err(OperationStoreError::Rows)
    }
}

fn is_legal_terminal_recovery(
    observed: &OperationDocument,
    replacement: &OperationDocument,
) -> bool {
    if observed.v != replacement.v
        || observed.cluster_id != replacement.cluster_id
        || observed.machine_id != replacement.machine_id
        || observed.initiator != replacement.initiator
    {
        return false;
    }
    match replacement.operation() {
        CorrosionOperation::Build { state, .. }
        | CorrosionOperation::MachineAdd { state, .. }
        | CorrosionOperation::MachineRemove { state, .. }
        | CorrosionOperation::Recovery { state, .. } => {
            let transition = match state {
                CorrosionOperationState::Succeeded { completed_at, .. } => {
                    CorrosionBasicTransition::Succeeded {
                        completed_at: *completed_at,
                    }
                }
                CorrosionOperationState::Failed {
                    completed_at,
                    failure,
                    ..
                } => CorrosionBasicTransition::Failed {
                    completed_at: *completed_at,
                    failure: failure.clone(),
                },
                CorrosionOperationState::Created { .. }
                | CorrosionOperationState::Running { .. } => return false,
            };
            observed
                .clone()
                .transition_basic(transition)
                .is_ok_and(|reconstructed| reconstructed == *replacement)
        }
        CorrosionOperation::Deploy { .. } => {
            deploy_terminal_preserves_history(observed, replacement)
        }
    }
}

fn deploy_terminal_preserves_history(
    observed: &OperationDocument,
    replacement: &OperationDocument,
) -> bool {
    match (observed.operation(), replacement.operation()) {
        (
            CorrosionOperation::Deploy {
                namespace_id: observed_namespace,
                service_ids: observed_services,
                state: CorrosionDeployState::Created { created_at },
            },
            CorrosionOperation::Deploy {
                namespace_id: replacement_namespace,
                service_ids: replacement_services,
                state:
                    CorrosionDeployState::Terminal {
                        created_at: replacement_created_at,
                        started_at: None,
                        ..
                    },
            },
        ) => {
            observed_namespace == replacement_namespace
                && observed_services == replacement_services
                && created_at == replacement_created_at
        }
        (
            CorrosionOperation::Deploy {
                namespace_id: observed_namespace,
                service_ids: observed_services,
                state:
                    CorrosionDeployState::Running {
                        created_at,
                        started_at,
                    },
            },
            CorrosionOperation::Deploy {
                namespace_id: replacement_namespace,
                service_ids: replacement_services,
                state:
                    CorrosionDeployState::Terminal {
                        created_at: replacement_created_at,
                        started_at: Some(replacement_started_at),
                        ..
                    },
            },
        ) => {
            observed_namespace == replacement_namespace
                && observed_services == replacement_services
                && created_at == replacement_created_at
                && started_at == replacement_started_at
        }
        (
            CorrosionOperation::Build { .. }
            | CorrosionOperation::MachineAdd { .. }
            | CorrosionOperation::MachineRemove { .. }
            | CorrosionOperation::Recovery { .. }
            | CorrosionOperation::Deploy { .. },
            CorrosionOperation::Build { .. }
            | CorrosionOperation::Deploy { .. }
            | CorrosionOperation::MachineAdd { .. }
            | CorrosionOperation::MachineRemove { .. }
            | CorrosionOperation::Recovery { .. },
        ) => false,
    }
}

fn one_operation(
    cluster_id: &ClusterId,
    operation_id: &OperationRowId,
    rows: Vec<StoredRow>,
) -> Result<Option<ObservedOperation>, OperationStoreError> {
    let report = read_rows::<OperationDocument>(cluster_id, rows);
    if !report.skipped.is_empty() {
        return Err(OperationStoreError::RejectedRows {
            table: CorrosionTable::Operations,
            count: report.skipped.len(),
        });
    }
    let mut accepted = report.accepted.into_iter();
    let Some(row) = accepted.next() else {
        return Ok(None);
    };
    if accepted.next().is_some() || row.source.key != operation_id.as_str() {
        return Err(OperationStoreError::AmbiguousOperation);
    }
    Ok(Some(ObservedOperation {
        id: operation_id.clone(),
        exact_document: row.source.document,
        document: row.value,
    }))
}

fn accepted_namespaces(
    cluster_id: &ClusterId,
    rows: Vec<StoredRow>,
) -> Result<Vec<ObservedNamespace>, OperationStoreError> {
    let report = read_rows::<NamespaceDocument>(cluster_id, rows);
    if !report.skipped.is_empty() {
        return Err(OperationStoreError::RejectedRows {
            table: CorrosionTable::Namespaces,
            count: report.skipped.len(),
        });
    }
    report
        .accepted
        .into_iter()
        .map(|row| {
            let id = NamespaceRowId::try_new(row.source.key.clone()).map_err(|_| {
                OperationStoreError::InvalidRowId {
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

fn select_by_id(table: CorrosionTable, id: &str) -> Statement {
    Statement::with_params(
        format!("SELECT id, document FROM {} WHERE id = ?", table.as_str()),
        vec![SqliteParameter::Text(id.to_owned())],
    )
}

fn insert_statement(table: CorrosionTable, id: &str, document: String) -> Statement {
    Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) VALUES (?, ?)",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(document),
        ],
    )
}

fn replace_operation_statement(
    operation_id: &OperationRowId,
    exact_document: &str,
    replacement: String,
) -> Statement {
    Statement::with_params(
        "UPDATE operations SET document = ? WHERE id = ? AND document = ?",
        vec![
            SqliteParameter::Text(replacement),
            SqliteParameter::Text(operation_id.as_str().to_owned()),
            SqliteParameter::Text(exact_document.to_owned()),
        ],
    )
}

fn delete_empty_namespace_statement(
    namespace_id: &NamespaceRowId,
    exact_document: &str,
) -> Statement {
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

fn namespace_dependents_statement(
    table: CorrosionTable,
    namespace_id: &NamespaceRowId,
) -> Statement {
    Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE namespace_id = ?",
            table.as_str()
        ),
        vec![SqliteParameter::Text(namespace_id.as_str().to_owned())],
    )
}

fn encode<Document: serde::Serialize + ?Sized>(
    table: CorrosionTable,
    document: &Document,
) -> Result<String, OperationStoreError> {
    serde_json::to_string(document).map_err(|source| OperationStoreError::Encode {
        table,
        detail: source.to_string(),
    })
}

fn affected_rows(
    table: CorrosionTable,
    id: &str,
    response: &TransactionResponse,
) -> Result<usize, OperationStoreError> {
    match response.results.as_slice() {
        [TransactionResult::Success(result)] => Ok(result.rows_affected),
        [TransactionResult::Error(_)] | [] | [_, _, ..] => {
            Err(OperationStoreError::UnexpectedWriteResult {
                table,
                id: id.to_owned(),
            })
        }
    }
}

fn require_one_write(
    table: CorrosionTable,
    id: &str,
    response: &TransactionResponse,
) -> Result<(), OperationStoreError> {
    let rows_affected = affected_rows(table, id, response)?;
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(OperationStoreError::UnexpectedWriteCount {
            table,
            id: id.to_owned(),
            rows_affected,
        })
    }
}

fn conditional_write(
    operation_id: &OperationRowId,
    response: &TransactionResponse,
) -> Result<ConditionalOperationWrite, OperationStoreError> {
    match affected_rows(CorrosionTable::Operations, operation_id.as_str(), response)? {
        0 => Ok(ConditionalOperationWrite::Stale),
        1 => Ok(ConditionalOperationWrite::Written),
        rows_affected => Err(OperationStoreError::UnexpectedWriteCount {
            table: CorrosionTable::Operations,
            id: operation_id.as_str().to_owned(),
            rows_affected,
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum OperationStoreError {
    #[error(transparent)]
    Client(#[from] CorrosionClientError),
    #[error(transparent)]
    Rows(StoredRowCollectionError),
    #[error(transparent)]
    Claim(#[from] NameClaimError),
    #[error("failed to encode {table:?} document: {detail}")]
    Encode {
        table: CorrosionTable,
        detail: String,
    },
    #[error("operation transition was refused: {0}")]
    Transition(ployz_core::corrosion::CorrosionOperationTransitionError),
    #[error("operation belongs to another cluster or machine")]
    ForeignOperation,
    #[error(
        "durable terminal operation changes immutable identity or is not a legal terminal step"
    )]
    IllegalTerminalRecovery,
    #[error("namespace belongs to another cluster")]
    ForeignNamespace,
    #[error("namespace row id is invalid")]
    InvalidNamespaceId,
    #[error("Corrosion returned rejected {table:?} rows: {count}")]
    RejectedRows { table: CorrosionTable, count: usize },
    #[error("Corrosion returned invalid {table:?} row id {id}")]
    InvalidRowId { table: CorrosionTable, id: String },
    #[error("Corrosion returned ambiguous operation rows")]
    AmbiguousOperation,
    #[error("Corrosion returned ambiguous namespace rows")]
    AmbiguousNamespace,
    #[error("Corrosion returned an unexpected write result for {table:?}/{id}")]
    UnexpectedWriteResult { table: CorrosionTable, id: String },
    #[error("Corrosion write for {table:?}/{id} affected {rows_affected} rows")]
    UnexpectedWriteCount {
        table: CorrosionTable,
        id: String,
        rows_affected: usize,
    },
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        CorrosionBasicOperation, CorrosionBasicTransition, CorrosionDocumentVersion,
        CorrosionTimestamp, OperationDocument, OperationInitiator, SqliteParameter, Statement,
    };
    use ployz_core::ids::{
        ClusterId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
    };

    use super::{
        delete_empty_namespace_statement, is_legal_terminal_recovery, replace_operation_statement,
    };

    fn operation_id() -> OperationRowId {
        OperationRowId::try_new("01J00000000000000000000001").expect("operation id")
    }

    fn namespace_id() -> NamespaceRowId {
        NamespaceRowId::try_new("01J00000000000000000000002").expect("namespace id")
    }

    #[test]
    fn operation_replacement_compares_the_exact_observed_document() {
        let statement = replace_operation_statement(
            &operation_id(),
            r#"{"state":"created", "spacing":"preserved"}"#,
            r#"{"state":"running"}"#.to_owned(),
        );

        assert_eq!(
            statement,
            Statement::with_params(
                "UPDATE operations SET document = ? WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(r#"{"state":"running"}"#.to_owned()),
                    SqliteParameter::Text(operation_id().as_str().to_owned()),
                    SqliteParameter::Text(
                        r#"{"state":"created", "spacing":"preserved"}"#.to_owned()
                    ),
                ],
            )
        );
    }

    #[test]
    fn namespace_delete_is_one_exact_conditional_statement() {
        let statement =
            delete_empty_namespace_statement(&namespace_id(), r#"{"name":"production"}"#);
        let Statement::WithParams(sql, parameters) = statement else {
            panic!("namespace delete must be parameterized");
        };

        assert!(sql.starts_with("DELETE FROM namespaces"));
        assert!(sql.contains("id = ? AND document = ?"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM services"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM route_bindings"));
        assert_eq!(
            parameters,
            vec![
                SqliteParameter::Text(namespace_id().as_str().to_owned()),
                SqliteParameter::Text(r#"{"name":"production"}"#.to_owned()),
                SqliteParameter::Text(namespace_id().as_str().to_owned()),
                SqliteParameter::Text(namespace_id().as_str().to_owned()),
            ]
        );
    }

    #[test]
    fn terminal_recovery_preserves_every_immutable_operation_field() {
        let observed = basic_created("01J00000000000000000000003");
        let terminal = observed
            .clone()
            .transition_basic(CorrosionBasicTransition::Succeeded {
                completed_at: at("2026-08-05T10:00:01Z"),
            })
            .expect("legal terminal");
        assert!(is_legal_terminal_recovery(&observed, &terminal));

        let other_terminal = basic_created("01J00000000000000000000004")
            .transition_basic(CorrosionBasicTransition::Succeeded {
                completed_at: at("2026-08-05T10:00:01Z"),
            })
            .expect("legal terminal");
        assert!(!is_legal_terminal_recovery(&observed, &other_terminal));
    }

    #[test]
    fn terminal_recovery_rejects_rewritten_creation_and_start_timestamps() {
        let created = basic_created("01J00000000000000000000003");
        let terminal = created
            .clone()
            .transition_basic(CorrosionBasicTransition::Succeeded {
                completed_at: at("2026-08-05T10:00:03Z"),
            })
            .expect("terminal");
        let mut rewritten_created = serde_json::to_value(&terminal).expect("terminal json");
        let Some(created_at) = rewritten_created.get_mut("created_at") else {
            panic!("created_at");
        };
        *created_at = serde_json::json!("2026-08-05T09:59:59Z");
        let rewritten_created =
            serde_json::from_value(rewritten_created).expect("valid rewritten terminal");
        assert!(!is_legal_terminal_recovery(&created, &rewritten_created));

        let running = created
            .transition_basic(CorrosionBasicTransition::Running {
                started_at: at("2026-08-05T10:00:01Z"),
            })
            .expect("running");
        let terminal = running
            .clone()
            .transition_basic(CorrosionBasicTransition::Succeeded {
                completed_at: at("2026-08-05T10:00:03Z"),
            })
            .expect("terminal");
        let mut rewritten_started = serde_json::to_value(&terminal).expect("terminal json");
        let Some(started_at) = rewritten_started.get_mut("started_at") else {
            panic!("started_at");
        };
        *started_at = serde_json::json!("2026-08-05T10:00:02Z");
        let rewritten_started =
            serde_json::from_value(rewritten_started).expect("valid rewritten terminal");
        assert!(!is_legal_terminal_recovery(&running, &rewritten_started));
    }

    fn basic_created(service_id: &str) -> OperationDocument {
        OperationDocument::basic_created(
            CorrosionDocumentVersion::V1,
            ClusterId::try_new("01J00000000000000000000005").expect("cluster"),
            MachineRowId::try_new("01J00000000000000000000006").expect("machine"),
            OperationInitiator::Peer {
                peer_id: PeerId::try_new("01J00000000000000000000007").expect("peer"),
            },
            CorrosionBasicOperation::Build {
                service_id: ServiceRowId::try_new(service_id).expect("service"),
            },
            at("2026-08-05T10:00:00Z"),
        )
    }

    fn at(value: &str) -> CorrosionTimestamp {
        CorrosionTimestamp::try_new(value).expect("timestamp")
    }
}
