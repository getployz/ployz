//! Which checks exist, from Postgres, without NOTIFY and without an `updated_at` cursor.
//!
//! [`CheckFeed::load`] takes the change-log cursor first, then keyset-loads every enabled check in
//! pages, each its own short transaction. Anything committed during the load has an xid at or
//! above that cursor, so [`CheckFeed::next`] replays it. `next` tails `organization_change` for
//! the two uptime tables and re-reads the checks it names. The log's ids are only hints: the
//! re-read decides whether a check is still held.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio_postgres::types::Type;
use tokio_postgres::{Row, Transaction};
use uuid::Uuid;

use crate::control::pg::{Pg, PgError};
use crate::domain::{
    BadExpect, BadInterval, BadTarget, Check, CheckId, Expect, IncidentId, Interval, OrgId,
    Revision, Target, UnixMs,
};

const PAGE: i64 = 10_000;

const HORIZON: &str = "SELECT pg_snapshot_xmin(pg_current_snapshot())::text AS cursor";

const SELECT: &str = "SELECT c.id, c.organization_id, c.name, c.url, c.interval_seconds, \
    c.timeout_ms, c.expect, c.enabled, c.revision, \
    i.id AS incident_id, i.check_revision, i.opened_at \
    FROM uptime_check c \
    LEFT JOIN uptime_incident i ON i.check_id = c.id AND i.closed_at IS NULL";

const TAIL: &str = "WITH horizon AS (SELECT pg_snapshot_xmin(pg_current_snapshot()) AS xid), \
    logged AS (SELECT organization_id, changed_ids, deleted_ids, all_rows FROM organization_change \
      WHERE source_table IN ('uptime_check', 'uptime_incident') \
        AND xid >= $1::text::xid8 AND xid < (SELECT xid FROM horizon)) \
    SELECT (SELECT xid::text FROM horizon) AS cursor, \
      coalesce($1::text::xid8 < (SELECT min(xid) FROM organization_change), false) AS expired, \
      array(SELECT DISTINCT unnest(changed_ids || deleted_ids) FROM logged) AS check_ids, \
      array(SELECT DISTINCT organization_id FROM logged WHERE all_rows) AS full_orgs";

#[derive(Clone, Debug)]
pub struct OpenIncident {
    pub id: IncidentId,
    pub revision: Revision,
    pub since: UnixMs,
}

#[derive(Clone, Debug)]
pub struct CheckRow {
    pub check: Check,
    pub open_incident: Option<OpenIncident>,
}

/// A row the feed refused. It is not held: a check that becomes malformed is dropped.
#[derive(Debug)]
pub struct Skipped {
    pub check: CheckId,
    pub org: OrgId,
    pub reason: BadRow,
}

#[derive(Debug, thiserror::Error)]
pub enum BadRow {
    #[error(transparent)]
    Target(#[from] BadTarget),
    #[error(transparent)]
    Interval(#[from] BadInterval),
    #[error("Timeout of {0}ms is outside 1000..=30000 or not below the interval.")]
    Timeout(i32),
    #[error(transparent)]
    Expect(#[from] BadExpect),
}

/// Every enabled check, as of one load.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub rows: Vec<CheckRow>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug)]
pub enum FeedEvent {
    /// The complete set: boot, an expired cursor, or a requested reload.
    Loaded(Snapshot),
    /// One tail window. Apply in order: drop every held check of `replaced_orgs` (a statement
    /// touched too many of their rows to name them), drop `gone`, then hold `reloaded`.
    Changed {
        reloaded: Vec<CheckRow>,
        /// Hinted checks that are deleted, disabled or now malformed.
        gone: Vec<CheckId>,
        replaced_orgs: Vec<OrgId>,
        skipped: Vec<Skipped>,
    },
}

/// The enabled checks the control plane holds, kept current by applying [`FeedEvent`]s.
#[derive(Debug, Default)]
pub struct Checks(HashMap<CheckId, CheckRow>);

impl Checks {
    pub fn apply(&mut self, event: FeedEvent) {
        match event {
            FeedEvent::Loaded(snapshot) => {
                self.0 = snapshot.rows.into_iter().map(|r| (r.check.id, r)).collect();
            }
            FeedEvent::Changed {
                reloaded,
                gone,
                replaced_orgs,
                skipped: _,
            } => {
                if !replaced_orgs.is_empty() {
                    let orgs: HashSet<OrgId> = replaced_orgs.into_iter().collect();
                    self.0.retain(|_, row| !orgs.contains(&row.check.org));
                }
                for id in gone {
                    self.0.remove(&id);
                }
                self.0.extend(reloaded.into_iter().map(|r| (r.check.id, r)));
            }
        }
    }

    #[must_use]
    pub fn get(&self, id: CheckId) -> Option<&CheckRow> {
        self.0.get(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Mirrors `readChangeWindow` in the dashboard's change-log.server.ts, across organizations.
pub struct CheckFeed {
    pg: Pg,
    /// A `pg_snapshot_xmin` as text. Advanced only when a whole window has been read.
    cursor: String,
}

impl CheckFeed {
    /// The cursor first, then keyset pages of enabled checks.
    ///
    /// # Errors
    /// Any statement failing; [`PgError::SchemaNotReady`] when the uptime tables aren't there yet.
    pub async fn load(pg: Pg) -> Result<(CheckFeed, Snapshot), PgError> {
        let mut feed = CheckFeed {
            pg,
            cursor: String::new(),
        };
        let snapshot = feed.load_all().await?;
        Ok((feed, snapshot))
    }

    /// Asks for a fresh cursor and a full load, for when held state can't be trusted.
    ///
    /// # Errors
    /// As [`CheckFeed::load`]. The held cursor is kept on failure.
    pub async fn reload(&mut self) -> Result<FeedEvent, PgError> {
        self.load_all().await.map(FeedEvent::Loaded)
    }

    async fn load_all(&mut self) -> Result<Snapshot, PgError> {
        let cursor = self
            .pg
            .tx(async |tx| {
                let row = tx.query_typed_one(HORIZON, &[]).await?;
                column::<String>(&row, "cursor")
            })
            .await?;
        let mut snapshot = Snapshot::default();
        let mut after = Uuid::nil();
        loop {
            let page = self
                .pg
                .tx(async |tx| {
                    let sql =
                        format!("{SELECT} WHERE c.id > $1 AND c.enabled ORDER BY c.id LIMIT $2");
                    let rows = tx
                        .query_typed(&sql, &[(&after, Type::UUID), (&PAGE, Type::INT8)])
                        .await?;
                    Ok(rows)
                })
                .await?;
            let Some(last) = page.last() else { break };
            after = column(last, "id")?;
            let full = page.len() == usize::try_from(PAGE).unwrap_or(usize::MAX);
            for row in &page {
                match parse(row)? {
                    Parsed::Held(row) => snapshot.rows.push(row),
                    Parsed::Skipped(skipped) => snapshot.skipped.push(log(skipped)),
                    Parsed::Disabled => {}
                }
            }
            if !full {
                break;
            }
        }
        self.cursor = cursor;
        Ok(snapshot)
    }

    /// One tail window. Call it about once a second. It always reads one row, so an empty window
    /// still advances the cursor and still notices that retention passed it, which turns into a
    /// full reload. On any error the cursor stays put and the next call reads the same window.
    ///
    /// # Errors
    /// Any statement failing; [`PgError::SchemaNotReady`] when the uptime tables aren't there yet.
    pub async fn next(&mut self) -> Result<FeedEvent, PgError> {
        let cursor = self.cursor.clone();
        let window = self
            .pg
            .tx(async |tx| {
                let row = tx.query_typed_one(TAIL, &[(&cursor, Type::TEXT)]).await?;
                let next: String = column(&row, "cursor")?;
                if column::<bool>(&row, "expired")? {
                    return Ok(None);
                }
                let hinted: Vec<String> = column(&row, "check_ids")?;
                let orgs: Vec<Uuid> = column(&row, "full_orgs")?;
                let ids: Vec<Uuid> = hinted.iter().filter_map(|id| id.parse().ok()).collect();
                let event = if ids.is_empty() && orgs.is_empty() {
                    FeedEvent::Changed {
                        reloaded: Vec::new(),
                        gone: Vec::new(),
                        replaced_orgs: Vec::new(),
                        skipped: Vec::new(),
                    }
                } else {
                    reread(tx, &ids, &orgs).await?
                };
                Ok(Some((next, event)))
            })
            .await?;
        match window {
            Some((next, event)) => {
                self.cursor = next;
                Ok(event)
            }
            None => self.reload().await,
        }
    }
}

async fn reread(tx: &Transaction<'_>, ids: &[Uuid], orgs: &[Uuid]) -> Result<FeedEvent, PgError> {
    let sql =
        format!("{SELECT} WHERE c.id = ANY($1::uuid[]) OR c.organization_id = ANY($2::uuid[])");
    let rows = tx
        .query_typed(&sql, &[(&ids, Type::UUID_ARRAY), (&orgs, Type::UUID_ARRAY)])
        .await?;
    let mut reloaded = Vec::new();
    let mut skipped = Vec::new();
    let mut gone: HashSet<CheckId> = ids.iter().copied().map(CheckId).collect();
    for row in &rows {
        match parse(row)? {
            Parsed::Held(row) => {
                gone.remove(&row.check.id);
                reloaded.push(row);
            }
            Parsed::Skipped(row) => skipped.push(log(row)),
            Parsed::Disabled => {}
        }
    }
    Ok(FeedEvent::Changed {
        reloaded,
        gone: gone.into_iter().collect(),
        replaced_orgs: orgs.iter().copied().map(OrgId).collect(),
        skipped,
    })
}

enum Parsed {
    Held(CheckRow),
    Disabled,
    Skipped(Skipped),
}

/// A column the build can't read is a schema the build doesn't match, not a bad row.
fn parse(row: &Row) -> Result<Parsed, PgError> {
    let id = CheckId(column(row, "id")?);
    if !column::<bool>(row, "enabled")? {
        return Ok(Parsed::Disabled);
    }
    let org = OrgId(column(row, "organization_id")?);
    let url: String = column(row, "url")?;
    let interval: i32 = column(row, "interval_seconds")?;
    let timeout_ms: i32 = column(row, "timeout_ms")?;
    let expect: String = column(row, "expect")?;
    let revision = Revision(column(row, "revision")?);
    let name: String = column(row, "name")?;
    let open_incident = match column::<Option<Uuid>>(row, "incident_id")? {
        None => None,
        Some(incident) => Some(OpenIncident {
            id: IncidentId(incident),
            revision: Revision(column(row, "check_revision")?),
            since: unix_ms(column(row, "opened_at")?),
        }),
    };
    let checked = (|| -> Result<Check, BadRow> {
        let interval = Interval::parse(u32::try_from(interval).unwrap_or(0))?;
        let timeout = u64::try_from(timeout_ms)
            .ok()
            .filter(|ms| (1000..=30_000).contains(ms))
            .map(Duration::from_millis)
            .filter(|t| *t < interval.as_duration())
            .ok_or(BadRow::Timeout(timeout_ms))?;
        Ok(Check {
            id,
            org,
            revision,
            name: Arc::from(name),
            target: Arc::new(Target::parse(&url)?),
            url: Arc::from(url.as_str()),
            interval,
            timeout,
            expect: Expect::parse(&expect)?,
        })
    })();
    Ok(match checked {
        Ok(check) => Parsed::Held(CheckRow {
            check,
            open_incident,
        }),
        Err(reason) => Parsed::Skipped(Skipped {
            check: id,
            org,
            reason,
        }),
    })
}

fn log(skipped: Skipped) -> Skipped {
    tracing::warn!(
        check = %skipped.check.0,
        org = %skipped.org.0,
        reason = %skipped.reason,
        "Skipped an uptime_check row the checker can't probe; it is not held.",
    );
    skipped
}

fn column<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a Row,
    name: &str,
) -> Result<T, PgError> {
    row.try_get(name)
        .map_err(|e| PgError::Fatal(format!("Column {name} doesn't read as expected: {e}")))
}

fn unix_ms(at: SystemTime) -> UnixMs {
    let ms = match at.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    };
    UnixMs(ms)
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
