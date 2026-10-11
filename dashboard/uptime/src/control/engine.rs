//! The engine: one task that owns every check's health, schedule and job queue. Nothing else
//! holds that state, so there is no lock around it; the plane talks to it over a channel and
//! waits for a reply where it needs one.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Notify, mpsc, oneshot, watch};
use uuid::Uuid;

use super::detect::{Ctx, Health, Input, Outage, RoundId, Step, step};
use super::feed::{CheckRow, FeedEvent, OpenIncident};
use super::incidents::{IncidentOp, IncidentWriter, Submit, Written};
use super::results::Results;
use super::schedule::{
    ConfirmBudget, Liveness, SeenBatches, drop_after, next_slot, ranked, region_for, slot_index,
};
use crate::domain::{
    Check, CheckId, IncidentId, Interval, Job, Judged, Observation, ProbeKey, ProbeKind, Quorum,
    Region, UnixMs,
};
use crate::link::Pulled;

pub(crate) enum Msg {
    Take {
        region: Region,
        want: u32,
        reply: oneshot::Sender<Pulled>,
    },
    Observed {
        region: Region,
        batch: Uuid,
        observations: Vec<Observation>,
        reply: oneshot::Sender<()>,
    },
    Feed(FeedEvent),
    Drain,
}

pub(crate) enum FeedCmd {
    Reload,
}

/// Divides every schedule duration so a test sees periods of seconds instead of minutes.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tempo {
    pub(crate) divisor: u32,
}

pub(crate) struct Deps {
    pub(crate) writer: IncidentWriter,
    pub(crate) results: Results,
    pub(crate) feed_cmds: mpsc::Sender<FeedCmd>,
    pub(crate) wake: HashMap<Region, Arc<Notify>>,
}

const TICK: Duration = Duration::from_millis(250);
/// Routine jobs are handed out this far ahead so an agent can start them on the dot.
const LOOKAHEAD: Duration = Duration::from_secs(2);
const DRAIN_IDLE: Duration = Duration::from_secs(5);
const DRAIN_MAX: Duration = Duration::from_secs(30);

struct Entry {
    check: Check,
    /// Bumped on every revision change so timers armed for the old config fall silent.
    generation: u64,
    /// Observations scheduled before this were for an older generation.
    since: UnixMs,
    health: Health,
    writes_in_flight: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum What {
    Slot { generation: u64 },
    RoundDeadline { round: RoundId },
    Recheck { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Due {
    at: UnixMs,
    check: CheckId,
    what: What,
}

#[derive(Default)]
struct RegionQueue {
    urgent: VecDeque<Job>,
    routine: VecDeque<(Job, UnixMs)>,
}

pub(crate) struct Engine {
    regions: Vec<Region>,
    deps: Deps,
    #[cfg(test)]
    tempo: Tempo,
    entries: HashMap<CheckId, Entry>,
    /// Routine slots, fired [`LOOKAHEAD`] early.
    slots: BinaryHeap<Reverse<Due>>,
    /// Round deadlines and recheck times, fired on time.
    timers: BinaryHeap<Reverse<Due>>,
    queues: HashMap<Region, RegionQueue>,
    budget: ConfirmBudget,
    seen: SeenBatches,
    liveness: Liveness,
    next_round: u64,
    draining_since: Option<Instant>,
    last_msg: Instant,
    dropped_routine: u64,
}

impl Engine {
    pub(crate) fn new(regions: Vec<Region>, deps: Deps) -> Engine {
        let now = Instant::now();
        Engine {
            regions,
            deps,
            #[cfg(test)]
            tempo: Tempo { divisor: 1 },
            entries: HashMap::new(),
            slots: BinaryHeap::new(),
            timers: BinaryHeap::new(),
            queues: HashMap::new(),
            budget: ConfirmBudget::new(),
            seen: SeenBatches::new(now),
            liveness: Liveness::new(),
            next_round: 1,
            draining_since: None,
            last_msg: now,
            dropped_routine: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_tempo(mut self, tempo: Tempo) -> Engine {
        self.tempo = Tempo {
            divisor: tempo.divisor.max(1),
        };
        self
    }

    pub(crate) async fn run(
        mut self,
        mut msgs: mpsc::Receiver<Msg>,
        mut written: mpsc::Receiver<Written>,
        stopped: watch::Sender<bool>,
    ) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut written_open = true;
        loop {
            tokio::select! {
                msg = msgs.recv() => match msg {
                    Some(msg) => self.handle(msg),
                    None => break,
                },
                report = written.recv(), if written_open => match report {
                    Some(report) => self.written(report),
                    None => written_open = false,
                },
                _ = tick.tick() => {
                    self.tick();
                    if self.drained() {
                        break;
                    }
                }
            }
        }
        tracing::info!(
            checks = self.entries.len(),
            dropped_routine = self.dropped_routine,
            "engine stopped"
        );
        stopped.send_replace(true);
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Take {
                region,
                want,
                reply,
            } => {
                let pulled = self.take(&region, want);
                drop(reply.send(pulled));
            }
            Msg::Observed {
                region,
                batch,
                observations,
                reply,
            } => {
                self.observed(&region, batch, observations);
                let _ = reply.send(());
            }
            Msg::Feed(event) => self.feed(event),
            Msg::Drain => self.drain(),
        }
    }

    fn take(&mut self, region: &Region, want: u32) -> Pulled {
        let instant = Instant::now();
        self.liveness.mark(region, instant);
        self.last_msg = instant;
        let now = UnixMs::now();
        let mut jobs = Vec::new();
        if self.draining_since.is_some() {
            return Pulled {
                jobs,
                server_now: now,
            };
        }
        let want = usize::try_from(want).unwrap_or(usize::MAX);
        if let Some(queue) = self.queues.get_mut(region) {
            while jobs.len() < want
                && let Some(job) = queue.urgent.pop_front()
            {
                jobs.push(job);
            }
            while jobs.len() < want
                && let Some((job, drop_at)) = queue.routine.pop_front()
            {
                if drop_at < now {
                    self.dropped_routine += 1;
                    continue;
                }
                jobs.push(job);
            }
        }
        Pulled {
            jobs,
            server_now: now,
        }
    }

    fn observed(&mut self, region: &Region, batch: Uuid, observations: Vec<Observation>) {
        let instant = Instant::now();
        self.last_msg = instant;
        if !self.seen.insert(batch, instant) {
            tracing::debug!(batch = %batch, "batch seen before; not judged again");
            return;
        }
        let now = UnixMs::now();
        let mut judged_all = Vec::with_capacity(observations.len());
        let mut ignored = 0usize;
        for observation in observations {
            let id = observation.key.check;
            let Some(entry) = self.entries.get(&id) else {
                ignored += 1;
                continue;
            };
            if observation.key.region != *region || observation.key.scheduled_at < entry.since {
                ignored += 1;
                continue;
            }
            let judged = Judged {
                org: entry.check.org,
                verdict: entry.check.expect.judge(&observation.outcome),
                observation,
            };
            self.drive(id, Input::Observed(&judged), now);
            judged_all.push(judged);
        }
        if ignored > 0 {
            tracing::debug!(
                region = region.as_str(),
                ignored,
                "observations for unknown, stale or foreign jobs ignored"
            );
        }
        if !judged_all.is_empty() {
            self.deps.results.push(judged_all);
        }
    }

    /// Runs the detector for one check and carries out what it asks.
    fn drive(&mut self, id: CheckId, input: Input<'_>, now: UnixMs) {
        let live = self.live_for(id);
        let quorum = self.ctx_quorum();
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        let interval = ctx_interval(
            #[cfg(test)]
            self.tempo,
            entry.check.interval,
        );
        let timeout = scaled(
            #[cfg(test)]
            self.tempo,
            entry.check.timeout,
        );
        let ctx = Ctx {
            now,
            live: &live,
            quorum,
            interval,
            timeout,
            check: id,
            revision: entry.check.revision,
            next_round: RoundId(self.next_round),
            next_incident: IncidentId(Uuid::now_v7()),
        };
        self.next_round += 1;
        let before = recheck_at(&entry.health);
        let round_before = round_out(&entry.health);
        let step = step(&mut entry.health, input, &ctx);
        let after = recheck_at(&entry.health);
        let round_after = round_out(&entry.health);
        let check = entry.check.clone();
        let generation = entry.generation;
        // A finished round's results are ignored, so its unsent jobs would only crowd out routine
        // work. This runs before `act` so a round the step just opened keeps its jobs.
        if round_before.is_some() && round_before != round_after {
            self.drop_urgent(id);
        }
        self.act(step, &check, now);
        if let Some(at) = after
            && before != after
        {
            self.timers.push(Reverse(Due {
                at,
                check: id,
                what: What::Recheck { generation },
            }));
        }
    }

    fn act(&mut self, step: Step, check: &Check, now: UnixMs) {
        match step {
            Step::Nothing => {}
            Step::Dispatch(plan) => {
                self.timers.push(Reverse(Due {
                    at: plan.deadline,
                    check: check.id,
                    what: What::RoundDeadline { round: plan.round },
                }));
                let address = low_bits(plan.round.0);
                let jobs: Vec<Job> = plan
                    .probes
                    .into_iter()
                    .map(|key| Job {
                        key,
                        target: check.target.clone(),
                        timeout: check.timeout,
                        address,
                    })
                    .collect();
                self.budget.push(check.org, jobs);
            }
            write @ (Step::Open { .. } | Step::Close { .. }) => {
                let Some(op) = IncidentOp::of(write, check, now) else {
                    return;
                };
                match self.deps.writer.submit(op) {
                    Submit::Queued => {
                        if let Some(entry) = self.entries.get_mut(&check.id) {
                            entry.writes_in_flight += 1;
                        }
                    }
                    Submit::Full => {
                        tracing::warn!(check = %check.id.0, "incident queue full; resyncing");
                        self.resync();
                    }
                }
            }
        }
    }

    fn tick(&mut self) {
        let now = UnixMs::now();
        let horizon = now.plus(LOOKAHEAD);
        while let Some(Reverse(top)) = self.slots.peek()
            && top.at <= horizon
        {
            let due = *top;
            self.slots.pop();
            self.fire_slot(due);
        }
        while let Some(Reverse(top)) = self.timers.peek()
            && top.at <= now
        {
            let due = *top;
            self.timers.pop();
            self.fire_timer(due, now);
        }
        let mut grown: Vec<Region> = Vec::new();
        for job in self.budget.tick() {
            let region = job.key.region.clone();
            self.queues
                .entry(region.clone())
                .or_default()
                .urgent
                .push_back(job);
            if !grown.contains(&region) {
                grown.push(region);
            }
        }
        for region in grown {
            self.notify(&region);
        }
        for queue in self.queues.values_mut() {
            while let Some((_, drop_at)) = queue.routine.front()
                && *drop_at < now
            {
                queue.routine.pop_front();
                self.dropped_routine += 1;
            }
        }
        self.seen.expire(Instant::now());
    }

    fn fire_slot(&mut self, due: Due) {
        let What::Slot { generation } = due.what else {
            return;
        };
        let live = self.live_for(due.check);
        let Some(entry) = self.entries.get(&due.check) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        let period = scaled(
            #[cfg(test)]
            self.tempo,
            entry.check.interval.as_duration(),
        );
        let k = slot_index(due.check, period, due.at);
        let job = region_for(k, &live).map(|region| Job {
            key: ProbeKey {
                check: due.check,
                region,
                scheduled_at: due.at,
                kind: ProbeKind::Routine,
            },
            target: entry.check.target.clone(),
            timeout: entry.check.timeout,
            address: low_bits(k),
        });
        self.slots.push(Reverse(Due {
            at: next_slot(due.check, period, due.at),
            check: due.check,
            what: What::Slot { generation },
        }));
        if let Some(job) = job {
            let region = job.key.region.clone();
            self.queues
                .entry(region.clone())
                .or_default()
                .routine
                .push_back((job, drop_after(due.at, period)));
            self.notify(&region);
        }
    }

    fn fire_timer(&mut self, due: Due, now: UnixMs) {
        let Some(entry) = self.entries.get(&due.check) else {
            return;
        };
        match due.what {
            What::Slot { .. } => {}
            What::RoundDeadline { round } => {
                self.drive(due.check, Input::RoundExpired(round), now);
            }
            What::Recheck { generation } => {
                if entry.generation != generation {
                    return;
                }
                match recheck_at(&entry.health) {
                    Some(at) if at <= now => self.drive(due.check, Input::RecheckDue, now),
                    Some(at) => self.timers.push(Reverse(Due { at, ..due })),
                    None => {}
                }
            }
        }
    }

    fn feed(&mut self, event: FeedEvent) {
        match event {
            FeedEvent::Loaded(snapshot) => {
                let held: Vec<CheckId> = self.entries.keys().copied().collect();
                for id in held {
                    if !snapshot.rows.iter().any(|row| row.check.id == id) {
                        self.remove(id);
                    }
                }
                for row in snapshot.rows {
                    self.hold(row);
                }
                tracing::info!(
                    checks = self.entries.len(),
                    skipped = snapshot.skipped.len(),
                    "checks loaded"
                );
            }
            FeedEvent::Changed {
                reloaded,
                gone,
                replaced_orgs,
                skipped,
            } => {
                for org in replaced_orgs {
                    let ids: Vec<CheckId> = self
                        .entries
                        .iter()
                        .filter(|(_, e)| e.check.org == org)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in ids {
                        self.remove(id);
                    }
                }
                for id in gone {
                    self.remove(id);
                }
                for row in reloaded {
                    self.hold(row);
                }
                if !skipped.is_empty() {
                    tracing::debug!(skipped = skipped.len(), "malformed checks skipped");
                }
            }
        }
        let load: f64 = self
            .entries
            .values()
            .map(|e| {
                let period = scaled(
                    #[cfg(test)]
                    self.tempo,
                    e.check.interval.as_duration(),
                );
                TICK.as_secs_f64() / period.as_secs_f64().max(f64::EPSILON)
            })
            .sum();
        self.budget.set_load(load);
    }

    fn hold(&mut self, row: CheckRow) {
        let now = UnixMs::now();
        let id = row.check.id;
        let adopt = |open: &OpenIncident| Input::Adopted {
            incident: open.id,
            revision: open.revision,
            since: open.since,
        };
        match self.entries.get_mut(&id) {
            None => {
                self.entries.insert(
                    id,
                    Entry {
                        check: row.check,
                        generation: 0,
                        since: now,
                        health: Health::Reachable,
                        writes_in_flight: 0,
                    },
                );
                self.arm_slot(id, 0, now);
                if let Some(open) = &row.open_incident {
                    self.drive(id, adopt(open), now);
                }
            }
            Some(entry) if entry.check.revision != row.check.revision => {
                entry.check = row.check;
                entry.generation += 1;
                entry.since = now;
                entry.health = Health::Reachable;
                let generation = entry.generation;
                self.budget.forget(id);
                self.unqueue(id);
                self.arm_slot(id, generation, now);
                if let Some(open) = &row.open_incident {
                    self.drive(id, adopt(open), now);
                }
            }
            Some(entry) if entry.writes_in_flight == 0 => match &row.open_incident {
                Some(open) => self.drive(id, adopt(open), now),
                None => {
                    if matches!(entry.health, Health::Unreachable(_)) {
                        self.drive(id, Input::ClosedElsewhere, now);
                    }
                }
            },
            // A write of ours is still landing; its reply says what Postgres holds.
            Some(_) => {}
        }
    }

    fn arm_slot(&mut self, id: CheckId, generation: u64, now: UnixMs) {
        let Some(entry) = self.entries.get(&id) else {
            return;
        };
        let period = scaled(
            #[cfg(test)]
            self.tempo,
            entry.check.interval.as_duration(),
        );
        self.slots.push(Reverse(Due {
            at: next_slot(id, period, now),
            check: id,
            what: What::Slot { generation },
        }));
    }

    fn remove(&mut self, id: CheckId) {
        self.entries.remove(&id);
        self.budget.forget(id);
        self.unqueue(id);
    }

    fn drop_urgent(&mut self, id: CheckId) {
        self.budget.forget(id);
        for queue in self.queues.values_mut() {
            queue.urgent.retain(|job| job.key.check != id);
        }
    }

    fn unqueue(&mut self, id: CheckId) {
        for queue in self.queues.values_mut() {
            queue.urgent.retain(|job| job.key.check != id);
            queue.routine.retain(|(job, _)| job.key.check != id);
        }
    }

    fn written(&mut self, report: Written) {
        if let Some(entry) = self.entries.get_mut(&report.check) {
            entry.writes_in_flight = entry.writes_in_flight.saturating_sub(1);
        }
        if !report.applied {
            tracing::warn!(
                check = %report.check.0,
                incident = %report.incident.0,
                transition = report.transition.as_str(),
                "incident write not applied; reloading checks"
            );
            self.resync();
        }
    }

    fn resync(&mut self) {
        // Full means a reload is already queued, which is the same reload.
        drop(self.deps.feed_cmds.try_send(FeedCmd::Reload));
    }

    fn drain(&mut self) {
        if self.draining_since.is_none() {
            self.draining_since = Some(Instant::now());
            self.budget.clear();
            for region in &self.regions {
                if let Some(wake) = self.deps.wake.get(region) {
                    wake.notify_waiters();
                }
            }
        }
    }

    fn drained(&self) -> bool {
        self.draining_since.is_some_and(|since| {
            self.last_msg.elapsed() >= DRAIN_IDLE || since.elapsed() >= DRAIN_MAX
        })
    }

    fn notify(&self, region: &Region) {
        if let Some(wake) = self.deps.wake.get(region) {
            wake.notify_waiters();
        }
    }

    fn live_for(&self, id: CheckId) -> Vec<Region> {
        self.liveness
            .live(&ranked(id, &self.regions), Instant::now())
    }

    fn ctx_quorum(&self) -> Quorum {
        let quorum = Quorum::for_configured(self.regions.len());
        match quorum {
            Quorum::DistinctRegions(_) => quorum,
            Quorum::OneRegion { probes, spacing } => Quorum::OneRegion {
                probes,
                spacing: scaled(
                    #[cfg(test)]
                    self.tempo,
                    spacing,
                ),
            },
        }
    }
}

/// When the next recheck round is due, if the check is in an outage with no round out.
fn recheck_at(health: &Health) -> Option<UnixMs> {
    match health {
        Health::Unreachable(outage) if outage.round.is_none() => Some(outage.next_round_at),
        Health::Reachable | Health::Suspect(_) | Health::Unreachable(_) => None,
    }
}

fn round_out(health: &Health) -> Option<RoundId> {
    match health {
        Health::Suspect(round)
        | Health::Unreachable(Outage {
            round: Some(round), ..
        }) => Some(round.id),
        Health::Reachable | Health::Unreachable(_) => None,
    }
}

fn low_bits(n: u64) -> u32 {
    u32::try_from(n & u64::from(u32::MAX)).unwrap_or(u32::MAX)
}

#[cfg(not(test))]
fn scaled(duration: Duration) -> Duration {
    duration
}

#[cfg(test)]
fn scaled(tempo: Tempo, duration: Duration) -> Duration {
    duration / tempo.divisor.max(1)
}

#[cfg(not(test))]
fn ctx_interval(interval: Interval) -> Interval {
    interval
}

#[cfg(test)]
fn ctx_interval(tempo: Tempo, interval: Interval) -> Interval {
    let secs = u32::try_from(interval.as_duration().as_secs()).unwrap_or(u32::MAX);
    Interval::unchecked((secs / tempo.divisor.max(1)).max(1))
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
