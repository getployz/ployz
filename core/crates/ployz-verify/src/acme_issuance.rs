//! ACME issuance scheduling for one Ingress Hostname with no Certificate Material: when each
//! Machine's certificate loop pushes the hostname to `to_order`, against the backoff clock in
//! the replicated row and each Machine's own Hostname Verdict. A bare `:n` is a line of
//! `core/crates/ployzd/src/certificates.rs`; every other path is written in full from the repo
//! root.
//!
//! Each daemon runs one certificate loop, `run` (:494-550), once it participates
//! (core/crates/ployzd/src/daemon.rs:496-509). Each pass of `issue_wanted` (:632-766) reads the
//! row, and `step` moves the loop-local `HostIssuance` it keeps for the hostname (:211-302, :502,
//! :672-676). The hostname is pushed to `to_order` only when `issuance_gate` lets it through
//! (core/crates/ployz-core/src/domain/issuance.rs:66-98) and `issuance_action` says the
//! Machine's rank delay has passed since `Waiting`'s `orderable_since` (:286-301). The rank is
//! the Machine's place among every `machines` row (:610-615).
//!
//! `orderable_since` is the Machine's first pass the gate let through since the gate last held
//! or refused the hostname, or since a pass stopped before the gate (:286-296, :650-655). Every
//! rank records its own refusal (:678-699).
//!
//! The code before this change, `as_it_was` in the tests, kept the first pass's instant through
//! a hold and through a pass that stopped before the gate, and only rank 0 recorded a refusal.
//! Rank 0's order failed, and its record wrote an Authority clock. Rank 1's passes were held
//! until the clock passed, by when rank 1 was past its rank delay, so it pushed the hostname in
//! the same tick as rank 0.

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime},
};

use ployz_core::{
    CertificatePolicy, ClusterRoute, HostnameVerdict, IssuanceClock, IssuanceFailure, IssuanceGate,
    Refusal, issuance_backoff, issuance_failure_clock, issuance_gate,
};
use stateright::{Model, Property};

/// `CHALLENGE_WAIT` (:42).
const CHALLENGE_WAIT: Duration = Duration::from_secs(30);
/// `RETRY_INTERVAL` (:44).
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// `RANK_STEP` (:46-47).
const RANK_STEP: Duration = CHALLENGE_WAIT;

/// Machines in the model. Slot `who` holds the Machine whose id sorts `who`th, so its rank is
/// `who`.
const MACHINES: usize = 2;
/// The largest rank delay, in ticks. Ages are capped there: past it, every `action_due` answer
/// is the same.
const AGE_CAP: u8 = (MACHINES - 1) as u8;
/// The most failures a recorded clock may count. A step that would record more is not taken.
const MAX_FAILURES: u32 = 2;

/// `machine_rank` (:56-63), over Machine slots, whose order is their ids' order.
fn machine_rank(this: usize, machines: impl IntoIterator<Item = usize>) -> usize {
    machines.into_iter().filter(|id| *id < this).count()
}

/// The rank `machine_rank` gives slot `who` among every `machines` row (:612-615).
fn rank(who: usize) -> usize {
    machine_rank(who, 0..MACHINES)
}

/// `rank_delay` (:81-83).
fn rank_delay(rank: usize, step: Duration) -> Duration {
    step.saturating_mul(u32::try_from(rank).unwrap_or(u32::MAX))
}

/// `issuance_step` (:85-87).
fn issuance_step(policy: &CertificatePolicy) -> Duration {
    RANK_STEP.max(policy.probe_timeout())
}

/// `IssuanceAction` (:49-54) without `Renew`, which only the renewal arm returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssuanceAction {
    Nothing,
    Order,
}

/// `action_due` (:89-124). Its `None` arm (:99-107) is copied. Its `Some(material)` arm is
/// renewal (:108-122), which the model leaves out: with material the model answers as that arm
/// does when `renew_at` cannot read the certificate (:121).
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

/// `poll_wait` (:197-209), which `wake` answers for a `Waiting` hostname (:321-328). Only the
/// story prints it.
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

/// `wake` (:308-342) for a hostname the gate held or refused: until `clock` passes, and no later
/// than the next probe. Only the story prints it.
fn held_wake(clock: Clock) -> Duration {
    let probe = match clock.last {
        Failure::DoesNotResolve => RANK_STEP,
        Failure::Authority => RETRY_INTERVAL,
    };
    (tick() * u32::from(clock.left)).min(probe)
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

/// The instant every pass in the current tick reads as `now` and `wall` (:657-658). Only
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
    /// (core/crates/ployz-core/src/domain/issuance.rs:77-84).
    fn holds(self, verdict: Verdict) -> bool {
        self.left > 0
            && !(self.last == Failure::DoesNotResolve && verdict != Verdict::DoesNotResolve)
    }
}

/// Which `last_error` a recorded refusal holds. Only `record_certificate_error` compares it
/// (core/crates/ployzd/src/corrosion/store.rs:1243-1244), and only with the policy's reason.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Reason {
    /// `refusal_reason` for DoesNotResolve (:681).
    DoesNotResolve,
    /// The failed order's error (:740).
    Authority,
    /// The Certificate Policy's refusal (:589).
    Policy,
}

/// `RecordedRefusal` (core/crates/ployzd/src/corrosion/certificate.rs:214-219).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Recorded {
    reason: Reason,
    clock: Option<Clock>,
}

/// The hostname's `CertificateRow::Acme` (core/crates/ployzd/src/corrosion/certificate.rs:221-235),
/// one row that every Machine reads as soon as it is written. Its challenge is left out: no step
/// reads it.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Row {
    material: bool,
    refusal: Option<Recorded>,
}

impl Row {
    /// `CertificateRow::clock` (core/crates/ployzd/src/corrosion/certificate.rs:326-330).
    fn clock(self) -> Option<Clock> {
        self.refusal.and_then(|refusal| refusal.clock)
    }

    /// `CertificateRow::last_error` (core/crates/ployzd/src/corrosion/certificate.rs:320-324).
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

/// An `obtain` in flight inside `join_all` (:708-719). What the failure record needs from its
/// pass (:726-735) travels with it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Order {
    /// The failures and last failure of the pass's snapshot clock (:727), which is all of it
    /// `issuance_failure_clock` reads (core/crates/ployz-core/src/domain/issuance.rs:117-120).
    snapshot: Option<(u8, Failure)>,
    /// Ticks since its pass, whose `wall` the failure clock counts from (:658, :729), capped at
    /// `ago_cap`.
    ago: u8,
}

/// One Machine's certificate loop and what its probe sees.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Machine {
    /// What this Machine's next probe sees (:665-671).
    verdict: Verdict,
    /// The age of its loop's `orderable_since` for the hostname, in ticks capped at `AGE_CAP`:
    /// `Some` while its `HostIssuance` is `Waiting`, or `Order`, which is past every rank delay
    /// (:286-301). `None` while it is anything else or absent (:502, :650-656).
    orderable_since: Option<u8>,
    /// Its obtain in flight. The loop passes again only once it ends (:706-719).
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
    /// (core/crates/ployzd/src/corrosion/store.rs:1243-1244).
    Same,
    /// `record_certificate_failure` found material and wrote nothing
    /// (core/crates/ployzd/src/corrosion/store.rs:1189-1190).
    HasMaterial,
    /// The Machine is not rank 0, so it records no refusal: only under
    /// `Broken::only_rank_zero_records`.
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
    /// The policy refusal: `record_certificate_error`, then return (:584-599). `dropped` says
    /// the write replaced a clock; `reset` says `hosts.clear` dropped an `orderable_since`.
    PolicyRefused {
        who: usize,
        record: Record,
        dropped: bool,
        reset: bool,
    },
    /// No directory: return (:601-603). `reset` says `hosts.clear` dropped an
    /// `orderable_since`.
    NoDirectory { who: usize, reset: bool },
    /// A call before the gate returned Err, and `issue_wanted` returned it. `reset` says
    /// `hosts.clear` dropped an `orderable_since`.
    Errored { who: usize, reset: bool },
    /// The row holds material whose renewal is not due: `Issued`, and the hostname skipped
    /// (:236-251, :662-663).
    HasMaterial { who: usize },
    /// The gate answered Hold (:277). `due` says the rank delay had passed since the
    /// `orderable_since` the pass found; `reset` says the pass dropped it.
    Held { who: usize, due: bool, reset: bool },
    /// The gate answered Refuse (:278-280, :678-699).
    Refused { who: usize, record: Record },
    /// The gate answered Order, and the rank delay has not passed (:300).
    Waiting { who: usize, gate: Gate },
    /// Pushed to `to_order` (:299, :700-702). `early` says it came less than the Machine's rank
    /// delay after its first pass since its last blocked pass.
    Pushed { who: usize, gate: Gate, early: bool },
    /// `obtain`'s recheck found material and returned (:786-798).
    Rechecked { who: usize },
    /// `obtain` ordered, and `publish_certificate` wrote the material (:654-656).
    /// `over_material` says its re-read found material.
    Issued { who: usize, over_material: bool },
    /// `obtain` ordered and failed, and its record ran (:720-757). `from` is the snapshot
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
    /// The re-read row holds material, and the recheck returns (:786-798).
    Rechecked,
    /// The order succeeds (:800-831).
    Issued,
    /// The order fails; `recorded` says `record_certificate_failure` returned Ok (:736-755).
    Failed { recorded: bool },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Machine `who`'s loop runs one pass of `issue_wanted`. `lost` says the record the pass
    /// makes returns Err.
    Pass { who: usize, lost: bool },
    /// Machine `who`'s loop runs one pass of `issue_wanted`, and a call before the gate returns
    /// Err: a store read (:580, :585-586, :604-605, :610) or `probe_client` (:611).
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

/// Which models a `COVERAGE` row is reached on: every model, or only those that keep or that
/// take out one guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    /// Unless `Broken::hold_keeps_orderable_since`.
    HoldResets,
    /// Unless `Broken::early_return_keeps_orderable_since`.
    EarlyReturnResets,
    /// Unless `Broken::only_rank_zero_records`.
    EveryRankRecords,
    /// Only under `Broken::only_rank_zero_records`, as in the code before this change.
    OnlyRankZeroRecords,
}

impl Reach {
    fn on(self, broken: Broken) -> bool {
        match self {
            Self::Both => true,
            Self::HoldResets => !broken.hold_keeps_orderable_since,
            Self::EarlyReturnResets => !broken.early_return_keeps_orderable_since,
            Self::EveryRankRecords => !broken.only_rank_zero_records,
            Self::OnlyRankZeroRecords => broken.only_rank_zero_records,
        }
    }
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

/// Each branch the bounds reach, on every model or only on those `Reach` names.
/// `the_code_keeps_every_property` and `the_code_as_it_was_breaks_only_the_rank_delay` expect
/// every row their model reaches.
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
        name: "rank 1 refuses and records the refusal and its clock",
        reach: Reach::EveryRankRecords,
        reached: |_, state| {
            took(
                state,
                Branch::Refused {
                    who: 1,
                    record: Record::Wrote,
                },
            ) && state
                .row
                .clock()
                .is_some_and(|clock| clock.last == Failure::DoesNotResolve)
        },
    },
    Coverage {
        name: RANK_1_RECORDS_NOTHING,
        reach: Reach::OnlyRankZeroRecords,
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
            ) && state
                .row
                .clock()
                .is_some_and(|c| c.left < if c.failures == 1 { 2 } else { 4 })
        },
    },
    Coverage {
        name: "a restart empties hosts",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Restarted {
                    who,
                    ended_order: false,
                }) if state.machine(who).orderable_since.is_none()
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
        name: "a held pass drops orderable_since",
        reach: Reach::HoldResets,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Held { who, reset: true, .. })
                    if state.machine(who).orderable_since.is_none()
            )
        },
    },
    Coverage {
        name: "a pass that stops before the gate drops orderable_since",
        reach: Reach::EarlyReturnResets,
        reached: |_, state| {
            matches!(
                state.took,
                Some(
                    Branch::PolicyRefused { who, reset: true, .. }
                        | Branch::NoDirectory { who, reset: true }
                        | Branch::Errored { who, reset: true }
                ) if state.machine(who).orderable_since.is_none()
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

/// A guard taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `IssuanceGate::Hold(clock) => return HostIssuance::Held(clock)` (:277) replaced by
    /// ordering along `ClusterRoute::Direct`, the model's only route, as `IssuanceGate::Order`
    /// does.
    hold_orders: bool,
    /// A pass after a refused one keeps the `orderable_since` before the refusal, where `step`
    /// starts it again (:292).
    refusal_keeps_orderable_since: bool,
    /// A pass after a held one keeps the `orderable_since` before the hold, where `step` starts
    /// it again (:291).
    hold_keeps_orderable_since: bool,
    /// `hosts.clear()` (:653) deleted, so a pass that stops before the gate keeps
    /// `orderable_since`.
    early_return_keeps_orderable_since: bool,
    /// The refusal record (:678-699) made only at rank 0.
    only_rank_zero_records: bool,
    /// `obtain`'s recheck (:787-798) deleted.
    obtain_skips_recheck: bool,
    /// A held or refused hostname wakes the loop at `poll_wait`'s answer, as the code before
    /// this change did, rather than at `wake`'s (:329-339). No property reads it; only the
    /// story's wait does.
    held_wakes_on_rank_step: bool,
}

/// The code before this change: a hold, and a pass that stopped before the gate, kept
/// `orderable_since`; only rank 0 recorded a refusal; and a held or refused hostname woke the
/// loop at `poll_wait`'s answer.
const AS_IT_WAS: Broken = Broken {
    hold_orders: false,
    refusal_keeps_orderable_since: false,
    hold_keeps_orderable_since: true,
    early_return_keeps_orderable_since: true,
    only_rank_zero_records: true,
    obtain_skips_recheck: false,
    held_wakes_on_rank_step: true,
};

const RANK_1_RECORDS_NOTHING: &str =
    "rank 1 refuses and records nothing, and the row holds no reason";

/// Two Machines' certificate loops over one Ingress Hostname.
#[derive(Clone, Copy, Debug, Default)]
struct AcmeIssuance {
    broken: Broken,
}

/// `record_certificate_failure` with no material tried
/// (core/crates/ployzd/src/corrosion/store.rs:1178-1196): `write_acme_row` reads the row; with
/// material it writes nothing; otherwise it writes `with_backoff`, which keeps the material and
/// replaces the refusal (core/crates/ployzd/src/corrosion/certificate.rs:281-293, :366-379).
/// `None` when the write would be lost but none would be made.
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
    /// A pass that stops before the gate: `read_pass` answers `None` or Err (:571-630), and
    /// `issue_wanted` clears `hosts` (:650-655). Says whether an `orderable_since` was dropped.
    fn early_return(self, machine: &mut Machine) -> bool {
        block(machine);
        !self.broken.early_return_keeps_orderable_since && machine.orderable_since.take().is_some()
    }

    /// One pass of `issue_wanted` (:632-705) by Machine `who`, for the one hostname. A record
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
            // :584-599: every Machine records the policy's reason for every wanted hostname,
            // with `record_certificate_error` (core/crates/ployzd/src/corrosion/store.rs:1237-1250),
            // whose `with_error` keeps the material and replaces the refusal with the reason and
            // no clock (core/crates/ployzd/src/corrosion/certificate.rs:356-379).
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
            // :601-603.
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
        // :604-606, :656: the row is read once; the hostname is wanted, so `retain` keeps its
        // entry.
        let snapshot = state.row;
        let machine = state.machine_mut(who);
        if snapshot.material {
            // :662-663: with material `action_due` answers Nothing in the model, so `issued`
            // answers `Issued`.
            if lost {
                return None;
            }
            machine.orderable_since = None;
            block(machine);
            state.took = Some(Branch::HasMaterial { who });
            return Some(());
        }
        // :286-297: the pass's `orderable_since` is the one it found, or this pass.
        let found = machine.orderable_since;
        let age = found.unwrap_or(0);
        let (due, _) = action_due(false, rank, tick() * u32::from(age), &policy);
        // :665-671, :270-276.
        let verdict = machine.verdict;
        let clock = snapshot.clock();
        let gate = match issuance_gate(
            clock.map(Clock::real),
            verdict.real(),
            now(),
            policy.backoff_base(),
            policy.backoff_cap(),
        ) {
            IssuanceGate::Hold(_) if self.broken.hold_orders => {
                IssuanceGate::Order(ClusterRoute::Direct)
            }
            gate @ (IssuanceGate::Hold(_)
            | IssuanceGate::Refuse { .. }
            | IssuanceGate::Order(_)) => gate,
        };
        match gate {
            // :277, and the next pass starts `orderable_since` again (:291).
            IssuanceGate::Hold(_) => {
                if lost {
                    return None;
                }
                block(machine);
                let reset = !self.broken.hold_keeps_orderable_since;
                machine.orderable_since = (!reset).then_some(age);
                state.took = Some(Branch::Held {
                    who,
                    due: due == IssuanceAction::Order,
                    reset: reset && found.is_some(),
                });
            }
            // :278-280, the next pass starts `orderable_since` again (:292), and the record
            // (:678-699).
            IssuanceGate::Refuse { refusal, clock } => {
                assert_eq!(refusal, Refusal::DoesNotResolve);
                machine.orderable_since = self.broken.refusal_keeps_orderable_since.then_some(age);
                block(machine);
                let record = if rank == 0 || !self.broken.only_rank_zero_records {
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
            // :281, :298-301, :700-702.
            IssuanceGate::Order(route) => {
                if lost {
                    return None;
                }
                assert_eq!(route, ClusterRoute::Direct);
                machine.orderable_since = Some(age);
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

    /// `obtain` (:777-833) and the failure record after `join_all` (:720-757), as one step.
    fn obtain(self, state: &mut State, who: usize, ends: Ends) -> Option<()> {
        let order = state.machine(who).obtaining?;
        // :786.
        let reread = state.row;
        // :787-798: with material, `issuance_action` answers Nothing in the model.
        let rechecks = reread.material && !self.broken.obtain_skips_recheck;
        let over_material = reread.material;
        let policy = built_in();
        state.took = Some(match (rechecks, ends) {
            (true, Ends::Rechecked) => Branch::Rechecked { who },
            (true, Ends::Issued | Ends::Failed { .. }) | (false, Ends::Rechecked) => return None,
            // :800-831: `publish_certificate` writes `CertificateRow::issued`, material with no
            // challenge and no refusal (core/crates/ployzd/src/corrosion/store.rs:966-986,
            // core/crates/ployzd/src/corrosion/certificate.rs:259-261).
            (false, Ends::Issued) => {
                state.row = Row {
                    material: true,
                    refusal: None,
                };
                Branch::Issued { who, over_material }
            }
            (false, Ends::Failed { recorded }) => {
                // :726-732, with the pass's snapshot clock and wall.
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
                // :733-755: the snapshot held no material (:733-735).
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
            machine.orderable_since = machine.orderable_since.map(|age| (age + 1).min(AGE_CAP));
            machine.unblocked = machine.unblocked.map(|age| (age + 1).min(AGE_CAP));
            if let Some(order) = &mut machine.obtaining {
                order.ago = (order.ago + 1).min(ago_cap);
            }
        }
    }

    /// A daemon restart: `run` builds an empty `hosts` (:502), and an obtain in flight
    /// ends with nothing recorded.
    fn restart(state: &mut State, who: usize) {
        let machine = state.machine_mut(who);
        let ended_order = machine.obtaining.take().is_some();
        machine.orderable_since = None;
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
            orderable_since: None,
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
    /// story shows. Nothing is fair: a container change wakes a loop at any time (:537-544), so
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
                    machine.orderable_since.is_some() || machine.obtaining.is_some()
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
                .filter(|row| row.reach.on(self.broken))
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
        "{}: probe {:?}, orderable_since {}, {}, unblocked {}",
        name(who),
        machine.verdict,
        machine
            .orderable_since
            .map_or_else(|| "none".to_owned(), age),
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

/// The `orderable_since` a pass found.
fn found_words(before: Machine) -> String {
    match before.orderable_since {
        None => "no orderable_since".to_owned(),
        Some(ticks) => format!("orderable_since is {} old", age(ticks)),
    }
}

/// The `orderable_since` a pass that the gate let through runs its rank delay from.
fn runs_from_words(before: Machine) -> String {
    match before.orderable_since {
        None => "the rank delay runs from this pass (:286-296)".to_owned(),
        Some(ticks) => format!(
            "the rank delay runs from orderable_since, {} old (:286-287)",
            age(ticks)
        ),
    }
}

fn verdict_words(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Direct => "its probe sees the hostname reach the Cluster directly (:665-671)",
        Verdict::DoesNotResolve => "its probe finds the hostname does not resolve (:665-671)",
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

/// What a pass that stopped before the gate did to `orderable_since`.
fn reset_words(model: &AcmeIssuance, reset: bool) -> &'static str {
    if reset {
        "; issue_wanted clears hosts, dropping orderable_since (:650-655)"
    } else if model.broken.early_return_keeps_orderable_since {
        "; hosts is kept (under Broken)"
    } else {
        "; issue_wanted clears hosts (:650-655)"
    }
}

/// What `obtain`'s re-read found, and what the recheck made of it.
fn reread_words(over_material: bool) -> &'static str {
    if over_material {
        "the re-read row holds material, but the recheck is skipped (under Broken)"
    } else {
        "the re-read row holds no material (:786-787)"
    }
}

/// How long Machine `who`'s loop sleeps after the pass or obtain from `before` to `after`,
/// unless a container change wakes it first (:537-544): `wake` (:308-342), or `poll_wait` under
/// `Broken::held_wakes_on_rank_step`.
fn wait_words(model: &AcmeIssuance, who: usize, before: &State, after: &State) -> String {
    let machine = after.machine(who);
    let poll = || {
        let elapsed = tick() * u32::from(machine.orderable_since.unwrap_or(0));
        poll_wait(after.row.material, rank(who), elapsed, &built_in())
    };
    let held = match after.took {
        _ if model.broken.held_wakes_on_rank_step => None,
        Some(Branch::Held { .. }) => before.row.clock(),
        Some(Branch::Refused { .. }) => {
            match issuance_gate(
                before.row.clock().map(Clock::real),
                before.machine(who).verdict.real(),
                now(),
                built_in().backoff_base(),
                built_in().backoff_cap(),
            ) {
                IssuanceGate::Refuse { clock, .. } => Some(Clock::of(clock)),
                IssuanceGate::Hold(_) | IssuanceGate::Order(_) => {
                    unreachable!("the pass refused")
                }
            }
        }
        _ => None,
    };
    match held {
        Some(clock) => format!(
            "wake answers {} s, when the clock passes or at the next probe (:329-339)",
            held_wake(clock).as_secs()
        ),
        None if model.broken.held_wakes_on_rank_step => {
            format!("poll_wait answers {} s (under Broken)", poll().as_secs())
        }
        None => format!("wake answers {} s (:308-342)", poll().as_secs()),
    }
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
    let wait = || wait_words(model, who, before, after);
    let what = match after.took {
        Some(Branch::Restarted { ended_order, .. }) => format!(
            "the daemon restarts, and run builds an empty hosts (:502){}; the loop starts again once the daemon participates (core/crates/ployzd/src/daemon.rs:496-509)",
            if ended_order {
                "; the obtain in flight ends with nothing recorded"
            } else {
                ""
            }
        ),
        Some(Branch::PolicyRefused { record, dropped, reset, .. }) => format!(
            "pass: resolve_certificate_policy refuses the policy, so record_certificate_error records its reason (:579-599, core/crates/ployzd/src/corrosion/store.rs:1237-1250): {}; read_pass stops before the gate{}, and run sleeps 60 s ({})",
            match record {
                Record::Wrote if dropped =>
                    "with_error keeps the material and replaces the refusal with the reason and no clock, dropping the clock (core/crates/ployzd/src/corrosion/certificate.rs:356-379)",
                Record::Wrote =>
                    "with_error keeps the material and replaces the refusal with the reason and no clock (core/crates/ployzd/src/corrosion/certificate.rs:356-379)",
                Record::Same =>
                    "the row already holds that reason, so it writes nothing (core/crates/ployzd/src/corrosion/store.rs:1243-1244)",
                Record::Lost => "it returns Err and writes nothing",
                Record::HasMaterial | Record::NotRankZero => unreachable!("the policy refusal records with record_certificate_error"),
            },
            reset_words(model, reset),
            if record == Record::Lost {
                "the Err, :529-535"
            } else {
                ":654"
            }
        ),
        Some(Branch::NoDirectory { reset, .. }) => format!(
            "pass: the policy has no directory, so read_pass stops before the gate (:601-603){}, and run sleeps 60 s (:654)",
            reset_words(model, reset)
        ),
        Some(Branch::Errored { reset, .. }) => format!(
            "pass: a call before the gate returns Err, so issue_wanted returns it{}, and run prints \"failed to obtain certificates: <error>\" and waits 60 s (:529-535)",
            reset_words(model, reset)
        ),
        Some(Branch::HasMaterial { .. }) => "pass: the row holds material with no renewal due, so issued answers Issued and the hostname is skipped (:236-251, :662-663)".to_owned(),
        Some(Branch::Held { due, .. }) => format!(
            "pass: {}{}; {}; issuance_gate answers Hold: {} (core/crates/ployz-core/src/domain/issuance.rs:77-84), so {}; {}",
            found_words(machine),
            if due { ", past its rank delay" } else { "" },
            verdict_words(machine.verdict),
            before.row.clock().map_or_else(String::new, clock_words),
            if model.broken.hold_keeps_orderable_since {
                "the hostname stays Waiting from that orderable_since (under Broken)"
            } else {
                "the hostname is Held, and the next pass the gate lets through starts orderable_since again (:277, :291)"
            },
            wait()
        ),
        Some(Branch::Refused { record, .. }) => format!(
            "pass: {}; {}; issuance_gate answers Refuse with {} (core/crates/ployz-core/src/domain/issuance.rs:85-97), so {}; {}; {}",
            found_words(machine),
            verdict_words(machine.verdict),
            after
                .row
                .clock()
                .filter(|_| record == Record::Wrote)
                .map_or_else(|| "a clock".to_owned(), clock_words),
            if model.broken.refusal_keeps_orderable_since {
                "the hostname stays Waiting from that orderable_since (under Broken)"
            } else {
                "the hostname is Refused, and the next pass the gate lets through starts orderable_since again (:278-280, :292)"
            },
            match record {
                Record::Wrote => "it records the reason and the clock with record_certificate_failure (:678-699)",
                Record::Lost => "its record_certificate_failure returns Err and writes nothing, which is only logged (:694-697)",
                Record::NotRankZero => "it is not rank 0, so it records nothing (under Broken)",
                Record::HasMaterial | Record::Same => unreachable!("the pass's snapshot held no material"),
            },
            wait()
        ),
        Some(Branch::Waiting { gate, .. }) => format!(
            "pass: {}; issuance_gate answers Order, as {} (core/crates/ployz-core/src/domain/issuance.rs:77-86); {}, and issuance_action answers Nothing, as it has not passed (:297-300, :70-79), so the hostname is Waiting and not pushed; {}",
            verdict_words(machine.verdict),
            gate_words(gate),
            runs_from_words(machine),
            wait()
        ),
        Some(Branch::Pushed { gate, early, .. }) => format!(
            "pass: {}; {}; {}, and issuance_action answers Order (:297-299); the hostname is pushed to to_order (:700-702){}, and account and obtain are next (:706-719)",
            verdict_words(machine.verdict),
            if gate == Gate::Held {
                "issuance_gate answers Hold, as the clock holds, but the pass orders along Direct as Order would (under Broken)".to_owned()
            } else {
                format!(
                    "issuance_gate answers Order, as {} (core/crates/ployz-core/src/domain/issuance.rs:77-86)",
                    gate_words(gate)
                )
            },
            runs_from_words(machine),
            if early {
                ", less than its rank delay after its first pass since its last blocked pass"
            } else {
                ""
            }
        ),
        Some(Branch::Rechecked { .. }) => "obtain: certificate_row re-reads the row, which holds material, and the recheck returns (:786-798)".to_owned(),
        Some(Branch::Issued { over_material, .. }) => format!(
            "obtain: {}, so it orders and is issued, and publish_certificate writes the material with no challenge and no refusal (:800-831, core/crates/ployzd/src/corrosion/store.rs:966-986, core/crates/ployzd/src/corrosion/certificate.rs:259-261); the next pass skips the hostname (:662-663)",
            reread_words(over_material)
        ),
        Some(Branch::Failed {
            record,
            over_material,
            ..
        }) => {
            let ago = machine.obtaining.map_or(0, |order| order.ago);
            format!(
                "obtain: {}, so it orders, and the order fails; issue_wanted prints \"failed to obtain certificate for <hostname>: <error>\" (:721-725), and issuance_failure_clock counts from the pass's wall{} and its snapshot clock (:726-732); {}; {}",
                reread_words(over_material),
                if ago == 0 {
                    String::new()
                } else {
                    format!(", {} back,", age_of(ago))
                },
                match record {
                    Record::Wrote => format!(
                        "record_certificate_failure writes {} (:736-745, core/crates/ployzd/src/corrosion/store.rs:1186-1196)",
                        after.row.clock().map_or_else(String::new, clock_words)
                    ),
                    Record::Lost => "record_certificate_failure returns Err and writes nothing, which is only logged (:752-754)".to_owned(),
                    Record::HasMaterial => "record_certificate_failure finds material and writes nothing (core/crates/ployzd/src/corrosion/store.rs:1189-1190)".to_owned(),
                    Record::Same | Record::NotRankZero => unreachable!("an authority failure records with record_certificate_failure"),
                },
                if after.row.material {
                    "the next pass skips the hostname (:662-663)".to_owned()
                } else {
                    wait()
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

    /// The code before this change.
    fn as_it_was() -> AcmeIssuance {
        without(AS_IT_WAS)
    }

    fn without(broken: Broken) -> AcmeIssuance {
        AcmeIssuance { broken }
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

    /// The authority hold's schedule. At 0 s, rank 0's pass pushes the hostname, and its order
    /// fails; the record writes an Authority clock, 60 s out. Rank 1's pass is held. At 30 s both
    /// are held, and at 60 s the clock has passed. Before this change rank 1's held passes kept
    /// their first pass's instant and `poll_wait` woke it at 30 s and 60 s; now `wake` answers
    /// when the clock passes (:329-339), and a container change can wake a loop at any time
    /// (:537-544).
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

    /// `schedule` then a tick and one more pass by rank 1, which pushes the hostname with its
    /// rank delay kept.
    fn rank_1_pushes_a_tick_later(schedule: &[Action]) {
        let mut later = schedule.to_vec();
        later.extend([Action::Tick, pass(R1)]);
        assert!(matches!(
            last(&follow(as_is(), &later)).took,
            Some(Branch::Pushed {
                who: R1,
                early: false,
                ..
            })
        ));
    }

    /// Rank 0's order fails, and its record writes an Authority clock, 60 s out (:720-757).
    /// Rank 1's passes are held (:277). Once the clock passes, rank 0 pushes the hostname, and
    /// rank 1's first pass the gate lets through starts its rank delay (:291), so it pushes a
    /// tick later. On the code as it was, rank 1's held passes kept `orderable_since`, and it
    /// pushed in the same tick as rank 0, with no rank delay since its last held pass.
    ///
    /// The checker keeps one path to each state, the first in BFS order. A shorter path ends
    /// in the same state, so the test replays the old schedule with `follow`, and then asks the
    /// checker for its first path to the same broken state.
    #[test]
    fn an_authority_hold_restarts_rank_1_s_delay() {
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
                    reset: true
                },
                Branch::Held {
                    who: R1,
                    due: false,
                    reset: false
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::Passed,
                    early: false
                },
                Branch::Waiting {
                    who: R1,
                    gate: Gate::Passed
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
        rank_1_pushes_a_tick_later(&THE_HOLD);

        let path = follow(as_it_was(), &THE_HOLD);
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
        let broken = last(&path);
        assert!(broken.early());
        assert_eq!(broken.machine(R1).orderable_since, Some(AGE_CAP));
        assert!(broken.machine(R0).obtaining.is_some());
        assert!(broken.machine(R1).obtaining.is_some());
        let found = breaks_where(as_it_was(), RANK_DELAY_KEPT, move |path| {
            last(path) == broken
        });
        assert_eq!(last(&found), broken);
    }

    /// The verdict change's schedule. Both probes find the hostname does not resolve. At 0 s,
    /// rank 0 refuses and records a DoesNotResolve clock, 60 s out, and rank 1's pass is held.
    /// DNS is fixed, and at 30 s both probes reach the Cluster, which differs from the clock's
    /// refusal, so the gate opens with 30 s still on the clock
    /// (core/crates/ployz-core/src/domain/issuance.rs:77-84).
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

    /// A DoesNotResolve clock holds rank 1. When the verdict changes, `issuance_gate` opens
    /// before the clock passes, and rank 1's first pass through it starts its rank delay. On the
    /// code as it was, rank 1 pushed in the same tick as rank 0.
    #[test]
    fn a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay() {
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
                Branch::Waiting {
                    who: R1,
                    gate: Gate::VerdictChanged
                },
            ]
        );
        rank_1_pushes_a_tick_later(&THE_VERDICT_CHANGE);

        let path = follow(as_it_was(), &THE_VERDICT_CHANGE);
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
        let found = breaks_where(as_it_was(), RANK_DELAY_KEPT, move |path| {
            last(path) == broken
        });
        assert_eq!(last(&found), broken);
    }

    /// The policy refusal's schedule, with rank 0 on time. At 0 s rank 1's pass waits out its
    /// rank delay. The operator sets a policy the daemon refuses, and rank 0's first pass, also
    /// at 0 s, records the policy's reason and stops before the gate; `run` sleeps 60 s (:654).
    /// At 30 s rank 1's pass stops the same way. The operator repairs the policy. At 60 s rank
    /// 0's sleep ends and it pushes the hostname, and a container change wakes rank 1's loop
    /// at that moment (:537-544).
    const THE_POLICY_REFUSAL: [Action; 9] = [
        pass(R1),
        Action::Operator(Policy::Refused),
        pass(R0),
        Action::Tick,
        pass(R1),
        Action::Operator(Policy::Repaired),
        Action::Tick,
        pass(R0),
        pass(R1),
    ];

    /// A policy refusal stops the pass before the gate, and `issue_wanted` clears `hosts`
    /// (:650-655), so rank 1's next pass starts its rank delay. On the code as it was, rank 1's
    /// `orderable_since` aged through the refusal, and once the policy was repaired rank 0 and
    /// rank 1 pushed the hostname in the same tick.
    #[test]
    fn a_policy_refusal_restarts_rank_1_s_delay() {
        let path = follow(as_is(), &THE_POLICY_REFUSAL);
        assert_eq!(
            branches(&path),
            [
                Branch::Waiting {
                    who: R1,
                    gate: Gate::NoClock
                },
                Branch::PolicyRefused {
                    who: R0,
                    record: Record::Wrote,
                    dropped: false,
                    reset: false
                },
                Branch::PolicyRefused {
                    who: R1,
                    record: Record::Same,
                    dropped: false,
                    reset: true
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::NoClock,
                    early: false
                },
                Branch::Waiting {
                    who: R1,
                    gate: Gate::NoClock
                },
            ]
        );
        rank_1_pushes_a_tick_later(&THE_POLICY_REFUSAL);

        let path = follow(as_it_was(), &THE_POLICY_REFUSAL);
        assert_eq!(
            branches(&path).last(),
            Some(&Branch::Pushed {
                who: R1,
                gate: Gate::NoClock,
                early: true
            })
        );
        let broken = last(&path);
        let found = breaks_where(as_it_was(), RANK_DELAY_KEPT, move |path| {
            last(path) == broken
        });
        assert_eq!(last(&found), broken);
    }

    /// The error's schedule, with rank 0 on time. At 0 s rank 1's pass waits out its rank
    /// delay. Rank 0's first pass, also at 0 s, returns Err before the gate, and `run` waits
    /// `RETRY_INTERVAL`, 60 s (:529-535). At 30 s rank 1's pass returns Err the same way. At
    /// 60 s rank 0's sleep ends and it pushes the hostname, and a container change wakes rank
    /// 1's loop, as in the policy refusal's schedule.
    const THE_ERROR: [Action; 7] = [
        pass(R1),
        Action::Errs(R0),
        Action::Tick,
        Action::Errs(R1),
        Action::Tick,
        pass(R0),
        pass(R1),
    ];

    /// A pass that returns Err before the gate clears `hosts` too (:650-655). On the code as it
    /// was, `orderable_since` aged through the error, and rank 0 and rank 1 then pushed the
    /// hostname in the same tick.
    #[test]
    fn an_error_before_the_gate_restarts_rank_1_s_delay() {
        let path = follow(as_is(), &THE_ERROR);
        assert_eq!(
            branches(&path),
            [
                Branch::Waiting {
                    who: R1,
                    gate: Gate::NoClock
                },
                Branch::Errored {
                    who: R0,
                    reset: false
                },
                Branch::Errored {
                    who: R1,
                    reset: true
                },
                Branch::Pushed {
                    who: R0,
                    gate: Gate::NoClock,
                    early: false
                },
                Branch::Waiting {
                    who: R1,
                    gate: Gate::NoClock
                },
            ]
        );
        rank_1_pushes_a_tick_later(&THE_ERROR);

        let path = follow(as_it_was(), &THE_ERROR);
        assert_eq!(
            branches(&path).last(),
            Some(&Branch::Pushed {
                who: R1,
                gate: Gate::NoClock,
                early: true
            })
        );
        let broken = last(&path);
        let found = breaks_where(as_it_was(), RANK_DELAY_KEPT, move |path| {
            last(path) == broken
        });
        assert_eq!(last(&found), broken);
    }

    /// The code keeps all three `always` properties, and reaches every `sometimes` it has.
    #[test]
    fn the_code_keeps_every_property() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// The code as it was broke only `RANK_DELAY_KEPT`, and reached every row it has. The
    /// checker's first broken path is printed too.
    #[test]
    fn the_code_as_it_was_breaks_only_the_rank_delay() {
        let model = as_it_was();
        let checker = check(model);
        println!("as it was: {} states", checker.unique_state_count());
        assert!(checker.discovery(RANK_DELAY_KEPT).is_some());
        assert!(checker.discovery(HOLD_KEPT).is_none(), "{HOLD_KEPT} breaks");
        assert!(
            checker.discovery(NO_ORDER_OVER_MATERIAL).is_none(),
            "{NO_ORDER_OVER_MATERIAL} breaks"
        );
        assert!(last(&breaks(model, RANK_DELAY_KEPT)).early());
        for row in COVERAGE.iter().filter(|row| row.reach.on(model.broken)) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The gate's hold (:277). With it read as an order along Direct, a pass orders while the
    /// clock holds. A held DoesNotResolve verdict has no route to order along, so the break
    /// orders along Direct, the model's only route; the second path shows it.
    #[test]
    fn the_gate_holds_while_the_clock_holds() {
        let model = without(Broken {
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

    /// `obtain`'s recheck (:787-798). Without it, an obtain whose re-read finds material that
    /// another Machine published orders anyway.
    #[test]
    fn obtain_rechecks_material() {
        let path = breaks(
            without(Broken {
                obtain_skips_recheck: true,
                ..Broken::default()
            }),
            NO_ORDER_OVER_MATERIAL,
        );
        assert!(last(&path).ordered_over_material());
    }

    /// Rank 1 refuses with no clock recorded yet, its verdict changes, and it passes again a
    /// tick later.
    const REFUSED_THEN_REACHED: [Action; 5] = [
        Action::Probe(R1),
        pass(R1),
        Action::Tick,
        Action::Probe(R1),
        pass(R1),
    ];

    /// The refusal's restart (:292). Without it, rank 1 refuses and records a clock, the
    /// verdict changes, and rank 1 is Order-due at once on the `orderable_since` its refused
    /// pass started. With it, rank 1 waits out its rank delay.
    #[test]
    fn a_refusal_restarts_the_rank_delay() {
        let model = without(Broken {
            refusal_keeps_orderable_since: true,
            ..Broken::default()
        });
        let path = follow(model, &REFUSED_THEN_REACHED);
        assert_eq!(
            branches(&path),
            [
                Branch::Refused {
                    who: R1,
                    record: Record::Wrote
                },
                Branch::Pushed {
                    who: R1,
                    gate: Gate::VerdictChanged,
                    early: true
                },
            ]
        );
        assert!(last(&breaks(model, RANK_DELAY_KEPT)).early());
        let path = follow(as_is(), &REFUSED_THEN_REACHED);
        assert_eq!(
            last(&path).took,
            Some(Branch::Waiting {
                who: R1,
                gate: Gate::VerdictChanged
            })
        );
    }

    /// The hold's restart (:291). Without it, the authority hold and the verdict change let
    /// rank 1 push with rank 0.
    #[test]
    fn a_held_pass_restarts_the_rank_delay() {
        let model = without(Broken {
            hold_keeps_orderable_since: true,
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

    /// `hosts.clear()` (:653). Without it, a policy refusal lets rank 1 order with rank 0 once
    /// it is repaired, and so do a policy with no directory and an Err before the gate. The
    /// second schedule is the first with the policy left without a directory.
    #[test]
    fn a_pass_that_stops_before_the_gate_restarts_the_rank_delay() {
        let model = without(Broken {
            early_return_keeps_orderable_since: true,
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
        let path = follow(as_is(), &no_directory);
        assert!(matches!(
            last(&path).took,
            Some(Branch::Waiting { who: R1, .. })
        ));
    }

    /// Every rank records its own refusal (:678-699). No `always` property needs that: with
    /// only rank 0 recording, all three still hold. What changes is the row: rank 1's refusal
    /// leaves it without a reason. Under the break the row where rank 1 records nothing is
    /// reached, and every other row the break leaves in is still reached.
    #[test]
    fn every_rank_records_its_refusal() {
        let refused = [Action::Probe(R1), pass(R1)];
        let after = last(&follow(as_is(), &refused));
        assert_eq!(
            after.took,
            Some(Branch::Refused {
                who: R1,
                record: Record::Wrote
            })
        );
        assert_eq!(after.row.last_error(), Some(Reason::DoesNotResolve));
        let model = without(Broken {
            only_rank_zero_records: true,
            ..Broken::default()
        });
        let after = last(&follow(model, &refused));
        assert_eq!(
            after.took,
            Some(Branch::Refused {
                who: R1,
                record: Record::NotRankZero
            })
        );
        assert_eq!(after.row.refusal, None);
        let checker = check(model);
        println!(
            "only rank 0 records: {} states",
            checker.unique_state_count()
        );
        for property in model.properties() {
            let found = checker.discovery(property.name).is_some();
            match property.expectation {
                Expectation::Always => assert!(!found, "{} breaks", property.name),
                Expectation::Sometimes => assert!(found, "{} is never reached", property.name),
                Expectation::Eventually => unreachable!("the model has no eventually"),
            }
        }
        assert!(
            model
                .properties()
                .iter()
                .any(|property| property.name == RANK_1_RECORDS_NOTHING)
        );
    }

    /// A restart empties `hosts` (:502). On the code as it was, a late restart ended the hold's
    /// route: rank 1's daemon restarted after the gate opened and before its last pass, and
    /// that pass waited out its rank delay. `the_old_route_survived_an_early_restart` is the
    /// early case.
    #[test]
    fn a_late_restart_ended_the_old_route() {
        let mut schedule = THE_HOLD.to_vec();
        schedule.insert(schedule.len() - 1, Action::Restart(R1));
        let path = follow(as_it_was(), &schedule);
        assert_eq!(
            last(&path).took,
            Some(Branch::Waiting {
                who: R1,
                gate: Gate::Passed
            })
        );
    }

    /// On the code as it was, a restart early in the hold did not end the route: the next held
    /// pass started `orderable_since` again, and it aged through the rest of the hold.
    #[test]
    fn the_old_route_survived_an_early_restart() {
        let mut schedule = THE_HOLD.to_vec();
        schedule.insert(3, Action::Restart(R1));
        let path = follow(as_it_was(), &schedule);
        assert_eq!(
            last(&path).took,
            Some(Branch::Pushed {
                who: R1,
                gate: Gate::Passed,
                early: true
            })
        );
    }

    /// The old meeting at the opening was one schedule, not a cadence: with both Machines'
    /// passes 60 s after the gate opened, rank 1 still pushed with rank 0. The code as it is
    /// keeps the rank delay on the same schedule.
    #[test]
    fn the_old_route_survived_a_late_recheck() {
        let mut schedule = THE_HOLD[..7].to_vec();
        schedule.extend([Action::Tick, Action::Tick, pass(R0), pass(R1)]);
        let broken = last(&follow(as_it_was(), &schedule));
        assert!(broken.early());
        assert!(broken.machine(R0).obtaining.is_some());
        assert!(broken.machine(R1).obtaining.is_some());
        let kept = last(&follow(as_is(), &schedule));
        assert!(matches!(kept.took, Some(Branch::Waiting { who: R1, .. })));
    }

    /// One pass by `who` from `state` under `model`, with no lost record.
    fn passed(model: AcmeIssuance, state: &State, who: usize) -> State {
        model
            .next_state(state, pass(who))
            .expect("the pass is taken")
    }

    /// A pass on each state it can meet, under the code and the code as it was: no clock; an
    /// Authority clock not yet due; an unchanged DoesNotResolve clock not yet due; a clock that
    /// is due; a DoesNotResolve clock whose verdict changed; a DoesNotResolve verdict with no
    /// clock; material present, where the pass skips at :662-663; and rank 0, which owes no
    /// delay.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one block per edge state, each beside the code as it was"
    )]
    fn a_pass_on_each_edge_state() {
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
        // Rank 1, past its rank delay since its first pass and since its orderable_since.
        let aged = |row: Row, verdict: Verdict| {
            let mut state = State { row, ..start };
            let machine = state.machine_mut(R1);
            machine.verdict = verdict;
            machine.orderable_since = Some(AGE_CAP);
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
            for model in [as_is(), as_it_was()] {
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
                assert_eq!(after.machine(R1).orderable_since, Some(AGE_CAP), "{name}");
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
            let was = passed(as_it_was(), &state, R1);
            for (after, reset) in [(code, true), (was, false)] {
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
            assert_eq!(code.machine(R1).orderable_since, None, "{name}");
            assert_eq!(was.machine(R1).orderable_since, Some(AGE_CAP), "{name}");
        }
        let refused = aged(Row::default(), Verdict::DoesNotResolve);
        for (model, record, row) in [
            (as_is(), Record::Wrote, Some(Reason::DoesNotResolve)),
            (as_it_was(), Record::NotRankZero, None),
        ] {
            let after = passed(model, &refused, R1);
            assert_eq!(after.took, Some(Branch::Refused { who: R1, record }));
            assert_eq!(after.row.last_error(), row);
            assert_eq!(after.machine(R1).orderable_since, None);
        }
        let material = aged(
            Row {
                material: true,
                refusal: None,
            },
            Verdict::Direct,
        );
        for model in [as_is(), as_it_was()] {
            let after = passed(model, &material, R1);
            assert_eq!(after.took, Some(Branch::HasMaterial { who: R1 }));
            assert_eq!(after.machine(R1).orderable_since, None);
        }
        // After a held pass: no orderable_since, and no pass since the block.
        let mut dropped = State {
            row: clocked(Failure::Authority, 0),
            ..start
        };
        for who in [R0, R1] {
            let machine = dropped.machine_mut(who);
            machine.orderable_since = None;
            machine.unblocked = None;
        }
        let rank_0 = passed(as_is(), &dropped, R0);
        assert_eq!(
            rank_0.took,
            Some(Branch::Pushed {
                who: R0,
                gate: Gate::Passed,
                early: false
            }),
            "rank 0 owes no delay"
        );
        let rank_1 = passed(as_is(), &dropped, R1);
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

    /// The failure record counts from the pass's `wall` (:729), so an order that ends ticks
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
