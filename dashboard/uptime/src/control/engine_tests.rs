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
use crate::domain::{Check, CheckId, Expect, Interval, OrgId, ProbeKind, Region, Revision, Target};

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
