use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_postgres::Client;
use uuid::Uuid;

use super::*;
use crate::control::test_pg::{LostCommit, Postgres, check, connect, org, postgres};
use crate::domain::{Expect, HttpCheckError, Interval, Phase, ProbeKind, Region, Target, Verdict};

struct Fixture {
    pg: Postgres,
    admin: Client,
    check: Check,
}

async fn fixture() -> Fixture {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let url = "https://shop.example.com/";
    let id = check(&admin, org, url).await;
    let check = Check {
        id: CheckId(id),
        org: OrgId(org),
        revision: Revision(1),
        name: "Check".into(),
        url: url.into(),
        target: Arc::new(Target::parse(url).unwrap()),
        interval: Interval::parse(60).unwrap(),
        timeout: Duration::from_secs(10),
        expect: Expect::Below400,
    };
    Fixture { pg, admin, check }
}

fn writer(pg: Pg) -> (IncidentWriter, mpsc::Receiver<Written>) {
    let (tx, rx) = mpsc::channel(16);
    (IncidentWriter::spawn(pg, IncidentWriter::QUEUE, tx), rx)
}

async fn written(rx: &mut mpsc::Receiver<Written>) -> Written {
    timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("the writer never reported")
        .unwrap()
}

fn now_minus(secs: u64) -> UnixMs {
    let since = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    UnixMs(i64::try_from((since - Duration::from_secs(secs)).as_millis()).unwrap())
}

fn failing() -> Evidence {
    Evidence {
        quorum: Quorum::DistinctRegions(2),
        observations: vec![
            EvidenceItem {
                region: Region::parse("eu-west").unwrap(),
                kind: ProbeKind::Confirm,
                verdict: Verdict::Fail,
                status: None,
                failed_phase: Some(Phase::Connect),
                error: Some(HttpCheckError::ConnectionRefused),
                remote_ip: Some("203.0.113.7".parse().unwrap()),
                at: UnixMs(1_709_210_096_789),
            },
            EvidenceItem {
                region: Region::parse("us-east").unwrap(),
                kind: ProbeKind::Routine,
                verdict: Verdict::Fail,
                status: Some(503),
                failed_phase: None,
                error: None,
                remote_ip: None,
                at: UnixMs(951_782_400_123),
            },
        ],
    }
}

/// The JSON the dashboard parses, shared with uptime-status.postgres.test.ts. Tests here prove the statements write it.
fn contract(key: &str) -> Value {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../src/modules/uptime/incident-evidence.fixture.json"),
    )
    .unwrap();
    serde_json::from_str::<Value>(&text)
        .unwrap()
        .get(key)
        .cloned()
        .expect("the fixture has every evidence key")
}

fn failing_json() -> Value {
    contract("openedBy")
}

fn passing() -> Evidence {
    Evidence {
        quorum: Quorum::OneRegion {
            probes: 2,
            spacing: Duration::from_secs(30),
        },
        observations: vec![
            EvidenceItem {
                region: Region::parse("eu-west").unwrap(),
                kind: ProbeKind::Recheck,
                verdict: Verdict::Pass,
                status: Some(200),
                failed_phase: None,
                error: None,
                remote_ip: Some("2001:db8::1".parse().unwrap()),
                at: UnixMs(1_709_210_096_789),
            },
            EvidenceItem {
                region: Region::parse("us-east").unwrap(),
                kind: ProbeKind::Triggered,
                verdict: Verdict::Pass,
                status: Some(204),
                failed_phase: None,
                error: None,
                remote_ip: None,
                at: UnixMs(1_709_210_100_000),
            },
        ],
    }
}

fn open(check: &Check, revision: i32, at: UnixMs) -> IncidentOp {
    let step = Step::Open {
        incident: IncidentId(Uuid::now_v7()),
        revision: Revision(revision),
        evidence: failing(),
    };
    IncidentOp::of(step, check, at).unwrap()
}

fn close(check: &Check, opened: &IncidentOp, at: UnixMs) -> IncidentOp {
    let step = Step::Close {
        incident: opened.incident,
        revision: opened.revision,
        evidence: passing(),
    };
    IncidentOp::of(step, check, at).unwrap()
}

async fn alerts(admin: &Client, incident: IncidentId) -> Vec<(String, Value)> {
    admin
        .query(
            "SELECT kind, payload FROM uptime_alert WHERE incident_id = $1 ORDER BY kind",
            &[&incident.0],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

async fn incidents(admin: &Client, check: CheckId) -> i64 {
    admin
        .query_one(
            "SELECT count(*) FROM uptime_incident WHERE check_id = $1",
            &[&check.0],
        )
        .await
        .unwrap()
        .get(0)
}

/// The `CHECK_CHANGED` close `editUptimeCheckStatement` runs when a user edits the URL.
async fn edit_url(admin: &Client, check: CheckId) {
    admin
        .execute(
            "WITH edited AS (UPDATE uptime_check SET url = 'https://edited.example.com/', \
               revision = revision + 1, updated_at = now() WHERE id = $1 RETURNING id, enabled), \
             closed AS (UPDATE uptime_incident i SET closed_at = now(), \
               closed_by = jsonb_build_object('kind', \
                 CASE WHEN e.enabled THEN 'check_changed' ELSE 'check_disabled' END) \
               FROM edited e WHERE i.check_id = e.id AND i.closed_at IS NULL RETURNING i.id) \
             SELECT * FROM edited",
            &[&check.0],
        )
        .await
        .unwrap();
}

/// Catches an open that doesn't land, lands without its alert, writes evidence or a payload the
/// dashboard can't read (keys, spellings, timestamps), or alerts twice when replayed.
#[tokio::test]
async fn an_open_applies_once_with_one_alert_and_the_contract_json() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let at = now_minus(60);
    let op = open(&f.check, 1, at);
    assert_eq!(writer.submit(op.clone()), Submit::Queued);
    let report = written(&mut reports).await;
    assert_eq!(
        report,
        Written {
            check: f.check.id,
            incident: op.incident,
            transition: Transition::Opened,
            applied: true,
        }
    );

    let row = f
        .admin
        .query_one(
            "SELECT check_revision, (extract(epoch FROM opened_at) * 1000)::bigint, opened_by, \
             closed_at IS NULL FROM uptime_incident WHERE id = $1",
            &[&op.incident.0],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i32>(0), 1);
    assert_eq!(row.get::<_, i64>(1), at.0);
    assert_eq!(row.get::<_, Value>(2), failing_json());
    assert!(row.get::<_, bool>(3));
    let expected = json!({
        "organizationId": f.check.org.0.to_string(),
        "checkId": f.check.id.0.to_string(),
        "checkName": "Check",
        "url": "https://shop.example.com/",
        "transition": "opened",
        "evidence": failing_json(),
        "at": iso(at),
    });
    assert_eq!(
        alerts(&f.admin, op.incident).await,
        vec![("opened".to_owned(), expected)]
    );

    assert_eq!(writer.submit(op.clone()), Submit::Queued);
    assert!(
        written(&mut reports).await.applied,
        "a replay of a recorded open"
    );
    assert_eq!(incidents(&f.admin, f.check.id).await, 1);
    assert_eq!(alerts(&f.admin, op.incident).await.len(), 1);
}

/// Catches a writer that gives up on a dropped connection, and one that trusts the retried
/// statement's zero: the first attempt committed, so the honest answer is "applied", and the
/// engine must not re-derive and open again.
#[tokio::test]
async fn an_open_whose_commit_reply_was_lost_reports_applied_and_alerts_once() {
    let f = fixture().await;
    let proxy = LostCommit::start(&f.pg).await;
    let (writer, mut reports) = writer(connect(&proxy.url).await);
    let op = open(&f.check, 1, now_minus(60));
    proxy.arm();
    assert_eq!(writer.submit(op.clone()), Submit::Queued);
    let report = written(&mut reports).await;
    assert!(proxy.fired(), "the commit reply was dropped");
    assert!(report.applied, "{report:?}");
    assert_eq!(incidents(&f.admin, f.check.id).await, 1);
    let kinds: Vec<String> = alerts(&f.admin, op.incident)
        .await
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(kinds, ["opened"]);
}

/// Catches the revision fence not holding (an open judged before an edit lands after it), and
/// an "already recorded" check keyed on the check instead of the incident, which would call a
/// second episode's refused open applied.
#[tokio::test]
async fn a_stale_or_second_open_reports_not_applied() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let stale = open(&f.check, 1, now_minus(60));
    edit_url(&f.admin, f.check.id).await;
    assert_eq!(writer.submit(stale.clone()), Submit::Queued);
    assert!(!written(&mut reports).await.applied);
    assert_eq!(incidents(&f.admin, f.check.id).await, 0);
    assert!(alerts(&f.admin, stale.incident).await.is_empty());

    let first = open(&f.check, 2, now_minus(60));
    let second = open(&f.check, 2, now_minus(50));
    assert_eq!(writer.submit(first.clone()), Submit::Queued);
    assert_eq!(writer.submit(second.clone()), Submit::Queued);
    assert!(written(&mut reports).await.applied);
    let refused = written(&mut reports).await;
    assert_eq!(refused.incident, second.incident);
    assert!(!refused.applied, "one open incident per check");
    assert_eq!(incidents(&f.admin, f.check.id).await, 1);
    assert!(alerts(&f.admin, second.incident).await.is_empty());
}

/// Catches a close that doesn't land, lands without its resolved alert, writes a closed_by the
/// dashboard can't read as an observed recovery, or alerts twice when replayed.
#[tokio::test]
async fn a_close_applies_once_with_one_resolved_alert() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let opened = open(&f.check, 1, now_minus(60));
    let closing = close(&f.check, &opened, now_minus(30));
    assert_eq!(writer.submit(opened.clone()), Submit::Queued);
    assert_eq!(writer.submit(closing.clone()), Submit::Queued);
    assert!(written(&mut reports).await.applied);
    let report = written(&mut reports).await;
    assert_eq!(report.transition, Transition::Resolved);
    assert!(report.applied);

    let row = f
        .admin
        .query_one(
            "SELECT (extract(epoch FROM closed_at) * 1000)::bigint, closed_by \
             FROM uptime_incident WHERE id = $1",
            &[&opened.incident.0],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), closing.at.0);
    let observed = contract("observedClosedBy");
    assert_eq!(row.get::<_, Value>(1), observed);
    let resolved = alerts(&f.admin, opened.incident).await;
    let kinds: Vec<&str> = resolved.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, ["opened", "resolved"]);
    let payload = &resolved.get(1).unwrap().1;
    assert_eq!(payload.get("transition"), Some(&json!("resolved")));
    assert_eq!(payload.get("at"), Some(&json!(iso(closing.at))));

    assert_eq!(writer.submit(closing.clone()), Submit::Queued);
    assert!(
        written(&mut reports).await.applied,
        "a replay of a recorded close"
    );
    assert_eq!(alerts(&f.admin, opened.incident).await.len(), 2);
}

/// Catches the same lost-reply defects as the open, on the close path.
#[tokio::test]
async fn a_close_whose_commit_reply_was_lost_reports_applied_and_resolves_once() {
    let f = fixture().await;
    let proxy = LostCommit::start(&f.pg).await;
    let (writer, mut reports) = writer(connect(&proxy.url).await);
    let opened = open(&f.check, 1, now_minus(60));
    assert_eq!(writer.submit(opened.clone()), Submit::Queued);
    assert!(written(&mut reports).await.applied);
    proxy.arm();
    assert_eq!(
        writer.submit(close(&f.check, &opened, now_minus(30))),
        Submit::Queued
    );
    let report = written(&mut reports).await;
    assert!(proxy.fired(), "the commit reply was dropped");
    assert!(report.applied, "{report:?}");
    let kinds: Vec<String> = alerts(&f.admin, opened.incident)
        .await
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(kinds, ["opened", "resolved"]);
}

/// Catches a close that overrides a config-edit close (telling users "resolved" for something
/// never observed recovering), and an "already recorded" check that counts the opened alert.
#[tokio::test]
async fn a_close_after_a_config_edit_reports_not_applied() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let opened = open(&f.check, 1, now_minus(60));
    assert_eq!(writer.submit(opened.clone()), Submit::Queued);
    assert!(written(&mut reports).await.applied);
    edit_url(&f.admin, f.check.id).await;

    assert_eq!(
        writer.submit(close(&f.check, &opened, now_minus(1))),
        Submit::Queued
    );
    assert!(!written(&mut reports).await.applied);
    let closed_by: Value = f
        .admin
        .query_one(
            "SELECT closed_by FROM uptime_incident WHERE id = $1",
            &[&opened.incident.0],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(closed_by, json!({ "kind": "check_changed" }));
    let kinds: Vec<String> = alerts(&f.admin, opened.incident)
        .await
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(kinds, ["opened"]);
}

/// Catches a close judged against an old config landing because only the incident's revision
/// was compared: any writer that bumps the check's revision without closing must still fence it.
#[tokio::test]
async fn a_close_after_a_bare_revision_bump_reports_not_applied() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let opened = open(&f.check, 1, now_minus(60));
    assert_eq!(writer.submit(opened.clone()), Submit::Queued);
    assert!(written(&mut reports).await.applied);
    f.admin
        .execute(
            "UPDATE uptime_check SET revision = revision + 1 WHERE id = $1",
            &[&f.check.id.0],
        )
        .await
        .unwrap();

    assert_eq!(
        writer.submit(close(&f.check, &opened, now_minus(1))),
        Submit::Queued
    );
    assert!(!written(&mut reports).await.applied);
    let open: bool = f
        .admin
        .query_one(
            "SELECT closed_at IS NULL FROM uptime_incident WHERE id = $1",
            &[&opened.incident.0],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        open,
        "the incident stays open for the new revision to judge"
    );
}

/// Catches a submit that waits for room, which would stall the engine behind Postgres.
#[tokio::test]
async fn a_full_queue_refuses_at_once() {
    let f = fixture().await;
    let (tx, _rx) = mpsc::channel(16);
    let writer = IncidentWriter::spawn(f.pg.pg().await, 1, tx);
    // The writer task hasn't run yet on this single-threaded runtime, so nothing leaves the queue.
    assert_eq!(
        writer.submit(open(&f.check, 1, now_minus(60))),
        Submit::Queued
    );
    assert_eq!(
        writer.submit(open(&f.check, 1, now_minus(60))),
        Submit::Full
    );
}

async fn answered_row(admin: &Client, check: CheckId) -> (Option<SystemTime>, i32, SystemTime) {
    let row = admin
        .query_one(
            "SELECT probe_answered_at, revision, updated_at FROM uptime_check WHERE id = $1",
            &[&check.0],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

/// Catches an answer that moves back when an older one lands late, or that bumps the revision and
/// so resets the check it answers. The open behind the answers proves they ran: the queue is FIFO.
#[tokio::test]
async fn an_answer_records_the_newest_request_and_leaves_the_check_alone() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let (_, revision, updated_at) = answered_row(&f.admin, f.check.id).await;
    let newer = now_minus(10);
    let older = now_minus(20);
    for requested in [newer, older] {
        let answered = ProbeAnswered {
            check: f.check.id,
            requested,
        };
        assert_eq!(writer.answer(answered), Submit::Queued);
    }
    assert_eq!(
        writer.submit(open(&f.check, 1, now_minus(5))),
        Submit::Queued
    );
    assert!(written(&mut reports).await.applied);
    assert_eq!(
        answered_row(&f.admin, f.check.id).await,
        (Some(newer.to_system_time()), revision, updated_at)
    );
}

/// Catches a missing grant breaking the plane: before the owner runs the GRANT, an answer is
/// refused and incident writes behind it still apply.
#[tokio::test]
async fn an_answer_the_role_may_not_write_is_dropped_and_incidents_still_apply() {
    let f = fixture().await;
    let grants = include_str!("../../control-role.sql").replace(", probe_answered_at", "");
    let url = f.pg.role(&f.admin, "uptime_control", &grants).await;
    let (writer, mut reports) = writer(connect(&url).await);
    let answered = ProbeAnswered {
        check: f.check.id,
        requested: now_minus(10),
    };
    assert_eq!(writer.answer(answered), Submit::Queued);
    assert_eq!(
        writer.submit(open(&f.check, 1, now_minus(5))),
        Submit::Queued
    );
    assert!(written(&mut reports).await.applied);
    assert_eq!(answered_row(&f.admin, f.check.id).await.0, None);
}

/// Catches an answer that retries a held lock forever at the head of the queue: one older than the
/// dashboard shows is dropped at its first lock timeout, while a fresh one waits the lock out.
#[tokio::test]
async fn an_answer_blocked_past_its_showing_is_dropped_and_a_fresh_one_waits() {
    let f = fixture().await;
    let (writer, mut reports) = writer(f.pg.pg().await);
    let (stale, fresh) = (now_minus(90), now_minus(1));
    for (requested, want) in [(stale, None), (fresh, Some(fresh.to_system_time()))] {
        f.admin
            .batch_execute(&format!(
                "BEGIN; SELECT 1 FROM uptime_check WHERE id = '{}' FOR UPDATE",
                f.check.id.0
            ))
            .await
            .unwrap();
        let answered = ProbeAnswered {
            check: f.check.id,
            requested,
        };
        assert_eq!(writer.answer(answered), Submit::Queued);
        // Past the writer's one-second lock_timeout, so its first attempt has failed.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        f.admin.batch_execute("COMMIT").await.unwrap();
        let open = open(&f.check, 1, now_minus(5));
        assert_eq!(writer.submit(open), Submit::Queued);
        written(&mut reports).await;
        assert_eq!(
            answered_row(&f.admin, f.check.id).await.0,
            want,
            "requested {requested:?}"
        );
    }
}

/// Catches the Rust statements drifting from the TS copies the dashboard's Postgres test proves.
#[test]
fn the_statements_match_the_dashboard_copies() {
    let ts = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/modules/uptime/incident-fence.ts"),
    )
    .unwrap();
    let quoted = |key: &str| {
        let start = ts.find(&format!("{key}: `")).unwrap() + key.len() + 3;
        let rest = ts.get(start..).unwrap();
        rest.get(..rest.find('`').unwrap()).unwrap().to_owned()
    };
    let words = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(words(OPEN), words(&quoted("open")));
    assert_eq!(words(OBSERVED_CLOSE), words(&quoted("observedClose")));
}

/// Catches a date formatter off by a day around leap days, epochs and year ends.
#[test]
fn timestamps_are_iso_utc_with_milliseconds() {
    for (ms, want) in [
        (0, "1970-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (951_782_400_123, "2000-02-29T00:00:00.123Z"),
        (1_709_210_096_789, "2024-02-29T12:34:56.789Z"),
        (1_791_700_012_345, "2026-10-11T06:26:52.345Z"),
        (4_102_444_799_999, "2099-12-31T23:59:59.999Z"),
    ] {
        assert_eq!(iso(UnixMs(ms)), want);
    }
}

/// Catches the evidence's error spelling drifting from core's serde, which the TS types read.
#[test]
fn error_spellings_match_core() {
    for error in [
        HttpCheckError::ConnectionRefused,
        HttpCheckError::TimedOut,
        HttpCheckError::Other,
    ] {
        assert_eq!(
            json!(http_error_str(error)),
            serde_json::to_value(error).unwrap()
        );
    }
}
