//! Domain status rows over Polis store primitives.

mod codecs;
mod statements;

use crate::acme::CertificateUsability;
use crate::domain::{
    DomainFailure, DomainName, DomainPendingReason, DomainReadyRecord, DomainStatus,
    DomainStatusFailure, DomainStatusPort,
};
use crate::operation::{MutationContext, MutationWriteIdentity, ScopeId};
use crate::serving::ServingGeneration;

const DOMAIN_STATUS_TABLE: &str = "ployz_domain_status";
const DOMAIN_STATUS_COLUMNS: &[polis::StoreTableColumn] = &[
    polis::StoreTableColumn::new(
        "scope",
        "TEXT",
        polis::StorePrimaryKey::order(1),
        Some("length(trim(scope)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "domain",
        "TEXT",
        polis::StorePrimaryKey::order(2),
        Some("length(trim(domain)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "status",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("status IN ('pending', 'ready', 'failed')"),
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
    polis::StoreTableColumn::new("status_payload", "TEXT", polis::StorePrimaryKey::None, None),
];

#[derive(Clone)]
pub(crate) struct CorrosionDomainStatus {
    store: polis::CorrosionStore,
}

impl CorrosionDomainStatus {
    #[must_use]
    pub(crate) fn new(store: polis::CorrosionStore) -> Self {
        Self { store }
    }
}

pub(crate) fn start_corrosion_domain_status(store: polis::CorrosionStore) -> impl DomainStatusPort {
    CorrosionDomainStatus::new(store)
}

pub(crate) fn domain_status_schema_statements()
-> Result<Vec<polis::StoreStatement>, polis::StoreError> {
    statements::schema_statements()
}

pub(crate) async fn verify_domain_status_schema(
    store: &polis::CorrosionStore,
    timeout: polis::StoreTimeout,
) -> Result<(), polis::StoreError> {
    polis::verify_table_schema(store, timeout, DOMAIN_STATUS_TABLE, DOMAIN_STATUS_COLUMNS).await
}

impl DomainStatusPort for CorrosionDomainStatus {
    async fn status(
        &self,
        context: &MutationContext,
        domain: &DomainName,
    ) -> Result<DomainStatus, DomainFailure> {
        self.observe_domain(context.authority().scope(), domain)
            .await
            .map_err(map_domain_store_error)?
            .map(|row| row.into_status())
            .transpose()
            .map(|status| status.unwrap_or(DomainStatus::Unknown))
    }

    async fn record_pending(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        reason: DomainPendingReason,
    ) -> Result<(), DomainFailure> {
        self.upsert_domain(&DomainStatusRow::pending(context, domain.clone(), reason))
            .await
            .map_err(map_domain_store_error)
    }

    async fn record_ready(
        &self,
        context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        self.upsert_domain(&DomainStatusRow::ready(context, ready))
            .await
            .map_err(map_domain_store_error)
    }

    async fn record_failed(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        failure: DomainStatusFailure,
    ) -> Result<(), DomainFailure> {
        self.upsert_domain(&DomainStatusRow::failed(context, domain.clone(), failure))
            .await
            .map_err(map_domain_store_error)
    }
}

impl CorrosionDomainStatus {
    async fn observe_domain(
        &self,
        scope: &ScopeId,
        domain: &DomainName,
    ) -> Result<Option<DomainStatusRow>, polis::StoreError> {
        let statement = statements::domain_status_row_statement(scope, domain)?;
        let rows = self
            .store
            .query(&statement, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        let Some(row) = rows.rows().first() else {
            return Ok(None);
        };
        codecs::decode_domain_status_row(row).map(Some)
    }

    async fn upsert_domain(&self, row: &DomainStatusRow) -> Result<(), polis::StoreError> {
        let statements = statements::domain_upsert_statements(row)?;
        self.store
            .execute_transaction(&statements, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DomainStatusRow {
    domain: DomainName,
    status: StoredDomainStatus,
    write: MutationWriteIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredDomainStatus {
    Pending(DomainPendingReason),
    Ready {
        certificate: CertificateUsability,
        serving_generation: ServingGeneration,
    },
    Failed(DomainStatusFailure),
}

impl DomainStatusRow {
    fn pending(context: &MutationContext, domain: DomainName, reason: DomainPendingReason) -> Self {
        Self {
            domain,
            status: StoredDomainStatus::Pending(reason),
            write: MutationWriteIdentity::from_context(context),
        }
    }

    fn ready(context: &MutationContext, ready: DomainReadyRecord) -> Self {
        Self {
            domain: ready.domain().clone(),
            status: StoredDomainStatus::Ready {
                certificate: ready.certificate().clone(),
                serving_generation: ready.serving_generation(),
            },
            write: MutationWriteIdentity::from_context(context),
        }
    }

    fn failed(context: &MutationContext, domain: DomainName, failure: DomainStatusFailure) -> Self {
        Self {
            domain,
            status: StoredDomainStatus::Failed(failure),
            write: MutationWriteIdentity::from_context(context),
        }
    }

    fn into_status(self) -> Result<DomainStatus, DomainFailure> {
        match self.status {
            StoredDomainStatus::Pending(reason) => Ok(DomainStatus::Pending(reason)),
            StoredDomainStatus::Ready {
                certificate,
                serving_generation,
            } => DomainReadyRecord::new(self.domain, certificate, serving_generation)
                .map(DomainStatus::Ready),
            StoredDomainStatus::Failed(failure) => Ok(DomainStatus::Failed(failure)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DomainStatusKind {
    Pending,
    Ready,
    Failed,
}

impl StoredDomainStatus {
    fn kind(&self) -> DomainStatusKind {
        match self {
            Self::Pending(_) => DomainStatusKind::Pending,
            Self::Ready { .. } => DomainStatusKind::Ready,
            Self::Failed(_) => DomainStatusKind::Failed,
        }
    }
}

fn map_domain_store_error(error: polis::StoreError) -> DomainFailure {
    match error {
        polis::StoreError::MalformedPayload => DomainFailure::StatusRowsPayloadInvalid,
        polis::StoreError::Timeout => DomainFailure::StatusRowsTimeout,
        polis::StoreError::MissedChange { .. } => DomainFailure::StatusRowsMissedChanges,
        polis::StoreError::Stream { .. } => DomainFailure::StatusRowsStreamInterrupted,
        polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => DomainFailure::StatusUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::acme::{
        CertificateActivation, CertificateMaterialState, Hostname, RevocationFreshness,
    };
    use crate::operation::ScopeId;

    #[test]
    fn schema_creates_domain_status_tables_only() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in domain_status_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }

        let mut query = conn
            .prepare(
                "SELECT name FROM sqlite_schema
                WHERE type = 'table'
                  AND name LIKE 'ployz_domain_status%'
                ORDER BY name",
            )
            .expect("prepare");
        let tables = query
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("tables");

        assert_eq!(tables, vec!["ployz_domain_status".to_string()]);
    }

    #[test]
    fn status_query_decodes_payload_row() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in domain_status_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }

        let domain = domain();
        let ready = ready_record();
        let payload = codecs::status_payload(&StoredDomainStatus::Ready {
            certificate: ready.certificate().clone(),
            serving_generation: ready.serving_generation(),
        })
        .expect("payload");
        conn.execute(
            "INSERT INTO ployz_domain_status (
                scope,
                domain,
                status,
                operation,
                idempotency,
                status_payload
            ) VALUES ('cluster', ?1, 'ready', 'operation-1', 'idem-1', ?2)",
            (domain.as_str(), payload),
        )
        .expect("status row");

        let scope = ScopeId::parse("cluster").expect("scope");
        let statement =
            statements::domain_status_row_statement(&scope, &domain).expect("statement");
        let row = conn
            .query_row(statement.sql(), [scope.as_str(), domain.as_str()], |row| {
                Ok((
                    row.get::<_, String>("domain")?,
                    row.get::<_, String>("status")?,
                    row.get::<_, String>("scope")?,
                    row.get::<_, String>("operation")?,
                    row.get::<_, String>("idempotency")?,
                    row.get::<_, String>("status_payload")?,
                ))
            })
            .expect("status row");
        let store_row = polis::StoreRow::from_fields([
            ("domain", polis::StoreValue::Text(row.0)),
            ("status", polis::StoreValue::Text(row.1)),
            ("scope", polis::StoreValue::Text(row.2)),
            ("operation", polis::StoreValue::Text(row.3)),
            ("idempotency", polis::StoreValue::Text(row.4)),
            ("status_payload", polis::StoreValue::Text(row.5)),
        ])
        .expect("store row");

        assert_eq!(
            codecs::decode_domain_status_row(&store_row)
                .expect("decode")
                .into_status(),
            Ok(DomainStatus::Ready(ready_record()))
        );
    }

    #[test]
    fn status_query_rejects_mismatched_status_column_and_payload() {
        let payload = codecs::status_payload(&StoredDomainStatus::Ready {
            certificate: ready_record().certificate().clone(),
            serving_generation: ready_record().serving_generation(),
        })
        .expect("payload");
        let store_row = polis::StoreRow::from_fields([
            (
                "domain",
                polis::StoreValue::Text(domain().as_str().to_string()),
            ),
            ("status", polis::StoreValue::Text("pending".to_string())),
            ("scope", polis::StoreValue::Text("cluster".to_string())),
            (
                "operation",
                polis::StoreValue::Text("operation-1".to_string()),
            ),
            ("idempotency", polis::StoreValue::Text("idem-1".to_string())),
            ("status_payload", polis::StoreValue::Text(payload)),
        ])
        .expect("store row");

        assert_eq!(
            codecs::decode_domain_status_row(&store_row),
            Err(polis::StoreError::MalformedPayload)
        );
    }

    #[test]
    fn store_errors_map_to_distinct_domain_status_failures() {
        assert_eq!(
            map_domain_store_error(polis::StoreError::MalformedPayload),
            DomainFailure::StatusRowsPayloadInvalid
        );
        assert_eq!(
            map_domain_store_error(polis::StoreError::Timeout),
            DomainFailure::StatusRowsTimeout
        );
        assert_eq!(
            map_domain_store_error(polis::StoreError::MissedChange {
                expected: polis::StoreChangeId::new(1),
                got: polis::StoreChangeId::new(3),
            }),
            DomainFailure::StatusRowsMissedChanges
        );
        assert_eq!(
            map_domain_store_error(polis::StoreError::Stream {
                message: "closed".to_string(),
            }),
            DomainFailure::StatusRowsStreamInterrupted
        );
        assert_eq!(
            map_domain_store_error(polis::StoreError::Client {
                message: "client".to_string(),
            }),
            DomainFailure::StatusUnavailable
        );
    }

    fn ready_record() -> DomainReadyRecord {
        DomainReadyRecord::new(
            domain(),
            CertificateUsability {
                hostname: Hostname::parse("app.example.com").expect("hostname"),
                not_after: UNIX_EPOCH + Duration::from_secs(3_600),
                activation: CertificateActivation::Acknowledged,
                material: CertificateMaterialState::PresentProtected,
                revocation: RevocationFreshness::KnownFresh,
            },
            ServingGeneration::new(7),
        )
        .expect("ready")
    }

    fn domain() -> DomainName {
        DomainName::parse("app.example.com").expect("domain")
    }
}
