use std::num::NonZeroU8;

use crate::{CorrosionStore, StoreParam, StoreResult, StoreStatement, StoreTimeout, StoreValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorePrimaryKey {
    None,
    Order(NonZeroU8),
}

impl StorePrimaryKey {
    #[must_use]
    pub const fn order(order: u8) -> Self {
        match NonZeroU8::new(order) {
            Some(order) => Self::Order(order),
            None => panic!("store primary key order must be nonzero"),
        }
    }

    fn sqlite_value(self) -> i64 {
        match self {
            Self::None => 0,
            Self::Order(order) => i64::from(order.get()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreTableColumn {
    name: &'static str,
    sql_type: &'static str,
    primary_key: StorePrimaryKey,
    check: Option<&'static str>,
    default_value: Option<&'static str>,
    not_null: bool,
}

impl StoreTableColumn {
    #[must_use]
    pub const fn new(
        name: &'static str,
        sql_type: &'static str,
        primary_key: StorePrimaryKey,
        check: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            sql_type,
            primary_key,
            check,
            default_value: None,
            not_null: true,
        }
    }

    #[must_use]
    pub const fn nullable(
        name: &'static str,
        sql_type: &'static str,
        primary_key: StorePrimaryKey,
        check: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            sql_type,
            primary_key,
            check,
            default_value: None,
            not_null: false,
        }
    }

    #[must_use]
    pub const fn with_default(
        name: &'static str,
        sql_type: &'static str,
        primary_key: StorePrimaryKey,
        check: Option<&'static str>,
        default_value: &'static str,
    ) -> Self {
        Self {
            name,
            sql_type,
            primary_key,
            check,
            default_value: Some(default_value),
            not_null: true,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    fn definition(self) -> String {
        let default_value = self
            .default_value
            .map(|value| format!(" DEFAULT {value}"))
            .unwrap_or_default();
        let check = self
            .check
            .map(|check| format!(" CHECK({check})"))
            .unwrap_or_default();
        let not_null = if self.not_null { " NOT NULL" } else { "" };
        format!(
            "{} {}{}{}{}",
            self.name, self.sql_type, not_null, default_value, check
        )
    }

    fn expected(self) -> (String, String, i64, i64, Option<String>) {
        (
            self.name.to_string(),
            self.sql_type.to_string(),
            i64::from(self.not_null),
            self.primary_key.sqlite_value(),
            self.default_value.map(str::to_string),
        )
    }
}

#[must_use]
pub fn create_table_sql(table: &str, columns: &[StoreTableColumn]) -> String {
    let mut definitions = columns
        .iter()
        .map(|column| column.definition())
        .collect::<Vec<_>>();
    let mut primary_key = columns
        .iter()
        .filter(|column| matches!(column.primary_key, StorePrimaryKey::Order(_)))
        .collect::<Vec<_>>();
    primary_key.sort_by_key(|column| column.primary_key);
    if !primary_key.is_empty() {
        definitions.push(format!(
            "PRIMARY KEY({})",
            primary_key
                .into_iter()
                .map(|column| column.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
    {}
);",
        definitions.join(",\n    "),
    )
}

pub fn schema_statements(sql: &str) -> StoreResult<Vec<StoreStatement>> {
    sql.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(StoreStatement::new)
        .collect()
}

#[must_use]
pub fn select_columns(columns: &[StoreTableColumn]) -> String {
    columns
        .iter()
        .map(|column| column.name)
        .collect::<Vec<_>>()
        .join(",\n            ")
}

pub async fn verify_table_schema(
    store: &CorrosionStore,
    timeout: StoreTimeout,
    table: &str,
    expected: &[StoreTableColumn],
) -> StoreResult<()> {
    verify_table_columns(store, timeout, table, expected).await?;
    verify_table_constraints(store, timeout, table, expected).await
}

async fn verify_table_columns(
    store: &CorrosionStore,
    timeout: StoreTimeout,
    table: &str,
    expected: &[StoreTableColumn],
) -> StoreResult<()> {
    let statement = StoreStatement::new(format!(
        "SELECT name, type, \"notnull\" AS not_null, pk, dflt_value
        FROM pragma_table_info('{table}')
        ORDER BY cid"
    ))?;
    let rows = store.query(&statement, timeout).await?;
    let actual = rows
        .rows()
        .iter()
        .map(|row| {
            Ok((
                row.text("name")?.to_string(),
                row.text("type")?.to_string(),
                row.integer("not_null")?,
                row.integer("pk")?,
                match row.value("dflt_value") {
                    Some(StoreValue::Null) => None,
                    Some(StoreValue::Text(value)) => Some(value.clone()),
                    Some(_) | None => return Err(crate::StoreError::MalformedPayload),
                },
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    let expected = expected
        .iter()
        .map(|column| column.expected())
        .collect::<Vec<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(crate::StoreError::MalformedPayload)
    }
}

async fn verify_table_constraints(
    store: &CorrosionStore,
    timeout: StoreTimeout,
    table: &str,
    expected: &[StoreTableColumn],
) -> StoreResult<()> {
    let statement = StoreStatement::with_params(
        "SELECT sql
        FROM sqlite_schema
        WHERE type = 'table'
          AND name = ?1",
        vec![StoreParam::Text(table.to_string())],
    )?;
    let rows = store.query(&statement, timeout).await?;
    let Some(sql) = rows.rows().first().and_then(|row| row.text("sql").ok()) else {
        return Err(crate::StoreError::MalformedPayload);
    };
    let actual = compact_sql(sql);
    if primary_key_matches(&actual, expected)
        && expected
            .iter()
            .filter_map(|column| column.check)
            .all(|check| actual.contains(&compact_sql(&format!("CHECK({check})"))))
    {
        Ok(())
    } else {
        Err(crate::StoreError::MalformedPayload)
    }
}

fn primary_key_matches(actual: &str, expected: &[StoreTableColumn]) -> bool {
    let mut primary_key = expected
        .iter()
        .filter(|column| matches!(column.primary_key, StorePrimaryKey::Order(_)))
        .collect::<Vec<_>>();
    primary_key.sort_by_key(|column| column.primary_key);
    let primary_key = primary_key
        .into_iter()
        .map(|column| column.name)
        .collect::<Vec<_>>();
    primary_key.is_empty()
        || actual.contains(&compact_sql(&format!(
            "PRIMARY KEY({})",
            primary_key.join(", ")
        )))
}

fn compact_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}
