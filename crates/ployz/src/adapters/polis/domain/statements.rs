use crate::domain::DomainName;
use crate::operation::ScopeId;

use super::{DOMAIN_STATUS_COLUMNS, DOMAIN_STATUS_TABLE, DomainStatusRow, codecs};

pub(super) fn schema_statements() -> Result<Vec<polis::StoreStatement>, polis::StoreError> {
    polis::schema_statements(&schema_sql())
}

pub(super) fn schema_sql() -> String {
    polis::create_table_sql(DOMAIN_STATUS_TABLE, DOMAIN_STATUS_COLUMNS)
}

pub(super) fn domain_status_row_statement(
    scope: &ScopeId,
    domain: &DomainName,
) -> Result<polis::StoreStatement, polis::StoreError> {
    polis::StoreStatement::with_params(
        format!(
            "SELECT {}
        FROM {DOMAIN_STATUS_TABLE}
        WHERE scope = ?1
          AND domain = ?2",
            polis::select_columns(DOMAIN_STATUS_COLUMNS)
        ),
        vec![
            polis::StoreParam::Text(scope.as_str().to_string()),
            polis::StoreParam::Text(domain.as_str().to_string()),
        ],
    )
}

pub(super) fn domain_upsert_statements(
    row: &DomainStatusRow,
) -> Result<Vec<polis::StoreStatement>, polis::StoreError> {
    Ok(vec![domain_upsert_statement(row)?])
}

fn domain_upsert_statement(
    row: &DomainStatusRow,
) -> Result<polis::StoreStatement, polis::StoreError> {
    polis::StoreStatement::with_params(
        format!(
            "INSERT INTO {DOMAIN_STATUS_TABLE} (
            scope,
            domain,
            status,
            operation,
            idempotency,
            status_payload
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(scope, domain) DO UPDATE SET
            status = excluded.status,
            operation = excluded.operation,
            idempotency = excluded.idempotency,
            status_payload = excluded.status_payload"
        ),
        vec![
            polis::StoreParam::Text(row.write.scope().as_str().to_string()),
            polis::StoreParam::Text(row.domain.as_str().to_string()),
            polis::StoreParam::Text(codecs::domain_status_kind_tag(row.status.kind()).to_string()),
            polis::StoreParam::Text(row.write.operation().as_str().to_string()),
            polis::StoreParam::Text(row.write.idempotency().as_str().to_string()),
            polis::StoreParam::Text(codecs::status_payload(&row.status)?),
        ],
    )
}
