//! Probe result storage. The engine calls [`Results::push`] and nothing else: it never blocks,
//! never errors, and never learns where rows go.
//!
//! A task drains a bounded buffer into `uptime_result`, one statement per batch. Rows stay in the
//! buffer until their insert commits, so a failed flush retries the same rows. The primary key is
//! the probe key and the insert ignores conflicts, so a replayed batch lands once.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;
use tokio::time::sleep;
use tokio_postgres::Transaction;
use tokio_postgres::types::{ToSql, Type};
use uuid::Uuid;

use crate::control::pg::{Pg, PgError};
use crate::domain::{Judged, NotProbed, Outcome, http_error_str};

/// The contract's unnest insert, joined to the check row it references: a result for a check
/// deleted since it was judged is dropped instead of failing the whole batch on the foreign key.
const INSERT: &str = "INSERT INTO uptime_result (check_id, organization_id, region, scheduled_at, \
    kind, start_lag_ms, verdict, status, failed_phase, error, detail, dns_ms, connect_ms, tls_ms, \
    ttfb_ms, total_ms, remote_ip, cert_not_after, agent) \
    SELECT r.* FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::timestamptz[], $5::text[], \
      $6::int[], $7::text[], $8::int2[], $9::text[], $10::text[], $11::text[], $12::int[], \
      $13::int[], $14::int[], $15::int[], $16::int[], $17::inet[], $18::timestamptz[], \
      $19::text[]) \
      AS r(check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status, \
        failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, remote_ip, \
        cert_not_after, agent) \
    JOIN uptime_check c ON c.id = r.check_id AND c.organization_id = r.organization_id \
    FOR KEY SHARE OF c \
    ON CONFLICT DO NOTHING";

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Rows held at most. Past it the oldest are dropped.
    pub capacity: usize,
    /// Rows per statement, and the buffer size that triggers a flush before `every`.
    pub batch: usize,
    pub every: Duration,
    pub backoff_max: Duration,
}

impl Config {
    pub const PRODUCTION: Config = Config {
        capacity: 500_000,
        batch: 2_000,
        every: Duration::from_secs(1),
        backoff_max: Duration::from_secs(5),
    };
}

const BACKOFF_MIN: Duration = Duration::from_millis(200);

/// The handle the engine holds. Dropping it lets the task flush what it can and stop.
pub struct Results {
    shared: Arc<Shared>,
    batch: usize,
}

struct Shared {
    buffer: Mutex<Buffer>,
    wake: Notify,
    closed: AtomicBool,
}

struct Buffer {
    rows: VecDeque<Row>,
    /// Sequence number of `rows.front()`, so a flush can confirm exactly the rows it sent even
    /// if overflow evicted some of them meanwhile.
    head: u64,
    capacity: usize,
    dropped: u64,
}

impl Results {
    #[must_use]
    pub fn spawn(pg: Pg, config: Config) -> Results {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(Buffer {
                rows: VecDeque::new(),
                head: 0,
                capacity: config.capacity,
                dropped: 0,
            }),
            wake: Notify::new(),
            closed: AtomicBool::new(false),
        });
        tokio::spawn(drain(pg, shared.clone(), config));
        Results {
            shared,
            batch: config.batch,
        }
    }

    /// When the buffer is full the oldest rows go, counted: probing and detection never wait on
    /// storage, and a dropped row is a gap in history, never a pass.
    pub fn push(&self, judged: Vec<Judged>) {
        let mut buffer = self.shared.lock();
        buffer.rows.extend(judged.iter().map(Row::of));
        let over = buffer.rows.len().saturating_sub(buffer.capacity);
        buffer.rows.drain(..over);
        buffer.head += over as u64;
        buffer.dropped += over as u64;
        let due = buffer.rows.len() >= self.batch;
        drop(buffer);
        if due {
            self.shared.wake.notify_one();
        }
    }

    /// Rows lost so far, to overflow or to an insert Postgres refused for good.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.lock().dropped
    }
}

impl Drop for Results {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Buffer> {
        self.buffer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Up to `max` rows from the front, as columns, and the sequence number just past them.
    fn front(&self, max: usize) -> Option<(u64, Columns)> {
        let buffer = self.lock();
        if buffer.rows.is_empty() {
            return None;
        }
        let rows: Vec<&Row> = buffer.rows.iter().take(max).collect();
        Some((buffer.head + rows.len() as u64, Columns::of(&rows)))
    }

    /// Forgets the rows before `end`, whether they landed or are given up on.
    fn settle(&self, end: u64, lost: bool) {
        let mut buffer = self.lock();
        let gone = usize::try_from(end.saturating_sub(buffer.head))
            .unwrap_or(usize::MAX)
            .min(buffer.rows.len());
        buffer.rows.drain(..gone);
        buffer.head += gone as u64;
        if lost {
            buffer.dropped += gone as u64;
        }
    }
}

async fn drain(pg: Pg, shared: Arc<Shared>, config: Config) {
    let mut backoff = BACKOFF_MIN;
    loop {
        tokio::select! {
            () = shared.wake.notified() => {}
            () = sleep(config.every) => {}
        }
        while let Some((end, columns)) = shared.front(config.batch) {
            match pg.tx(async |tx| insert(tx, &columns).await).await {
                Ok(()) => {
                    shared.settle(end, false);
                    backoff = BACKOFF_MIN;
                }
                Err(PgError::Fatal(reason)) => {
                    tracing::error!(%reason, "uptime_result batch refused; its rows are dropped");
                    shared.settle(end, true);
                }
                Err(error @ (PgError::Transient(_) | PgError::SchemaNotReady { .. })) => {
                    if shared.closed() {
                        return;
                    }
                    tracing::warn!(%error, "uptime_result flush will retry");
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(config.backoff_max);
                }
            }
        }
        if shared.closed() {
            return;
        }
    }
}

async fn insert(tx: &Transaction<'_>, c: &Columns) -> Result<(), PgError> {
    let params: [(&(dyn ToSql + Sync), Type); 19] = [
        (&c.check_id, Type::UUID_ARRAY),
        (&c.organization_id, Type::UUID_ARRAY),
        (&c.region, Type::TEXT_ARRAY),
        (&c.scheduled_at, Type::TIMESTAMPTZ_ARRAY),
        (&c.kind, Type::TEXT_ARRAY),
        (&c.start_lag_ms, Type::INT4_ARRAY),
        (&c.verdict, Type::TEXT_ARRAY),
        (&c.status, Type::INT2_ARRAY),
        (&c.failed_phase, Type::TEXT_ARRAY),
        (&c.error, Type::TEXT_ARRAY),
        (&c.detail, Type::TEXT_ARRAY),
        (&c.dns_ms, Type::INT4_ARRAY),
        (&c.connect_ms, Type::INT4_ARRAY),
        (&c.tls_ms, Type::INT4_ARRAY),
        (&c.ttfb_ms, Type::INT4_ARRAY),
        (&c.total_ms, Type::INT4_ARRAY),
        (&c.remote_ip, Type::INET_ARRAY),
        (&c.cert_not_after, Type::TIMESTAMPTZ_ARRAY),
        (&c.agent, Type::TEXT_ARRAY),
    ];
    tx.query_typed(INSERT, &params).await?;
    Ok(())
}

/// One `uptime_result` row, converted once at push time.
#[derive(Clone, Debug)]
struct Row {
    check_id: Uuid,
    organization_id: Uuid,
    region: String,
    scheduled_at: SystemTime,
    kind: &'static str,
    start_lag_ms: i32,
    verdict: &'static str,
    /// 0 when no HTTP response arrived.
    status: i16,
    failed_phase: &'static str,
    error: &'static str,
    detail: String,
    timings: [i32; 5],
    remote_ip: Option<IpAddr>,
    cert_not_after: Option<SystemTime>,
    agent: String,
}

impl Row {
    fn of(judged: &Judged) -> Row {
        let o = &judged.observation;
        let (status, failed_phase, error, detail) = match &o.outcome {
            Outcome::Http { status } => (*status, "none", "", ""),
            Outcome::Unreachable {
                phase,
                error,
                detail,
            } => (0, phase.as_str(), http_error_str(*error), detail.as_str()),
            Outcome::NotProbed(NotProbed::PrivateAddress) => (0, "none", "private_address", ""),
            Outcome::NotProbed(NotProbed::Saturated) => (0, "none", "saturated", ""),
        };
        let lag = o.started_at.0.saturating_sub(o.key.scheduled_at.0);
        let ms = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let t = o.timings;
        Row {
            check_id: o.key.check.0,
            organization_id: judged.org.0,
            region: o.key.region.as_str().to_owned(),
            scheduled_at: o.key.scheduled_at.to_system_time(),
            kind: o.key.kind.as_str(),
            start_lag_ms: i32::try_from(lag).unwrap_or(if lag < 0 { i32::MIN } else { i32::MAX }),
            verdict: judged.verdict.as_str(),
            status: i16::try_from(status).unwrap_or(i16::MAX),
            failed_phase,
            error,
            // Postgres text can't hold NUL, and one row must not sink its batch.
            detail: detail.replace('\0', ""),
            timings: [
                ms(t.dns_ms),
                ms(t.connect_ms),
                ms(t.tls_ms),
                ms(t.ttfb_ms),
                ms(t.total_ms),
            ],
            remote_ip: o.remote_ip,
            cert_not_after: o
                .cert_not_after
                .map(|secs| crate::domain::UnixMs(secs.saturating_mul(1000)).to_system_time()),
            agent: o.agent.0.to_string(),
        }
    }
}

#[derive(Default)]
struct Columns {
    check_id: Vec<Uuid>,
    organization_id: Vec<Uuid>,
    region: Vec<String>,
    scheduled_at: Vec<SystemTime>,
    kind: Vec<&'static str>,
    start_lag_ms: Vec<i32>,
    verdict: Vec<&'static str>,
    status: Vec<i16>,
    failed_phase: Vec<&'static str>,
    error: Vec<&'static str>,
    detail: Vec<String>,
    dns_ms: Vec<i32>,
    connect_ms: Vec<i32>,
    tls_ms: Vec<i32>,
    ttfb_ms: Vec<i32>,
    total_ms: Vec<i32>,
    remote_ip: Vec<Option<IpAddr>>,
    cert_not_after: Vec<Option<SystemTime>>,
    agent: Vec<String>,
}

impl Columns {
    fn of(rows: &[&Row]) -> Columns {
        let mut c = Columns::default();
        for row in rows {
            let [dns, connect, tls, ttfb, total] = row.timings;
            c.check_id.push(row.check_id);
            c.organization_id.push(row.organization_id);
            c.region.push(row.region.clone());
            c.scheduled_at.push(row.scheduled_at);
            c.kind.push(row.kind);
            c.start_lag_ms.push(row.start_lag_ms);
            c.verdict.push(row.verdict);
            c.status.push(row.status);
            c.failed_phase.push(row.failed_phase);
            c.error.push(row.error);
            c.detail.push(row.detail.clone());
            c.dns_ms.push(dns);
            c.connect_ms.push(connect);
            c.tls_ms.push(tls);
            c.ttfb_ms.push(ttfb);
            c.total_ms.push(total);
            c.remote_ip.push(row.remote_ip);
            c.cert_not_after.push(row.cert_not_after);
            c.agent.push(row.agent.clone());
        }
        c
    }
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
