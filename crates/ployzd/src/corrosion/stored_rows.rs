//! Bounded collection of `id, document` Corrosion query results.

use ployz_core::corrosion::{SqliteValue, StoredRow};

use super::{CorrosionClientError, QueryStream, QueryStreamEvent};

/// The maximum number of stored rows one query collector accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StoredRowLimit(usize);

impl StoredRowLimit {
    #[must_use]
    pub(crate) const fn new(max_rows: usize) -> Self {
        Self(max_rows)
    }

    #[must_use]
    pub(crate) const fn get(self) -> usize {
        self.0
    }
}

/// A malformed or over-large `SELECT id, document` Corrosion response.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoredRowCollectionError {
    #[error(transparent)]
    Client(#[from] CorrosionClientError),
    #[error("Corrosion query omitted its columns frame")]
    MissingColumns,
    #[error("Corrosion query repeated its columns frame")]
    DuplicateColumns,
    #[error("Corrosion query returned a row before its columns frame")]
    RowBeforeColumns,
    #[error("Corrosion query returned columns other than id and document: {columns:?}")]
    UnexpectedColumns { columns: Vec<String> },
    #[error("Corrosion query returned an unexpected stored row: {values:?}")]
    UnexpectedRow { values: Vec<SqliteValue> },
    #[error("Corrosion query exceeded its {limit}-row bound")]
    RowLimit { limit: usize },
}

/// Collects one complete, bounded `SELECT id, document` result set.
///
/// A query has exactly one `id, document` columns frame, zero or more text
/// rows after that frame, and one end-of-query frame. `QueryStream` turns a
/// transport EOF before the end frame into [`CorrosionClientError`], and this
/// collector treats a completed stream without that frame the same way.
pub(crate) async fn collect_stored_rows(
    stream: &mut QueryStream,
    limit: StoredRowLimit,
) -> Result<Vec<StoredRow>, StoredRowCollectionError> {
    let mut collector = StoredRowCollector::new(limit);

    loop {
        let Some(event) = stream.next().await? else {
            return collector.finish_without_end();
        };
        if let Some(rows) = collector.accept(event)? {
            return Ok(rows);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredRowPhase {
    AwaitingColumns,
    Rows,
}

struct StoredRowCollector {
    limit: StoredRowLimit,
    phase: StoredRowPhase,
    rows: Vec<StoredRow>,
}

impl StoredRowCollector {
    fn new(limit: StoredRowLimit) -> Self {
        Self {
            limit,
            phase: StoredRowPhase::AwaitingColumns,
            rows: Vec::new(),
        }
    }

    fn accept(
        &mut self,
        event: QueryStreamEvent,
    ) -> Result<Option<Vec<StoredRow>>, StoredRowCollectionError> {
        match event {
            QueryStreamEvent::Columns(columns) => match self.phase {
                StoredRowPhase::AwaitingColumns if columns == ["id", "document"] => {
                    self.phase = StoredRowPhase::Rows;
                    Ok(None)
                }
                StoredRowPhase::AwaitingColumns => {
                    Err(StoredRowCollectionError::UnexpectedColumns { columns })
                }
                StoredRowPhase::Rows => Err(StoredRowCollectionError::DuplicateColumns),
            },
            QueryStreamEvent::Row(_, values) => {
                if self.phase == StoredRowPhase::AwaitingColumns {
                    return Err(StoredRowCollectionError::RowBeforeColumns);
                }
                let [SqliteValue::Text(key), SqliteValue::Text(document)] = values.as_slice()
                else {
                    return Err(StoredRowCollectionError::UnexpectedRow { values });
                };
                if self.rows.len() == self.limit.get() {
                    return Err(StoredRowCollectionError::RowLimit {
                        limit: self.limit.get(),
                    });
                }
                self.rows
                    .push(StoredRow::new(key.clone(), document.clone()));
                Ok(None)
            }
            QueryStreamEvent::EndOfQuery(_) => match self.phase {
                StoredRowPhase::AwaitingColumns => Err(StoredRowCollectionError::MissingColumns),
                StoredRowPhase::Rows => Ok(Some(std::mem::take(&mut self.rows))),
            },
        }
    }

    fn finish_without_end(self) -> Result<Vec<StoredRow>, StoredRowCollectionError> {
        let _ = self;
        Err(CorrosionClientError::MissingEndOfQuery.into())
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{EndOfQuery, RowId};

    use super::*;

    fn end_of_query() -> QueryStreamEvent {
        QueryStreamEvent::EndOfQuery(EndOfQuery {
            time: 0.0,
            change_id: None,
        })
    }

    fn row(key: &str, document: &str) -> QueryStreamEvent {
        QueryStreamEvent::Row(
            RowId::new(1),
            vec![
                SqliteValue::Text(key.to_owned()),
                SqliteValue::Text(document.to_owned()),
            ],
        )
    }

    #[test]
    fn collects_rows_after_the_expected_columns_and_end_frame() {
        let mut collector = StoredRowCollector::new(StoredRowLimit::new(2));

        assert_eq!(
            collector
                .accept(QueryStreamEvent::Columns(vec![
                    "id".to_owned(),
                    "document".to_owned(),
                ]))
                .expect("expected columns"),
            None
        );
        assert_eq!(
            collector.accept(row("one", "first")).expect("first row"),
            None
        );
        assert_eq!(
            collector.accept(row("two", "second")).expect("second row"),
            None
        );

        let rows = collector
            .accept(end_of_query())
            .expect("end-of-query")
            .expect("complete rows");
        assert_eq!(
            rows,
            [
                StoredRow::new("one", "first"),
                StoredRow::new("two", "second"),
            ]
        );
    }

    #[test]
    fn rejects_a_row_before_columns() {
        let error = StoredRowCollector::new(StoredRowLimit::new(1))
            .accept(row("one", "first"))
            .expect_err("row before columns must fail");

        assert!(matches!(error, StoredRowCollectionError::RowBeforeColumns));
    }

    #[test]
    fn rejects_wrong_and_duplicate_columns() {
        let wrong_columns = StoredRowCollector::new(StoredRowLimit::new(1))
            .accept(QueryStreamEvent::Columns(vec!["document".to_owned()]))
            .expect_err("wrong columns must fail");
        assert!(matches!(
            wrong_columns,
            StoredRowCollectionError::UnexpectedColumns { columns } if columns == ["document"]
        ));

        let mut collector = StoredRowCollector::new(StoredRowLimit::new(1));
        _ = collector
            .accept(QueryStreamEvent::Columns(vec![
                "id".to_owned(),
                "document".to_owned(),
            ]))
            .expect("expected columns");
        let duplicate = collector
            .accept(QueryStreamEvent::Columns(vec![
                "id".to_owned(),
                "document".to_owned(),
            ]))
            .expect_err("duplicate columns must fail");
        assert!(matches!(
            duplicate,
            StoredRowCollectionError::DuplicateColumns
        ));
    }

    #[test]
    fn rejects_non_text_stored_rows_and_missing_columns() {
        let mut collector = StoredRowCollector::new(StoredRowLimit::new(1));
        _ = collector
            .accept(QueryStreamEvent::Columns(vec![
                "id".to_owned(),
                "document".to_owned(),
            ]))
            .expect("expected columns");
        let unexpected_row = collector
            .accept(QueryStreamEvent::Row(
                RowId::new(1),
                vec![SqliteValue::Integer(1), SqliteValue::Text("doc".to_owned())],
            ))
            .expect_err("non-text key must fail");
        assert!(matches!(
            unexpected_row,
            StoredRowCollectionError::UnexpectedRow { .. }
        ));

        let missing_columns = StoredRowCollector::new(StoredRowLimit::new(1))
            .accept(end_of_query())
            .expect_err("end before columns must fail");
        assert!(matches!(
            missing_columns,
            StoredRowCollectionError::MissingColumns
        ));
    }

    #[test]
    fn enforces_the_caller_supplied_row_limit() {
        let mut collector = StoredRowCollector::new(StoredRowLimit::new(1));
        _ = collector
            .accept(QueryStreamEvent::Columns(vec![
                "id".to_owned(),
                "document".to_owned(),
            ]))
            .expect("expected columns");
        _ = collector.accept(row("one", "first")).expect("first row");

        let error = collector
            .accept(row("two", "second"))
            .expect_err("second row exceeds the bound");
        assert!(matches!(
            error,
            StoredRowCollectionError::RowLimit { limit: 1 }
        ));
    }

    #[test]
    fn rejects_a_stream_that_ends_without_an_end_frame() {
        let error = StoredRowCollector::new(StoredRowLimit::new(1))
            .finish_without_end()
            .expect_err("an end-of-query frame is required");

        assert!(matches!(
            error,
            StoredRowCollectionError::Client(CorrosionClientError::MissingEndOfQuery)
        ));
    }
}
