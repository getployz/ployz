use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

use super::{Deps, Engine, Tempo, What};
use crate::control::detect::Health;
use crate::control::feed::{CheckRow, FeedEvent, Snapshot};
use crate::control::incidents::IncidentWriter;
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

/// A statement that touches many rows reloads the whole org; a request already acted on must not
/// fire again because its check came back in that reload.
#[tokio::test(flavor = "multi_thread")]
async fn an_org_reload_does_not_fire_a_request_already_acted_on() {
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
    let requested = CheckRow {
        probe_requested: Some(UnixMs::now()),
        ..row(1)
    };
    let org = requested.check.org;
    let triggered = |engine: &mut Engine, replaced_orgs: Vec<OrgId>| {
        engine.feed(FeedEvent::Changed {
            reloaded: vec![requested.clone()],
            gone: Vec::new(),
            replaced_orgs,
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

    engine.feed(FeedEvent::Loaded(Snapshot {
        rows: vec![row(1)],
        skipped: Vec::new(),
    }));
    assert!(triggered(&mut engine, Vec::new()) > 0, "fresh request");
    let deadline = engine
        .timers
        .iter()
        .map(|due| due.0)
        .find(|due| matches!(due.what, What::RoundDeadline { .. }))
        .expect("the request started a triggered round");
    engine.fire_timer(deadline, deadline.at);

    assert_eq!(triggered(&mut engine, vec![org]), 0, "org reload");
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
