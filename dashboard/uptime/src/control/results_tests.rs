use std::time::Duration;

use tokio::time::sleep;
use tokio_postgres::Client;

use super::*;
use crate::control::test_pg::{Postgres, check, connect, cut, org, postgres, restore};
use crate::domain::{
    AgentName, CheckId, Detail, HttpCheckError, Observation, OrgId, Phase, ProbeKey, ProbeKind,
    Region, Timings, UnixMs, Verdict,
};

const BASE: i64 = 1_791_700_000_000;

fn fast(capacity: usize) -> Config {
    Config {
        capacity,
        batch: 64,
        every: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
    }
}

/// The `n`th minute's routine probe of `check`, passing.
fn judged(org: Uuid, check: Uuid, n: i64) -> Judged {
    Judged {
        org: OrgId(org),
        verdict: Verdict::Pass,
        observation: Observation {
            key: ProbeKey {
                check: CheckId(check),
                region: Region::parse("eu-west").unwrap(),
                scheduled_at: UnixMs(BASE + n * 60_000),
                kind: ProbeKind::Routine,
            },
            started_at: UnixMs(BASE + n * 60_000 + 7),
            outcome: Outcome::Http { status: 200 },
            timings: Timings::default(),
            remote_ip: None,
            cert_not_after: None,
            agent: AgentName("agent-1".into()),
        },
    }
}

fn many(org: Uuid, check: Uuid, minutes: std::ops::Range<i64>) -> Vec<Judged> {
    minutes.map(|n| judged(org, check, n)).collect()
}

/// The minutes stored for `check`, in order.
async fn minutes(admin: &Client, check: Uuid) -> Vec<i64> {
    admin
        .query(
            "SELECT ((extract(epoch FROM scheduled_at) * 1000)::bigint - $2) / 60000 \
             FROM uptime_result WHERE check_id = $1 ORDER BY scheduled_at",
            &[&check, &BASE],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// Waits until `check` holds `want` rows. Fails after about ten seconds.
async fn landed(admin: &Client, check: Uuid, want: usize) -> Vec<i64> {
    for _ in 0..200 {
        let got = minutes(admin, check).await;
        if got.len() >= want {
            return got;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "only {} of {want} rows landed",
        minutes(admin, check).await.len()
    );
}

/// The sink's own role, so an outage can be held open. FOR KEY SHARE needs UPDATE on the check.
async fn sink(pg: &Postgres, admin: &Client) -> Pg {
    let url = pg
        .role(
            admin,
            "sink",
            "GRANT SELECT, INSERT ON uptime_result TO sink; \
             GRANT SELECT, UPDATE ON uptime_check TO sink",
        )
        .await;
    connect(&url).await
}

/// Catches a column bound out of order or mistyped, a spelling the table's CHECKs refuse, a
/// lost lag, timing, address or certificate, and a NUL in an error message sinking the batch.
#[tokio::test]
async fn every_outcome_lands_with_every_column() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let id = check(&admin, org, "https://shop.example.com/").await;
    let results = Results::spawn(sink(&pg, &admin).await, fast(1000));

    let mut http = judged(org, id, 0);
    http.observation.key.kind = ProbeKind::Confirm;
    http.observation.timings = Timings {
        dns_ms: 1,
        connect_ms: 2,
        tls_ms: 3,
        ttfb_ms: 4,
        total_ms: 5,
    };
    http.observation.remote_ip = Some("2001:db8::1".parse().unwrap());
    http.observation.cert_not_after = Some(1_800_000_000);
    let mut refused = judged(org, id, 1);
    refused.verdict = Verdict::Fail;
    refused.observation.key.region = Region::parse("us-east").unwrap();
    refused.observation.key.kind = ProbeKind::Recheck;
    refused.observation.outcome = Outcome::Unreachable {
        phase: Phase::Connect,
        error: HttpCheckError::ConnectionRefused,
        detail: Detail::new("refused\0 by peer"),
    };
    refused.observation.remote_ip = Some("203.0.113.7".parse().unwrap());
    let mut saturated = judged(org, id, 2);
    saturated.verdict = Verdict::Inconclusive;
    saturated.observation.outcome = Outcome::NotProbed(NotProbed::Saturated);
    results.push(vec![http, refused, saturated]);
    landed(&admin, id, 3).await;

    let rows: Vec<String> = admin
        .query(
            "SELECT concat_ws('|', organization_id, region, kind, start_lag_ms, verdict, status, \
               failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, \
               coalesce(host(remote_ip), '-'), \
               coalesce(extract(epoch FROM cert_not_after)::bigint::text, '-'), agent) \
             FROM uptime_result WHERE check_id = $1 ORDER BY scheduled_at",
            &[&id],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    let org = org.to_string();
    let want = [
        [
            &*org, "eu-west", "confirm", "7", "pass", "200", "none", "", "",
        ],
        [
            &org,
            "us-east",
            "recheck",
            "7",
            "fail",
            "0",
            "connect",
            "connection_refused",
            "refused by peer",
        ],
        [
            &org,
            "eu-west",
            "routine",
            "7",
            "inconclusive",
            "0",
            "none",
            "saturated",
            "",
        ],
    ];
    let rest = [
        "1|2|3|4|5|2001:db8::1|1800000000|agent-1",
        "0|0|0|0|0|203.0.113.7|-|agent-1",
        "0|0|0|0|0|-|-|agent-1",
    ];
    let want: Vec<String> = want
        .iter()
        .zip(rest)
        .map(|(head, rest)| format!("{}|{rest}", head.join("|")))
        .collect();
    assert_eq!(rows, want);
    assert_eq!(results.dropped(), 0);
}

/// Catches an insert that fails on, or duplicates, a row already stored: a flush whose commit
/// reply was lost resends its batch, and that batch must not be counted lost.
#[tokio::test]
async fn a_replayed_batch_lands_once_beside_new_rows() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let id = check(&admin, org, "https://shop.example.com/").await;
    let results = Results::spawn(sink(&pg, &admin).await, fast(1000));
    results.push(many(org, id, 0..10));
    landed(&admin, id, 10).await;
    results.push(many(org, id, 5..15));
    let got = landed(&admin, id, 15).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(minutes(&admin, id).await, got);
    assert_eq!(got, (0..15).collect::<Vec<_>>());
    assert_eq!(results.dropped(), 0);
}

/// Catches one deleted check's late result failing the foreign key and taking the live checks'
/// rows in the same statement down with it.
#[tokio::test]
async fn a_deleted_checks_rows_do_not_sink_their_batch() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let live = check(&admin, org, "https://a.example.com/").await;
    let gone = check(&admin, org, "https://b.example.com/").await;
    admin
        .execute("DELETE FROM uptime_check WHERE id = $1", &[&gone])
        .await
        .unwrap();
    let results = Results::spawn(sink(&pg, &admin).await, fast(1000));
    let mut batch = many(org, live, 0..3);
    batch.push(judged(org, gone, 1));
    batch.extend(many(org, live, 3..6));
    results.push(batch);
    assert_eq!(landed(&admin, live, 6).await, (0..6).collect::<Vec<_>>());
    assert!(minutes(&admin, gone).await.is_empty());
    assert_eq!(results.dropped(), 0);
}

/// Catches a full buffer that blocks, drops the newest rows instead of the oldest, loses count of
/// what it dropped, or confirms rows evicted while their batch was in flight as stored.
#[tokio::test]
async fn a_full_buffer_drops_the_oldest_and_counts_them() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let id = check(&admin, org, "https://shop.example.com/").await;
    let results = Results::spawn(sink(&pg, &admin).await, fast(100));
    cut(&admin, "sink").await;
    for start in (0..150).step_by(10) {
        results.push(many(org, id, start..start + 10));
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(results.dropped(), 50);
    restore(&admin, "sink").await;
    assert_eq!(landed(&admin, id, 100).await, (50..150).collect::<Vec<_>>());
    sleep(Duration::from_millis(300)).await;
    assert_eq!(minutes(&admin, id).await.len(), 100);
    assert_eq!(results.dropped(), 50);
}

/// Catches a flush that forgets its batch when Postgres is unreachable: below the bound, an
/// outage only delays rows.
#[tokio::test]
async fn an_outage_below_the_bound_loses_nothing() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let id = check(&admin, org, "https://shop.example.com/").await;
    let results = Results::spawn(sink(&pg, &admin).await, fast(1000));
    cut(&admin, "sink").await;
    for start in (0..600).step_by(100) {
        results.push(many(org, id, start..start + 100));
        sleep(Duration::from_millis(100)).await;
    }
    restore(&admin, "sink").await;
    assert_eq!(landed(&admin, id, 600).await, (0..600).collect::<Vec<_>>());
    assert_eq!(results.dropped(), 0);
}

/// Catches a refused batch that is retried forever (wedging every row behind it) or dropped
/// without being counted. Also shows the sink needs UPDATE on `uptime_check` for FOR KEY SHARE.
#[tokio::test]
async fn a_refused_batch_is_dropped_and_counted() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let id = check(&admin, org, "https://shop.example.com/").await;
    let url = pg
        .role(
            &admin,
            "sink",
            "GRANT SELECT, INSERT ON uptime_result TO sink; GRANT SELECT ON uptime_check TO sink",
        )
        .await;
    let results = Results::spawn(connect(&url).await, fast(1000));
    results.push(many(org, id, 0..30));
    for _ in 0..200 {
        if results.dropped() == 30 {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(results.dropped(), 30);
    assert!(minutes(&admin, id).await.is_empty());
}
