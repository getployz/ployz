//! The only Rust writer of `uptime_incident` and `uptime_alert`.
//!
//! The detector's `Step::Open` and `Step::Close` become [`IncidentOp`]s on a bounded queue. One
//! task runs them in order, each as one fenced statement whose incident row and outbox row commit
//! together, and reports every op's outcome as a [`Written`] so the engine can adopt what applied
//! and re-derive what didn't.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::time::sleep;
use tokio_postgres::Transaction;
use tokio_postgres::types::Type;

use crate::control::detect::{Evidence, EvidenceItem, Step};
use crate::control::pg::{Pg, PgError, column};
use crate::domain::{Check, CheckId, IncidentId, OrgId, Quorum, Revision, UnixMs, http_error_str};

/// `controlPlaneStatements.open` in `dashboard/src/modules/uptime/incident-fence.ts`, verbatim
/// up to whitespace. A test holds the two copies together.
/// $1 check id, $2 revision, $3 incident id, $4 opened_at, $5 opened_by, $6 alert payload.
pub const OPEN: &str = "
  WITH target AS (
    SELECT id, organization_id, revision FROM uptime_check
    WHERE id = $1 AND revision = $2 AND enabled
    FOR SHARE),
  opened AS (
    INSERT INTO uptime_incident (id, organization_id, check_id, check_revision,
      opened_at, opened_by)
    SELECT $3, organization_id, id, revision, $4, $5 FROM target
    ON CONFLICT (check_id) WHERE closed_at IS NULL DO NOTHING
    RETURNING id, organization_id),
  alerted AS (
    INSERT INTO uptime_alert (organization_id, incident_id, kind, payload)
    SELECT organization_id, id, 'opened', $6 FROM opened)
  SELECT count(*)::int AS applied FROM opened;";

/// `controlPlaneStatements.observedClose`, held to the TS copy the same way.
/// $1 incident id, $2 revision, $3 closed_at, $4 closed_by, $5 alert payload.
pub const OBSERVED_CLOSE: &str = "
  WITH closed AS (
    UPDATE uptime_incident i SET closed_at = $3, closed_by = $4
    WHERE i.id = $1 AND i.check_revision = $2 AND i.closed_at IS NULL
      AND EXISTS (SELECT 1 FROM uptime_check c
        WHERE c.id = i.check_id AND c.enabled AND c.revision = $2)
    RETURNING i.id, i.organization_id),
  alerted AS (
    INSERT INTO uptime_alert (organization_id, incident_id, kind, payload)
    SELECT organization_id, id, 'resolved', $5 FROM closed
    ON CONFLICT (incident_id, kind) DO NOTHING)
  SELECT count(*)::int AS applied FROM closed;";

/// Only this writer records an observed transition, and it records each with its alert, so the
/// alert row is proof the op committed even when a statement replay touched nothing.
const RECORDED: &str = "SELECT EXISTS (SELECT 1 FROM uptime_alert \
    WHERE incident_id = $1 AND kind = $2) AS recorded";

const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transition {
    Opened,
    Resolved,
}

impl Transition {
    /// The `uptime_alert.kind` spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Transition::Opened => "opened",
            Transition::Resolved => "resolved",
        }
    }
}

/// One fenced incident write. Both kinds name the revision the detector judged at; Postgres
/// refuses a stale one.
#[derive(Clone, Debug)]
pub struct IncidentOp {
    pub transition: Transition,
    pub incident: IncidentId,
    pub check: CheckId,
    pub org: OrgId,
    pub revision: Revision,
    /// Snapshots for the alert payload.
    pub name: Arc<str>,
    pub url: Arc<str>,
    /// `opened_at` or `closed_at`.
    pub at: UnixMs,
    pub evidence: Evidence,
}

impl IncidentOp {
    /// The write a detector step asks for, if any.
    #[must_use]
    pub fn of(step: Step, check: &Check, at: UnixMs) -> Option<IncidentOp> {
        let (transition, incident, revision, evidence) = match step {
            Step::Nothing | Step::Dispatch(_) => return None,
            Step::Open {
                incident,
                revision,
                evidence,
            } => (Transition::Opened, incident, revision, evidence),
            Step::Close {
                incident,
                revision,
                evidence,
            } => (Transition::Resolved, incident, revision, evidence),
        };
        Some(IncidentOp {
            transition,
            incident,
            check: check.id,
            org: check.org,
            revision,
            name: check.name.clone(),
            url: check.url.clone(),
            at,
            evidence,
        })
    }
}

/// An op's outcome. `applied: false` means Postgres refused it (a stale revision, an incident
/// already closed or already open) or it failed for good: the engine re-derives from Postgres.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Written {
    pub check: CheckId,
    pub incident: IncidentId,
    pub transition: Transition,
    pub applied: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Submit {
    Queued,
    /// The queue is full or the writer is gone. Nothing was queued; the engine resyncs.
    Full,
}

/// A bounded FIFO in front of one task that runs ops one at a time, retrying with backoff while
/// Postgres is unreachable or its schema is behind. Dropping the handle lets the task drain the
/// queue and stop.
pub struct IncidentWriter {
    queue: mpsc::Sender<IncidentOp>,
}

impl IncidentWriter {
    pub const QUEUE: usize = 10_000;

    #[must_use]
    pub fn spawn(pg: Pg, capacity: usize, written: mpsc::Sender<Written>) -> IncidentWriter {
        let (queue, mut ops) = mpsc::channel::<IncidentOp>(capacity);
        tokio::spawn(async move {
            while let Some(op) = ops.recv().await {
                let applied = write_until_settled(&pg, &op).await;
                let report = Written {
                    check: op.check,
                    incident: op.incident,
                    transition: op.transition,
                    applied,
                };
                if written.send(report).await.is_err() {
                    return;
                }
            }
        });
        IncidentWriter { queue }
    }

    #[must_use]
    pub fn submit(&self, op: IncidentOp) -> Submit {
        match self.queue.try_send(op) {
            Ok(()) => Submit::Queued,
            Err(TrySendError::Full(_) | TrySendError::Closed(_)) => Submit::Full,
        }
    }
}

async fn write_until_settled(pg: &Pg, op: &IncidentOp) -> bool {
    let mut backoff = BACKOFF_MIN;
    loop {
        match pg.tx(async |tx| write(tx, op).await).await {
            Ok(applied) => return applied,
            Err(PgError::Fatal(reason)) => {
                tracing::error!(
                    check = %op.check.0,
                    incident = %op.incident.0,
                    transition = op.transition.as_str(),
                    %reason,
                    "incident write refused; the engine re-derives from Postgres",
                );
                return false;
            }
            Err(error @ (PgError::Transient(_) | PgError::SchemaNotReady { .. })) => {
                tracing::warn!(incident = %op.incident.0, %error, "incident write will retry");
                sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

async fn write(tx: &Transaction<'_>, op: &IncidentOp) -> Result<bool, PgError> {
    let evidence = evidence_json(&op.evidence);
    let payload = json!({
        "organizationId": op.org.0.to_string(),
        "checkId": op.check.0.to_string(),
        "checkName": &*op.name,
        "url": &*op.url,
        "transition": op.transition.as_str(),
        "evidence": evidence,
        "at": iso(op.at),
    });
    let at = op.at.to_system_time();
    let row = match op.transition {
        Transition::Opened => {
            tx.query_typed_one(
                OPEN,
                &[
                    (&op.check.0, Type::UUID),
                    (&op.revision.0, Type::INT4),
                    (&op.incident.0, Type::UUID),
                    (&at, Type::TIMESTAMPTZ),
                    (&evidence, Type::JSONB),
                    (&payload, Type::JSONB),
                ],
            )
            .await?
        }
        Transition::Resolved => {
            let mut closed_by = evidence;
            if let Value::Object(fields) = &mut closed_by {
                fields.insert("kind".into(), "observed".into());
            }
            tx.query_typed_one(
                OBSERVED_CLOSE,
                &[
                    (&op.incident.0, Type::UUID),
                    (&op.revision.0, Type::INT4),
                    (&at, Type::TIMESTAMPTZ),
                    (&closed_by, Type::JSONB),
                    (&payload, Type::JSONB),
                ],
            )
            .await?
        }
    };
    if column::<i32>(&row, "applied")? > 0 {
        return Ok(true);
    }
    // A retry after a commit whose reply was lost lands here too: report what Postgres holds.
    let kind = op.transition.as_str();
    let row = tx
        .query_typed_one(
            RECORDED,
            &[(&op.incident.0, Type::UUID), (&kind, Type::TEXT)],
        )
        .await?;
    column(&row, "recorded")
}

/// The `IncidentEvidence` shape the dashboard reads from `opened_by` and an observed `closed_by`.
fn evidence_json(evidence: &Evidence) -> Value {
    let quorum = match evidence.quorum {
        Quorum::DistinctRegions(regions) => json!({ "distinctRegions": regions }),
        Quorum::OneRegion { probes, spacing } => json!({
            "oneRegion": {
                "probes": probes,
                "spacingMs": u64::try_from(spacing.as_millis()).unwrap_or(u64::MAX),
            }
        }),
    };
    let observations: Vec<Value> = evidence.observations.iter().map(observation_json).collect();
    json!({ "quorum": quorum, "observations": observations })
}

fn observation_json(item: &EvidenceItem) -> Value {
    json!({
        "region": item.region.as_str(),
        "kind": item.kind.as_str(),
        "verdict": item.verdict.as_str(),
        "status": item.status.unwrap_or(0),
        "failedPhase": item.failed_phase.map(|phase| phase.as_str()),
        "error": item.error.map(http_error_str),
        "remoteIp": item.remote_ip.map(|ip| ip.to_string()),
        "at": iso(item.at),
    })
}

/// ISO-8601 in UTC with milliseconds, the dashboard's timestamp spelling.
fn iso(at: UnixMs) -> String {
    let ms = at.0.rem_euclid(1000);
    let secs = at.0.div_euclid(1000);
    let day_secs = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        day_secs / 3600,
        day_secs % 3600 / 60,
        day_secs % 60
    )
}

#[cfg(test)]
#[path = "incidents_tests.rs"]
mod tests;
