//! ACME issuance scheduling for one Ingress Hostname with no Certificate Material: when each
//! Machine's certificate loop pushes the hostname to `to_order`, against the backoff clock in
//! the replicated row and each Machine's own Hostname Verdict. A bare `:n` is a line of
//! `core/crates/ployzd/src/certificates.rs`; every other path is written in full from the repo
//! root.
//!
//! Each daemon runs one certificate loop, `run` (:359-415), once it participates
//! (core/crates/ployzd/src/daemon.rs:494-507). Each pass of `issue_wanted` (:424-603) reads the
//! row, keeps a loop-local `first_seen` instant for the hostname (:367, :480-485), and pushes the
//! hostname to `to_order` only when `action_due` says the Machine's rank delay has passed since
//! `first_seen` (:87-105) and `issuance_gate` lets it through
//! (core/crates/ployz-core/src/domain/issuance.rs:65-97). The rank is the Machine's place among
//! every `machines` row (:465-468). A gate that refuses removes `first_seen` (:512); a gate that
//! holds leaves it (:510).
//!
//! The finding: `first_seen` keeps aging through a hold of the shared clock, and through a pass
//! that returns before the gate. Rank 0's order fails, and its record writes an Authority clock
//! (:552-578). Rank 1's pass inserts `first_seen`, and its passes are held until the clock
//! passes. By then rank 1 is past its rank delay, so it pushes the hostname in the same tick as
//! rank 0.

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime},
};

use ployz_core::{
    CertificatePolicy, ClusterRoute, HostnameVerdict, IssuanceClock, IssuanceFailure, IssuanceGate,
    Refusal, issuance_backoff, issuance_failure_clock, issuance_gate,
};
use stateright::{Model, Property};

/// `CHALLENGE_WAIT` (:40).
const CHALLENGE_WAIT: Duration = Duration::from_secs(30);
/// `RETRY_INTERVAL` (:42).
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// `RANK_STEP` (:44-45).
const RANK_STEP: Duration = CHALLENGE_WAIT;

/// Machines in the model. Slot `who` holds the Machine whose id sorts `who`th, so its rank is
/// `who`.
const MACHINES: usize = 2;
/// The largest rank delay, in ticks. Ages are capped there: past it, every `action_due` answer
/// is the same.
const AGE_CAP: u8 = (MACHINES - 1) as u8;
/// The most failures a recorded clock may count. A step that would record more is not taken.
const MAX_FAILURES: u32 = 2;

/// `machine_rank` (:54-61), over Machine slots, whose order is their ids' order.
fn machine_rank(this: usize, machines: impl IntoIterator<Item = usize>) -> usize {
    machines.into_iter().filter(|id| *id < this).count()
}

/// The rank `machine_rank` gives slot `who` among every `machines` row (:465-468).
fn rank(who: usize) -> usize {
    machine_rank(who, 0..MACHINES)
}

/// `rank_delay` (:79-81).
fn rank_delay(rank: usize, step: Duration) -> Duration {
    step.saturating_mul(u32::try_from(rank).unwrap_or(u32::MAX))
}

/// `issuance_step` (:83-85).
fn issuance_step(policy: &CertificatePolicy) -> Duration {
    RANK_STEP.max(policy.probe_timeout())
}

/// `IssuanceAction` (:47-52) without `Renew`, which only the renewal arm returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssuanceAction {
    Nothing,
    Order,
}

/// `action_due` (:87-122). Its `None` arm (:97-105) is copied. Its `Some(material)` arm is
/// renewal (:106-120), which the model leaves out: with material the model answers as that arm
/// does when `renew_at` cannot read the certificate (:119).
fn action_due(
    material: bool,
    rank: usize,
    elapsed: Duration,
    policy: &CertificatePolicy,
) -> (IssuanceAction, Duration) {
    let delay = rank_delay(rank, issuance_step(policy));
    if material {
        return (IssuanceAction::Nothing, RETRY_INTERVAL);
    }
    let wait = delay.saturating_sub(elapsed);
    let action = if wait.is_zero() {
        IssuanceAction::Order
    } else {
        IssuanceAction::Nothing
    };
    (action, wait)
}

/// `poll_wait` (:195-207): how long the loop sleeps after a pass, unless a container change
/// wakes it first (:402-409). Only the story prints it.
fn poll_wait(
    material: bool,
    rank: usize,
    elapsed: Duration,
    policy: &CertificatePolicy,
) -> Duration {
    match action_due(material, rank, elapsed, policy) {
        (IssuanceAction::Order, _) => RANK_STEP,
        (IssuanceAction::Nothing, due_in) => due_in.min(RETRY_INTERVAL),
    }
}

/// The built-in Certificate Policy (core/crates/ployz-core/src/domain/certificate_policy.rs:93-103).
/// The model reads only its backoff and probe values; `Policy` stands in for its directory and
/// for an operator's policy row.
fn built_in() -> CertificatePolicy {
    CertificatePolicy::built_in(None)
}

/// One tick of model time: `issuance_step` under the built-in policy, 30 s.
fn tick() -> Duration {
    issuance_step(&built_in())
}

/// The instant every pass in the current tick reads as `now` and `wall` (:469-470). Only
/// differences from it matter. It sits far enough past the epoch that an order's wall, ticks
/// back, exists.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

/// `duration` in whole ticks.
fn ticks(duration: Duration) -> u8 {
    let tick = tick().as_nanos();
    assert_eq!(
        duration.as_nanos() % tick,
        0,
        "{duration:?} is a whole number of ticks"
    );
    u8::try_from(duration.as_nanos() / tick).expect("a short wait")
}

/// Whole ticks from `now` until `at`, none once `at` has passed.
fn ticks_until(at: SystemTime) -> u8 {
    ticks(at.duration_since(now()).unwrap_or(Duration::ZERO))
}

/// The most ticks an order's pass can lie behind it that change its failure clock: the longest
/// backoff a recorded clock can hold.
fn ago_cap() -> u8 {
    let policy = built_in();
    ticks(issuance_backoff(
        MAX_FAILURES,
        policy.backoff_base(),
        policy.backoff_cap(),
    ))
}

/// What one Machine's probe of the hostname sees (`HostnameVerdict`,
/// core/crates/ployz-core/src/domain/hostname_verdict.rs:45-50), in the one refusal and the one
/// route the model explores.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Verdict {
    /// `Refused(Refusal::DoesNotResolve)`.
    DoesNotResolve,
    /// `ReachesCluster(ClusterRoute::Direct)`.
    Direct,
}

impl Verdict {
    fn real(self) -> HostnameVerdict {
        match self {
            Self::DoesNotResolve => HostnameVerdict::Refused(Refusal::DoesNotResolve),
            Self::Direct => HostnameVerdict::ReachesCluster(ClusterRoute::Direct),
        }
    }

    fn flipped(self) -> Self {
        match self {
            Self::DoesNotResolve => Self::Direct,
            Self::Direct => Self::DoesNotResolve,
        }
    }
}

/// `IssuanceFailure` (core/crates/ployz-core/src/domain/issuance.rs:7-12), for the model's one
/// refusal.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Failure {
    /// `Refused(Refusal::DoesNotResolve)`.
    DoesNotResolve,
    Authority,
}

impl Failure {
    fn real(self) -> IssuanceFailure {
        match self {
            Self::DoesNotResolve => IssuanceFailure::Refused(Refusal::DoesNotResolve),
            Self::Authority => IssuanceFailure::Authority,
        }
    }

    fn of(real: IssuanceFailure) -> Self {
        match real {
            IssuanceFailure::Refused(Refusal::DoesNotResolve) => Self::DoesNotResolve,
            IssuanceFailure::Authority => Self::Authority,
            IssuanceFailure::Refused(
                refusal @ (Refusal::Unreachable
                | Refusal::RedirectsToHttps
                | Refusal::ReachesElsewhere),
            ) => unreachable!("the model's verdicts never refuse with {refusal:?}"),
        }
    }
}

/// `IssuanceClock` (core/crates/ployz-core/src/domain/issuance.rs:14-50), with its
/// `next_attempt_at` held as the ticks left until it. `IssuanceClock` derives no `Hash`, so the
/// model keeps this and rebuilds the real clock at each call.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Clock {
    failures: u8,
    /// Ticks until `next_attempt_at`; zero once it has passed.
    left: u8,
    last: Failure,
}

impl Clock {
    fn real(self) -> IssuanceClock {
        IssuanceClock::new(
            u32::from(self.failures),
            now() + tick() * u32::from(self.left),
            self.last.real(),
        )
    }

    fn of(real: IssuanceClock) -> Self {
        Self {
            failures: u8::try_from(real.failures()).expect("a small count"),
            left: ticks_until(real.next_attempt_at()),
            last: Failure::of(real.last_failure()),
        }
    }

    /// The model's own reading of the rule the gate keeps: the clock has not passed, and the
    /// verdict leaves it standing, which only a different verdict after a refusal does not
    /// (core/crates/ployz-core/src/domain/issuance.rs:65, :76-83).
    fn holds(self, verdict: Verdict) -> bool {
        self.left > 0
            && !(self.last == Failure::DoesNotResolve && verdict != Verdict::DoesNotResolve)
    }
}

/// Which `last_error` a recorded refusal holds. Only `record_certificate_error` compares it
/// (core/crates/ployzd/src/corrosion/store.rs:1028-1030), and only with the policy's reason.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Reason {
    /// `refusal_reason` for DoesNotResolve (:514).
    DoesNotResolve,
    /// The failed order's error (:570).
    Authority,
    /// The Certificate Policy's refusal (:441).
    Policy,
}

/// `RecordedRefusal` (core/crates/ployzd/src/corrosion/certificate.rs:214-219).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Recorded {
    reason: Reason,
    clock: Option<Clock>,
}

/// The hostname's `CertificateRow::Acme` (core/crates/ployzd/src/corrosion/certificate.rs:221-233),
/// one row that every Machine reads as soon as it is written. Its challenge is left out: no step
/// reads it.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Row {
    material: bool,
    refusal: Option<Recorded>,
}

impl Row {
    /// `CertificateRow::clock` (core/crates/ployzd/src/corrosion/certificate.rs:324-328).
    fn clock(self) -> Option<Clock> {
        self.refusal.and_then(|refusal| refusal.clock)
    }

    /// `CertificateRow::last_error` (core/crates/ployzd/src/corrosion/certificate.rs:318-322).
    fn last_error(self) -> Option<Reason> {
        self.refusal.map(|refusal| refusal.reason)
    }
}

/// What `resolve_certificate_policy` makes of the Certificate Policy row
/// (core/crates/ployz-core/src/domain/certificate_policy.rs:205). An operator stops issuance at
/// most once, and repairs it at most once.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Policy {
    /// The built-in values, with a directory.
    BuiltIn,
    /// A row `resolve_certificate_policy` refuses, such as a challenge kind other than http-01
    /// (core/crates/ployz-core/src/domain/certificate_policy.rs:215-224).
    Refused,
    /// A row whose `directory_url` is empty, which leaves the policy no directory
    /// (core/crates/ployz-core/src/domain/certificate_policy.rs:226-228).
    NoDirectory,
    /// The operator repaired the row: the built-in values again.
    Repaired,
}

/// An `obtain` in flight inside `join_all` (:540-551). What the failure record needs from its
/// pass (:558-571) travels with it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Order {
    /// The failures and last failure of the pass's snapshot clock (:559), which is all of it
    /// `issuance_failure_clock` reads (core/crates/ployz-core/src/domain/issuance.rs:116-119).
    snapshot: Option<(u8, Failure)>,
    /// Ticks since its pass, whose `wall` the failure clock counts from (:470, :561), capped at
    /// `ago_cap`.
    ago: u8,
}

/// One Machine's certificate loop and what its probe sees.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Machine {
    /// What this Machine's next probe sees (:495-501).
    verdict: Verdict,
    /// Its loop's `first_seen` entry for the hostname, as an age in ticks capped at `AGE_CAP`
    /// (:367, :480-486).
    first_seen: Option<u8>,
    /// Its obtain in flight. The loop passes again only once it ends (:538-551).
    obtaining: Option<Order>,
    /// The property's: ticks since this Machine's first pass after its last blocked pass,
    /// capped at `AGE_CAP`. None before its first pass, and after a blocked pass until the next.
    unblocked: Option<u8>,
}

/// What a record returned.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Record {
    /// It wrote the row.
    Wrote,
    /// It returned Err and wrote nothing.
    Lost,
    /// `record_certificate_error` found the same reason and wrote nothing
    /// (core/crates/ployzd/src/corrosion/store.rs:1028-1030).
    Same,
    /// `record_certificate_failure` found material and wrote nothing
    /// (core/crates/ployzd/src/corrosion/store.rs:987-990).
    HasMaterial,
    /// The Machine is not rank 0, so it records no refusal (:513).
    NotRankZero,
}

/// Why the gate let a pass through, read from the pass's snapshot clock and its verdict.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Gate {
    NoClock,
    /// The clock's `next_attempt_at` has passed.
    Passed,
    /// The verdict differs from a refused clock's refusal.
    VerdictChanged,
    /// The clock still holds.
    Held,
}

impl Gate {
    fn of(clock: Option<Clock>, verdict: Verdict) -> Self {
        match clock {
            None => Self::NoClock,
            Some(clock) if clock.left == 0 => Self::Passed,
            Some(clock) if clock.holds(verdict) => Self::Held,
            Some(_) => Self::VerdictChanged,
        }
    }
}

/// The branch the last step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// The policy refusal: `record_certificate_error`, then return (:436-445). `dropped` says
    /// the write replaced a clock; `reset` says the fix dropped a `first_seen` entry.
    PolicyRefused {
        who: usize,
        record: Record,
        dropped: bool,
        reset: bool,
    },
    /// No directory: return (:447-449). `reset` says the fix dropped a `first_seen` entry.
    NoDirectory { who: usize, reset: bool },
    /// A call before the gate returned Err, and `issue_wanted` returned it. `reset` says the
    /// fix dropped a `first_seen` entry.
    Errored { who: usize, reset: bool },
    /// The row holds material: `first_seen` removed (:476), and the hostname skipped (:490-494).
    HasMaterial { who: usize },
    /// The gate answered Nothing: continue (:510). `due` says `action_due` answered Order;
    /// `reset` says the fix dropped `first_seen`.
    Held { who: usize, due: bool, reset: bool },
    /// The gate answered Refuse (:511-531).
    Refused { who: usize, record: Record },
    /// The gate answered Order, and the rank delay has not passed.
    Waiting { who: usize, gate: Gate },
    /// Pushed to `to_order` (:534-536). `early` says it came less than the Machine's rank
    /// delay after its first pass since its last blocked pass.
    Pushed { who: usize, gate: Gate, early: bool },
    /// `obtain`'s recheck found material and returned (:623-635).
    Rechecked { who: usize },
    /// `obtain` ordered, and `publish_certificate` wrote the material (:654-656).
    /// `over_material` says its re-read found material.
    Issued { who: usize, over_material: bool },
    /// `obtain` ordered and failed, and its record ran (:552-578). `from` is the snapshot
    /// clock's last failure; `late` says the order ended ticks after its pass;
    /// `over_material` says its re-read found material.
    Failed {
        who: usize,
        record: Record,
        from: Option<Failure>,
        late: bool,
        over_material: bool,
    },
    /// The daemon restarted. `ended_order` says an obtain was in flight.
    Restarted { who: usize, ended_order: bool },
}

/// How `obtain` ends.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Ends {
    /// The re-read row holds material, and the recheck returns (:623-635).
    Rechecked,
    /// The order succeeds (:637-656).
    Issued,
    /// The order fails; `recorded` says `record_certificate_failure` returned Ok (:565-578).
    Failed { recorded: bool },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Machine `who`'s loop runs one pass of `issue_wanted`. `lost` says the record the pass
    /// makes returns Err.
    Pass { who: usize, lost: bool },
    /// Machine `who`'s loop runs one pass of `issue_wanted`, and a call before the gate returns
    /// Err: a store read (:432, :437-438, :450-451, :457) or `probe_client` (:464).
    Errs(usize),
    /// Machine `who`'s obtain ends.
    Obtain { who: usize, ends: Ends },
    /// One tick, 30 s, passes.
    Tick,
    /// What Machine `who`'s probe sees changes.
    Probe(usize),
    /// Machine `who`'s daemon restarts.
    Restart(usize),
    /// The operator edits the Certificate Policy.
    Operator(Policy),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    row: Row,
    policy: Policy,
    machines: [Machine; MACHINES],
    took: Option<Branch>,
}

impl State {
    fn machine(&self, who: usize) -> Machine {
        *self.machines.get(who).expect("a Machine slot")
    }

    fn machine_mut(&mut self, who: usize) -> &mut Machine {
        self.machines.get_mut(who).expect("a Machine slot")
    }

    /// A pushed hostname came less than its Machine's rank delay after the Machine's first pass
    /// since its last blocked pass.
    fn early(&self) -> bool {
        matches!(self.took, Some(Branch::Pushed { early: true, .. }))
    }

    /// A pass pushed the hostname while its snapshot's clock held under its verdict.
    fn ordered_in_hold(&self) -> bool {
        matches!(
            self.took,
            Some(Branch::Pushed {
                gate: Gate::Held,
                ..
            })
        )
    }

    /// An obtain ordered though the row it re-read held material.
    fn ordered_over_material(&self) -> bool {
        matches!(
            self.took,
            Some(
                Branch::Issued {
                    over_material: true,
                    ..
                } | Branch::Failed {
                    over_material: true,
                    ..
                }
            )
        )
    }
}

/// Which model a `COVERAGE` row is reached on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    /// Only with the fix.
    Fix,
}

/// One `sometimes`: a branch the code handles, read from the state that took it.
struct Coverage {
    name: &'static str,
    reach: Reach,
    reached: fn(&AcmeIssuance, &State) -> bool,
}

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

/// Each branch the bounds reach, on both models or only with the fix.
/// `current_code_breaks_only_the_rank_delay` and `the_fix_keeps_the_rank_delay` expect every row
/// their model reaches.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "the policy refusal records its reason and drops the row's clock",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::PolicyRefused {
                    record: Record::Wrote,
                    dropped: true,
                    ..
                })
            ) && state.row.clock().is_none()
        },
    },
    Coverage {
        name: "the policy refusal writes nothing over its own reason",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::PolicyRefused {
                    record: Record::Same,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the policy refusal's record_certificate_error returns Err",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::PolicyRefused {
                    record: Record::Lost,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "with no directory, the pass returns before the gate",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::NoDirectory { .. })),
    },
    Coverage {
        name: "a call before the gate returns Err",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Errored { .. })),
    },
    Coverage {
        name: "a pass skips the hostname once the row holds material",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::HasMaterial { .. })),
    },
    Coverage {
        name: "rank 1 waits out its rank delay with the gate open",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Waiting { who: 1, .. })),
    },
    Coverage {
        name: "the gate holds a Machine still in its rank delay",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Held { due: false, .. })),
    },
    Coverage {
        name: "the gate holds an Order-due Machine",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Held { due: true, .. })),
    },
    Coverage {
        name: "rank 0 refuses and records the refusal and its clock",
        reach: Reach::Both,
        reached: |_, state| {
            took(
                state,
                Branch::Refused {
                    who: 0,
                    record: Record::Wrote,
                },
            ) && state
                .row
                .clock()
                .is_some_and(|clock| clock.last == Failure::DoesNotResolve)
        },
    },
    Coverage {
        name: "a refusal's record_certificate_failure returns Err",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Refused {
                    record: Record::Lost,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "rank 1 refuses and records nothing, and the row holds no reason",
        reach: Reach::Both,
        reached: |_, state| {
            took(
                state,
                Branch::Refused {
                    who: 1,
                    record: Record::NotRankZero,
                },
            ) && state.row.refusal.is_none()
        },
    },
    Coverage {
        name: "rank 0 pushes the hostname with no clock",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Pushed {
                    who: 0,
                    gate: Gate::NoClock,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "rank 1 pushes the hostname once its rank delay passed",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Pushed {
                    who: 1,
                    early: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a Machine pushes the hostname once the clock passed",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Pushed {
                    gate: Gate::Passed,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a verdict change opens the gate before a refused clock passes",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Pushed {
                    gate: Gate::VerdictChanged,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "obtain's recheck finds material and returns",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Rechecked { .. })),
    },
    Coverage {
        name: "obtain publishes the certificate",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Issued { .. })) && state.row.material,
    },
    Coverage {
        name: "a failed order records an Authority clock",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Failed {
                    record: Record::Wrote,
                    ..
                })
            ) && state
                .row
                .clock()
                .is_some_and(|clock| clock.last == Failure::Authority)
        },
    },
    Coverage {
        name: "a failed order's record_certificate_failure returns Err",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Failed {
                    record: Record::Lost,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a second failure of the same kind doubles the backoff",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(
                    Branch::Failed {
                        record: Record::Wrote,
                        ..
                    } | Branch::Refused {
                        record: Record::Wrote,
                        ..
                    }
                )
            ) && state.row.clock().is_some_and(|clock| clock.failures == 2)
        },
    },
    Coverage {
        name: "a failed order after a refusal counts from one",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Failed {
                    record: Record::Wrote,
                    from: Some(Failure::DoesNotResolve),
                    ..
                })
            ) && state.row.clock().is_some_and(|clock| clock.failures == 1)
        },
    },
    Coverage {
        name: "an order that ends ticks after its pass counts its clock from the pass",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Failed {
                    record: Record::Wrote,
                    late: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "a restart empties first_seen",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Restarted {
                    who,
                    ended_order: false,
                }) if state.machine(who).first_seen.is_none()
            )
        },
    },
    Coverage {
        name: "rank 1 pushes the hostname over a passed clock once its rank delay passed",
        reach: Reach::Both,
        reached: |_, state| {
            took(
                state,
                Branch::Pushed {
                    who: 1,
                    gate: Gate::Passed,
                    early: false,
                },
            )
        },
    },
    Coverage {
        name: "rank 1 pushes the hostname on a changed verdict once its rank delay passed",
        reach: Reach::Both,
        reached: |_, state| {
            took(
                state,
                Branch::Pushed {
                    who: 1,
                    gate: Gate::VerdictChanged,
                    early: false,
                },
            )
        },
    },
    Coverage {
        name: "the fix drops first_seen on a held pass",
        reach: Reach::Fix,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Held { who, reset: true, .. })
                    if state.machine(who).first_seen.is_none()
            )
        },
    },
    Coverage {
        name: "the fix drops first_seen on a pass that returns before the gate",
        reach: Reach::Fix,
        reached: |_, state| {
            matches!(
                state.took,
                Some(
                    Branch::PolicyRefused { who, reset: true, .. }
                        | Branch::NoDirectory { who, reset: true }
                        | Branch::Errored { who, reset: true }
                ) if state.machine(who).first_seen.is_none()
            )
        },
    },
    Coverage {
        name: "a restart ends an obtain in flight",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Restarted {
                    who,
                    ended_order: true,
                }) if state.machine(who).obtaining.is_none()
            )
        },
    },
];

/// A guard taken out, to show the check catches what it prevents. The first four are guards
/// the code has; the last two are parts of the fix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `IssuanceGate::Nothing => continue` (:510) replaced by ordering along
    /// `ClusterRoute::Direct`, the model's only route, as `IssuanceGate::Order` does.
    hold_orders: bool,
    /// `first_seen.remove(hostname);` (:512) deleted.
    refusal_keeps_first_seen: bool,
    /// `if rank == 0` (:513) read as `if true`.
    any_rank_records: bool,
    /// `obtain`'s recheck (:624-635) deleted.
    obtain_skips_recheck: bool,
    /// The fix's drop of `first_seen` on a held pass deleted.
    fix_hold_keeps_first_seen: bool,
    /// The fix's drop of `first_seen` on a pass that returns before the gate deleted.
    fix_early_return_keeps_first_seen: bool,
}

/// Two Machines' certificate loops over one Ingress Hostname.
#[derive(Clone, Copy, Debug, Default)]
struct AcmeIssuance {
    /// The fix: a pass whose gate answers Nothing drops `first_seen`, as a refused pass does
    /// (:512), and so does a pass that returns before the gate, with Ok or Err.
    fix: bool,
    broken: Broken,
}

/// `record_certificate_failure` with no material tried (core/crates/ployzd/src/corrosion/store.rs:977-994):
/// `acme_row` reads the row; with material it writes nothing; otherwise it upserts
/// `with_backoff`, which keeps the material and replaces the refusal
/// (core/crates/ployzd/src/corrosion/certificate.rs:279-291, :364-377). `None` when the write
/// would be lost but none would be made.
fn record_failure(row: &mut Row, reason: Reason, clock: Clock, lost: bool) -> Option<Record> {
    if row.material {
        return (!lost).then_some(Record::HasMaterial);
    }
    if lost {
        return Some(Record::Lost);
    }
    row.refusal = Some(Recorded {
        reason,
        clock: Some(clock),
    });
    Some(Record::Wrote)
}

/// A pass that reached no push decision: the property measures the rank delay from the
/// Machine's next pass.
fn block(machine: &mut Machine) {
    machine.unblocked = None;
}

impl AcmeIssuance {
    /// The fix drops `first_seen` on a held pass.
    fn resets_on_hold(self) -> bool {
        self.fix && !self.broken.fix_hold_keeps_first_seen
    }

    /// The fix drops `first_seen` on a pass that returns before the gate.
    fn resets_on_early_return(self) -> bool {
        self.fix && !self.broken.fix_early_return_keeps_first_seen
    }

    /// A pass that returns before the gate: before `first_seen.retain` (:436-451), or with an
    /// Err from a call after it (:457, :464). The fix drops the entry there; the code keeps it.
    /// Says whether an entry was dropped.
    fn early_return(self, machine: &mut Machine) -> bool {
        block(machine);
        self.resets_on_early_return() && machine.first_seen.take().is_some()
    }

    /// One pass of `issue_wanted` (:424-536) by Machine `who`, for the one hostname. A record
    /// that returns Err writes nothing (`lost`). `None` when `lost` but the pass makes no
    /// record.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per statement of the code the model copies"
    )]
    fn pass(self, state: &mut State, who: usize, lost: bool) -> Option<()> {
        let rank = rank(who);
        let policy = built_in();
        match state.policy {
            // :436-445: every Machine records the policy's reason for every wanted hostname,
            // with `record_certificate_error` (core/crates/ployzd/src/corrosion/store.rs:1020-1033),
            // whose `with_error` keeps the material and replaces the refusal with the reason and
            // no clock (core/crates/ployzd/src/corrosion/certificate.rs:354-377).
            Policy::Refused => {
                let dropped = state.row.clock().is_some();
                let record = if state.row.last_error() == Some(Reason::Policy) {
                    if lost {
                        return None;
                    }
                    Record::Same
                } else if lost {
                    Record::Lost
                } else {
                    state.row.refusal = Some(Recorded {
                        reason: Reason::Policy,
                        clock: None,
                    });
                    Record::Wrote
                };
                let reset = self.early_return(state.machine_mut(who));
                state.took = Some(Branch::PolicyRefused {
                    who,
                    record,
                    dropped: dropped && record == Record::Wrote,
                    reset,
                });
                return Some(());
            }
            // :447-449.
            Policy::NoDirectory => {
                if lost {
                    return None;
                }
                let reset = self.early_return(state.machine_mut(who));
                state.took = Some(Branch::NoDirectory { who, reset });
                return Some(());
            }
            Policy::BuiltIn | Policy::Repaired => {}
        }
        // :450-453: the row is read once; the hostname is wanted, so `retain` keeps its entry.
        let snapshot = state.row;
        let machine = state.machine_mut(who);
        if snapshot.material {
            // :474-476, then :489-494: with material `action_due` answers Nothing in the model.
            if lost {
                return None;
            }
            machine.first_seen = None;
            block(machine);
            state.took = Some(Branch::HasMaterial { who });
            return Some(());
        }
        // :479-486.
        let age = *machine.first_seen.get_or_insert(0);
        // :489.
        let (due, _) = action_due(false, rank, tick() * u32::from(age), &policy);
        // :495-508.
        let verdict = machine.verdict;
        let clock = snapshot.clock();
        let gate = match issuance_gate(
            clock.map(Clock::real),
            verdict.real(),
            now(),
            policy.backoff_base(),
            policy.backoff_cap(),
        ) {
            IssuanceGate::Nothing if self.broken.hold_orders => {
                IssuanceGate::Order(ClusterRoute::Direct)
            }
            gate @ (IssuanceGate::Nothing
            | IssuanceGate::Refuse { .. }
            | IssuanceGate::Order(_)) => gate,
        };
        match gate {
            // :510, and the fix's drop.
            IssuanceGate::Nothing => {
                if lost {
                    return None;
                }
                block(machine);
                let reset = self.resets_on_hold();
                if reset {
                    machine.first_seen = None;
                }
                state.took = Some(Branch::Held {
                    who,
                    due: due == IssuanceAction::Order,
                    reset,
                });
            }
            // :511-531.
            IssuanceGate::Refuse { refusal, clock } => {
                assert_eq!(refusal, Refusal::DoesNotResolve);
                if !self.broken.refusal_keeps_first_seen {
                    machine.first_seen = None;
                }
                block(machine);
                let record = if rank == 0 || self.broken.any_rank_records {
                    record_failure(
                        &mut state.row,
                        Reason::DoesNotResolve,
                        Clock::of(clock),
                        lost,
                    )?
                } else if lost {
                    return None;
                } else {
                    Record::NotRankZero
                };
                state.took = Some(Branch::Refused { who, record });
            }
            // :532-536.
            IssuanceGate::Order(route) => {
                if lost {
                    return None;
                }
                assert_eq!(route, ClusterRoute::Direct);
                let since = *machine.unblocked.get_or_insert(0);
                let gate = Gate::of(clock, verdict);
                state.took = Some(if due == IssuanceAction::Order {
                    machine.obtaining = Some(Order {
                        snapshot: clock.map(|clock| (clock.failures, clock.last)),
                        ago: 0,
                    });
                    Branch::Pushed {
                        who,
                        gate,
                        early: usize::from(since) < rank,
                    }
                } else {
                    Branch::Waiting { who, gate }
                });
            }
        }
        Some(())
    }

    /// `obtain` (:614-658) and the failure record after `join_all` (:552-578), as one step.
    fn obtain(self, state: &mut State, who: usize, ends: Ends) -> Option<()> {
        let order = state.machine(who).obtaining?;
        // :623.
        let reread = state.row;
        // :624-635: with material, `issuance_action` answers Nothing in the model.
        let rechecks = reread.material && !self.broken.obtain_skips_recheck;
        let over_material = reread.material;
        let policy = built_in();
        state.took = Some(match (rechecks, ends) {
            (true, Ends::Rechecked) => Branch::Rechecked { who },
            (true, Ends::Issued | Ends::Failed { .. }) | (false, Ends::Rechecked) => return None,
            // :637-656: `publish_certificate` writes `CertificateRow::issued`, material with no
            // challenge and no refusal (core/crates/ployzd/src/corrosion/store.rs:851-865,
            // core/crates/ployzd/src/corrosion/certificate.rs:255-259).
            (false, Ends::Issued) => {
                state.row = Row {
                    material: true,
                    refusal: None,
                };
                Branch::Issued { who, over_material }
            }
            (false, Ends::Failed { recorded }) => {
                // :558-564, with the pass's snapshot clock and wall.
                let wall = now() - tick() * u32::from(order.ago);
                let clock = issuance_failure_clock(
                    order.snapshot.map(|(failures, last)| {
                        IssuanceClock::new(u32::from(failures), wall, last.real())
                    }),
                    IssuanceFailure::Authority,
                    wall,
                    policy.backoff_base(),
                    policy.backoff_cap(),
                );
                // :565-578: the snapshot held no material (:568-569).
                let record = record_failure(
                    &mut state.row,
                    Reason::Authority,
                    Clock::of(clock),
                    !recorded,
                )?;
                Branch::Failed {
                    who,
                    record,
                    from: order.snapshot.map(|(_, last)| last),
                    late: order.ago > 0,
                    over_material,
                }
            }
        });
        state.machine_mut(who).obtaining = None;
        Some(())
    }

    /// 30 s pass: every age grows and every clock's ticks left shrink.
    fn tick(state: &mut State) {
        if let Some(Recorded {
            clock: Some(clock), ..
        }) = &mut state.row.refusal
        {
            clock.left = clock.left.saturating_sub(1);
        }
        let ago_cap = ago_cap();
        for machine in &mut state.machines {
            machine.first_seen = machine.first_seen.map(|age| (age + 1).min(AGE_CAP));
            machine.unblocked = machine.unblocked.map(|age| (age + 1).min(AGE_CAP));
            if let Some(order) = &mut machine.obtaining {
                order.ago = (order.ago + 1).min(ago_cap);
            }
        }
    }

    /// A daemon restart: `run` builds an empty `first_seen` (:367), and an obtain in flight
    /// ends with nothing recorded.
    fn restart(state: &mut State, who: usize) {
        let machine = state.machine_mut(who);
        let ended_order = machine.obtaining.take().is_some();
        machine.first_seen = None;
        state.took = Some(Branch::Restarted { who, ended_order });
    }
}

impl Model for AcmeIssuance {
    type State = State;
    type Action = Action;

    /// No row, the built-in policy, and both Machines' probes reaching the Cluster directly,
    /// before either loop's first pass.
    fn init_states(&self) -> Vec<State> {
        let machine = Machine {
            verdict: Verdict::Direct,
            first_seen: None,
            obtaining: None,
            unblocked: None,
        };
        vec![State {
            row: Row::default(),
            policy: Policy::BuiltIn,
            machines: [machine; MACHINES],
            took: None,
        }]
    }

    /// Each Machine's pass or obtain in slot order, then the tick, each probe change, each
    /// restart and the operator's edits. The order decides only which of the shortest paths a
    /// story shows. Nothing is fair: a container change wakes a loop at any time (:402-409), so
    /// a pass is offered at any tick, and a Machine may also skip ticks.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for who in 0..MACHINES {
            if state.machine(who).obtaining.is_some() {
                actions.extend(
                    [
                        Ends::Rechecked,
                        Ends::Issued,
                        Ends::Failed { recorded: true },
                        Ends::Failed { recorded: false },
                    ]
                    .map(|ends| Action::Obtain { who, ends }),
                );
            } else {
                actions.extend([false, true].map(|lost| Action::Pass { who, lost }));
                actions.push(Action::Errs(who));
            }
        }
        actions.push(Action::Tick);
        actions.extend((0..MACHINES).map(Action::Probe));
        actions.extend(
            (0..MACHINES)
                .filter(|who| {
                    let machine = state.machine(*who);
                    machine.first_seen.is_some() || machine.obtaining.is_some()
                })
                .map(Action::Restart),
        );
        match state.policy {
            Policy::BuiltIn => actions.extend([
                Action::Operator(Policy::Refused),
                Action::Operator(Policy::NoDirectory),
            ]),
            Policy::Refused | Policy::NoDirectory => {
                actions.push(Action::Operator(Policy::Repaired));
            }
            Policy::Repaired => {}
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        state.took = None;
        match action {
            Action::Pass { who, lost } => self.pass(&mut state, who, lost)?,
            Action::Errs(who) => {
                let reset = self.early_return(state.machine_mut(who));
                state.took = Some(Branch::Errored { who, reset });
            }
            Action::Obtain { who, ends } => self.obtain(&mut state, who, ends)?,
            Action::Tick => Self::tick(&mut state),
            Action::Probe(who) => {
                let machine = state.machine_mut(who);
                machine.verdict = machine.verdict.flipped();
            }
            Action::Restart(who) => Self::restart(&mut state, who),
            Action::Operator(policy) => state.policy = policy,
        }
        state
            .row
            .clock()
            .is_none_or(|clock| u32::from(clock.failures) <= MAX_FAILURES)
            .then_some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(RANK_DELAY_KEPT, |_, state: &State| !state.early()),
            Property::always(HOLD_KEPT, |_, state: &State| !state.ordered_in_hold()),
            Property::always(NO_ORDER_OVER_MATERIAL, |_, state: &State| {
                !state.ordered_over_material()
            }),
        ];
        properties.extend(
            COVERAGE
                .iter()
                .filter(|row| self.fix || row.reach == Reach::Both)
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

const RANK_DELAY_KEPT: &str = "a Machine of rank r pushes the hostname to to_order only r issuance steps or more after its first pass since its last blocked pass";
const HOLD_KEPT: &str = "no pass pushes the hostname to to_order while its snapshot's clock holds and its verdict leaves the clock standing";
const NO_ORDER_OVER_MATERIAL: &str =
    "no obtain orders while the row it re-reads holds material that is not due for renewal";

/// The name a story gives Machine `who`.
fn name(who: usize) -> String {
    format!("Rank {}", rank(who))
}

/// `ticks` ticks as seconds, "or more" where the age is capped.
fn age(ticks: u8) -> String {
    let seconds = u64::from(ticks) * tick().as_secs();
    if ticks == AGE_CAP && ticks > 0 {
        format!("{seconds} s or more")
    } else {
        format!("{seconds} s")
    }
}

fn clock_words(clock: Clock) -> String {
    format!(
        "{} clock, {} failure{}, {}",
        match clock.last {
            Failure::DoesNotResolve => "a DoesNotResolve",
            Failure::Authority => "an Authority",
        },
        clock.failures,
        if clock.failures == 1 { "" } else { "s" },
        if clock.left == 0 {
            "passed".to_owned()
        } else {
            format!("{} s left", u64::from(clock.left) * tick().as_secs())
        }
    )
}

fn row_words(row: Row) -> String {
    format!(
        "{}, {}",
        if row.material {
            "material"
        } else {
            "no material"
        },
        match row.refusal {
            None => "no reason".to_owned(),
            Some(Recorded { reason, clock }) => format!(
                "reason {reason:?}, {}",
                clock.map_or_else(|| "no clock".to_owned(), clock_words)
            ),
        }
    )
}

fn machine_words(who: usize, machine: Machine) -> String {
    format!(
        "{}: probe {:?}, first_seen {}, {}, unblocked {}",
        name(who),
        machine.verdict,
        machine.first_seen.map_or_else(|| "none".to_owned(), age),
        machine.obtaining.map_or_else(
            || "no obtain".to_owned(),
            |order| format!("obtain in flight from a pass {} ago", age_of(order.ago))
        ),
        machine.unblocked.map_or_else(|| "none".to_owned(), age),
    )
}

/// An order's `ticks` since its pass as seconds, "or more" where `ago_cap` caps it.
fn age_of(ticks: u8) -> String {
    let seconds = u64::from(ticks) * tick().as_secs();
    if ticks == ago_cap() {
        format!("{seconds} s or more")
    } else {
        format!("{seconds} s")
    }
}

/// `path` in the code's words: each step, at the time it ran, then the row and each Machine.
fn story(model: &AcmeIssuance, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the action
    // that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    let mut elapsed = 0_u64;
    for (n, (state, action)) in path.iter().enumerate() {
        if matches!(led_by, Some((_, Action::Tick))) {
            elapsed += tick().as_secs();
        }
        let what = led_by.map_or_else(
            || {
                if n == 0 && model.init_states().contains(state) {
                    "start: no row, the built-in policy, and no pass yet".to_owned()
                } else {
                    "from the state above".to_owned()
                }
            },
            |(before, action)| reads_as(model, &before, action, state),
        );
        // A row that skips a guard is reached by a flag or by a direct edit to the model; it
        // names Broken only when a flag is set.
        let what = if model.broken == Broken::default() {
            what.replace(" (under Broken)", "")
        } else {
            what
        };
        led_by = action.map(|action| (*state, action));
        let machines: Vec<String> = (0..MACHINES)
            .map(|who| machine_words(who, state.machine(who)))
            .collect();
        let _ = writeln!(
            story,
            "{n:2}. [{elapsed} s] {what}\n    row: {} | policy {:?} | {}",
            row_words(state.row),
            state.policy,
            machines.join(" | "),
        );
    }
    story
}

/// What a pass found in `first_seen`.
fn first_seen_words(before: Machine) -> String {
    match before.first_seen {
        None => "no first_seen, so it is inserted now (:480-485)".to_owned(),
        Some(ticks) => format!("first_seen is {} old (:480-486)", age(ticks)),
    }
}

fn verdict_words(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Direct => "its probe sees the hostname reach the Cluster directly (:495-501)",
        Verdict::DoesNotResolve => "its probe finds the hostname does not resolve (:495-501)",
    }
}

fn gate_words(gate: Gate) -> &'static str {
    match gate {
        Gate::NoClock => "there is no clock",
        Gate::Passed => "the clock has passed",
        Gate::VerdictChanged => "the verdict differs from the clock's refusal",
        Gate::Held => "the clock holds",
    }
}

/// What the fix did on a pass that returned before the gate.
fn reset_words(reset: bool) -> &'static str {
    if reset {
        "; the fix drops first_seen"
    } else {
        ""
    }
}

/// What `obtain`'s re-read found, and what the recheck made of it.
fn reread_words(over_material: bool) -> &'static str {
    if over_material {
        "the re-read row holds material, but the recheck is skipped (under Broken)"
    } else {
        "the re-read row holds no material (:623-624)"
    }
}

/// `poll_wait` (:195-207) after a pass or an obtain of Machine `who`, from `after`.
fn wait_words(who: usize, after: &State) -> String {
    let machine = after.machine(who);
    let elapsed = tick() * u32::from(machine.first_seen.unwrap_or(0));
    format!(
        "poll_wait answers {} s (:195-207)",
        poll_wait(after.row.material, rank(who), elapsed, &built_in()).as_secs()
    )
}

/// `action` from `before`, read with `after` for the branch it took.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement of the code the model copies"
)]
fn reads_as(model: &AcmeIssuance, before: &State, action: Action, after: &State) -> String {
    let who = match action {
        Action::Tick => return "Clock: 30 s pass".to_owned(),
        Action::Probe(who) => {
            return format!(
                "DNS: {}'s probe now {}",
                name(who),
                match after.machine(who).verdict {
                    Verdict::Direct => "sees the hostname reach the Cluster directly",
                    Verdict::DoesNotResolve => "finds the hostname does not resolve",
                }
            );
        }
        Action::Operator(policy) => {
            return match policy {
                Policy::Refused => "Operator: sets a Certificate Policy that resolve_certificate_policy refuses, such as a challenge kind other than http-01 (core/crates/ployz-core/src/domain/certificate_policy.rs:215-224)".to_owned(),
                Policy::NoDirectory => "Operator: sets a Certificate Policy whose directory_url is empty, so the policy has no directory (core/crates/ployz-core/src/domain/certificate_policy.rs:226-228)".to_owned(),
                Policy::Repaired => "Operator: repairs the Certificate Policy, which resolves to the built-in values again".to_owned(),
                Policy::BuiltIn => unreachable!("no edit restores the start"),
            };
        }
        Action::Restart(who)
        | Action::Errs(who)
        | Action::Pass { who, .. }
        | Action::Obtain { who, .. } => who,
    };
    let machine = before.machine(who);
    let what = match after.took {
        Some(Branch::Restarted { ended_order, .. }) => format!(
            "the daemon restarts, and run builds an empty first_seen (:367){}; the loop starts again once the daemon participates (core/crates/ployzd/src/daemon.rs:494-507)",
            if ended_order {
                "; the obtain in flight ends with nothing recorded"
            } else {
                ""
            }
        ),
        Some(Branch::PolicyRefused { record, dropped, reset, .. }) => format!(
            "pass: resolve_certificate_policy refuses the policy, so record_certificate_error records its reason (:431-443, core/crates/ployzd/src/corrosion/store.rs:1020-1033): {}; the pass returns before first_seen.retain and the gate{}, and run sleeps 60 s ({})",
            match record {
                Record::Wrote if dropped =>
                    "with_error keeps the material and replaces the refusal with the reason and no clock, dropping the clock (core/crates/ployzd/src/corrosion/certificate.rs:354-377)",
                Record::Wrote =>
                    "with_error keeps the material and replaces the refusal with the reason and no clock (core/crates/ployzd/src/corrosion/certificate.rs:354-377)",
                Record::Same =>
                    "the row already holds that reason, so it writes nothing (core/crates/ployzd/src/corrosion/store.rs:1028-1030)",
                Record::Lost => "it returns Err and writes nothing",
                Record::HasMaterial | Record::NotRankZero => unreachable!("the policy refusal records with record_certificate_error"),
            },
            reset_words(reset),
            if record == Record::Lost {
                "the Err, :394-400"
            } else {
                ":444"
            }
        ),
        Some(Branch::NoDirectory { reset, .. }) => format!(
            "pass: the policy has no directory, so the pass returns before first_seen.retain and the gate{}, and run sleeps 60 s (:447-449)",
            reset_words(reset)
        ),
        Some(Branch::Errored { reset, .. }) => format!(
            "pass: a call before the gate returns Err, so issue_wanted returns it before the gate{}, and run prints \"failed to obtain certificates: <error>\" and waits 60 s (:394-400)",
            reset_words(reset)
        ),
        Some(Branch::HasMaterial { .. }) => "pass: the row holds material, so first_seen is removed (:474-476), and with no renewal due the hostname is skipped (:489-494)".to_owned(),
        Some(Branch::Held { due, reset, .. }) => format!(
            "pass: {}, and action_due answers {} (:489, :95-104); {}; issuance_gate answers Nothing: {} (core/crates/ployz-core/src/domain/issuance.rs:76-83), so continue (:510), and {}; {}",
            first_seen_words(machine),
            if due { "Order" } else { "Nothing" },
            verdict_words(machine.verdict),
            before.row.clock().map_or_else(String::new, clock_words),
            if reset {
                "the fix drops first_seen"
            } else {
                "first_seen stays"
            },
            wait_words(who, after)
        ),
        Some(Branch::Refused { record, .. }) => format!(
            "pass: {}; {}; issuance_gate answers Refuse with {} (core/crates/ployz-core/src/domain/issuance.rs:84-95), so {}; {}; {}",
            first_seen_words(machine),
            verdict_words(machine.verdict),
            after
                .row
                .clock()
                .filter(|_| record == Record::Wrote)
                .map_or_else(|| "a clock".to_owned(), clock_words),
            if model.broken.refusal_keeps_first_seen {
                "first_seen stays (under Broken)"
            } else {
                "first_seen is removed (:512)"
            },
            match record {
                Record::Wrote if who != 0 => "it records the reason and the clock with record_certificate_failure, though it is not rank 0 (under Broken)",
                Record::Wrote => "rank 0 records the reason and the clock with record_certificate_failure (:513-528)",
                Record::Lost if who != 0 => "its record_certificate_failure returns Err and writes nothing, though it is not rank 0 (under Broken)",
                Record::Lost => "rank 0's record_certificate_failure returns Err and writes nothing, which is only logged (:513-528)",
                Record::NotRankZero => "it is not rank 0, so it records nothing (:513)",
                Record::HasMaterial | Record::Same => unreachable!("the pass's snapshot held no material"),
            },
            wait_words(who, after)
        ),
        Some(Branch::Waiting { gate, .. }) => format!(
            "pass: {}, and action_due answers Nothing: its rank delay has not passed (:489, :95-104); {}; issuance_gate answers Order, as {} (core/crates/ployz-core/src/domain/issuance.rs:76-85), and the hostname is not pushed (:534); {}",
            first_seen_words(machine),
            verdict_words(machine.verdict),
            gate_words(gate),
            wait_words(who, after)
        ),
        Some(Branch::Pushed { gate, early, .. }) => format!(
            "pass: {}, and action_due answers Order (:489, :95-104); {}; {}; the hostname is pushed to to_order (:534-536){}, and account and obtain are next (:538-551)",
            first_seen_words(machine),
            verdict_words(machine.verdict),
            if gate == Gate::Held {
                "issuance_gate answers Nothing, as the clock holds, but the pass orders along Direct as Order would (under Broken)".to_owned()
            } else {
                format!(
                    "issuance_gate answers Order, as {} (core/crates/ployz-core/src/domain/issuance.rs:76-85)",
                    gate_words(gate)
                )
            },
            if early {
                ", less than its rank delay after its first pass since its last blocked pass"
            } else {
                ""
            }
        ),
        Some(Branch::Rechecked { .. }) => "obtain: certificate_row re-reads the row, which holds material, and the recheck returns (:623-635)".to_owned(),
        Some(Branch::Issued { over_material, .. }) => format!(
            "obtain: {}, so it orders and is issued, and publish_certificate writes the material with no challenge and no refusal (:637-656, core/crates/ployzd/src/corrosion/store.rs:851-865, core/crates/ployzd/src/corrosion/certificate.rs:255-259); the next pass skips the hostname (:476, :490-494)",
            reread_words(over_material)
        ),
        Some(Branch::Failed {
            record,
            over_material,
            ..
        }) => {
            let ago = machine.obtaining.map_or(0, |order| order.ago);
            format!(
                "obtain: {}, so it orders, and the order fails; issue_wanted prints \"failed to obtain certificate for <hostname>: <error>\" (:553-557), and issuance_failure_clock counts from the pass's wall{} and its snapshot clock (:558-564); {}; {}",
                reread_words(over_material),
                if ago == 0 {
                    String::new()
                } else {
                    format!(", {} back,", age_of(ago))
                },
                match record {
                    Record::Wrote => format!(
                        "record_certificate_failure writes {} (:565-573, core/crates/ployzd/src/corrosion/store.rs:987-994)",
                        after.row.clock().map_or_else(String::new, clock_words)
                    ),
                    Record::Lost => "record_certificate_failure returns Err and writes nothing, which is only logged (:574-578)".to_owned(),
                    Record::HasMaterial => "record_certificate_failure finds material and writes nothing (core/crates/ployzd/src/corrosion/store.rs:987-990)".to_owned(),
                    Record::Same | Record::NotRankZero => unreachable!("an authority failure records with record_certificate_failure"),
                },
                if after.row.material {
                    "the next pass skips the hostname (:476, :490-494)".to_owned()
                } else {
                    wait_words(who, after)
                }
            )
        }
        None => unreachable!("{action:?} records its branch"),
    };
    format!("{}: {what}", name(who))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use stateright::{Checker, Expectation, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    const R0: usize = 0;
    const R1: usize = 1;

    /// The code as it is.
    fn as_is() -> AcmeIssuance {
        AcmeIssuance::default()
    }

    /// The fix: a held pass, and a pass that returns before the gate, drop `first_seen`.
    fn fixed() -> AcmeIssuance {
        AcmeIssuance {
            fix: true,
            ..as_is()
        }
    }

    fn as_is_without(broken: Broken) -> AcmeIssuance {
        AcmeIssuance { broken, ..as_is() }
    }

    fn fixed_without(broken: Broken) -> AcmeIssuance {
        AcmeIssuance { broken, ..fixed() }
    }

    /// Every state `model` reaches. An empty `AnyOf` never matches, so the checker does not
    /// stop at its discoveries, and the state count is the same from run to run.
    fn check(model: AcmeIssuance) -> impl Checker<AcmeIssuance> {
        model
            .checker()
            .threads(4)
            .finish_when(HasDiscoveries::AnyOf(BTreeSet::new()))
            .spawn_bfs()
            .join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so no
    /// shorter path breaks it, printed as a story under the test's name. For a `sometimes`,
    /// such as a `COVERAGE` row's name, it is the first path that reaches it. It panics when no
    /// path does, as for an `always` that holds. The checker's own discovery is not that path:
    /// `check_block` stores every violation it visits, each overwriting the last, so a visitor
    /// keeps the first. One thread keeps the path the same from run to run.
    fn breaks(model: AcmeIssuance, property: &'static str) -> Path {
        first_break(model, property, |_| true, true).expect("a path breaks the property")
    }

    /// As `breaks`, the first such path that also satisfies `and`. It explores the whole
    /// model, since the first discovery may not pass `and`. The checker keeps one path to
    /// each state, so `and` sees only the first path to the state it ends in.
    fn breaks_where(
        model: AcmeIssuance,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Path {
        first_break(model, property, and, false).expect("a path breaks the property")
    }

    /// As `breaks_where`, or `None` when no path breaks `property` and satisfies `and`.
    fn first_break(
        model: AcmeIssuance,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
        stop_at_discovery: bool,
    ) -> Option<Path> {
        let named = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property");
        let reached = matches!(named.expectation, Expectation::Sometimes);
        let holds = named.condition;
        let first = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&first);
        let checker =
            model
                .checker()
                .threads(1)
                .visitor(move |path: stateright::Path<State, Action>| {
                    let mut first = seen.lock().expect("the first break");
                    if first.is_none() && holds(&model, path.last_state()) == reached {
                        let path = path.into_vec();
                        if and(&path) {
                            *first = Some(path);
                        }
                    }
                });
        // The default, `HasDiscoveries::All`, stops once every property has a discovery.
        // An empty `AnyOf` never matches, so the checker visits every state.
        let checker = checker.finish_when(if stop_at_discovery {
            HasDiscoveries::AllOf([property].into())
        } else {
            HasDiscoveries::AnyOf(BTreeSet::new())
        });
        checker.spawn_bfs().join();
        let path = first.lock().expect("the first break").take()?;
        let test = std::thread::current();
        println!(
            "[{}] {property} {} under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            if reached { "is reached" } else { "fails" },
            story(&model, &path)
        );
        Some(path)
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    /// Steps `actions` from the start under `model`, each one offered where it is taken, and
    /// prints the path as a story under the test's name.
    fn follow(model: AcmeIssuance, actions: &[Action]) -> Path {
        let mut state = model
            .init_states()
            .into_iter()
            .next()
            .expect("a start state");
        let mut path = Vec::new();
        for &action in actions {
            let mut offered = Vec::new();
            model.actions(&state, &mut offered);
            assert!(offered.contains(&action), "{action:?} is offered");
            let next = model.next_state(&state, action).expect("a next state");
            path.push((state, Some(action)));
            state = next;
        }
        path.push((state, None));
        let test = std::thread::current();
        println!(
            "[{}] a schedule under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            story(&model, &path)
        );
        path
    }

    /// The branch each step took, in order.
    fn branches(path: &Path) -> Vec<Branch> {
        path.iter().filter_map(|(state, _)| state.took).collect()
    }

    const fn pass(who: usize) -> Action {
        Action::Pass { who, lost: false }
    }

    /// The finding's schedule, with each pass at a time `poll_wait` allows. At 0 s, rank 0's
    /// pass pushes the hostname, and its order fails; the record writes an Authority clock,
    /// 60 s out, and `poll_wait` answers 30 s. Rank 1's pass inserts `first_seen` and is held;
    /// `poll_wait` answers 30 s. At 30 s both are held, rank 1 now Order-due, and both
    /// `poll_wait`s answer `RANK_STEP`, 30 s (:204). At 60 s the clock has passed, and both
    /// push the hostname.
    const THE_HOLD: [Action; 9] = [
        pass(R0),
        Action::Obtain {
            who: R0,
            ends: Ends::Failed { recorded: true },
        },
        pass(R1),
        Action::Tick,
        pass(R0),
        pass(R1),
        Action::Tick,
        pass(R0),
        pass(R1),
    ];

    /// The finding. Rank 0's order fails, and its record writes an Authority clock, 60 s out
    /// (:552-578). Rank 1's pass inserts `first_seen` (:483), and the gate holds it (:510). A
    /// tick later rank 1 is Order-due and still held, and `first_seen` stays. Once the clock
    /// passes, rank 0 and rank 1 both push the hostname in the same tick, though rank 1 has
    /// waited no rank delay since its last held pass.
    ///
    /// The checker keeps one path to each state, the first in BFS order. A shorter path ends
    /// in the same state, so the test replays this schedule with `follow`, and then asks the
    /// checker for its first path to the same broken state.
    #[test]
    fn current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold() {
        let path = follow(as_is(), &THE_HOLD);
        assert_eq!(
            branches(&path),
            [
                Branch::Pushed {
                    who: R0,
                    gate: Gate::NoClock,
                    early: false
                },
                Branch::Failed {
                    who: R0,
                    record: Record::Wrote,
                    from: None,
                    late: false,
                    over_material: false
                },
                Branch::Held {
                    who: R1,
                    due: false,
                    reset: false
                },
                Branch::Held {
                    who: R0,
                    due: true,
                    reset: false
                },
                Branch::Held {
                    who: R1,
                    due: true,
                    reset: false
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::Passed,
                    early: false
                },
                Branch::Pushed {
                    who: R1,
                    gate: Gate::Passed,
                    early: true
                },
            ]
        );
        let recorded = path
            .get(2)
            .expect("the failure record's state")
            .0
            .row
            .clock()
            .expect("the record wrote a clock");
        assert_eq!(
            recorded,
            Clock {
                failures: 1,
                left: 2,
                last: Failure::Authority
            }
        );
        let broken = last(&path);
        assert!(broken.early());
        assert_eq!(broken.machine(R1).first_seen, Some(AGE_CAP));
        assert!(broken.machine(R0).obtaining.is_some());
        assert!(broken.machine(R1).obtaining.is_some());
        let found = breaks_where(as_is(), RANK_DELAY_KEPT, move |path| last(path) == broken);
        assert_eq!(last(&found), broken);
    }

    /// The verdict change's schedule. Both probes find the hostname does not resolve. At 0 s,
    /// rank 0 refuses and records a DoesNotResolve clock, 60 s out (:511-528), and `poll_wait`
    /// answers 30 s, since `first_seen` was removed (:512). Rank 1's pass inserts `first_seen`,
    /// and the gate holds it; `poll_wait` answers 30 s. DNS is fixed, and at 30 s both probes
    /// reach the Cluster, which differs from the clock's refusal, so the gate opens with 30 s
    /// still on the clock (core/crates/ployz-core/src/domain/issuance.rs:77-83).
    const THE_VERDICT_CHANGE: [Action; 9] = [
        Action::Probe(R0),
        Action::Probe(R1),
        pass(R0),
        pass(R1),
        Action::Tick,
        Action::Probe(R0),
        Action::Probe(R1),
        pass(R0),
        pass(R1),
    ];

    /// The second route. A DoesNotResolve clock holds rank 1 while its `first_seen` ages. When
    /// the verdict changes, `issuance_gate` opens before the clock passes (issuance.rs:77-83),
    /// and rank 0 and rank 1 push the hostname in the same tick.
    #[test]
    fn current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold() {
        let path = follow(as_is(), &THE_VERDICT_CHANGE);
        assert_eq!(
            branches(&path),
            [
                Branch::Refused {
                    who: R0,
                    record: Record::Wrote
                },
                Branch::Held {
                    who: R1,
                    due: false,
                    reset: false
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::VerdictChanged,
                    early: false
                },
                Branch::Pushed {
                    who: R1,
                    gate: Gate::VerdictChanged,
                    early: true
                },
            ]
        );
        let broken = last(&path);
        assert!(broken.early());
        assert_eq!(
            broken.row.clock(),
            Some(Clock {
                failures: 1,
                left: 1,
                last: Failure::DoesNotResolve
            }),
            "the clock had not passed"
        );
        let found = breaks_where(as_is(), RANK_DELAY_KEPT, move |path| last(path) == broken);
        assert_eq!(last(&found), broken);
    }

    /// The policy refusal's schedule. At 0 s rank 1's pass inserts `first_seen` and waits out
    /// its rank delay; `poll_wait` answers 30 s. The operator sets a policy the daemon refuses.
    /// At 30 s rank 1's pass records the policy's reason and returns before the gate, and `run`
    /// sleeps 60 s (:444). The operator repairs the policy. At 90 s rank 0's loop makes its
    /// first pass and pushes the hostname, and so does rank 1's.
    const THE_POLICY_REFUSAL: [Action; 9] = [
        pass(R1),
        Action::Operator(Policy::Refused),
        Action::Tick,
        pass(R1),
        Action::Tick,
        Action::Operator(Policy::Repaired),
        Action::Tick,
        pass(R0),
        pass(R1),
    ];

    /// A policy refusal returns before `first_seen.retain` and the gate (:436-445), so
    /// `first_seen` keeps aging through it, and its `record_certificate_error` drops any clock.
    /// Once the policy is repaired, rank 0 and rank 1 push the hostname in the same tick.
    #[test]
    fn current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal() {
        let path = follow(as_is(), &THE_POLICY_REFUSAL);
        assert_eq!(
            branches(&path),
            [
                Branch::Waiting {
                    who: R1,
                    gate: Gate::NoClock
                },
                Branch::PolicyRefused {
                    who: R1,
                    record: Record::Wrote,
                    dropped: false,
                    reset: false
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::NoClock,
                    early: false
                },
                Branch::Pushed {
                    who: R1,
                    gate: Gate::NoClock,
                    early: true
                },
            ]
        );
        let broken = last(&path);
        assert!(broken.early());
        let found = breaks_where(as_is(), RANK_DELAY_KEPT, move |path| last(path) == broken);
        assert_eq!(last(&found), broken);
    }

    /// The error's schedule. At 0 s rank 1's pass inserts `first_seen` and waits out its rank
    /// delay; `poll_wait` answers 30 s. At 30 s a call before the gate returns Err, and `run`
    /// waits `RETRY_INTERVAL`, 60 s (:394-400). At 90 s rank 0's loop makes its first pass and
    /// pushes the hostname, and so does rank 1's.
    const THE_ERROR: [Action; 7] = [
        pass(R1),
        Action::Tick,
        Action::Errs(R1),
        Action::Tick,
        Action::Tick,
        pass(R0),
        pass(R1),
    ];

    /// A pass that returns Err before the gate keeps `first_seen`, as the policy refusal does,
    /// so it ages through the error. Rank 0 and rank 1 then push the hostname in the same tick.
    #[test]
    fn current_code_lets_rank_1_order_with_rank_0_after_an_error_before_the_gate() {
        let path = follow(as_is(), &THE_ERROR);
        assert_eq!(
            branches(&path),
            [
                Branch::Waiting {
                    who: R1,
                    gate: Gate::NoClock
                },
                Branch::Errored {
                    who: R1,
                    reset: false
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::NoClock,
                    early: false
                },
                Branch::Pushed {
                    who: R1,
                    gate: Gate::NoClock,
                    early: true
                },
            ]
        );
        let broken = last(&path);
        assert!(broken.early());
        let found = breaks_where(as_is(), RANK_DELAY_KEPT, move |path| last(path) == broken);
        assert_eq!(last(&found), broken);
    }

    /// The code as it is breaks only `RANK_DELAY_KEPT`, and reaches every row that is not
    /// `Reach::Fix`. The checker's first broken path is printed too.
    #[test]
    fn current_code_breaks_only_the_rank_delay() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(RANK_DELAY_KEPT).is_some());
        assert!(checker.discovery(HOLD_KEPT).is_none(), "{HOLD_KEPT} breaks");
        assert!(
            checker.discovery(NO_ORDER_OVER_MATERIAL).is_none(),
            "{NO_ORDER_OVER_MATERIAL} breaks"
        );
        assert!(last(&breaks(as_is(), RANK_DELAY_KEPT)).early());
        for row in COVERAGE.iter().filter(|row| row.reach == Reach::Both) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The gate's hold (:510). With `continue` read as an order along Direct, a pass orders
    /// while the clock holds. A held DoesNotResolve verdict has no route to order along, so the
    /// break orders along Direct, the model's only route; the second path shows it.
    #[test]
    fn the_gate_holds_while_the_clock_holds() {
        let model = as_is_without(Broken {
            hold_orders: true,
            ..Broken::default()
        });
        let path = breaks(model, HOLD_KEPT);
        assert!(last(&path).ordered_in_hold());
        let path = breaks_where(model, HOLD_KEPT, |path| {
            let [.., (before, Some(Action::Pass { who, .. })), (_, None)] = path.as_slice() else {
                return false;
            };
            before.machine(*who).verdict == Verdict::DoesNotResolve
        });
        let broken = last(&path);
        assert!(broken.ordered_in_hold());
        assert_eq!(
            broken.row.clock().map(|clock| clock.last),
            Some(Failure::DoesNotResolve)
        );
    }

    /// `obtain`'s recheck (:624-635). Without it, an obtain whose re-read finds material that
    /// another Machine published orders anyway.
    #[test]
    fn obtain_rechecks_material() {
        let path = breaks(
            as_is_without(Broken {
                obtain_skips_recheck: true,
                ..Broken::default()
            }),
            NO_ORDER_OVER_MATERIAL,
        );
        assert!(last(&path).ordered_over_material());
    }

    /// Rank 1 refuses with no clock recorded yet, its verdict changes, and it pushes the
    /// hostname a tick later. `poll_wait` after the refused pass answers 30 s.
    const REFUSED_THEN_REACHED: [Action; 5] = [
        Action::Probe(R1),
        pass(R1),
        Action::Tick,
        Action::Probe(R1),
        pass(R1),
    ];

    /// The refusal reset (:512). Under the fix without it, rank 1 refuses with no clock
    /// recorded yet, the verdict changes, and rank 1 is then Order-due at once. With it, rank 1
    /// waits out its rank delay.
    #[test]
    fn a_refusal_restarts_the_rank_delay() {
        let model = fixed_without(Broken {
            refusal_keeps_first_seen: true,
            ..Broken::default()
        });
        let path = follow(model, &REFUSED_THEN_REACHED);
        assert_eq!(
            branches(&path),
            [
                Branch::Refused {
                    who: R1,
                    record: Record::NotRankZero
                },
                Branch::Pushed {
                    who: R1,
                    gate: Gate::NoClock,
                    early: true
                },
            ]
        );
        assert!(last(&breaks(model, RANK_DELAY_KEPT)).early());
        let path = follow(fixed(), &REFUSED_THEN_REACHED);
        assert_eq!(
            last(&path).took,
            Some(Branch::Waiting {
                who: R1,
                gate: Gate::NoClock
            })
        );
    }

    const RANK_1_RECORDS_NOTHING: &str =
        "rank 1 refuses and records nothing, and the row holds no reason";

    /// Only rank 0 records a refusal (:513). No `always` property here needs that: with any
    /// rank recording, the fix still keeps all three. What changes is that the row where rank 1
    /// refuses and records nothing goes unreached, and every other row is still reached.
    #[test]
    fn only_rank_zero_records_a_refusal() {
        let model = fixed_without(Broken {
            any_rank_records: true,
            ..Broken::default()
        });
        let checker = check(model);
        println!(
            "fixed, any rank records: {} states",
            checker.unique_state_count()
        );
        for property in model.properties() {
            let found = checker.discovery(property.name).is_some();
            match property.expectation {
                Expectation::Always => assert!(!found, "{} breaks", property.name),
                Expectation::Sometimes => assert_eq!(
                    found,
                    property.name != RANK_1_RECORDS_NOTHING,
                    "{}",
                    property.name
                ),
                Expectation::Eventually => unreachable!("the model has no eventually"),
            }
        }
    }

    /// The fix holds all three `always` properties, and every `sometimes` it reaches is
    /// reached.
    #[test]
    fn the_fix_keeps_the_rank_delay() {
        let checker = check(fixed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Each finding's schedule under the fix. Rank 1's last pass inserts `first_seen` and waits
    /// out its rank delay, since the held, refused, policy-refused or errored pass before it
    /// dropped the entry; a tick later it pushes the hostname.
    #[test]
    fn the_fix_waits_on_each_finding_s_schedule() {
        for schedule in [
            &THE_HOLD[..],
            &THE_VERDICT_CHANGE[..],
            &THE_POLICY_REFUSAL[..],
            &THE_ERROR[..],
        ] {
            let path = follow(fixed(), schedule);
            assert!(matches!(
                last(&path).took,
                Some(Branch::Waiting { who: R1, .. })
            ));
            let mut later = schedule.to_vec();
            later.extend([Action::Tick, pass(R1)]);
            let path = follow(fixed(), &later);
            assert!(matches!(
                last(&path).took,
                Some(Branch::Pushed {
                    who: R1,
                    early: false,
                    ..
                })
            ));
        }
    }

    /// Without the drop on a held pass, the fix lets the authority hold and the verdict change
    /// through.
    #[test]
    fn the_fix_needs_its_hold_reset() {
        let model = fixed_without(Broken {
            fix_hold_keeps_first_seen: true,
            ..Broken::default()
        });
        assert!(last(&follow(model, &THE_HOLD)).early());
        assert!(last(&follow(model, &THE_VERDICT_CHANGE)).early());
        let path = breaks(model, RANK_DELAY_KEPT);
        assert!(path.iter().any(|(state, _)| matches!(
            state.took,
            Some(Branch::Held {
                who: R1,
                reset: false,
                ..
            })
        )));
    }

    /// Without the drop on a pass that returns before the gate, a policy refusal lets rank 1
    /// order with rank 0 once it is repaired, and so do a policy with no directory and an Err
    /// before the gate. The second schedule is the first with the policy left without a
    /// directory.
    #[test]
    fn the_fix_needs_its_early_return_reset() {
        let model = fixed_without(Broken {
            fix_early_return_keeps_first_seen: true,
            ..Broken::default()
        });
        assert!(last(&follow(model, &THE_POLICY_REFUSAL)).early());
        let no_directory = THE_POLICY_REFUSAL.map(|action| {
            if action == Action::Operator(Policy::Refused) {
                Action::Operator(Policy::NoDirectory)
            } else {
                action
            }
        });
        let path = follow(model, &no_directory);
        assert!(
            path.iter()
                .any(|(state, _)| matches!(state.took, Some(Branch::NoDirectory { who: R1, .. })))
        );
        assert!(last(&path).early());
        assert!(last(&follow(model, &THE_ERROR)).early());
        assert!(last(&breaks(model, RANK_DELAY_KEPT)).early());
    }

    /// A restart empties `first_seen` (:367), so it ends the finding's route: on the hold's
    /// schedule, rank 1's daemon restarts before its last pass, and that pass inserts `first_seen`
    /// and waits out its rank delay.
    #[test]
    fn a_restart_restarts_the_rank_delay() {
        let mut schedule = THE_HOLD.to_vec();
        schedule.insert(schedule.len() - 1, Action::Restart(R1));
        let path = follow(as_is(), &schedule);
        assert_eq!(
            last(&path).took,
            Some(Branch::Waiting {
                who: R1,
                gate: Gate::Passed
            })
        );
    }

    /// One pass by `who` from `state` under `model`, with no lost record.
    fn passed(model: AcmeIssuance, state: &State, who: usize) -> State {
        model
            .next_state(state, pass(who))
            .expect("the pass is taken")
    }

    /// The fix's drop on each state a pass can meet, beside the code's: no clock; an Authority
    /// clock not yet due; an unchanged DoesNotResolve clock not yet due; a clock that is due; a
    /// DoesNotResolve clock whose verdict changed; material present, where the pass skips at
    /// :490-494; and rank 0, which owes no delay.
    #[test]
    fn the_fix_on_each_edge_state() {
        let start = as_is()
            .init_states()
            .into_iter()
            .next()
            .expect("a start state");
        let clocked = |last: Failure, left: u8| Row {
            material: false,
            refusal: Some(Recorded {
                reason: match last {
                    Failure::DoesNotResolve => Reason::DoesNotResolve,
                    Failure::Authority => Reason::Authority,
                },
                clock: Some(Clock {
                    failures: 1,
                    left,
                    last,
                }),
            }),
        };
        // Rank 1, past its rank delay since its first pass and since its first_seen.
        let aged = |row: Row, verdict: Verdict| {
            let mut state = State { row, ..start };
            let machine = state.machine_mut(R1);
            machine.verdict = verdict;
            machine.first_seen = Some(AGE_CAP);
            machine.unblocked = Some(AGE_CAP);
            state
        };
        let open = [
            (
                "no clock",
                aged(Row::default(), Verdict::Direct),
                Gate::NoClock,
            ),
            (
                "a clock that is due",
                aged(clocked(Failure::Authority, 0), Verdict::Direct),
                Gate::Passed,
            ),
            (
                "a DoesNotResolve clock whose verdict changed",
                aged(clocked(Failure::DoesNotResolve, 2), Verdict::Direct),
                Gate::VerdictChanged,
            ),
        ];
        for (name, state, gate) in open {
            for model in [as_is(), fixed()] {
                let after = passed(model, &state, R1);
                assert_eq!(
                    after.took,
                    Some(Branch::Pushed {
                        who: R1,
                        gate,
                        early: false
                    }),
                    "{name}"
                );
                assert_eq!(after.machine(R1).first_seen, Some(AGE_CAP), "{name}");
            }
        }
        let held = [
            (
                "an Authority clock not yet due",
                aged(clocked(Failure::Authority, 2), Verdict::Direct),
            ),
            (
                "an unchanged DoesNotResolve clock not yet due",
                aged(clocked(Failure::DoesNotResolve, 2), Verdict::DoesNotResolve),
            ),
        ];
        for (name, state) in held {
            let code = passed(as_is(), &state, R1);
            let fix = passed(fixed(), &state, R1);
            for (after, reset) in [(code, false), (fix, true)] {
                assert_eq!(
                    after.took,
                    Some(Branch::Held {
                        who: R1,
                        due: true,
                        reset
                    }),
                    "{name}"
                );
                assert_eq!(after.machine(R1).unblocked, None, "{name}");
            }
            assert_eq!(code.machine(R1).first_seen, Some(AGE_CAP), "{name}");
            assert_eq!(fix.machine(R1).first_seen, None, "{name}");
        }
        let material = aged(
            Row {
                material: true,
                refusal: None,
            },
            Verdict::Direct,
        );
        for model in [as_is(), fixed()] {
            let after = passed(model, &material, R1);
            assert_eq!(after.took, Some(Branch::HasMaterial { who: R1 }));
            assert_eq!(after.machine(R1).first_seen, None);
        }
        // After a held pass under the fix: no first_seen, and no pass since the block.
        let mut dropped = State {
            row: clocked(Failure::Authority, 0),
            ..start
        };
        for who in [R0, R1] {
            let machine = dropped.machine_mut(who);
            machine.first_seen = None;
            machine.unblocked = None;
        }
        let rank_0 = passed(fixed(), &dropped, R0);
        assert_eq!(
            rank_0.took,
            Some(Branch::Pushed {
                who: R0,
                gate: Gate::Passed,
                early: false
            }),
            "rank 0 owes no delay"
        );
        let rank_1 = passed(fixed(), &dropped, R1);
        assert_eq!(
            rank_1.took,
            Some(Branch::Waiting {
                who: R1,
                gate: Gate::Passed
            }),
            "rank 1 owes one step"
        );
    }

    /// One tick is `issuance_step` under the built-in policy, and the built-in backoff base is
    /// a whole number of ticks, so every clock the model records is too.
    #[test]
    fn a_tick_is_one_issuance_step() {
        let policy = built_in();
        assert_eq!(tick(), Duration::from_secs(30));
        assert_eq!(tick(), RANK_STEP);
        assert_eq!(policy.probe_timeout(), RANK_STEP);
        assert_eq!(ticks(policy.backoff_base()), 2);
        assert_eq!(ticks(policy.backoff_cap()), 120);
        assert_eq!(ago_cap(), 4);
    }

    /// The failure record counts from the pass's `wall` (:561), so an order that ends ticks
    /// after its pass writes a clock with that much less left. A second Authority failure backs
    /// off 120 s, `ago_cap`'s 4 ticks: an order 3 ticks late leaves 30 s on it, and one 4 ticks
    /// late leaves none, as does any later one.
    #[test]
    fn an_order_s_clock_counts_from_its_pass() {
        let failed = Action::Obtain {
            who: R0,
            ends: Ends::Failed { recorded: true },
        };
        let mut schedule = vec![pass(R0), failed, Action::Tick, Action::Tick, pass(R0)];
        for (late, left) in [(3, 1), (4, 0), (5, 0)] {
            let mut late_order = schedule.clone();
            late_order.extend(std::iter::repeat_n(Action::Tick, late));
            late_order.push(failed);
            let path = follow(as_is(), &late_order);
            assert_eq!(
                last(&path).row.clock(),
                Some(Clock {
                    failures: 2,
                    left,
                    last: Failure::Authority
                }),
                "an order {late} ticks late"
            );
        }
        schedule.push(failed);
        let on_time = last(&follow(as_is(), &schedule));
        assert_eq!(on_time.row.clock().map(|clock| clock.left), Some(4));
    }
}
