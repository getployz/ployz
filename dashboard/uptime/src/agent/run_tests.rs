use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{AgentRun, ServerClock, run};
use crate::agent::probe::{ProbeConfig, Prober};
use crate::domain::{AgentName, CheckId, Job, ProbeKey, ProbeKind, Region, Target, UnixMs};
use crate::link::{ControlLink, LinkError, Pulled, ResultBatch};
use crate::run::{Shutdown, Trigger};

#[derive(Default)]
struct FakeLink {
    jobs: Mutex<VecDeque<Job>>,
    wants: Mutex<Vec<u32>>,
    handed: AtomicUsize,
    server_offset_ms: i64,
    broken: AtomicBool,
    attempts: Mutex<Vec<(Uuid, usize)>>,
    acked: Mutex<Vec<ResultBatch>>,
}

impl ControlLink for FakeLink {
    async fn pull(&self, want: u32) -> Result<Pulled, LinkError> {
        self.wants.lock().unwrap().push(want);
        let mut jobs = Vec::new();
        {
            let mut queue = self.jobs.lock().unwrap();
            while jobs.len() < usize::try_from(want).unwrap()
                && let Some(job) = queue.pop_front()
            {
                jobs.push(job);
            }
        }
        self.handed.fetch_add(jobs.len(), Ordering::SeqCst);
        if jobs.is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(Pulled {
            jobs,
            server_now: UnixMs(UnixMs::now().0 + self.server_offset_ms),
        })
    }

    async fn report(&self, batch: &ResultBatch) -> Result<(), LinkError> {
        self.attempts
            .lock()
            .unwrap()
            .push((batch.id, batch.observations.len()));
        if self.broken.load(Ordering::SeqCst) {
            return Err(LinkError::Broken("down".to_owned()));
        }
        self.acked.lock().unwrap().push(ResultBatch {
            id: batch.id,
            observations: batch.observations.clone(),
        });
        Ok(())
    }
}

fn job(n: u128, scheduled_at: UnixMs) -> Job {
    Job {
        key: ProbeKey {
            check: CheckId(Uuid::from_u128(n)),
            region: Region::parse("fra").unwrap(),
            scheduled_at,
            kind: ProbeKind::Routine,
        },
        target: Arc::new(Target::parse("http://127.0.0.1/").unwrap()),
        timeout: Duration::from_secs(1),
        address: 0,
    }
}

fn prober() -> Arc<Prober> {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(Prober::new(ProbeConfig::default(), crypto, AgentName("test".into())).unwrap())
}

fn settings(max_inflight: u32) -> AgentRun {
    AgentRun {
        max_inflight,
        flush_every: Duration::from_millis(100),
        flush_at: 500,
        max_unacked: 50_000,
    }
}

struct Running {
    link: Arc<FakeLink>,
    trigger: Trigger,
    task: tokio::task::JoinHandle<Result<(), crate::run::Fatal>>,
}

fn start(link: FakeLink, settings: AgentRun) -> Running {
    let link = Arc::new(link);
    let (trigger, shutdown) = Shutdown::new();
    let task = tokio::spawn(run(link.clone(), prober(), settings, shutdown));
    Running {
        link,
        trigger,
        task,
    }
}

async fn until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    done()
}

#[tokio::test(flavor = "multi_thread")]
async fn never_asks_for_or_holds_more_jobs_than_max_inflight() {
    let link = FakeLink::default();
    let ahead = UnixMs::now().plus(Duration::from_secs(2));
    for n in 0..10 {
        link.jobs.lock().unwrap().push_back(job(n, ahead));
    }
    let running = start(link, settings(3));
    tokio::time::sleep(Duration::from_millis(600)).await;
    let wants = running.link.wants.lock().unwrap().clone();
    assert!(!wants.is_empty());
    assert!(wants.iter().all(|w| *w <= 3), "{wants:?}");
    assert_eq!(running.link.handed.load(Ordering::SeqCst), 3);
    running.trigger.fire();
    running.task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn probes_start_on_the_control_plane_clock_not_the_local_one() {
    let link = FakeLink {
        server_offset_ms: 5_000,
        ..FakeLink::default()
    };
    let scheduled = UnixMs(UnixMs::now().0 + 5_000 + 200);
    link.jobs.lock().unwrap().push_back(job(1, scheduled));
    let running = start(link, settings(4));
    let link = running.link.clone();
    let reported = until(Duration::from_millis(1_500), || {
        !link.acked.lock().unwrap().is_empty()
    })
    .await;
    assert!(reported, "the observation did not arrive within 1.5 s");
    let started_at = {
        let acked = running.link.acked.lock().unwrap();
        acked
            .first()
            .unwrap()
            .observations
            .first()
            .unwrap()
            .started_at
    };
    assert!(
        started_at >= scheduled,
        "started before its slot on the plane's clock"
    );
    assert!(started_at.0 - scheduled.0 < 1_000);
    running.trigger.fire();
    running.task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_the_plane_did_not_acknowledge_is_sent_again_unchanged() {
    let link = FakeLink::default();
    link.broken.store(true, Ordering::SeqCst);
    link.jobs.lock().unwrap().push_back(job(1, UnixMs::now()));
    let running = start(link, settings(4));
    let link = running.link.clone();
    assert!(
        until(Duration::from_secs(2), || link
            .attempts
            .lock()
            .unwrap()
            .len()
            >= 2)
        .await
    );
    running.link.broken.store(false, Ordering::SeqCst);
    assert!(
        until(Duration::from_secs(3), || !link
            .acked
            .lock()
            .unwrap()
            .is_empty())
        .await
    );
    let attempts = running.link.attempts.lock().unwrap().clone();
    let (first_id, first_len) = attempts.first().copied().unwrap();
    assert!(
        attempts
            .iter()
            .all(|(id, len)| *id == first_id && *len == first_len),
        "{attempts:?}"
    );
    assert_eq!(first_len, 1);
    running.trigger.fire();
    running.task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_oldest_observations_go_first_when_the_plane_stays_down() {
    let link = FakeLink::default();
    link.broken.store(true, Ordering::SeqCst);
    for n in 0..4 {
        link.jobs.lock().unwrap().push_back(job(n, UnixMs::now()));
    }
    let running = start(
        link,
        AgentRun {
            max_inflight: 1,
            flush_every: Duration::from_millis(20),
            flush_at: 1,
            max_unacked: 2,
        },
    );
    let link = running.link.clone();
    assert!(
        until(Duration::from_secs(3), || link
            .handed
            .load(Ordering::SeqCst)
            == 4)
        .await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    running.link.broken.store(false, Ordering::SeqCst);
    assert!(
        until(Duration::from_secs(6), || link.acked.lock().unwrap().len()
            == 2)
        .await
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut checks: Vec<u128> = {
        let acked = running.link.acked.lock().unwrap();
        acked
            .iter()
            .flat_map(|b| b.observations.iter().map(|o| o.key.check.0.as_u128()))
            .collect()
    };
    checks.sort_unstable();
    assert_eq!(checks, vec![2, 3]);
    running.trigger.fire();
    running.task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_reports_what_was_observed_before_returning() {
    let link = FakeLink::default();
    link.jobs.lock().unwrap().push_back(job(1, UnixMs::now()));
    let running = start(
        link,
        AgentRun {
            flush_every: Duration::from_secs(60),
            ..settings(4)
        },
    );
    let link = running.link.clone();
    assert!(
        until(Duration::from_secs(2), || link
            .handed
            .load(Ordering::SeqCst)
            == 1)
        .await
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(running.link.acked.lock().unwrap().is_empty());
    running.trigger.fire();
    running.task.await.unwrap().unwrap();
    let acked = running.link.acked.lock().unwrap();
    assert_eq!(acked.len(), 1);
    assert_eq!(acked.first().unwrap().observations.len(), 1);
}

#[test]
fn the_server_clock_runs_ahead_or_behind_by_the_observed_offset() {
    let mut clock = ServerClock::default();
    let local = UnixMs::now();
    clock.observe(UnixMs(local.0 + 5_000), local);
    let now = clock.now();
    assert!(now.0 - local.0 >= 5_000 && now.0 - local.0 < 5_100);
    let wait = clock.until(UnixMs(local.0 + 5_000 + 300));
    assert!(wait <= Duration::from_millis(300));
    assert!(wait > Duration::from_millis(200));
    assert_eq!(clock.until(UnixMs(0)), Duration::ZERO);
}
