use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use super::{Ctx, Health, Input, RoundId, RoundPlan, Step, step};
use crate::domain::{
    AgentName, CheckId, Detail, HttpCheckError, IncidentId, Interval, Judged, NotProbed,
    Observation, OrgId, Outcome, Phase, ProbeKey, ProbeKind, Quorum, Region, Revision, Timings,
    UnixMs, Verdict,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const OPENED: IncidentId = IncidentId(Uuid::from_u128(7));
const CHECK: CheckId = CheckId(Uuid::from_u128(1));

fn regions(names: &[&str]) -> Vec<Region> {
    names.iter().map(|n| Region::parse(n).unwrap()).collect()
}

fn r(name: &str) -> Region {
    Region::parse(name).unwrap()
}

/// One check's detector, driven the way the engine drives it.
struct Detector {
    health: Health,
    live: Vec<Region>,
    quorum: Quorum,
    interval: Interval,
    revision: Revision,
    now: UnixMs,
    rounds: u64,
}

impl Detector {
    fn cloud(live: &[&str]) -> Detector {
        Detector::new(live, Quorum::for_configured(4))
    }

    fn self_host() -> Detector {
        Detector::new(&["local"], Quorum::for_configured(1))
    }

    fn new(live: &[&str], quorum: Quorum) -> Detector {
        Detector {
            health: Health::Reachable,
            live: regions(live),
            quorum,
            interval: Interval::parse(300).unwrap(),
            revision: Revision(3),
            now: UnixMs(1_000_000),
            rounds: 0,
        }
    }

    fn feed(&mut self, input: Input<'_>) -> Step {
        self.rounds += 1;
        let ctx = Ctx {
            now: self.now,
            live: &self.live,
            quorum: self.quorum,
            interval: self.interval,
            timeout: TIMEOUT,
            check: CHECK,
            revision: self.revision,
            next_round: RoundId(self.rounds),
            next_incident: OPENED,
        };
        step(&mut self.health, input, &ctx)
    }

    fn see(&mut self, key: &ProbeKey, verdict: Verdict) -> Step {
        let judged = judged(key, verdict, self.now);
        self.feed(Input::Observed(&judged))
    }

    fn routine(&mut self, region: &str, verdict: Verdict) -> Step {
        let key = ProbeKey {
            check: CHECK,
            region: r(region),
            scheduled_at: self.now,
            kind: ProbeKind::Routine,
        };
        self.see(&key, verdict)
    }

    fn wait(&mut self, by: Duration) {
        self.now = self.now.plus(by);
    }

    /// Opens an incident with a routine failure in `a` confirmed from the next region.
    fn open(&mut self) {
        let a = self.live.first().unwrap().as_str().to_owned();
        let plan = dispatched(self.routine(&a, Verdict::Fail));
        let step = self.see(plan.probes.first().unwrap(), Verdict::Fail);
        assert!(
            matches!(step, Step::Open { .. }),
            "confirmed failure opens, got {step:?}"
        );
    }

    /// Runs one recheck round where every probe reports `verdicts` in order; missing verdicts never
    /// arrive.
    fn recheck_round(&mut self, verdicts: &[Verdict]) -> Step {
        let plan = dispatched(self.feed(Input::RecheckDue));
        let mut last = Step::Nothing;
        for (key, verdict) in plan.probes.iter().zip(verdicts) {
            last = self.see(key, *verdict);
        }
        if matches!(&self.health, Health::Unreachable(o) if o.round.is_some()) {
            self.now = plan.deadline;
            last = self.feed(Input::RoundExpired(plan.round));
        }
        last
    }

    fn passing_rounds(&self) -> u8 {
        let Health::Unreachable(outage) = &self.health else {
            panic!("expected Unreachable, got {:?}", self.health)
        };
        outage.passing_rounds
    }
}

fn judged(key: &ProbeKey, verdict: Verdict, at: UnixMs) -> Judged {
    let outcome = match verdict {
        Verdict::Pass => Outcome::Http { status: 200 },
        Verdict::Fail => Outcome::Unreachable {
            phase: Phase::Connect,
            error: HttpCheckError::ConnectionRefused,
            detail: Detail::new("connection refused"),
        },
        Verdict::Inconclusive => Outcome::NotProbed(NotProbed::Saturated),
    };
    Judged {
        org: OrgId(Uuid::from_u128(9)),
        verdict,
        observation: Observation {
            key: key.clone(),
            started_at: at,
            outcome,
            timings: Timings::default(),
            remote_ip: None,
            cert_not_after: None,
            agent: AgentName(Arc::from("agent-1")),
        },
    }
}

fn dispatched(step: Step) -> RoundPlan {
    let Step::Dispatch(plan) = step else {
        panic!("expected a dispatch, got {step:?}")
    };
    plan
}

fn plan_regions(plan: &RoundPlan) -> Vec<&str> {
    plan.probes.iter().map(|k| k.region.as_str()).collect()
}

#[test]
fn routine_pass_or_inconclusive_changes_nothing() {
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    assert_eq!(d.routine("a", Verdict::Pass), Step::Nothing);
    assert_eq!(d.routine("a", Verdict::Inconclusive), Step::Nothing);
    assert!(matches!(d.health, Health::Reachable));
}

#[test]
fn routine_failure_confirms_from_the_next_two_live_regions() {
    let cases: &[(&[&str], &str, &[&str])] = &[
        (&["a", "b", "c", "d"], "a", &["b", "c"]),
        (&["a", "b", "c", "d"], "c", &["d", "a"]),
        (&["a", "b", "c", "d"], "d", &["a", "b"]),
        (&["a", "b", "c"], "b", &["c", "a"]),
        (&["a", "b"], "b", &["a"]),
    ];
    for (live, failing, confirm) in cases {
        let mut d = Detector::cloud(live);
        let start = d.now;
        let plan = dispatched(d.routine(failing, Verdict::Fail));
        assert_eq!(
            plan_regions(&plan),
            *confirm,
            "{failing} failing among {live:?}"
        );
        assert!(
            plan.probes.iter().all(|k| k.kind == ProbeKind::Confirm
                && k.scheduled_at == start
                && k.check == CHECK)
        );
        assert_eq!(plan.deadline, start.plus(TIMEOUT + Duration::from_secs(15)));
        assert!(matches!(d.health, Health::Suspect(_)));
    }
}

#[test]
fn a_lone_live_region_cannot_confirm_so_nothing_opens() {
    let mut d = Detector::cloud(&["a"]);
    assert_eq!(d.routine("a", Verdict::Fail), Step::Nothing);
    assert!(matches!(d.health, Health::Reachable));
}

#[test]
fn opening_needs_two_failing_regions_and_more_fails_than_passes() {
    use Verdict::{Fail as F, Inconclusive as I, Pass as P};
    // Live regions, then the confirm verdicts in dispatch order; None leaves the probe unanswered
    // until expiry.
    type Case<'a> = (&'a [&'a str], &'a [Option<Verdict>], bool);
    let cases: &[Case<'_>] = &[
        (&["a", "b", "c", "d"], &[Some(F)], true),
        (&["a", "b", "c", "d"], &[Some(P), Some(F)], true),
        (&["a", "b", "c", "d"], &[Some(P), Some(P)], false),
        (&["a", "b", "c", "d"], &[Some(I), Some(F)], true),
        (&["a", "b", "c", "d"], &[Some(I), Some(P)], false),
        (&["a", "b", "c", "d"], &[Some(I), Some(I)], false),
        (&["a", "b", "c", "d"], &[None, None], false),
        (&["a", "b", "c"], &[Some(P), Some(F)], true),
        (&["a", "b"], &[Some(F)], true),
        (&["a", "b"], &[Some(P)], false),
        (&["a", "b"], &[Some(I)], false),
        (&["a", "b"], &[None], false),
    ];
    for (live, verdicts, opens) in cases {
        let mut d = Detector::cloud(live);
        let trigger_at = d.now;
        let plan = dispatched(d.routine("a", F));
        d.wait(Duration::from_secs(2));
        let mut last = Step::Nothing;
        for (key, verdict) in plan.probes.iter().zip(*verdicts) {
            if let Some(verdict) = verdict {
                last = d.see(key, *verdict);
            }
        }
        if !matches!(d.health, Health::Unreachable(_) | Health::Reachable) {
            d.now = plan.deadline;
            last = d.feed(Input::RoundExpired(plan.round));
        }
        let label = format!("{live:?} {verdicts:?}");
        if *opens {
            let Step::Open {
                incident,
                revision,
                evidence,
            } = last
            else {
                panic!("{label} should open, got {last:?}")
            };
            assert_eq!((incident, revision), (OPENED, Revision(3)), "{label}");
            assert_eq!(evidence.quorum, Quorum::DistinctRegions(2), "{label}");
            assert_eq!(
                evidence.observations.first().map(|o| o.kind),
                Some(ProbeKind::Routine),
                "{label}"
            );
            let Health::Unreachable(outage) = &d.health else {
                panic!("{label} not unreachable")
            };
            assert_eq!(
                outage.since, trigger_at,
                "{label} dates the outage from the first failure"
            );
        } else {
            assert_eq!(last, Step::Nothing, "{label}");
            assert!(
                matches!(d.health, Health::Reachable),
                "{label} should return to Reachable"
            );
        }
    }
}

#[test]
fn late_confirmations_and_replays_do_not_count() {
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    let trigger = ProbeKey {
        check: CHECK,
        region: r("a"),
        scheduled_at: d.now,
        kind: ProbeKind::Routine,
    };
    let plan = dispatched(d.see(&trigger, Verdict::Fail));

    assert_eq!(
        d.see(&trigger, Verdict::Fail),
        Step::Nothing,
        "a replayed trigger is not a second region"
    );
    let stranger = ProbeKey {
        region: r("d"),
        ..plan.probes.first().unwrap().clone()
    };
    assert_eq!(
        d.see(&stranger, Verdict::Fail),
        Step::Nothing,
        "a confirm nobody asked for is ignored"
    );

    d.now = plan.deadline.plus(Duration::from_millis(1));
    assert_eq!(
        d.see(plan.probes.first().unwrap(), Verdict::Fail),
        Step::Nothing,
        "late"
    );
    assert_eq!(
        d.feed(Input::RoundExpired(RoundId(999))),
        Step::Nothing,
        "another round's expiry"
    );
    assert!(matches!(d.health, Health::Suspect(_)));
    assert_eq!(d.feed(Input::RoundExpired(plan.round)), Step::Nothing);
    assert!(matches!(d.health, Health::Reachable));
}

#[test]
fn closing_takes_two_consecutive_rounds_of_two_passing_regions_and_no_failures() {
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    d.open();
    let opened_at = Revision(3);
    d.revision = Revision(4);

    let plan = dispatched(d.feed(Input::RecheckDue));
    assert_eq!(
        plan_regions(&plan),
        ["a", "b", "c"],
        "rechecks go to the top three live regions"
    );
    assert!(plan.probes.iter().all(|k| k.kind == ProbeKind::Recheck));
    assert_eq!(
        d.feed(Input::RecheckDue),
        Step::Nothing,
        "one round at a time"
    );
    for key in &plan.probes {
        assert_eq!(d.see(key, Verdict::Pass), Step::Nothing);
    }
    assert_eq!(d.passing_rounds(), 1);

    let step = d.recheck_round(&[Verdict::Pass, Verdict::Pass, Verdict::Inconclusive]);
    let Step::Close {
        incident,
        revision,
        evidence,
    } = step
    else {
        panic!("second passing round closes, got {step:?}")
    };
    assert_eq!(
        (incident, revision),
        (OPENED, opened_at),
        "the close carries the revision it opened at"
    );
    assert_eq!(evidence.observations.len(), 3);
    assert!(
        evidence
            .observations
            .iter()
            .all(|o| o.kind == ProbeKind::Recheck)
    );
    assert!(matches!(d.health, Health::Reachable));
}

#[test]
fn a_round_that_falls_short_resets_the_streak() {
    use Verdict::{Fail as F, Inconclusive as I, Pass as P};
    let cases: &[(&[&str], &[Verdict])] = &[
        (&["a", "b", "c", "d"], &[P, P, F]),
        (&["a", "b", "c", "d"], &[F]),
        (&["a", "b", "c", "d"], &[P, I, I]),
        (&["a", "b", "c", "d"], &[P]),
        (&["a", "b"], &[P, I]),
        (&["a"], &[P]),
    ];
    for (live, short) in cases {
        let mut d = Detector::cloud(&["a", "b", "c", "d"]);
        d.open();
        assert_eq!(d.recheck_round(&[P, P, P]), Step::Nothing);
        assert_eq!(d.passing_rounds(), 1);
        d.live = regions(live);
        assert_eq!(d.recheck_round(short), Step::Nothing, "{live:?} {short:?}");
        assert_eq!(d.passing_rounds(), 0, "{live:?} {short:?} resets");
        let Health::Unreachable(outage) = &d.health else {
            unreachable!()
        };
        assert!(
            outage.round.is_none(),
            "{live:?} {short:?} finishes the round"
        );
    }
}

#[test]
fn one_failure_ends_a_recheck_round_without_waiting() {
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    d.open();
    let plan = dispatched(d.feed(Input::RecheckDue));
    let at = d.now;
    assert_eq!(
        d.see(plan.probes.first().unwrap(), Verdict::Fail),
        Step::Nothing
    );
    let Health::Unreachable(outage) = &d.health else {
        unreachable!()
    };
    assert!(outage.round.is_none());
    assert_eq!(outage.next_round_at, at.plus(Duration::from_secs(60)));
}

#[test]
fn rechecks_run_every_minute_for_fifteen_minutes_then_every_interval() {
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    d.open();
    let Health::Unreachable(opened) = &d.health else {
        unreachable!()
    };
    assert_eq!(opened.next_round_at, d.now.plus(Duration::from_secs(60)));
    let since = opened.since;
    d.now = since.plus(Duration::from_secs(16 * 60));
    assert_eq!(d.recheck_round(&[Verdict::Fail]), Step::Nothing);
    let Health::Unreachable(outage) = &d.health else {
        unreachable!()
    };
    assert_eq!(outage.next_round_at, d.now.plus(Duration::from_secs(300)));
}

#[test]
fn no_live_region_skips_the_round_and_keeps_the_incident_open() {
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    d.open();
    assert_eq!(
        d.recheck_round(&[Verdict::Pass, Verdict::Pass, Verdict::Pass]),
        Step::Nothing
    );
    d.live.clear();
    assert_eq!(d.feed(Input::RecheckDue), Step::Nothing);
    assert_eq!(d.passing_rounds(), 0);
}

#[test]
fn self_host_confirms_and_rechecks_from_its_one_region_thirty_seconds_apart() {
    let mut d = Detector::self_host();
    let start = d.now;
    let plan = dispatched(d.routine("local", Verdict::Fail));
    let confirm = plan.probes.first().unwrap();
    assert_eq!(plan.probes.len(), 1);
    assert_eq!(
        (confirm.region.as_str(), confirm.scheduled_at),
        ("local", start.plus(Duration::from_secs(30)))
    );
    assert_eq!(
        plan.deadline,
        start.plus(Duration::from_secs(30) + TIMEOUT + Duration::from_secs(15))
    );
    d.wait(Duration::from_secs(31));
    let step = d.see(confirm, Verdict::Fail);
    assert!(
        matches!(step, Step::Open { .. }),
        "two failures 30s apart open, got {step:?}"
    );

    let at = d.now;
    let plan = dispatched(d.feed(Input::RecheckDue));
    let times: Vec<UnixMs> = plan.probes.iter().map(|k| k.scheduled_at).collect();
    assert_eq!(times, [at, at.plus(Duration::from_secs(30))]);
    for key in &plan.probes {
        assert_eq!(d.see(key, Verdict::Pass), Step::Nothing);
    }
    let step = d.recheck_round(&[Verdict::Pass, Verdict::Pass]);
    assert!(
        matches!(step, Step::Close { .. }),
        "two passing pairs close, got {step:?}"
    );
}

#[test]
fn self_host_one_pass_in_the_pair_rejects_the_suspicion() {
    let mut d = Detector::self_host();
    let plan = dispatched(d.routine("local", Verdict::Fail));
    assert_eq!(
        d.see(plan.probes.first().unwrap(), Verdict::Pass),
        Step::Nothing
    );
    assert!(matches!(d.health, Health::Reachable));
}

#[test]
fn adopted_incidents_recheck_at_once_and_closed_elsewhere_forgets_them() {
    let adopted = IncidentId(Uuid::from_u128(42));
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    let since = UnixMs(5);
    assert_eq!(
        d.feed(Input::Adopted {
            incident: adopted,
            revision: Revision(2),
            since
        }),
        Step::Nothing
    );
    let plan = dispatched(d.feed(Input::RecheckDue));
    assert_eq!(
        d.feed(Input::Adopted {
            incident: adopted,
            revision: Revision(2),
            since
        }),
        Step::Nothing
    );
    let Health::Unreachable(outage) = &d.health else {
        unreachable!()
    };
    assert!(
        outage.round.is_some(),
        "re-adopting the held incident keeps its round"
    );

    d.now = plan.deadline;
    assert_eq!(d.feed(Input::RoundExpired(plan.round)), Step::Nothing);
    let step = d.recheck_round(&[Verdict::Pass, Verdict::Pass, Verdict::Pass]);
    assert_eq!(step, Step::Nothing);
    assert_eq!(d.feed(Input::ClosedElsewhere), Step::Nothing);
    assert!(matches!(d.health, Health::Reachable));
}

#[test]
fn closes_name_the_adopted_incident_and_its_revision() {
    let adopted = IncidentId(Uuid::from_u128(42));
    let mut d = Detector::cloud(&["a", "b", "c", "d"]);
    let _ = d.feed(Input::Adopted {
        incident: adopted,
        revision: Revision(2),
        since: UnixMs(5),
    });
    assert_eq!(
        d.recheck_round(&[Verdict::Pass, Verdict::Pass]),
        Step::Nothing
    );
    let step = d.recheck_round(&[Verdict::Pass, Verdict::Pass]);
    let Step::Close {
        incident, revision, ..
    } = step
    else {
        panic!("{step:?}")
    };
    assert_eq!((incident, revision), (adopted, Revision(2)));
}
