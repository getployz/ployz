use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{Notify, mpsc};
use tokio_postgres::Client;
use uuid::Uuid;

use super::{Deps, Engine, FeedCmd, Tempo, What};
use crate::control::detect::{Health, RoundId};
use crate::control::feed::{CheckRow, FeedEvent, Snapshot};
use crate::control::incidents::{IncidentWriter, Written};
use crate::control::results::{self, Results};
use crate::control::test_pg;
use crate::domain::{
    AgentName, Check, CheckId, Expect, Interval, Observation, OrgId, Outcome, ProbeKey, ProbeKind,
    Region, Revision, Target, Timings, UnixMs,
};

fn row(revision: i32) -> CheckRow {
    CheckRow {
        check: Check {
            id: CheckId(Uuid::from_u128(1)),
            org: OrgId(Uuid::from_u128(2)),
            revision: Revision(revision),
            name: "Check".into(),
            url: "https://example.com/".into(),
            target: Arc::new(Target::parse("https://example.com/").unwrap()),
            interval: Interval::parse(30).unwrap(),
            timeout: Duration::from_secs(1),
            expect: Expect::Below400,
        },
        open_incident: None,
        probe_requested: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slot_armed_for_an_old_revision_falls_silent_after_the_edit() {
    let (pg, _admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (written, _written_rx) = mpsc::channel(8);
    let (feed_cmds, _cmds_rx) = mpsc::channel(2);
    let deps = Deps {
        writer: IncidentWriter::spawn(pg.pg().await, 8, written),
        results: Results::spawn(pg.pg().await, results::Config::PRODUCTION),
        feed_cmds,
        wake: HashMap::from([(region.clone(), Arc::new(Notify::new()))]),
    };
    let mut engine = Engine::new(vec![region.clone()], deps).with_tempo(Tempo { divisor: 30 });

    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row(1)],
        skipped: Vec::new(),
    }));
    engine.feed(FeedEvent::Changed {
        reloaded: vec![row(2)],
        gone: Vec::new(),
        replaced_orgs: Vec::new(),
        skipped: Vec::new(),
    });
    // Period is one second and slots fire two seconds early, so one tick fires both chains.
    engine.tick();

    let armed: Vec<What> = engine.slots.iter().map(|due| due.0.what).collect();
    assert_eq!(armed, vec![What::Slot { generation: 1 }], "{armed:?}");
    let jobs = engine.take(&region, 10).jobs;
    assert!(!jobs.is_empty());
    assert!(jobs.iter().all(|j| j.key.kind == ProbeKind::Routine));
    let slots: Vec<i64> = jobs.iter().map(|j| j.key.scheduled_at.0).collect();
    assert!(
        slots.is_sorted_by(|a, b| a < b),
        "a slot was handed out twice: {slots:?}"
    );
    drop(pg);
}

/// Catches confirm and recheck jobs outliving their round: during a long outage every check queues
/// a round a minute, and a backlog of jobs whose results are ignored grows ahead of routine work.
#[tokio::test(flavor = "multi_thread")]
async fn jobs_left_from_a_finished_round_are_never_handed_out() {
    let (pg, _admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (written, _written_rx) = mpsc::channel(8);
    let (feed_cmds, _cmds_rx) = mpsc::channel(2);
    let deps = Deps {
        writer: IncidentWriter::spawn(pg.pg().await, 8, written),
        results: Results::spawn(pg.pg().await, results::Config::PRODUCTION),
        feed_cmds,
        wake: HashMap::from([(region.clone(), Arc::new(Notify::new()))]),
    };
    let mut engine = Engine::new(vec![region.clone()], deps);
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row(1)],
        skipped: Vec::new(),
    }));
    let check = row(1).check.id;
    let failed = Observation {
        key: ProbeKey {
            check,
            region: region.clone(),
            scheduled_at: UnixMs::now(),
            kind: ProbeKind::Routine,
        },
        started_at: UnixMs::now(),
        outcome: Outcome::Http { status: 503 },
        timings: Timings::default(),
        remote_ip: None,
        cert_not_after: None,
        agent: AgentName("local".into()),
    };
    engine.observed(&region, Uuid::now_v7(), vec![failed]);
    // The round's clock starts once its jobs leave the budget, so it can only expire after that.
    engine.tick();
    let deadline = engine
        .timers
        .iter()
        .map(|due| due.0)
        .filter(|due| matches!(due.what, What::RoundDeadline { .. }))
        .max_by_key(|due| due.at)
        .expect("the failure started a confirm round");
    engine.fire_timer(deadline, deadline.at);

    let budgeted = engine.budget.tick(UnixMs::now()).jobs;
    assert!(budgeted.is_empty(), "{budgeted:?}");
    let handed: Vec<ProbeKind> = engine
        .take(&region, 100)
        .jobs
        .iter()
        .map(|job| job.key.kind)
        .filter(|kind| *kind != ProbeKind::Routine)
        .collect();
    assert!(handed.is_empty(), "{handed:?}");
    drop(pg);
}

/// A deploy or Check now probes once: a stale request seen at startup and a request already acted
/// on both stay quiet.
#[tokio::test(flavor = "multi_thread")]
async fn a_probe_request_triggers_one_round_only_while_fresh() {
    let (pg, _admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (written, _written_rx) = mpsc::channel(8);
    let (feed_cmds, _cmds_rx) = mpsc::channel(2);
    let deps = Deps {
        writer: IncidentWriter::spawn(pg.pg().await, 8, written),
        results: Results::spawn(pg.pg().await, results::Config::PRODUCTION),
        feed_cmds,
        wake: HashMap::from([(region.clone(), Arc::new(Notify::new()))]),
    };
    let mut engine = Engine::new(vec![region.clone()], deps);
    let requested = |at: UnixMs| CheckRow {
        probe_requested: Some(at),
        ..row(1)
    };
    let triggered = |engine: &mut Engine, row: CheckRow| {
        engine.feed(FeedEvent::Changed {
            reloaded: vec![row],
            gone: Vec::new(),
            replaced_orgs: Vec::new(),
            skipped: Vec::new(),
        });
        engine.tick();
        engine
            .take(&region, 100)
            .jobs
            .iter()
            .filter(|job| job.key.kind == ProbeKind::Triggered)
            .count()
    };

    let stale = UnixMs(UnixMs::now().0 - 5 * 60 * 1000);
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![requested(stale)],
        skipped: Vec::new(),
    }));
    engine.tick();
    assert_eq!(triggered(&mut engine, requested(stale)), 0, "stale request");

    let fresh = UnixMs::now();
    assert!(
        triggered(&mut engine, requested(fresh)) > 0,
        "fresh request"
    );
    let deadline = engine
        .timers
        .iter()
        .map(|due| due.0)
        .find(|due| matches!(due.what, What::RoundDeadline { .. }))
        .expect("the request started a triggered round");
    engine.fire_timer(deadline, deadline.at);
    assert_eq!(
        triggered(&mut engine, requested(fresh)),
        0,
        "request already acted on"
    );
    drop(pg);
}

/// The receivers stay with the test so the engine's reports and reloads have somewhere to go.
async fn engine(
    pg: &test_pg::Postgres,
    regions: &[Region],
) -> (Engine, mpsc::Receiver<Written>, mpsc::Receiver<FeedCmd>) {
    let (written, written_rx) = mpsc::channel(8);
    let (feed_cmds, cmds_rx) = mpsc::channel(2);
    let deps = Deps {
        writer: IncidentWriter::spawn(pg.pg().await, 8, written),
        results: Results::spawn(pg.pg().await, results::Config::PRODUCTION),
        feed_cmds,
        wake: regions
            .iter()
            .map(|region| (region.clone(), Arc::new(Notify::new())))
            .collect(),
    };
    let engine = Engine::new(regions.to_vec(), deps).with_tempo(Tempo { divisor: 30 });
    (engine, written_rx, cmds_rx)
}

async fn stored_check(admin: &Client) -> CheckRow {
    let org = test_pg::org(admin).await;
    let id = test_pg::check(admin, org, "https://example.com/").await;
    let mut row = row(1);
    row.check.id = CheckId(id);
    row.check.org = OrgId(org);
    row
}

fn request(engine: &mut Engine, row: &CheckRow, at: UnixMs) {
    engine.feed(FeedEvent::Changed {
        reloaded: vec![CheckRow {
            probe_requested: Some(at),
            ..row.clone()
        }],
        gone: Vec::new(),
        replaced_orgs: Vec::new(),
        skipped: Vec::new(),
    });
}

async fn answered(admin: &Client, check: CheckId) -> Option<SystemTime> {
    admin
        .query_one(
            "SELECT probe_answered_at FROM uptime_check WHERE id = $1",
            &[&check.0],
        )
        .await
        .unwrap()
        .get(0)
}

/// The writer is a FIFO a few milliseconds deep, so this long without an answer means none was
/// queued.
async fn assert_unanswered(admin: &Client, check: CheckId, why: &str) {
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(answered(admin, check).await, None, "{why}");
}

async fn assert_answered(admin: &Client, check: CheckId, at: UnixMs, why: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let got = answered(admin, check).await;
        if got == Some(at.to_system_time()) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{why}: {got:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn observe(engine: &mut Engine, region: &Region, key: &ProbeKey, status: u16) {
    let observation = Observation {
        key: key.clone(),
        started_at: UnixMs::now(),
        outcome: Outcome::Http { status },
        timings: Timings::default(),
        remote_ip: None,
        cert_not_after: None,
        agent: AgentName("local".into()),
    };
    engine.observed(region, Uuid::now_v7(), vec![observation]);
}

fn health(engine: &Engine, check: CheckId) -> &Health {
    &engine
        .entries
        .get(&check)
        .expect("the check is held")
        .health
}

fn round_probes(engine: &Engine, check: CheckId) -> Vec<ProbeKey> {
    match health(engine, check) {
        Health::Suspect(round) => round.expected.clone(),
        health @ (Health::Reachable | Health::Unreachable(_)) => {
            panic!("no round out: {health:?}")
        }
    }
}

/// One-region mode sends two probes 30 s apart: the first result alone must not end Checking.
#[tokio::test(flavor = "multi_thread")]
async fn a_probe_request_is_answered_when_its_round_is_judged_not_at_its_first_result() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);

    let at = UnixMs::now();
    request(&mut engine, &row, at);
    let probes = round_probes(&engine, check);
    let [first, second] = probes.as_slice() else {
        panic!("one-region mode sends two probes: {probes:?}")
    };
    assert!(probes.iter().all(|p| p.kind == ProbeKind::Triggered));
    observe(&mut engine, &region, first, 200);
    assert_unanswered(&admin, check, "one of two results").await;
    observe(&mut engine, &region, second, 200);
    assert!(matches!(health(&engine, check), Health::Reachable));
    assert_answered(&admin, check, at, "both results judged").await;
    drop(pg);
}

/// A request made while a confirm round is out joins it, so no triggered row ever lands for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_joining_a_confirm_round_is_answered_when_that_round_ends() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);
    let routine = ProbeKey {
        check,
        region: region.clone(),
        scheduled_at: UnixMs::now(),
        kind: ProbeKind::Routine,
    };
    observe(&mut engine, &region, &routine, 503);
    let confirm = round_probes(&engine, check);
    assert!(confirm.iter().all(|p| p.kind == ProbeKind::Confirm));

    let at = UnixMs::now();
    request(&mut engine, &row, at);
    assert_eq!(
        round_probes(&engine, check),
        confirm,
        "joined, not replaced"
    );
    assert_unanswered(&admin, check, "confirm round still out").await;
    for probe in &confirm {
        observe(&mut engine, &region, probe, 200);
    }
    assert!(matches!(health(&engine, check), Health::Reachable));
    assert_answered(&admin, check, at, "confirm round rejected").await;
    drop(pg);
}

/// With no region nothing can probe, so waiting would show Checking for nothing. A request too old
/// to act on is never answered.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_no_region_can_run_is_answered_at_once_and_a_stale_one_never() {
    let (pg, admin) = test_pg::postgres().await;
    let (mut engine, _written, _cmds) = engine(&pg, &[]).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));

    request(&mut engine, &row, UnixMs(UnixMs::now().0 - 5 * 60 * 1000));
    assert_unanswered(&admin, check, "stale request").await;

    let at = UnixMs::now();
    request(&mut engine, &row, at);
    assert!(matches!(health(&engine, check), Health::Reachable));
    assert_answered(&admin, check, at, "no region").await;
    drop(pg);
}

fn org_reload(engine: &mut Engine, row: &CheckRow, at: UnixMs) {
    engine.feed(FeedEvent::Changed {
        reloaded: vec![CheckRow {
            probe_requested: Some(at),
            ..row.clone()
        }],
        gone: Vec::new(),
        replaced_orgs: vec![row.check.org],
        skipped: Vec::new(),
    });
}

/// A statement that touches many rows reloads the whole org: a request already acted on must not
/// fire again because its check came back in that reload.
#[tokio::test(flavor = "multi_thread")]
async fn an_org_reload_does_not_fire_a_request_already_acted_on() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);

    let at = UnixMs::now();
    request(&mut engine, &row, at);
    for probe in round_probes(&engine, check) {
        observe(&mut engine, &region, &probe, 200);
    }
    assert_answered(&admin, check, at, "round judged").await;

    org_reload(&mut engine, &row, at);
    assert!(
        matches!(health(&engine, check), Health::Reachable),
        "org reload fired the request again"
    );
    drop(pg);
}

/// The reload keeps the round the request is waiting on, so it is still answered when that round
/// ends rather than dropped or started over.
#[tokio::test(flavor = "multi_thread")]
async fn an_org_reload_mid_round_keeps_the_request_waiting_on_it() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);

    let at = UnixMs::now();
    request(&mut engine, &row, at);
    let probes = round_probes(&engine, check);
    org_reload(&mut engine, &row, at);
    assert_eq!(round_probes(&engine, check), probes, "kept, not replaced");
    assert_unanswered(&admin, check, "round still out").await;
    for probe in &probes {
        observe(&mut engine, &region, probe, 200);
    }
    assert_answered(&admin, check, at, "round judged after the reload").await;
    drop(pg);
}

fn round_id(engine: &Engine, check: CheckId) -> RoundId {
    match health(engine, check) {
        Health::Suspect(round) => round.id,
        health @ (Health::Reachable | Health::Unreachable(_)) => {
            panic!("no round out: {health:?}")
        }
    }
}

fn revise(engine: &mut Engine, row: &CheckRow, revision: i32, at: UnixMs) {
    let mut revised = row.clone();
    revised.check.revision = Revision(revision);
    revised.probe_requested = Some(at);
    engine.feed(FeedEvent::Changed {
        reloaded: vec![revised],
        gone: Vec::new(),
        replaced_orgs: Vec::new(),
        skipped: Vec::new(),
    });
}

/// A sync that edits the check without a newer request cancels the round the request waits on.
/// Nothing judged it, so the new config is probed and only that round answers.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_mid_round_probes_the_new_config_before_answering() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);

    let at = UnixMs::now();
    request(&mut engine, &row, at);
    let before = round_id(&engine, check);
    revise(&mut engine, &row, 2, at);
    assert_ne!(
        round_id(&engine, check),
        before,
        "a fresh round for the new config"
    );
    let after = round_probes(&engine, check);
    assert!(after.iter().all(|p| p.kind == ProbeKind::Triggered));
    assert_unanswered(&admin, check, "the edit judged nothing").await;
    for probe in &after {
        observe(&mut engine, &region, probe, 200);
    }
    assert_answered(&admin, check, at, "the new config's round judged").await;
    drop(pg);
}

/// A deploy that edits the check and requests again fires one round, answering the newer request.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_with_a_newer_request_mid_round_fires_one_round() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);

    let first_at = UnixMs::now();
    request(&mut engine, &row, first_at);
    let first = round_id(&engine, check);
    let at = UnixMs(first_at.0 + 1);
    revise(&mut engine, &row, 2, at);
    assert_ne!(round_id(&engine, check), first);
    engine.tick();
    // After the tick: a release a millisecond late shifts the round's probe times with its jobs.
    let round = round_probes(&engine, check);
    let handed: Vec<ProbeKey> = engine
        .take(&region, 100)
        .jobs
        .into_iter()
        .map(|job| job.key)
        .filter(|key| key.kind == ProbeKind::Triggered)
        .collect();
    assert_eq!(handed, round, "exactly one round's jobs");
    assert_unanswered(&admin, check, "the edit judged nothing").await;
    for probe in &round {
        observe(&mut engine, &region, probe, 200);
    }
    assert_answered(&admin, check, at, "the newer request's round judged").await;
    drop(pg);
}

/// An edit after the request has gone stale drops it unanswered: the dashboard's own cap ends
/// Checking.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_mid_round_drops_a_request_gone_stale() {
    let (pg, admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (mut engine, _written, _cmds) = engine(&pg, std::slice::from_ref(&region)).await;
    let row = stored_check(&admin).await;
    let check = row.check.id;
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row.clone()],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);

    let at = UnixMs(UnixMs::now().0 - 59_000);
    request(&mut engine, &row, at);
    let _ = round_id(&engine, check);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    revise(&mut engine, &row, 2, at);
    assert!(matches!(health(&engine, check), Health::Reachable));
    assert_unanswered(&admin, check, "stale request").await;
    drop(pg);
}

/// Catches a bulk trigger outrunning the confirm budget: rounds planned together wait their turn
/// at a few jobs a tick, and a deadline fixed at planning expires rounds whose jobs haven't left.
#[tokio::test(flavor = "multi_thread")]
async fn rounds_held_in_the_budget_expire_only_after_their_jobs_leave() {
    let (pg, _admin) = test_pg::postgres().await;
    let live = [Region::parse("a").unwrap(), Region::parse("b").unwrap()];
    let (written, _written_rx) = mpsc::channel(8);
    let (feed_cmds, _cmds_rx) = mpsc::channel(2);
    let deps = Deps {
        writer: IncidentWriter::spawn(pg.pg().await, 8, written),
        results: Results::spawn(pg.pg().await, results::Config::PRODUCTION),
        feed_cmds,
        wake: live
            .iter()
            .map(|region| (region.clone(), Arc::new(Notify::new())))
            .collect(),
    };
    let mut engine = Engine::new(live.to_vec(), deps).with_tempo(Tempo { divisor: 30 });
    let rows: Vec<CheckRow> = (100..160)
        .map(|n| {
            let mut row = row(1);
            row.check.id = CheckId(Uuid::from_u128(n));
            row
        })
        .collect();
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: rows.clone(),
        skipped: Vec::new(),
    }));
    for region in &live {
        let _ = engine.take(region, 0);
    }
    let at = UnixMs::now();
    engine.feed(FeedEvent::Changed {
        reloaded: rows
            .iter()
            .map(|row| CheckRow {
                probe_requested: Some(at),
                ..row.clone()
            })
            .collect(),
        gone: Vec::new(),
        replaced_orgs: Vec::new(),
        skipped: Vec::new(),
    });
    let planned: Vec<_> = engine
        .timers
        .iter()
        .map(|due| due.0)
        .filter(|due| matches!(due.what, What::RoundDeadline { .. }))
        .collect();
    assert_eq!(planned.len(), rows.len());
    let suspect = |engine: &Engine| {
        rows.iter()
            .filter(|row| {
                engine
                    .entries
                    .get(&row.check.id)
                    .is_some_and(|entry| matches!(entry.health, Health::Suspect(_)))
            })
            .count()
    };

    for due in &planned {
        engine.fire_timer(*due, due.at);
    }
    assert_eq!(suspect(&engine), rows.len(), "expired while held");

    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut handed: HashMap<CheckId, usize> = HashMap::new();
    for _ in 0..200 {
        engine.tick();
        for region in &live {
            for job in engine.take(region, 1000).jobs {
                if job.key.kind == ProbeKind::Triggered {
                    *handed.entry(job.key.check).or_default() += 1;
                }
            }
        }
    }
    for row in &rows {
        assert_eq!(
            handed.get(&row.check.id),
            Some(&live.len()),
            "every probe went out"
        );
    }
    for due in &planned {
        engine.fire_timer(*due, due.at);
    }
    assert_eq!(
        suspect(&engine),
        rows.len(),
        "the clock restarted when the jobs left"
    );
    drop(pg);
}

/// Catches a self-hosted pair collapsing into one moment: the budget held the round past its
/// spacing, and an overdue schedule runs at once, so both probes would see the same instant.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_one_region_round_keeps_its_spacing_and_its_results_count() {
    let (pg, _admin) = test_pg::postgres().await;
    let region = Region::parse("local").unwrap();
    let (written, _written_rx) = mpsc::channel(8);
    let (feed_cmds, _cmds_rx) = mpsc::channel(2);
    let deps = Deps {
        writer: IncidentWriter::spawn(pg.pg().await, 8, written),
        results: Results::spawn(pg.pg().await, results::Config::PRODUCTION),
        feed_cmds,
        wake: HashMap::from([(region.clone(), Arc::new(Notify::new()))]),
    };
    let mut engine = Engine::new(vec![region.clone()], deps).with_tempo(Tempo { divisor: 30 });
    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row(1)],
        skipped: Vec::new(),
    }));
    let _ = engine.take(&region, 0);
    engine.feed(FeedEvent::Changed {
        reloaded: vec![CheckRow {
            probe_requested: Some(UnixMs::now()),
            ..row(1)
        }],
        gone: Vec::new(),
        replaced_orgs: Vec::new(),
        skipped: Vec::new(),
    });
    // Thirty seconds apart, scaled by the tempo to one.
    let spacing = Duration::from_secs(1);
    tokio::time::sleep(spacing + Duration::from_millis(200)).await;

    let released = UnixMs::now();
    engine.tick();
    let pair: Vec<ProbeKey> = engine
        .take(&region, 100)
        .jobs
        .into_iter()
        .map(|job| job.key)
        .filter(|key| key.kind == ProbeKind::Triggered)
        .collect();
    let [first, second] = pair.as_slice() else {
        panic!("one round of two probes, got {pair:?}")
    };
    let first_runs = first.scheduled_at.max(released);
    assert!(
        second.scheduled_at >= first_runs.plus(spacing),
        "{first:?} then {second:?}, released at {released:?}"
    );

    for key in [first, second] {
        let failed = Observation {
            key: key.clone(),
            started_at: key.scheduled_at,
            outcome: Outcome::Http { status: 503 },
            timings: Timings::default(),
            remote_ip: None,
            cert_not_after: None,
            agent: AgentName("local".into()),
        };
        engine.observed(&region, Uuid::now_v7(), vec![failed]);
    }
    let opened = engine
        .entries
        .get(&row(1).check.id)
        .is_some_and(|entry| matches!(entry.health, Health::Unreachable(_)));
    assert!(opened, "results under the shifted keys count");
    drop(pg);
}
