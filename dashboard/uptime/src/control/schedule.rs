//! Pure scheduling arithmetic and the small bookkeeping structures the engine owns: where and
//! when a routine probe runs, how confirm rounds are rate-limited, which batches were seen and
//! which regions have an agent pulling.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::domain::{CheckId, Job, OrgId, Region, UnixMs};

/// A check's fixed offset within its period, from the first half of its id. Checks created in
/// the same second spread across the period instead of firing together.
fn phase(check: CheckId, period: Duration) -> i64 {
    let period = period_ms(period);
    let seed = check
        .0
        .as_bytes()
        .first_chunk::<8>()
        .map_or(0, |b| u64::from_be_bytes(*b));
    i64::try_from(seed % u64::try_from(period).unwrap_or(1)).unwrap_or(0)
}

fn period_ms(period: Duration) -> i64 {
    i64::try_from(period.as_millis()).unwrap_or(i64::MAX).max(1)
}

/// The first slot strictly after `after`.
#[must_use]
pub(crate) fn next_slot(check: CheckId, period: Duration, after: UnixMs) -> UnixMs {
    let period_ms = period_ms(period);
    let phase = phase(check, period);
    let k = (after.0 - phase).div_euclid(period_ms) + 1;
    UnixMs(phase + k * period_ms)
}

/// Which slot `at` is, counted from the epoch, so the region rotation survives restarts.
#[must_use]
pub(crate) fn slot_index(check: CheckId, period: Duration, at: UnixMs) -> u64 {
    let period_ms = period_ms(period);
    let phase = phase(check, period);
    u64::try_from((at.0 - phase).div_euclid(period_ms)).unwrap_or(0)
}

/// The configured regions rotated by the second half of the check's id, so every check's
/// rotation starts somewhere else and one region doesn't take every check's first slot.
#[must_use]
pub(crate) fn ranked(check: CheckId, regions: &[Region]) -> Vec<Region> {
    if regions.is_empty() {
        return Vec::new();
    }
    let seed = check
        .0
        .as_bytes()
        .last_chunk::<8>()
        .map_or(0, |b| u64::from_be_bytes(*b));
    let start = usize::try_from(seed % u64::try_from(regions.len()).unwrap_or(1)).unwrap_or(0);
    regions
        .iter()
        .cycle()
        .skip(start)
        .take(regions.len())
        .cloned()
        .collect()
}

/// Slot `k` runs from the `k`-th live region, round robin.
#[must_use]
pub(crate) fn region_for(k: u64, live: &[Region]) -> Option<Region> {
    let len = u64::try_from(live.len()).ok().filter(|n| *n > 0)?;
    live.get(usize::try_from(k % len).ok()?).cloned()
}

/// A routine job nobody pulled by half a period later is stale: the next slot covers it.
#[must_use]
pub(crate) fn drop_after(scheduled_at: UnixMs, period: Duration) -> UnixMs {
    scheduled_at.plus(period / 2)
}

/// Confirm and recheck jobs are capped to a share of the routine rate so a mass outage cannot
/// flood agents, and are handed out round robin per organization so one tenant's outage cannot
/// starve another's.
pub(crate) struct ConfirmBudget {
    per_tick: f64,
    credit: f64,
    waiting: HashMap<OrgId, VecDeque<Job>>,
    order: VecDeque<OrgId>,
    /// Jobs waiting per check, so [`ConfirmBudget::holds`] needn't scan every queue.
    held: HashMap<CheckId, Held>,
}

struct Held {
    jobs: usize,
    /// One of the round's jobs has left, so its times are fixed.
    started: bool,
}

/// What one budget tick hands out.
#[derive(Debug, Default)]
pub(crate) struct Released {
    pub(crate) jobs: Vec<Job>,
    /// Rounds whose probe times moved later by how long the budget held them, so a self-hosted
    /// pair still runs `spacing` apart instead of both at once.
    pub(crate) delayed: Vec<(CheckId, Duration)>,
}

impl ConfirmBudget {
    const SHARE: f64 = 0.2;
    /// Probes per tick even when the routine load is tiny, so a single check still confirms fast.
    const FLOOR: f64 = 4.0;

    pub(crate) fn new() -> ConfirmBudget {
        ConfirmBudget {
            per_tick: Self::FLOOR,
            credit: 0.0,
            waiting: HashMap::new(),
            order: VecDeque::new(),
            held: HashMap::new(),
        }
    }

    fn burst(&self) -> f64 {
        (self.per_tick * 8.0).max(32.0)
    }

    pub(crate) fn set_load(&mut self, routine_per_tick: f64) {
        self.per_tick = (routine_per_tick * Self::SHARE).max(Self::FLOOR);
    }

    pub(crate) fn push(&mut self, org: OrgId, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        for job in &jobs {
            self.held
                .entry(job.key.check)
                .or_insert(Held {
                    jobs: 0,
                    started: false,
                })
                .jobs += 1;
        }
        let queue = self.waiting.entry(org).or_default();
        if queue.is_empty() {
            self.order.push_back(org);
        }
        queue.extend(jobs);
    }

    pub(crate) fn forget(&mut self, check: CheckId) {
        for queue in self.waiting.values_mut() {
            queue.retain(|job| job.key.check != check);
        }
        self.held.remove(&check);
    }

    pub(crate) fn clear(&mut self) {
        self.waiting.clear();
        self.order.clear();
        self.held.clear();
    }

    pub(crate) fn holds(&self, check: CheckId) -> bool {
        self.held.contains_key(&check)
    }

    /// A round's first job to leave shifts the round's held jobs by however late it is, keeping
    /// their spacing.
    pub(crate) fn tick(&mut self, now: UnixMs) -> Released {
        self.credit = (self.credit + self.per_tick).min(self.burst());
        let mut out = Released::default();
        let mut spins = self.order.len();
        while self.credit >= 1.0 && spins > 0 {
            let Some(org) = self.order.pop_front() else {
                break;
            };
            spins -= 1;
            let Some(queue) = self.waiting.get_mut(&org) else {
                continue;
            };
            if let Some(mut job) = queue.pop_front() {
                let check = job.key.check;
                if let Entry::Occupied(mut held) = self.held.entry(check) {
                    let held_jobs = held.get_mut();
                    if !held_jobs.started {
                        held_jobs.started = true;
                        let earliest = queue
                            .iter()
                            .filter(|j| j.key.check == check)
                            .map(|j| j.key.scheduled_at)
                            .fold(job.key.scheduled_at, UnixMs::min);
                        let by = now.since(earliest);
                        if !by.is_zero() {
                            job.key.scheduled_at = job.key.scheduled_at.plus(by);
                            for waiting in queue.iter_mut().filter(|j| j.key.check == check) {
                                waiting.key.scheduled_at = waiting.key.scheduled_at.plus(by);
                            }
                            out.delayed.push((check, by));
                        }
                    }
                    held_jobs.jobs -= 1;
                    if held_jobs.jobs == 0 {
                        held.remove();
                    }
                }
                out.jobs.push(job);
                self.credit -= 1.0;
            }
            if queue.is_empty() {
                self.waiting.remove(&org);
            } else {
                self.order.push_back(org);
                spins += 1;
            }
        }
        out
    }
}

/// Batch ids already applied, so an agent's retry after a lost ack is not judged twice.
pub(crate) struct SeenBatches {
    seen: HashMap<Uuid, Instant>,
    last_expire: Instant,
}

impl SeenBatches {
    const WINDOW: Duration = Duration::from_secs(30 * 60);
    const EXPIRE_EVERY: Duration = Duration::from_secs(10);

    pub(crate) fn new(now: Instant) -> SeenBatches {
        SeenBatches {
            seen: HashMap::new(),
            last_expire: now,
        }
    }

    /// `true` if this is the first time. A repeat does not extend its own window.
    pub(crate) fn insert(&mut self, id: Uuid, now: Instant) -> bool {
        match self.seen.entry(id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(now);
                true
            }
        }
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        if now.duration_since(self.last_expire) < Self::EXPIRE_EVERY {
            return;
        }
        self.last_expire = now;
        self.seen
            .retain(|_, at| now.duration_since(*at) < Self::WINDOW);
    }
}

/// A region is live while an agent has pulled from it recently. Routine slots rotate over live
/// regions only, so a region with no agent does not swallow every third probe.
pub(crate) struct Liveness {
    last_pull: HashMap<Region, Instant>,
}

impl Liveness {
    const LIVE_FOR: Duration = Duration::from_secs(30);

    pub(crate) fn new() -> Liveness {
        Liveness {
            last_pull: HashMap::new(),
        }
    }

    pub(crate) fn mark(&mut self, region: &Region, now: Instant) {
        self.last_pull.insert(region.clone(), now);
    }

    pub(crate) fn is_live(&self, region: &Region, now: Instant) -> bool {
        self.last_pull
            .get(region)
            .is_some_and(|at| now.duration_since(*at) < Self::LIVE_FOR)
    }

    /// `ranked` filtered to live regions. When none is live every region counts, so jobs queue
    /// for the first agent to arrive instead of nobody.
    pub(crate) fn live(&self, ranked: &[Region], now: Instant) -> Vec<Region> {
        let live: Vec<Region> = ranked
            .iter()
            .filter(|r| self.is_live(r, now))
            .cloned()
            .collect();
        if live.is_empty() {
            ranked.to_vec()
        } else {
            live
        }
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
