//! The agent loop: pull jobs, run each at its scheduled time on the plane's clock, report what
//! was seen in batches, retry a batch until the plane acknowledges it.

use std::collections::VecDeque;
use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use uuid::Uuid;

use super::probe::{ProbeConfig, Prober};
use crate::domain::{Observation, UnixMs};
use crate::link::{ControlLink, LinkError, Pulled, ResultBatch};
use crate::run::{Fatal, Shutdown};

#[derive(Clone, Copy, Debug)]
pub struct AgentRun {
    /// Jobs out at once, pulled or running. Never more than the prober can run.
    pub max_inflight: u32,
    pub flush_every: Duration,
    pub flush_at: usize,
    /// Observations kept while the plane is unreachable. Oldest go first past this.
    pub max_unacked: usize,
}

impl AgentRun {
    #[must_use]
    pub fn production(probe: &ProbeConfig) -> AgentRun {
        AgentRun {
            max_inflight: u32::try_from(probe.max_inflight).unwrap_or(u32::MAX),
            flush_every: Duration::from_secs(1),
            flush_at: 500,
            max_unacked: 50_000,
        }
    }
}

/// The plane's clock as seen from here. Jobs start on the plane's schedule, not on a clock
/// that may drift from it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ServerClock {
    offset_ms: i64,
}

impl ServerClock {
    pub(crate) fn observe(&mut self, server_now: UnixMs, local_now: UnixMs) {
        self.offset_ms = server_now.0 - local_now.0;
    }

    pub(crate) fn now(self) -> UnixMs {
        UnixMs(UnixMs::now().0 + self.offset_ms)
    }

    pub(crate) fn until(self, at: UnixMs) -> Duration {
        let ms = at.0 - self.now().0;
        Duration::from_millis(u64::try_from(ms).unwrap_or(0))
    }
}

const EMPTY_PULL_PAUSE: Duration = Duration::from_millis(250);
const FULL_PAUSE: Duration = Duration::from_millis(100);
const BACKOFF: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
const STOP_PROBES_BOUND: Duration = Duration::from_secs(30);
const FINAL_REPORT_BOUND: Duration = Duration::from_secs(5);

type PullFuture = Pin<Box<dyn Future<Output = Result<Pulled, LinkError>> + Send>>;

pub async fn run<L: ControlLink>(
    link: Arc<L>,
    prober: Arc<Prober>,
    run: AgentRun,
    shutdown: Shutdown,
) -> Result<(), Fatal> {
    let (tx, rx) = mpsc::unbounded_channel();
    let reporter = tokio::spawn(report_loop(link.clone(), rx, run));
    pull_loop(link, prober, run, shutdown, tx).await;
    reporter
        .await
        .map_err(|e| Fatal(format!("The agent's reporter stopped: {e}")))
}

async fn pull_loop<L: ControlLink>(
    link: Arc<L>,
    prober: Arc<Prober>,
    run: AgentRun,
    shutdown: Shutdown,
    tx: mpsc::UnboundedSender<Observation>,
) {
    let mut probes: JoinSet<()> = JoinSet::new();
    let mut pull: Option<PullFuture> = None;
    let mut clock = ServerClock::default();
    let mut backoff = BACKOFF;
    let mut next_pull = Instant::now();
    loop {
        let want = run
            .max_inflight
            .saturating_sub(u32::try_from(probes.len()).unwrap_or(u32::MAX));
        let idle = pull.is_none();
        if idle && want > 0 && Instant::now() >= next_pull {
            let link = link.clone();
            pull = Some(Box::pin(async move { link.pull(want).await }));
        }
        let idle = pull.is_none();
        tokio::select! {
            () = shutdown.wait() => break,
            answer = async {
                match pull.as_mut() {
                    Some(p) => p.await,
                    None => pending().await,
                }
            }, if !idle => {
                pull = None;
                match answer {
                    Ok(pulled) => {
                        clock.observe(pulled.server_now, UnixMs::now());
                        backoff = BACKOFF;
                        if pulled.jobs.is_empty() {
                            next_pull = Instant::now() + EMPTY_PULL_PAUSE;
                        }
                        for job in pulled.jobs {
                            let prober = prober.clone();
                            let tx = tx.clone();
                            probes.spawn(async move {
                                tokio::time::sleep(clock.until(job.key.scheduled_at)).await;
                                let observation = prober.probe(&job, clock.now()).await;
                                drop(tx.send(observation));
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "pull failed");
                        next_pull = Instant::now() + backoff;
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                }
            }
            Some(_) = probes.join_next(), if !probes.is_empty() => {}
            () = tokio::time::sleep_until(next_pull.into()), if idle && want > 0 => {}
            () = tokio::time::sleep(FULL_PAUSE), if idle && want == 0 => {}
        }
    }
    drop(pull);
    let finish = async { while probes.join_next().await.is_some() {} };
    if tokio::time::timeout(STOP_PROBES_BOUND, finish)
        .await
        .is_err()
    {
        tracing::warn!(
            left = probes.len(),
            "probes still running at stop; abandoned"
        );
        probes.abort_all();
    }
}

async fn report_loop<L: ControlLink>(
    link: Arc<L>,
    mut rx: mpsc::UnboundedReceiver<Observation>,
    run: AgentRun,
) {
    let mut pending_obs: Vec<Observation> = Vec::new();
    let mut unacked: VecDeque<ResultBatch> = VecDeque::new();
    let mut held = 0usize;
    let mut dropped = 0usize;
    let mut backoff = BACKOFF;
    let mut next_send = Instant::now();
    let mut flush_at = Instant::now() + run.flush_every;
    let mut open = true;
    let mut final_deadline: Option<Instant> = None;
    loop {
        if !pending_obs.is_empty() && (!open || pending_obs.len() >= run.flush_at) {
            seal(&mut pending_obs, &mut unacked, &mut held);
        }
        while held > run.max_unacked
            && let Some(oldest) = unacked.pop_front()
        {
            held -= oldest.observations.len();
            dropped += oldest.observations.len();
        }
        if !open && unacked.is_empty() && pending_obs.is_empty() {
            break;
        }
        if let Some(deadline) = final_deadline
            && Instant::now() >= deadline
        {
            dropped += held + pending_obs.len();
            break;
        }
        let sending = !unacked.is_empty();
        let final_wait =
            final_deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            observation = rx.recv(), if open => match observation {
                Some(observation) => pending_obs.push(observation),
                None => {
                    open = false;
                    final_deadline = Some(Instant::now() + FINAL_REPORT_BOUND);
                }
            },
            () = tokio::time::sleep_until(flush_at.into()), if open => {
                if !pending_obs.is_empty() {
                    seal(&mut pending_obs, &mut unacked, &mut held);
                }
                flush_at = Instant::now() + run.flush_every;
            }
            () = tokio::time::sleep_until(next_send.into()), if sending => {
                let Some(batch) = unacked.front() else { continue };
                match link.report(batch).await {
                    Ok(()) => {
                        held -= batch.observations.len();
                        unacked.pop_front();
                        backoff = BACKOFF;
                    }
                    Err(LinkError::Broken(e)) => {
                        tracing::warn!(error = %e, "report failed; retrying the same batch");
                        next_send = Instant::now() + backoff;
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                    }
                    Err(LinkError::Rejected(why)) => {
                        tracing::warn!(error = %why, n = batch.observations.len(), "batch refused");
                        held -= batch.observations.len();
                        dropped += batch.observations.len();
                        unacked.pop_front();
                    }
                }
            }
            () = tokio::time::sleep_until(final_wait.into()), if final_deadline.is_some() => {}
        }
    }
    if dropped > 0 {
        tracing::warn!(dropped, "observations never reached the control plane");
    }
}

fn seal(pending_obs: &mut Vec<Observation>, unacked: &mut VecDeque<ResultBatch>, held: &mut usize) {
    let observations = std::mem::take(pending_obs);
    *held += observations.len();
    unacked.push_back(ResultBatch {
        id: Uuid::now_v7(),
        observations,
    });
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
