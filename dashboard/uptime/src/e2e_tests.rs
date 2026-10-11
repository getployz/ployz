//! The whole checker against a real Postgres in `all` mode: a loopback target flips between 200
//! and 500 and the incident and alert rows follow, including across a config edit.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::agent::probe::{ProbeConfig, Prober};
use crate::agent::run::AgentRun;
use crate::config::{ControlConfig, Secret};
use crate::control::engine::Tempo;
use crate::control::plane::ControlPlane;
use crate::control::test_pg;
use crate::domain::{AgentName, Region};
use crate::run::{self, Shutdown};

const STEP: Duration = Duration::from_secs(30);

/// Answers every request with the current status and nothing else, `hold` after reading it.
async fn responder(status: Arc<AtomicU16>, hold: Duration, requests: Arc<AtomicU16>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let status = status.clone();
            let requests = requests.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                    match stream.read(&mut byte).await {
                        Ok(1) => head.push(byte[0]),
                        Ok(_) | Err(_) => return,
                    }
                }
                requests.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(hold).await;
                let code = status.load(Ordering::SeqCst);
                let reply =
                    format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                drop(stream.write_all(reply.as_bytes()).await);
                drop(stream.shutdown().await);
            });
        }
    });
    port
}

async fn until(what: &str, mut check: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < STEP {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what} did not happen within {STEP:?}");
}

async fn count(admin: &Client, sql: &str, id: Uuid) -> i64 {
    admin.query_one(sql, &[&id]).await.unwrap().get(0)
}

const INCIDENTS: &str = "SELECT count(*) FROM uptime_incident WHERE check_id = $1";
const OPEN_INCIDENTS: &str =
    "SELECT count(*) FROM uptime_incident WHERE check_id = $1 AND closed_at IS NULL";
const RESULTS: &str = "SELECT count(*) FROM uptime_result WHERE check_id = $1";
const ALERTS_OF_KIND: &str = "SELECT count(*) FROM uptime_alert a JOIN uptime_incident i \
     ON i.id = a.incident_id WHERE i.check_id = $1 AND a.kind = $2";

async fn alerts(admin: &Client, check: Uuid, kind: &str) -> i64 {
    admin
        .query_one(ALERTS_OF_KIND, &[&check, &kind])
        .await
        .unwrap()
        .get(0)
}

async fn closed_kinds(admin: &Client, check: Uuid) -> Vec<String> {
    admin
        .query(
            "SELECT closed_by->>'kind' FROM uptime_incident WHERE check_id = $1 \
             AND closed_at IS NOT NULL ORDER BY opened_at",
            &[&check],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<_, Option<String>>(0).unwrap_or_default())
        .collect()
}

/// The dashboard's edit statement, so the engine sees the same revision bump and close.
const EDIT: &str = "WITH edited AS (UPDATE uptime_check SET url = url, interval_seconds = 30, \
     timeout_ms = 1000, enabled = true, revision = revision + 1, updated_at = now() \
     WHERE id = $1 AND organization_id = $2 RETURNING id, enabled), \
     closed AS (UPDATE uptime_incident i SET closed_at = now(), closed_by = \
     jsonb_build_object('kind', CASE WHEN e.enabled THEN 'check_changed' \
     ELSE 'check_disabled' END) FROM edited e WHERE i.check_id = e.id AND i.closed_at IS NULL \
     RETURNING i.id) SELECT * FROM edited";

#[tokio::test(flavor = "multi_thread")]
async fn an_outage_opens_one_incident_recovers_and_survives_a_config_edit() {
    let (pg, admin) = test_pg::postgres().await;
    let org = test_pg::org(&admin).await;
    let check = test_pg::check(&admin, org, "http://127.0.0.1/").await;
    admin
        .execute(
            "UPDATE uptime_check SET interval_seconds = 30, timeout_ms = 1000 WHERE id = $1",
            &[&check],
        )
        .await
        .unwrap();
    let status = Arc::new(AtomicU16::new(200));
    let port = responder(status.clone(), Duration::ZERO, Arc::default()).await;

    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let (trigger, shutdown) = Shutdown::new();
    let (plane_stop, plane_shutdown) = Shutdown::new();
    let region = Region::parse("local").unwrap();
    let control_url = pg
        .role(
            &admin,
            "uptime_control",
            include_str!("../control-role.sql"),
        )
        .await;
    let config = ControlConfig {
        database_url: Secret::new(&control_url),
        regions: vec![region.clone()],
        agent_token: None,
        listen: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    };
    let plane = ControlPlane::start_with(
        config,
        crypto.clone(),
        plane_shutdown,
        Tempo { divisor: 30 },
    )
    .await
    .unwrap();
    let mut prober =
        Prober::new(ProbeConfig::default(), crypto, AgentName("local".into())).unwrap();
    prober.loopback_ok = true;
    prober.loopback_port = Some(port);
    let agent_run = AgentRun {
        max_inflight: 64,
        flush_every: Duration::from_millis(100),
        flush_at: 50,
        max_unacked: 1_000,
    };
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let all = tokio::spawn(run::all(
        plane,
        plane_stop,
        Arc::new(prober),
        agent_run,
        listen,
        shutdown,
    ));

    until("routine results landing", async || {
        count(&admin, RESULTS, check).await >= 2
    })
    .await;
    assert_eq!(count(&admin, INCIDENTS, check).await, 0);

    status.store(500, Ordering::SeqCst);
    until("an incident opening", async || {
        count(&admin, OPEN_INCIDENTS, check).await == 1
    })
    .await;
    assert_eq!(alerts(&admin, check, "opened").await, 1);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        count(&admin, INCIDENTS, check).await,
        1,
        "a second incident opened for one outage"
    );
    assert_eq!(
        alerts(&admin, check, "opened").await,
        1,
        "alerted twice for one outage"
    );

    status.store(200, Ordering::SeqCst);
    until("the incident closing", async || {
        count(&admin, OPEN_INCIDENTS, check).await == 0
    })
    .await;
    assert_eq!(
        closed_kinds(&admin, check).await,
        vec!["observed".to_owned()]
    );
    assert_eq!(alerts(&admin, check, "resolved").await, 1);

    status.store(500, Ordering::SeqCst);
    until("a second incident opening", async || {
        count(&admin, OPEN_INCIDENTS, check).await == 1
    })
    .await;
    assert_eq!(count(&admin, INCIDENTS, check).await, 2);
    let second: Uuid = admin
        .query_one(
            "SELECT id FROM uptime_incident WHERE check_id = $1 AND closed_at IS NULL",
            &[&check],
        )
        .await
        .unwrap()
        .get(0);
    admin.execute(EDIT, &[&check, &org]).await.unwrap();
    until("a third incident at the new revision", async || {
        admin
            .query_one(
                "SELECT count(*) FROM uptime_incident WHERE check_id = $1 AND check_revision = 2 \
                 AND closed_at IS NULL",
                &[&check],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
            == 1
    })
    .await;
    assert_eq!(
        closed_kinds(&admin, check).await,
        vec!["observed".to_owned(), "check_changed".to_owned()]
    );
    assert_eq!(
        alerts(&admin, check, "resolved").await,
        1,
        "the edit's close was alerted"
    );
    let second_alerts: i64 = admin
        .query_one(
            "SELECT count(*) FROM uptime_alert WHERE incident_id = $1",
            &[&second],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(second_alerts, 1);
    assert_eq!(count(&admin, INCIDENTS, check).await, 3);

    trigger.fire();
    let outcome = tokio::time::timeout(Duration::from_secs(35), all).await;
    outcome
        .expect("all mode did not stop within 35 s")
        .unwrap()
        .unwrap();
    drop(pg);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_probe_still_in_flight_at_shutdown_is_written_before_all_mode_exits() {
    let (pg, admin) = test_pg::postgres().await;
    let org = test_pg::org(&admin).await;
    let check = test_pg::check(&admin, org, "http://127.0.0.1/").await;
    admin
        .execute(
            "UPDATE uptime_check SET interval_seconds = 30, timeout_ms = 15000 WHERE id = $1",
            &[&check],
        )
        .await
        .unwrap();
    let requests = Arc::new(AtomicU16::new(0));
    let hold = Duration::from_secs(8);
    let port = responder(Arc::new(AtomicU16::new(200)), hold, requests.clone()).await;

    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let (trigger, shutdown) = Shutdown::new();
    let (plane_stop, plane_shutdown) = Shutdown::new();
    let region = Region::parse("local").unwrap();
    let control_url = pg
        .role(
            &admin,
            "uptime_control",
            include_str!("../control-role.sql"),
        )
        .await;
    let config = ControlConfig {
        database_url: Secret::new(&control_url),
        regions: vec![region.clone()],
        agent_token: None,
        listen: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
    };
    let plane = ControlPlane::start_with(
        config,
        crypto.clone(),
        plane_shutdown,
        Tempo { divisor: 30 },
    )
    .await
    .unwrap();
    let mut prober =
        Prober::new(ProbeConfig::default(), crypto, AgentName("local".into())).unwrap();
    prober.loopback_ok = true;
    prober.loopback_port = Some(port);
    let agent_run = AgentRun {
        max_inflight: 64,
        flush_every: Duration::from_millis(100),
        flush_at: 50,
        max_unacked: 1_000,
    };
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let all = tokio::spawn(run::all(
        plane,
        plane_stop,
        Arc::new(prober),
        agent_run,
        listen,
        shutdown,
    ));

    until("a probe reaching the target", async || {
        requests.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert_eq!(count(&admin, RESULTS, check).await, 0);
    trigger.fire();
    tokio::time::timeout(Duration::from_secs(60), all)
        .await
        .expect("all mode did not stop within 60 s")
        .unwrap()
        .unwrap();
    assert!(
        count(&admin, RESULTS, check).await >= 1,
        "the in-flight probe's result was lost at shutdown"
    );
    drop(pg);
}
