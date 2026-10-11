//! The incident policy, as a pure function of (health, input, context). No I/O, no clock, no
//! randomness: the engine supplies `now`, the live regions and pre-minted ids, and carries out the
//! returned [`Step`].
//!
//! Open: a routine failure makes the check Suspect and sends a confirm round. The incident opens
//! when the quorum fails. Close: while Unreachable, recheck rounds run; a round passes when the
//! quorum passes and nothing fails, and two consecutive passing rounds close the incident.
//! Inconclusive never counts.

use std::net::IpAddr;
use std::time::Duration;

use crate::domain::{
    CheckId, HttpCheckError, IncidentId, Interval, Judged, Outcome, Phase, ProbeKey, ProbeKind,
    Quorum, Region, Revision, UnixMs, Verdict,
};

/// Named for what Cloud observed, never for the deployment's state. There is no "Down".
#[derive(Clone, Debug, Default)]
pub enum Health {
    #[default]
    Reachable,
    /// A routine probe failed and a confirm round is out.
    Suspect(Round),
    /// An incident is open and recheck rounds run until two consecutive rounds pass.
    Unreachable(Outage),
}

#[derive(Clone, Debug)]
pub struct Outage {
    pub incident: IncidentId,
    /// The revision it was opened at. The close carries it, so Postgres refuses a close across a
    /// config edit.
    pub revision: Revision,
    pub since: UnixMs,
    pub passing_rounds: u8,
    pub round: Option<Round>,
    /// When the engine should send [`Input::RecheckDue`]. Meaningless while a round is out.
    pub next_round_at: UnixMs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RoundId(pub u64);

/// Probes whose verdicts count together. Results match by [`ProbeKey`], so the round id never
/// travels to agents.
#[derive(Clone, Debug)]
pub struct Round {
    pub id: RoundId,
    pub kind: ProbeKind,
    pub expected: Vec<ProbeKey>,
    /// A confirm round's first entry is the routine failure that triggered it, which `expected`
    /// doesn't list.
    pub seen: Vec<Seen>,
    /// Results arriving later are stored but not counted.
    pub deadline: UnixMs,
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub key: ProbeKey,
    pub verdict: Verdict,
    pub evidence: EvidenceItem,
}

/// A config edit is not an observation, so there is no input for it: the engine resets health
/// itself and CRUD closes the episode in Postgres.
#[derive(Debug)]
pub enum Input<'a> {
    Observed(&'a Judged),
    RoundExpired(RoundId),
    RecheckDue,
    /// Postgres shows an open incident this engine doesn't hold (restart, resync, or another
    /// control plane).
    Adopted {
        incident: IncidentId,
        revision: Revision,
        since: UnixMs,
    },
    /// Postgres shows the incident this engine holds is closed.
    ClosedElsewhere,
}

#[derive(Debug)]
pub struct Ctx<'a> {
    pub now: UnixMs,
    /// The check's configured regions in rank order, filtered to live ones.
    pub live: &'a [Region],
    pub quorum: Quorum,
    pub interval: Interval,
    pub timeout: Duration,
    pub check: CheckId,
    pub revision: Revision,
    pub next_round: RoundId,
    /// Used only if this step opens.
    pub next_incident: IncidentId,
}

#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub enum Step {
    Nothing,
    Dispatch(RoundPlan),
    Open {
        incident: IncidentId,
        revision: Revision,
        evidence: Evidence,
    },
    Close {
        incident: IncidentId,
        revision: Revision,
        evidence: Evidence,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct RoundPlan {
    pub round: RoundId,
    pub kind: ProbeKind,
    pub probes: Vec<ProbeKey>,
    pub deadline: UnixMs,
}

/// What the incident row records: who saw what. No cause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Evidence {
    pub quorum: Quorum,
    pub observations: Vec<EvidenceItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceItem {
    pub region: Region,
    pub kind: ProbeKind,
    pub verdict: Verdict,
    pub status: Option<u16>,
    pub failed_phase: Option<Phase>,
    pub error: Option<HttpCheckError>,
    pub remote_ip: Option<IpAddr>,
    pub at: UnixMs,
}

impl EvidenceItem {
    fn of(judged: &Judged) -> EvidenceItem {
        let observation = &judged.observation;
        let (status, failed_phase, error) = match &observation.outcome {
            Outcome::Http { status } => (Some(*status), None, None),
            Outcome::Unreachable { phase, error, .. } => (None, Some(*phase), Some(*error)),
            Outcome::NotProbed(_) => (None, None, None),
        };
        EvidenceItem {
            region: observation.key.region.clone(),
            kind: observation.key.kind,
            verdict: judged.verdict,
            status,
            failed_phase,
            error,
            remote_ip: observation.remote_ip,
            at: observation.started_at,
        }
    }
}

/// Grace past the last probe's timeout before a round's results stop counting.
const ROUND_GRACE: Duration = Duration::from_secs(15);
/// Recheck every min(60s, interval) for this long after opening, then every interval.
const FAST_RECHECK_FOR: Duration = Duration::from_secs(15 * 60);
const RECHECK_REGIONS: usize = 3;
const PASSING_ROUNDS_TO_CLOSE: u8 = 2;

/// The whole policy. Mutates `health` and returns what the engine must do.
pub fn step(health: &mut Health, input: Input<'_>, ctx: &Ctx<'_>) -> Step {
    match input {
        Input::Adopted {
            incident,
            revision,
            since,
        } => {
            if let Health::Unreachable(outage) = health
                && outage.incident == incident
            {
                return Step::Nothing;
            }
            *health = Health::Unreachable(Outage {
                incident,
                revision,
                since,
                passing_rounds: 0,
                round: None,
                next_round_at: ctx.now,
            });
            Step::Nothing
        }
        Input::ClosedElsewhere => {
            if matches!(health, Health::Unreachable(_)) {
                *health = Health::Reachable;
            }
            Step::Nothing
        }
        Input::Observed(judged) => observed(health, judged, ctx),
        Input::RoundExpired(id) => expired(health, id, ctx),
        Input::RecheckDue => recheck_due(health, ctx),
    }
}

fn observed(health: &mut Health, judged: &Judged, ctx: &Ctx<'_>) -> Step {
    let key = &judged.observation.key;
    match health {
        Health::Reachable => {
            if key.kind == ProbeKind::Routine && judged.verdict == Verdict::Fail {
                suspect(health, judged, ctx)
            } else {
                Step::Nothing
            }
        }
        Health::Suspect(round) => {
            if !round.record(judged, ctx.now) {
                return Step::Nothing;
            }
            match judge_confirm(round, ctx.quorum) {
                Decision::Pending => Step::Nothing,
                Decision::Rejected => {
                    *health = Health::Reachable;
                    Step::Nothing
                }
                Decision::Confirmed => open(health, ctx),
            }
        }
        Health::Unreachable(outage) => {
            let Some(round) = &mut outage.round else {
                return Step::Nothing;
            };
            if !round.record(judged, ctx.now) {
                return Step::Nothing;
            }
            if round.missing().next().is_some()
                && !round.seen.iter().any(|s| s.verdict == Verdict::Fail)
            {
                return Step::Nothing;
            }
            finish_recheck(health, ctx)
        }
    }
}

fn expired(health: &mut Health, id: RoundId, ctx: &Ctx<'_>) -> Step {
    match health {
        Health::Suspect(round) if round.id == id => {
            *health = Health::Reachable;
            Step::Nothing
        }
        Health::Unreachable(Outage {
            round: Some(round), ..
        }) if round.id == id => finish_recheck(health, ctx),
        Health::Reachable | Health::Suspect(_) | Health::Unreachable(_) => Step::Nothing,
    }
}

fn suspect(health: &mut Health, trigger: &Judged, ctx: &Ctx<'_>) -> Step {
    let failing = &trigger.observation.key.region;
    let starts: Vec<(Region, UnixMs)> = match ctx.quorum {
        Quorum::DistinctRegions(n) => {
            // The next live regions after the failing one, wrapping, so confirmations spread like
            // routine probes.
            let at = ctx
                .live
                .iter()
                .position(|r| r == failing)
                .map_or(0, |i| i + 1);
            ctx.live
                .iter()
                .cycle()
                .skip(at)
                .take(ctx.live.len())
                .filter(|r| *r != failing)
                .take(usize::from(n))
                .map(|r| (r.clone(), ctx.now))
                .collect()
        }
        Quorum::OneRegion { probes, spacing } => (1..probes)
            .map(|i| (failing.clone(), ctx.now.plus(spacing * u32::from(i))))
            .collect(),
    };
    if starts.is_empty() {
        // No other live vantage point: a lone failure can't be confirmed, so it stays a stored
        // observation.
        return Step::Nothing;
    }
    let plan = plan(ProbeKind::Confirm, starts, ctx);
    *health = Health::Suspect(Round {
        id: plan.round,
        kind: plan.kind,
        expected: plan.probes.clone(),
        seen: vec![Seen::of(trigger)],
        deadline: plan.deadline,
    });
    Step::Dispatch(plan)
}

fn open(health: &mut Health, ctx: &Ctx<'_>) -> Step {
    let Health::Suspect(round) = health else {
        return Step::Nothing;
    };
    let since = round.seen.first().map_or(ctx.now, |s| s.evidence.at);
    let evidence = round.evidence(ctx.quorum);
    *health = Health::Unreachable(Outage {
        incident: ctx.next_incident,
        revision: ctx.revision,
        since,
        passing_rounds: 0,
        round: None,
        next_round_at: ctx
            .now
            .plus(recheck_cadence(ctx.now.since(since), ctx.interval)),
    });
    Step::Open {
        incident: ctx.next_incident,
        revision: ctx.revision,
        evidence,
    }
}

fn recheck_due(health: &mut Health, ctx: &Ctx<'_>) -> Step {
    let Health::Unreachable(outage) = health else {
        return Step::Nothing;
    };
    if outage.round.is_some() {
        return Step::Nothing;
    }
    let starts: Vec<(Region, UnixMs)> = match ctx.quorum {
        // With fewer live regions than the quorum the round still goes out, so the evidence is
        // stored, but it can't pass: one vantage point never closes what two opened.
        Quorum::DistinctRegions(_) => ctx
            .live
            .iter()
            .take(RECHECK_REGIONS)
            .map(|r| (r.clone(), ctx.now))
            .collect(),
        Quorum::OneRegion { probes, spacing } => match ctx.live.first() {
            Some(region) => (0..probes)
                .map(|i| (region.clone(), ctx.now.plus(spacing * u32::from(i))))
                .collect(),
            None => Vec::new(),
        },
    };
    if starts.is_empty() {
        outage.passing_rounds = 0;
        outage.next_round_at = ctx
            .now
            .plus(recheck_cadence(ctx.now.since(outage.since), ctx.interval));
        return Step::Nothing;
    }
    let plan = plan(ProbeKind::Recheck, starts, ctx);
    outage.round = Some(Round {
        id: plan.round,
        kind: plan.kind,
        expected: plan.probes.clone(),
        seen: Vec::new(),
        deadline: plan.deadline,
    });
    Step::Dispatch(plan)
}

/// Closes out the recheck round in flight: counts it as passing or not, and closes after enough in
/// a row.
fn finish_recheck(health: &mut Health, ctx: &Ctx<'_>) -> Step {
    let Health::Unreachable(outage) = health else {
        return Step::Nothing;
    };
    let Some(round) = outage.round.take() else {
        return Step::Nothing;
    };
    if round.passes(ctx.quorum) {
        outage.passing_rounds = outage.passing_rounds.saturating_add(1);
    } else {
        outage.passing_rounds = 0;
    }
    if outage.passing_rounds >= PASSING_ROUNDS_TO_CLOSE {
        let step = Step::Close {
            incident: outage.incident,
            revision: outage.revision,
            evidence: round.evidence(ctx.quorum),
        };
        *health = Health::Reachable;
        return step;
    }
    outage.next_round_at = ctx
        .now
        .plus(recheck_cadence(ctx.now.since(outage.since), ctx.interval));
    Step::Nothing
}

fn plan(kind: ProbeKind, starts: Vec<(Region, UnixMs)>, ctx: &Ctx<'_>) -> RoundPlan {
    let last = starts.iter().map(|(_, at)| *at).max().unwrap_or(ctx.now);
    RoundPlan {
        round: ctx.next_round,
        kind,
        probes: starts
            .into_iter()
            .map(|(region, scheduled_at)| ProbeKey {
                check: ctx.check,
                region,
                scheduled_at,
                kind,
            })
            .collect(),
        deadline: last.plus(ctx.timeout + ROUND_GRACE),
    }
}

fn recheck_cadence(open_for: Duration, interval: Interval) -> Duration {
    let interval = interval.as_duration();
    if open_for < FAST_RECHECK_FOR {
        interval.min(Duration::from_secs(60))
    } else {
        interval
    }
}

enum Decision {
    Pending,
    Confirmed,
    Rejected,
}

/// Confirmed once the quorum fails, rejected once every confirm is in without that. A round is the
/// trigger plus one confirm per missing vote, so two failing regions always outnumber the passes
/// and the quorum stays reachable until the last confirm arrives.
fn judge_confirm(round: &Round, quorum: Quorum) -> Decision {
    let fails = round.tally(Verdict::Fail, quorum);
    let needed = needed(quorum);
    if fails >= needed {
        Decision::Confirmed
    } else if round.missing().next().is_none() {
        Decision::Rejected
    } else {
        Decision::Pending
    }
}

fn needed(quorum: Quorum) -> usize {
    match quorum {
        Quorum::DistinctRegions(regions) => usize::from(regions),
        Quorum::OneRegion { probes, .. } => usize::from(probes),
    }
}

impl Seen {
    fn of(judged: &Judged) -> Seen {
        Seen {
            key: judged.observation.key.clone(),
            verdict: judged.verdict,
            evidence: EvidenceItem::of(judged),
        }
    }
}

impl Round {
    /// Counts the result if this round expects it, hasn't counted it yet and it arrived by the
    /// deadline.
    fn record(&mut self, judged: &Judged, now: UnixMs) -> bool {
        let key = &judged.observation.key;
        if now > self.deadline
            || !self.expected.contains(key)
            || self.seen.iter().any(|s| &s.key == key)
        {
            return false;
        }
        self.seen.push(Seen::of(judged));
        true
    }

    fn missing(&self) -> impl Iterator<Item = &ProbeKey> {
        self.expected
            .iter()
            .filter(|key| !self.seen.iter().any(|s| &s.key == *key))
    }

    /// Distinct regions with this verdict, or probes for a one-region quorum.
    fn tally(&self, verdict: Verdict, quorum: Quorum) -> usize {
        let matching = self.seen.iter().filter(|s| s.verdict == verdict);
        match quorum {
            Quorum::DistinctRegions(_) => {
                let mut regions: Vec<&Region> = matching.map(|s| &s.key.region).collect();
                regions.sort();
                regions.dedup();
                regions.len()
            }
            Quorum::OneRegion { .. } => matching.count(),
        }
    }

    fn passes(&self, quorum: Quorum) -> bool {
        self.tally(Verdict::Fail, quorum) == 0
            && self.tally(Verdict::Pass, quorum) >= needed(quorum)
    }

    fn evidence(&self, quorum: Quorum) -> Evidence {
        Evidence {
            quorum,
            observations: self.seen.iter().map(|s| s.evidence.clone()).collect(),
        }
    }
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod tests;
