use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

use super::{Deps, Engine, Tempo, What};
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
    for promoted in [false, true] {
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
        if promoted {
            engine.tick();
        }
        let deadline = engine
            .timers
            .iter()
            .map(|due| due.0)
            .find(|due| matches!(due.what, What::RoundDeadline { .. }))
            .expect("the failure started a confirm round");
        engine.fire_timer(deadline, deadline.at);

        let budgeted = engine.budget.tick();
        assert!(budgeted.is_empty(), "promoted {promoted}: {budgeted:?}");
        let handed: Vec<ProbeKind> = engine
            .take(&region, 100)
            .jobs
            .iter()
            .map(|job| job.key.kind)
            .filter(|kind| *kind != ProbeKind::Routine)
            .collect();
        assert!(handed.is_empty(), "promoted {promoted}: {handed:?}");
    }
    drop(pg);
}
